use std::collections::VecDeque;
use std::fs::{self, File, Permissions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::io::{AsRawFd, FromRawFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::Child;
use std::thread;
use std::time::Duration;

use anyhow::{Context as _, Result, ensure};
use command::blocking::Command;
use instant::Instant;
use nix::fcntl::{FcntlArg, OFlag, fcntl};
use nix::sys::signal::{Signal, killpg};
use nix::unistd::{Pid, getsid};
use uuid::Uuid;

use super::{
    Request, Response, Runtime, Session, SessionState, Startup, read_frame, validate_history_bytes,
    validate_name, validate_peer, write_frame,
};
use crate::SizeInfo;
use crate::local_tty::unix::{ToWinsize, spawn_command_in_pty};

const CONTROL_TIMEOUT: Duration = Duration::from_millis(300);
const POLL_TIMEOUT_MS: i32 = 100;
const EXIT_RETENTION: Duration = Duration::from_secs(60);
const KILL_GRACE: Duration = Duration::from_millis(250);
const IO_CHUNK_BYTES: usize = 8 * 1024;
const PTY_READS_PER_TICK: usize = 8;
const MAX_QUEUED_INPUT: usize = 64 * 1024;
const MAX_QUEUED_OUTPUT: usize = 256 * 1024;

struct ControlIo<'a> {
    stream: &'a mut UnixStream,
    deadline: Instant,
}

impl<'a> ControlIo<'a> {
    fn new(stream: &'a mut UnixStream) -> Self {
        Self {
            stream,
            deadline: Instant::now() + CONTROL_TIMEOUT,
        }
    }

    fn remaining(&self) -> io::Result<Option<Duration>> {
        self.deadline
            .checked_duration_since(Instant::now())
            .filter(|timeout| !timeout.is_zero())
            .map(Some)
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::TimedOut,
                    "Persistent terminal control request timed out",
                )
            })
    }
}

impl Read for ControlIo<'_> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.stream.set_read_timeout(self.remaining()?)?;
        self.stream.read(bytes)
    }
}

impl Write for ControlIo<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.stream.set_write_timeout(self.remaining()?)?;
        self.stream.write(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.stream.flush()
    }
}

struct BoundSocket {
    path: PathBuf,
    dev: u64,
    ino: u64,
}

impl BoundSocket {
    fn new(path: PathBuf) -> io::Result<Self> {
        let metadata = fs::symlink_metadata(&path)?;
        Ok(Self {
            path,
            dev: metadata.dev(),
            ino: metadata.ino(),
        })
    }
}

impl Drop for BoundSocket {
    fn drop(&mut self) {
        if fs::symlink_metadata(&self.path)
            .is_ok_and(|metadata| metadata.dev() == self.dev && metadata.ino() == self.ino)
        {
            let _ = fs::remove_file(&self.path);
        }
    }
}

pub(super) fn run(runtime: &Runtime, id: Uuid, startup: Startup) -> Result<()> {
    validate_name(startup.name.as_deref())?;
    validate_history_bytes(startup.history_bytes)?;
    ensure!(
        startup.shell.is_absolute(),
        "Persistent terminal shell must be an absolute path"
    );

    let socket_path = runtime.socket_path(id);
    let listener = UnixListener::bind(&socket_path)?;
    let _socket = BoundSocket::new(socket_path.clone())?;
    fs::set_permissions(socket_path, Permissions::from_mode(0o600))?;
    listener.set_nonblocking(true)?;

    let pty = PtyProcess::spawn(&startup)?;
    let mut worker = Worker {
        id,
        name: startup.name,
        pid: pty.child.id(),
        listener,
        pty: Some(pty),
        client: None,
        input: VecDeque::new(),
        history: History::new(startup.history_bytes),
        retire_at: None,
        stopping: false,
    };
    worker.run()
}

struct PtyProcess {
    child: Child,
    master: Option<File>,
}

impl PtyProcess {
    fn spawn(startup: &Startup) -> Result<Self> {
        validate_size(&startup.size)?;
        let mut command = Command::new(&startup.shell);
        command
            .env("TERM", "xterm-256color")
            .env("TERM_PROGRAM", "WarpTerminal")
            .env("COLORTERM", "truecolor")
            .env("SHELL", &startup.shell);
        if let Some(directory) = &startup.directory {
            command.current_dir(directory);
        } else if let Some(home) = std::env::var_os("HOME") {
            command.current_dir(Path::new(&home));
        }
        let spawned = spawn_command_in_pty(command, &startup.size, false)?;
        let master = unsafe { File::from_raw_fd(spawned.result.leader_fd) };
        let fd = master.as_raw_fd();
        let pty = Self {
            child: spawned.child,
            master: Some(master),
        };
        let flags = OFlag::from_bits_truncate(fcntl(fd, FcntlArg::F_GETFL)?);
        fcntl(fd, FcntlArg::F_SETFL(flags | OFlag::O_NONBLOCK))?;
        Ok(pty)
    }

    fn resize(&self, size: &SizeInfo) -> Result<()> {
        validate_size(size)?;
        let master = self
            .master
            .as_ref()
            .context("Persistent terminal PTY is closed")?;
        let winsize = size.to_winsize();
        if unsafe { libc::ioctl(master.as_raw_fd(), libc::TIOCSWINSZ, &winsize) } < 0 {
            return Err(io::Error::last_os_error()).context("Could not resize persistent terminal");
        }
        Ok(())
    }

    fn stop(&mut self) -> io::Result<()> {
        if self.child.try_wait()?.is_some() {
            return Ok(());
        }

        let shell_group = Pid::from_raw(self.child.id() as i32);
        let foreground_group = self
            .master
            .as_ref()
            .map(|master| unsafe { libc::tcgetpgrp(master.as_raw_fd()) })
            .filter(|&group| group > 0 && group != shell_group.as_raw())
            .map(Pid::from_raw);
        if let Some(group) = foreground_group {
            let _ = killpg(group, Signal::SIGHUP);
        }
        let _ = killpg(shell_group, Signal::SIGHUP);

        // Darwin may delay reporting child exit while the master is still open.
        drop(self.master.take());
        let deadline = Instant::now() + KILL_GRACE;
        while self.child.try_wait()?.is_none() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        if let Some(group) = foreground_group
            && getsid(Some(group)) == Ok(shell_group)
        {
            let _ = killpg(group, Signal::SIGKILL);
        }
        if self.child.try_wait()?.is_some() {
            return Ok(());
        }
        self.child.kill()?;
        let deadline = Instant::now() + KILL_GRACE;
        loop {
            if self.child.try_wait()?.is_some() {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "Persistent shell did not exit",
                ));
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for PtyProcess {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

fn validate_size(size: &SizeInfo) -> Result<()> {
    let range = 1..=u16::MAX as usize;
    ensure!(
        range.contains(&size.rows) && range.contains(&size.columns),
        "Invalid persistent terminal size"
    );
    Ok(())
}

struct Client {
    stream: UnixStream,
    output: VecDeque<u8>,
}

struct Worker {
    id: Uuid,
    name: Option<String>,
    pid: u32,
    listener: UnixListener,
    pty: Option<PtyProcess>,
    client: Option<Client>,
    input: VecDeque<u8>,
    history: History,
    retire_at: Option<Instant>,
    stopping: bool,
}

impl Worker {
    fn run(&mut self) -> Result<()> {
        while !self.stopping
            && self
                .retire_at
                .is_none_or(|deadline| Instant::now() < deadline)
        {
            let master = self.pty.as_ref().and_then(|pty| pty.master.as_ref());
            let mut descriptors = [
                libc::pollfd {
                    fd: self.listener.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: master.map_or(-1, AsRawFd::as_raw_fd),
                    events: libc::POLLIN
                        | if self.input.is_empty() {
                            0
                        } else {
                            libc::POLLOUT
                        },
                    revents: 0,
                },
                libc::pollfd {
                    fd: self
                        .client
                        .as_ref()
                        .map_or(-1, |client| client.stream.as_raw_fd()),
                    events: if self.input.len() < MAX_QUEUED_INPUT {
                        libc::POLLIN
                    } else {
                        0
                    } | if self
                        .client
                        .as_ref()
                        .is_some_and(|client| !client.output.is_empty())
                    {
                        libc::POLLOUT
                    } else {
                        0
                    },
                    revents: 0,
                },
            ];
            if unsafe {
                libc::poll(
                    descriptors.as_mut_ptr(),
                    descriptors.len() as _,
                    POLL_TIMEOUT_MS,
                )
            } < 0
            {
                let err = io::Error::last_os_error();
                if err.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(err).context("Persistent terminal poll failed");
            }

            let [listener, master, client] = descriptors;
            if client.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
                self.detach();
            } else if client.revents & libc::POLLIN != 0 {
                self.read_input();
            }
            if client.revents & libc::POLLOUT != 0 {
                self.send_output();
            }
            if master.revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0
            {
                self.read_output();
            }
            if master.revents & libc::POLLOUT != 0 {
                self.write_input();
            }
            self.reap()?;
            if listener.revents & libc::POLLIN != 0 {
                self.accept_request();
            }
        }
        Ok(())
    }

    fn accept_request(&mut self) {
        let Ok((mut stream, _)) = self.listener.accept() else {
            return;
        };
        if validate_peer(&stream).is_err() || stream.set_nonblocking(false).is_err() {
            return;
        }
        let request = match read_frame(&mut ControlIo::new(&mut stream)) {
            Ok(request) => request,
            Err(_) => {
                let _ = write_frame(
                    &mut ControlIo::new(&mut stream),
                    &Response::Error(
                        "Invalid or incompatible persistent terminal request".to_owned(),
                    ),
                );
                return;
            }
        };
        if matches!(request, Request::Attach) {
            self.attach(stream);
        } else {
            let response = self.respond(request);
            let _ = write_frame(&mut ControlIo::new(&mut stream), &response);
        }
    }

    fn respond(&mut self, request: Request) -> Response {
        match request {
            Request::Status => Response::Status(self.status()),
            Request::Rename(name) => {
                if let Err(err) = validate_name(name.as_deref()) {
                    return Response::Error(err.to_string());
                }
                self.name = name;
                Response::Status(self.status())
            }
            Request::Terminate => {
                if let Some(pty) = &mut self.pty
                    && pty.stop().is_err()
                {
                    return Response::Error("Could not terminate persistent terminal".to_owned());
                }
                self.stopping = true;
                Response::Ok
            }
            Request::Resize(size) => {
                let result = self
                    .pty
                    .as_ref()
                    .context("Persistent terminal has exited")
                    .and_then(|pty| pty.resize(&size));
                match result {
                    Ok(()) => Response::Ok,
                    Err(err) => Response::Error(err.to_string()),
                }
            }
            Request::Attach => unreachable!("Attachment needs to take ownership of its stream"),
        }
    }

    fn status(&self) -> Session {
        let state = match &self.pty {
            None => SessionState::Terminated,
            Some(pty) if pty.master.is_none() => SessionState::Failed,
            Some(_) if self.client.is_some() => SessionState::Attached,
            Some(_) => SessionState::Detached,
        };
        Session {
            id: self.id,
            name: self.name.clone(),
            pid: Some(self.pid),
            state,
        }
    }

    fn attach(&mut self, mut stream: UnixStream) {
        let error = match self.status().state {
            SessionState::Attached => Some("Persistent terminal is already attached"),
            SessionState::Terminated => Some("Persistent terminal has exited"),
            SessionState::Failed => Some("Persistent terminal PTY is unavailable"),
            SessionState::Detached => None,
        };
        if let Some(error) = error {
            let _ = write_frame(
                &mut ControlIo::new(&mut stream),
                &Response::Error(error.to_owned()),
            );
            return;
        }
        let response = Response::Attached {
            pid: self.pid,
            replay_len: self.history.bytes.len(),
            truncated: self.history.truncated,
        };
        let (first, second) = self.history.bytes.as_slices();
        let mut writer = ControlIo::new(&mut stream);
        if write_frame(&mut writer, &response).is_ok()
            && writer.write_all(first).is_ok()
            && writer.write_all(second).is_ok()
            && stream.set_read_timeout(None).is_ok()
            && stream.set_write_timeout(None).is_ok()
            && stream.set_nonblocking(true).is_ok()
        {
            self.client = Some(Client {
                stream,
                output: VecDeque::new(),
            });
        }
    }

    fn detach(&mut self) {
        self.client = None;
        self.input.clear();
    }

    fn read_input(&mut self) {
        let Some(client) = &mut self.client else {
            return;
        };
        let mut bytes = [0; IO_CHUNK_BYTES];
        let count = (MAX_QUEUED_INPUT - self.input.len()).min(bytes.len());
        match client.stream.read(&mut bytes[..count]) {
            Ok(0) => self.detach(),
            Ok(n) => self.input.extend(&bytes[..n]),
            Err(err) if would_retry(&err) => {}
            Err(_) => self.detach(),
        }
    }

    fn write_input(&mut self) {
        let Some(master) = self.pty.as_mut().and_then(|pty| pty.master.as_mut()) else {
            return;
        };
        if self.input.is_empty() {
            return;
        }
        match master.write(self.input.as_slices().0) {
            Ok(n) => {
                self.input.drain(..n);
            }
            Err(err) if would_retry(&err) => {}
            Err(_) => self.close_master(),
        }
    }

    fn read_output(&mut self) -> bool {
        let mut bytes = [0; IO_CHUNK_BYTES];
        for _ in 0..PTY_READS_PER_TICK {
            let Some(master) = self.pty.as_mut().and_then(|pty| pty.master.as_mut()) else {
                return true;
            };
            match master.read(&mut bytes) {
                Ok(0) => {
                    self.close_master();
                    return true;
                }
                Ok(n) => {
                    self.history.record(&bytes[..n]);
                    if let Some(client) = &mut self.client {
                        if client.output.len() + n <= MAX_QUEUED_OUTPUT {
                            client.output.extend(&bytes[..n]);
                        } else {
                            self.detach();
                        }
                    }
                }
                Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => return true,
                Err(_) => {
                    self.close_master();
                    return true;
                }
            }
        }
        false
    }

    fn send_output(&mut self) {
        let Some(client) = &mut self.client else {
            return;
        };
        if !client.output.is_empty() {
            match client.stream.write(client.output.as_slices().0) {
                Ok(n) => {
                    client.output.drain(..n);
                }
                Err(err) if would_retry(&err) => {}
                Err(_) => self.detach(),
            }
        }
        if self.pty.as_ref().is_none_or(|pty| pty.master.is_none())
            && self
                .client
                .as_ref()
                .is_some_and(|client| client.output.is_empty())
        {
            self.detach();
        }
    }

    fn close_master(&mut self) {
        if let Some(pty) = &mut self.pty {
            drop(pty.master.take());
        }
        self.input.clear();
        self.send_output();
    }

    fn reap(&mut self) -> Result<()> {
        if let Some(pty) = &mut self.pty
            && pty.child.try_wait()?.is_some()
            && self.read_output()
        {
            self.pty = None;
            self.input.clear();
            self.retire_at = Some(Instant::now() + EXIT_RETENTION);
            self.send_output();
        }
        Ok(())
    }
}

fn would_retry(err: &io::Error) -> bool {
    matches!(
        err.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
    )
}

struct History {
    bytes: VecDeque<u8>,
    limit: usize,
    truncated: bool,
}

impl History {
    fn new(limit: usize) -> Self {
        Self {
            bytes: VecDeque::new(),
            limit,
            truncated: false,
        }
    }

    fn record(&mut self, bytes: &[u8]) {
        let bytes_to_keep = bytes.len().min(self.limit);
        let evict = (self.bytes.len() + bytes_to_keep).saturating_sub(self.limit);
        self.truncated |= evict != 0 || bytes_to_keep < bytes.len();
        self.bytes.drain(..evict);
        self.bytes.extend(&bytes[bytes.len() - bytes_to_keep..]);
    }
}
