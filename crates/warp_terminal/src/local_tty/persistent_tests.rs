use std::fs::Permissions;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::thread::JoinHandle;

use nix::sys::stat::Mode;
use nix::unistd::mkfifo;
use tempfile::TempDir;

use super::*;

const TEST_TIMEOUT: Duration = Duration::from_secs(5);

struct Shell {
    runtime: Runtime,
    id: Uuid,
    worker: Option<JoinHandle<Result<()>>>,
    directory: TempDir,
}

impl Shell {
    fn start(history_bytes: usize) -> Self {
        let directory = tempfile::tempdir_in("/private/tmp").unwrap();
        let runtime = Runtime::new(directory.path().join("runtime")).unwrap();
        let id = Uuid::new_v4();
        let worker_runtime = runtime.clone();
        let startup = Startup {
            shell: PathBuf::from("/bin/sh"),
            directory: Some(directory.path().to_owned()),
            name: None,
            size: SizeInfo::new_without_font_metrics(24, 80),
            history_bytes,
        };
        let worker = thread::spawn(move || worker::run(&worker_runtime, id, startup));
        let shell = Self {
            runtime,
            id,
            worker: Some(worker),
            directory,
        };
        shell.wait_for_state(SessionState::Detached);
        shell
    }

    fn attach(&self) -> Attachment {
        let attachment = self.runtime.attach(self.id).unwrap();
        attachment.stream.set_nonblocking(false).unwrap();
        attachment
            .stream
            .set_read_timeout(Some(TEST_TIMEOUT))
            .unwrap();
        attachment
            .stream
            .set_write_timeout(Some(TEST_TIMEOUT))
            .unwrap();
        attachment
    }

    fn script(&self, source: &str) -> PathBuf {
        let path = self.directory.path().join("command.sh");
        fs::write(&path, source).unwrap();
        path
    }

    fn wait_for_state(&self, expected: SessionState) -> Session {
        let deadline = Instant::now() + TEST_TIMEOUT;
        loop {
            let status = self.runtime.status(self.id);
            if let Ok(session) = &status
                && session.state == expected
            {
                return session.clone();
            }
            assert!(
                Instant::now() < deadline,
                "Last terminal state was {status:?}"
            );
            thread::yield_now();
        }
    }

    fn attach_after_output(&self, marker: &str) -> Attachment {
        let deadline = Instant::now() + TEST_TIMEOUT;
        loop {
            let attachment = self.attach();
            if String::from_utf8_lossy(&attachment.replay).contains(marker) {
                return attachment;
            }
            assert!(
                Instant::now() < deadline,
                "Missing detached output {marker:?}"
            );
            drop(attachment);
            self.wait_for_state(SessionState::Detached);
        }
    }
}

impl Drop for Shell {
    fn drop(&mut self) {
        if self.runtime.terminate(self.id).is_ok()
            && let Some(worker) = self.worker.take()
        {
            worker.join().unwrap().unwrap();
        }
    }
}

fn read_through(stream: &mut UnixStream, marker: &str) -> String {
    let deadline = Instant::now() + TEST_TIMEOUT;
    let mut output = Vec::new();
    let mut chunk = [0; 1024];
    loop {
        let count = stream.read(&mut chunk).unwrap_or_else(|err| {
            panic!(
                "PTY read failed before {marker:?}: {err}. Output was {:?}",
                String::from_utf8_lossy(&output)
            )
        });
        assert!(count > 0, "PTY closed before {marker:?}");
        output.extend_from_slice(&chunk[..count]);
        let text = String::from_utf8_lossy(&output);
        if text
            .split_once(marker)
            .is_some_and(|(_, tail)| tail.contains('\n'))
        {
            return text.into_owned();
        }
        assert!(Instant::now() < deadline, "No {marker:?} in {text:?}");
    }
}

fn release_command(path: &Path, input: &str) {
    let deadline = Instant::now() + TEST_TIMEOUT;
    let mut fifo = loop {
        match OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(path)
        {
            Ok(fifo) => break fifo,
            Err(err) if err.raw_os_error() == Some(libc::ENXIO) && Instant::now() < deadline => {
                thread::yield_now();
            }
            Err(err) => panic!("Could not release foreground command: {err}"),
        }
    };
    writeln!(fifo, "{input}").unwrap();
}

#[test]
fn detached_output_replays_from_the_same_command() {
    let shell = Shell::start(32 * 1024);
    let release = shell.directory.path().join("release");
    mkfifo(&release, Mode::S_IRUSR | Mode::S_IWUSR).unwrap();
    let script = shell.script(
        r#"printf "started:%s\n" "$$"
read token < "$1"
printf "detached:%s:%s\n" "$$" "$token"
read token
printf "resumed:%s:%s\n" "$$" "$token"
"#,
    );
    let mut attached = shell.attach();
    writeln!(
        attached.stream,
        "/bin/sh '{}' '{}'",
        script.display(),
        release.display()
    )
    .unwrap();
    let before = read_through(&mut attached.stream, "\nstarted:");
    let command_pid = before
        .lines()
        .find_map(|line| line.strip_prefix("started:"))
        .unwrap()
        .parse::<u32>()
        .unwrap();
    let shell_pid = attached.pid;
    drop(attached);
    assert_eq!(
        shell.wait_for_state(SessionState::Detached).pid,
        Some(shell_pid)
    );

    release_command(&release, "while-away");

    let expected = format!("detached:{command_pid}:while-away");
    let mut reattached = shell.attach_after_output(&expected);
    assert_eq!(reattached.pid, shell_pid);
    writeln!(reattached.stream, "after-return").unwrap();
    read_through(
        &mut reattached.stream,
        &format!("resumed:{command_pid}:after-return"),
    );
}

#[test]
fn reattach_replays_output_larger_than_a_socket_buffer() {
    let shell = Shell::start(256 * 1024);
    let release = shell.directory.path().join("release");
    let done = shell.directory.path().join("done");
    mkfifo(&release, Mode::S_IRUSR | Mode::S_IWUSR).unwrap();
    let script = shell.script(
        r#"printf "ready\n"
read token < "$1"
head -c 200000 /dev/zero | tr '\000' x
printf '\nreplay-complete\n'
touch "$2"
"#,
    );
    let mut attached = shell.attach();
    writeln!(
        attached.stream,
        "/bin/sh '{}' '{}' '{}'",
        script.display(),
        release.display(),
        done.display()
    )
    .unwrap();
    read_through(&mut attached.stream, "\nready");
    drop(attached);
    shell.wait_for_state(SessionState::Detached);

    release_command(&release, "produce-output");
    let deadline = Instant::now() + TEST_TIMEOUT;
    while !done.exists() {
        assert!(
            Instant::now() < deadline,
            "Detached output producer stalled"
        );
        thread::yield_now();
    }
    let mut reattached = shell.attach_after_output("\nreplay-complete\r\n");

    let expected = vec![b'x'; 200_000];
    assert!(!reattached.truncated);
    assert!(
        reattached
            .replay
            .windows(expected.len())
            .any(|window| window == expected)
    );
    writeln!(reattached.stream, "stty size").unwrap();
    read_through(&mut reattached.stream, "24 80");
}

#[test]
fn attach_grants_one_writer_lease() {
    let shell = Shell::start(4096);
    let first = shell.attach();
    let second = shell.runtime.attach(shell.id).unwrap_err();
    assert!(second.to_string().contains("already attached"));

    drop(first);
    shell.wait_for_state(SessionState::Detached);
    assert_eq!(
        shell.attach().pid,
        shell.runtime.status(shell.id).unwrap().pid.unwrap()
    );
}

#[test]
fn attach_reports_truncated_history() {
    let shell = Shell::start(4096);
    let script = shell.script(
        r#"head -c 12000 /dev/zero | tr '\000' x
printf '\nmarker\n'
"#,
    );
    let mut attached = shell.attach();
    writeln!(attached.stream, "/bin/sh '{}'", script.display()).unwrap();
    read_through(&mut attached.stream, "\nmarker");
    drop(attached);
    shell.wait_for_state(SessionState::Detached);

    let reattached = shell.attach();
    assert!(reattached.truncated);
    assert_eq!(reattached.replay.len(), 4096);
    assert!(String::from_utf8_lossy(&reattached.replay).contains("\nmarker"));
}

#[test]
fn resize_changes_the_live_pty_size() {
    let shell = Shell::start(4096);
    let mut attached = shell.attach();
    shell
        .runtime
        .resize(shell.id, &SizeInfo::new_without_font_metrics(37, 103))
        .unwrap();

    writeln!(attached.stream, "stty size").unwrap();
    read_through(&mut attached.stream, "37 103");
}

#[test]
fn shell_exit_delivers_final_output_before_eof() {
    let shell = Shell::start(4096);
    let script = shell.script(
        r#"head -c 200000 /dev/zero | tr '\000' x
printf '\nterminal-ended\n'
"#,
    );
    let mut attached = shell.attach();
    writeln!(attached.stream, "exec /bin/sh '{}'", script.display()).unwrap();

    let mut output = Vec::new();
    attached.stream.read_to_end(&mut output).unwrap();

    assert!(output.ends_with(b"terminal-ended\r\n"));
    assert!(output.len() >= 200_000);
    shell.wait_for_state(SessionState::Terminated);
}

#[test]
fn control_c_reaches_foreground_job() {
    let shell = Shell::start(4096);
    let script = shell.script(
        r#"trap 'printf "\ninterrupted\n"; exit' INT
printf "running\n"
while :; do sleep 1; done
"#,
    );
    let mut attached = shell.attach();
    writeln!(attached.stream, "/bin/sh '{}'", script.display()).unwrap();
    read_through(&mut attached.stream, "\nrunning");

    attached.stream.write_all(&[3]).unwrap();
    read_through(&mut attached.stream, "\ninterrupted");
}

#[test]
fn terminate_reaps_the_shell_and_removes_the_socket() {
    let mut shell = Shell::start(4096);
    let pid = shell.runtime.status(shell.id).unwrap().pid.unwrap();

    shell.runtime.terminate(shell.id).unwrap();
    shell.worker.take().unwrap().join().unwrap().unwrap();

    assert!(shell.runtime.list().unwrap().is_empty());
    assert_eq!(
        nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid as i32), None),
        Err(nix::errno::Errno::ESRCH)
    );
}

#[test]
fn terminate_kills_a_foreground_job_that_ignores_hangup() {
    let mut shell = Shell::start(4096);
    let script = shell.script(
        r#"trap "" HUP
printf "running:%s\n" "$$"
while :; do sleep 1; done
"#,
    );
    let mut attached = shell.attach();
    writeln!(attached.stream, "/bin/sh '{}'", script.display()).unwrap();
    let output = read_through(&mut attached.stream, "\nrunning:");
    let pid = output
        .lines()
        .find_map(|line| line.strip_prefix("running:"))
        .unwrap()
        .parse::<i32>()
        .unwrap();

    shell.runtime.terminate(shell.id).unwrap();
    shell.worker.take().unwrap().join().unwrap().unwrap();

    let deadline = Instant::now() + TEST_TIMEOUT;
    let pid = nix::unistd::Pid::from_raw(pid);
    loop {
        if nix::sys::signal::kill(pid, None) == Err(nix::errno::Errno::ESRCH) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "Foreground process {pid} survived termination"
        );
        thread::yield_now();
    }
}

#[test]
fn list_reports_a_stale_socket_as_failed() {
    let directory = tempfile::tempdir_in("/private/tmp").unwrap();
    let runtime = Runtime::new(directory.path().join("runtime")).unwrap();
    let id = Uuid::new_v4();
    let path = runtime.socket_path(id);
    drop(UnixListener::bind(&path).unwrap());
    fs::set_permissions(path, Permissions::from_mode(0o600)).unwrap();

    assert_eq!(
        runtime.list().unwrap(),
        [Session {
            id,
            pid: None,
            name: None,
            state: SessionState::Failed
        }]
    );
}

#[test]
fn runtime_rejects_a_directory_open_to_other_users() {
    let directory = tempfile::tempdir_in("/private/tmp").unwrap();
    fs::set_permissions(directory.path(), Permissions::from_mode(0o755)).unwrap();

    let error = Runtime::new(directory.path().to_owned()).unwrap_err();

    assert!(error.to_string().contains("mode 0700"));
}

#[test]
fn protocol_version_mismatch_precedes_payload_decoding() {
    let mut frame = Vec::new();
    write_frame(&mut frame, &Request::Status).unwrap();
    frame[7] += 1;
    frame.truncate(12);

    let error = read_frame::<Request>(&mut frame.as_slice()).err().unwrap();

    assert!(error.to_string().contains("incompatible"));
}
