//! Local transport for PTYs whose lifetime is independent of a Warp window.

mod checkpoint;
mod worker;

use std::fs::{self, DirBuilder, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, OpenOptionsExt};
use std::os::unix::io::AsRawFd;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use std::{fmt, thread};

use anyhow::{Context as _, Result, bail, ensure};
use bincode::Options as _;
pub use checkpoint::{CheckpointFrame, NativeCheckpoint};
use command::blocking::Command;
use instant::Instant;
use nix::fcntl::{FlockArg, flock};
use nix::unistd::{geteuid, getpeereid};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use warp_core::SessionId;

use crate::SizeInfo;
use crate::local_tty::PtyOptions;
use crate::local_tty::shell::ShellStarter;
use crate::shell::{ShellLaunchData, ShellType};

const MAGIC: [u8; 4] = *b"WPPT";
const NATIVE_STARTUP_MAGIC: [u8; 4] = *b"WPPN";
const VERSION: u32 = 1;
const MAX_FRAME_BYTES: usize = 16 * 1024;
const MAX_NATIVE_STARTUP_BYTES: usize = 1024 * 1024;
const MAX_SESSIONS: usize = 8;
const DEFAULT_HISTORY_BYTES: usize = 1024 * 1024;
const MAX_HISTORY_BYTES: usize = 8 * 1024 * 1024;
const MAX_NATIVE_PREFIX_BYTES: usize = 256 * 1024;
const MAX_CHECKPOINT_BYTES: usize = 256 * 1024;
const MAX_SOCKET_PATH_BYTES: usize = 103;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(2);
const STARTUP_TIMEOUT: Duration = Duration::from_secs(5);

pub const WORKER_ARG: &str = "--persistent-pty-worker";

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum SessionState {
    Attached,
    Detached,
    Terminated,
    Failed,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Session {
    pub id: Uuid,
    pub name: Option<String>,
    pub pid: Option<u32>,
    pub state: SessionState,
    #[serde(default = "retained_by_default")]
    pub retained: bool,
}

fn retained_by_default() -> bool {
    true
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct NativeShell {
    pub session_id: SessionId,
    pub shell_type: ShellType,
    pub launch_data: ShellLaunchData,
    pub pane_uuid: Option<Uuid>,
}

/// A single writer lease. Dropping the stream releases it.
pub struct Attachment {
    pub stream: UnixStream,
    pub replay: Vec<u8>,
    pub pid: u32,
    pub truncated: bool,
    pub replay_gap: bool,
    pub retained: bool,
    pub native_shell: Option<NativeShell>,
    pub startup_replay: Vec<u8>,
    pub checkpoint: Option<Box<NativeCheckpoint>>,
    pub replay_offset: Option<u64>,
}

impl fmt::Debug for Attachment {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Attachment")
            .field("pid", &self.pid)
            .field("replay_len", &self.replay.len())
            .field("startup_replay_len", &self.startup_replay.len())
            .field("truncated", &self.truncated)
            .field("replay_gap", &self.replay_gap)
            .field("retained", &self.retained)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug)]
pub struct Runtime {
    path: PathBuf,
}

#[derive(Deserialize, Serialize)]
struct RawStartup {
    shell: PathBuf,
    directory: Option<PathBuf>,
    name: Option<String>,
    size: SizeInfo,
    history_bytes: usize,
}

#[derive(Deserialize, Serialize)]
struct NativeStartup {
    options: PtyOptions,
    name: Option<String>,
    history_bytes: usize,
}

enum Startup {
    Raw(RawStartup),
    Native(NativeStartup),
}

#[derive(Deserialize, Serialize)]
enum Request {
    Status,
    Attach,
    AttachWithCheckpoint,
    Rename(Option<String>),
    Retain,
    Terminate,
    Resize(SizeInfo),
}

#[derive(Deserialize, Serialize)]
enum Response {
    Status(Session),
    Attached {
        pid: u32,
        replay_len: usize,
        truncated: bool,
    },
    NativeAttached {
        pid: u32,
        replay_len: usize,
        truncated: bool,
        retained: bool,
        shell: NativeShell,
        startup_replay_len: usize,
        #[serde(default)]
        replay_gap: Option<bool>,
        #[serde(default)]
        checkpoint_len: Option<usize>,
        #[serde(default)]
        replay_offset: Option<u64>,
    },
    Ok,
    Error(String),
}

impl Runtime {
    pub fn new(path: PathBuf) -> Result<Self> {
        ensure!(
            path.is_absolute(),
            "Persistent terminal runtime must be an absolute path"
        );
        ensure!(
            path.as_os_str().as_bytes().len() + 33 <= MAX_SOCKET_PATH_BYTES,
            "Persistent terminal runtime path is too long for a macOS socket"
        );
        let parent = path
            .parent()
            .context("Persistent terminal runtime needs a parent directory")?;
        let parent_metadata = fs::metadata(parent)?;
        let parent_mode = parent_metadata.mode();
        ensure!(
            parent_metadata.is_dir()
                && ((parent_metadata.uid() == geteuid().as_raw() && parent_mode & 0o022 == 0)
                    || (parent_metadata.uid() == 0 && parent_mode & 0o1000 != 0)),
            "Persistent terminal runtime needs a private or root-owned sticky parent directory"
        );
        if let Err(err) = DirBuilder::new().mode(0o700).create(&path)
            && err.kind() != io::ErrorKind::AlreadyExists
        {
            return Err(err).context("Could not create persistent terminal runtime");
        }
        validate_directory(&path)?;
        Ok(Self { path })
    }

    pub fn create(
        &self,
        shell: &Path,
        directory: Option<&Path>,
        name: Option<String>,
        size: &SizeInfo,
    ) -> Result<Session> {
        validate_name(name.as_deref())?;
        ensure!(
            shell.is_absolute(),
            "Persistent terminal shell must be an absolute path"
        );
        let history_bytes = configured_history_bytes()?;

        validate_directory(&self.path)?;
        let lock_path = self.path.join("lock");
        let lock = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(lock_path)?;
        let metadata = lock.metadata()?;
        ensure!(
            metadata.is_file() && private_metadata(&metadata, 0o600),
            "Unsafe persistent terminal lock"
        );
        flock(lock.as_raw_fd(), FlockArg::LockExclusive)?;

        let mut count = 0;
        for id in self.session_ids(false)? {
            match self.connect_socket(self.socket_path(id)) {
                Err(err) if is_stale_socket(&err) => {
                    let _ = fs::remove_file(self.socket_path(id));
                }
                _ => count += 1,
            }
        }
        ensure!(
            count < MAX_SESSIONS,
            "Persistent terminal session limit reached"
        );

        self.start_worker(Startup::Raw(RawStartup {
            shell: shell.to_owned(),
            directory: directory.map(Path::to_owned),
            name,
            size: *size,
            history_bytes,
        }))
    }

    /// Starts an ordinary local shell with a lease that must be retained explicitly.
    pub fn create_native(&self, options: PtyOptions, name: Option<String>) -> Result<Session> {
        validate_name(name.as_deref())?;
        ensure!(
            matches!(options.shell_starter, ShellStarter::Direct(_)),
            "Persistent native terminals require a local shell executable"
        );
        self.start_worker(Startup::Native(NativeStartup {
            options,
            name,
            history_bytes: configured_history_bytes()?,
        }))
    }

    fn start_worker(&self, startup: Startup) -> Result<Session> {
        validate_directory(&self.path)?;
        let id = Uuid::new_v4();
        let socket_path = self.startup_socket_path(id, &startup);
        ensure!(
            socket_path.as_os_str().as_bytes().len() <= MAX_SOCKET_PATH_BYTES,
            "Persistent terminal runtime path is too long for a macOS socket"
        );
        let mut command = Command::new(std::env::current_exe()?);
        command
            .arg(WORKER_ARG)
            .arg(&self.path)
            .arg(id.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let fd_limit = unsafe { libc::sysconf(libc::_SC_OPEN_MAX) } as i32;
        // The PTY owner must survive the disappearance of the app or its foreground group.
        unsafe {
            command.pre_exec(move || {
                nix::unistd::setsid().map_err(io::Error::from)?;
                for fd in 3..fd_limit {
                    libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
                }
                Ok(())
            });
        }
        let mut child = command
            .spawn()
            .context("Could not start persistent terminal worker")?;
        let result = (|| {
            let mut stdin = child
                .stdin
                .take()
                .context("Missing persistent terminal startup pipe")?;
            write_startup(&mut stdin, &startup)?;
            drop(stdin);

            let deadline = Instant::now() + STARTUP_TIMEOUT;
            loop {
                if let Ok(session) = self.status(id) {
                    ensure!(
                        session.state == SessionState::Detached,
                        "Persistent terminal shell did not start"
                    );
                    return Ok(session);
                }
                ensure!(
                    child.try_wait()?.is_none() && Instant::now() < deadline,
                    "Persistent terminal worker did not start"
                );
                thread::sleep(Duration::from_millis(20));
            }
        })();
        match result {
            Ok(session) => {
                thread::spawn(move || {
                    let _ = child.wait();
                });
                Ok(session)
            }
            Err(err) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = fs::remove_file(socket_path);
                Err(err)
            }
        }
    }

    pub fn list(&self) -> Result<Vec<Session>> {
        Ok(self
            .session_ids(true)?
            .into_iter()
            .filter_map(|id| {
                let session = self.status(id).unwrap_or(Session {
                    id,
                    name: None,
                    pid: None,
                    state: SessionState::Failed,
                    retained: true,
                });
                session.retained.then_some(session)
            })
            .collect())
    }

    pub fn status(&self, id: Uuid) -> Result<Session> {
        self.request_status(id, Request::Status)
    }

    pub fn attach(&self, id: Uuid) -> Result<Attachment> {
        let mut stream = self.connect(id)?;
        write_frame(&mut stream, &Request::AttachWithCheckpoint)?;
        let response = read_frame(&mut stream)?;
        let (mut stream, response) = match response {
            Response::Error(error)
                if error == "Invalid or incompatible persistent terminal request" =>
            {
                self.request(id, Request::Attach)?
            }
            Response::Error(error) => bail!("{error}"),
            response => (stream, response),
        };
        let (
            pid,
            replay_len,
            truncated,
            replay_gap,
            retained,
            native_shell,
            startup_replay_len,
            checkpoint_len,
            replay_offset,
        ) = match response {
            Response::Attached {
                pid,
                replay_len,
                truncated,
            } => (
                pid, replay_len, truncated, truncated, true, None, 0, 0, None,
            ),
            Response::NativeAttached {
                pid,
                replay_len,
                truncated,
                retained,
                shell,
                startup_replay_len,
                replay_gap,
                checkpoint_len,
                replay_offset,
            } => (
                pid,
                replay_len,
                truncated,
                replay_gap.unwrap_or(truncated),
                retained,
                Some(shell),
                startup_replay_len,
                checkpoint_len.unwrap_or(0),
                replay_offset,
            ),
            _ => bail!("Unexpected persistent terminal attach response"),
        };
        ensure!(
            replay_len <= MAX_HISTORY_BYTES && startup_replay_len <= MAX_NATIVE_PREFIX_BYTES,
            "Persistent terminal replay exceeds its limit"
        );
        let checkpoint = if checkpoint_len == 0 {
            None
        } else {
            let payload = read_payload(&mut stream, checkpoint_len, MAX_CHECKPOINT_BYTES)?;
            let checkpoint: NativeCheckpoint = checkpoint_codec().deserialize(&payload)?;
            checkpoint.validate(replay_offset, replay_len)?;
            Some(Box::new(checkpoint))
        };
        let mut startup_replay = vec![0; startup_replay_len];
        stream
            .read_exact(&mut startup_replay)
            .context("Could not read persistent terminal startup replay")?;
        let mut replay = vec![0; replay_len];
        stream
            .read_exact(&mut replay)
            .context("Could not read persistent terminal replay")?;
        stream.set_read_timeout(None)?;
        stream.set_write_timeout(None)?;
        stream.set_nonblocking(true)?;
        Ok(Attachment {
            stream,
            replay,
            pid,
            truncated,
            replay_gap,
            retained,
            native_shell,
            startup_replay,
            checkpoint,
            replay_offset,
        })
    }

    /// Keeps an attached session alive after its current and future leases disconnect.
    pub fn retain(&self, id: Uuid) -> Result<()> {
        if self.status(id)?.retained {
            return Ok(());
        }
        match self.request(id, Request::Retain)?.1 {
            Response::Ok => Ok(()),
            _ => bail!("Unexpected persistent terminal retain response"),
        }
    }

    pub fn rename(&self, id: Uuid, name: Option<String>) -> Result<Session> {
        validate_name(name.as_deref())?;
        self.request_status(id, Request::Rename(name))
    }

    pub fn terminate(&self, id: Uuid) -> Result<()> {
        match self.request(id, Request::Terminate)?.1 {
            Response::Ok => Ok(()),
            _ => bail!("Unexpected persistent terminal terminate response"),
        }
    }

    pub fn resize(&self, id: Uuid, size: &SizeInfo) -> Result<()> {
        match self.request(id, Request::Resize(*size))?.1 {
            Response::Ok => Ok(()),
            _ => bail!("Unexpected persistent terminal resize response"),
        }
    }

    fn request_status(&self, id: Uuid, request: Request) -> Result<Session> {
        match self.request(id, request)?.1 {
            Response::Status(session) => Ok(session),
            _ => bail!("Unexpected persistent terminal status response"),
        }
    }

    fn request(&self, id: Uuid, request: Request) -> Result<(UnixStream, Response)> {
        let mut stream = self.connect(id)?;
        write_frame(&mut stream, &request)?;
        let response = match read_frame(&mut stream)? {
            Response::Error(error) => bail!("{error}"),
            response => response,
        };
        Ok((stream, response))
    }

    fn connect(&self, id: Uuid) -> Result<UnixStream> {
        let native_path = self.native_socket_path(id);
        match fs::symlink_metadata(&native_path) {
            Ok(_) => self.connect_socket(native_path),
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                self.connect_socket(self.socket_path(id))
            }
            Err(err) => Err(err.into()),
        }
    }

    fn connect_socket(&self, path: PathBuf) -> Result<UnixStream> {
        validate_directory(&self.path)?;
        let metadata = fs::symlink_metadata(&path)?;
        ensure!(
            metadata.file_type().is_socket() && private_metadata(&metadata, 0o600),
            "Unsafe persistent terminal socket"
        );
        let stream = UnixStream::connect(path)?;
        validate_peer(&stream)?;
        stream.set_read_timeout(Some(REQUEST_TIMEOUT))?;
        stream.set_write_timeout(Some(REQUEST_TIMEOUT))?;
        Ok(stream)
    }

    fn socket_path(&self, id: Uuid) -> PathBuf {
        self.path.join(id.simple().to_string())
    }

    fn native_socket_path(&self, id: Uuid) -> PathBuf {
        self.path.join(format!("n{}", id.simple()))
    }

    fn startup_socket_path(&self, id: Uuid, startup: &Startup) -> PathBuf {
        match startup {
            Startup::Raw(_) => self.socket_path(id),
            Startup::Native(_) => self.native_socket_path(id),
        }
    }

    fn session_ids(&self, include_native: bool) -> Result<Vec<Uuid>> {
        validate_directory(&self.path)?;
        let mut ids = Vec::new();
        for entry in fs::read_dir(&self.path)? {
            let entry = entry?;
            let file_name = entry.file_name();
            let Some(name) = file_name.to_str() else {
                continue;
            };
            let name = if include_native {
                name.strip_prefix('n').unwrap_or(name)
            } else {
                name
            };
            if name.len() == 32
                && let Ok(id) = Uuid::parse_str(name)
                && name == id.simple().to_string()
                && entry.file_type()?.is_socket()
            {
                ids.push(id);
            }
        }
        ids.sort_unstable();
        Ok(ids)
    }
}

pub fn run_worker(runtime_path: PathBuf, id: Uuid) -> Result<()> {
    let startup = read_startup(&mut io::stdin().lock())?;
    worker::run(&Runtime::new(runtime_path)?, id, startup)
}

fn validate_name(name: Option<&str>) -> Result<()> {
    ensure!(
        name.is_none_or(|name| !name.is_empty()
            && name.len() <= 128
            && !name.chars().any(char::is_control)),
        "Persistent terminal name must contain 1 to 128 bytes of printable text"
    );
    Ok(())
}

fn validate_history_bytes(bytes: usize) -> Result<()> {
    ensure!(
        (1..=MAX_HISTORY_BYTES).contains(&bytes),
        "Persistent terminal history limit must be between 1 and {MAX_HISTORY_BYTES} bytes"
    );
    Ok(())
}

fn configured_history_bytes() -> Result<usize> {
    let bytes = match std::env::var("WARP_PERSISTENT_PTY_HISTORY_BYTES") {
        Ok(value) => value
            .parse()
            .context("Invalid persistent terminal history limit")?,
        Err(std::env::VarError::NotPresent) => DEFAULT_HISTORY_BYTES,
        Err(err) => return Err(err).context("Invalid persistent terminal history limit"),
    };
    validate_history_bytes(bytes)?;
    Ok(bytes)
}

fn validate_directory(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    ensure!(
        metadata.is_dir() && private_metadata(&metadata, 0o700),
        "Persistent terminal runtime must be a mode 0700 directory owned by the current user"
    );
    Ok(())
}

fn private_metadata(metadata: &fs::Metadata, mode: u32) -> bool {
    metadata.uid() == geteuid().as_raw() && metadata.mode() & 0o777 == mode
}

fn validate_peer(stream: &UnixStream) -> Result<()> {
    let (uid, _) = getpeereid(stream.as_raw_fd())?;
    ensure!(
        uid == geteuid(),
        "Persistent terminal peer has a different user ID"
    );
    Ok(())
}

fn is_stale_socket(err: &anyhow::Error) -> bool {
    err.downcast_ref::<io::Error>().is_some_and(|err| {
        matches!(
            err.kind(),
            io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
        )
    })
}

fn write_frame(writer: &mut impl Write, value: &impl Serialize) -> Result<()> {
    write_payload(writer, MAGIC, &serde_json::to_vec(value)?, MAX_FRAME_BYTES)
}

fn write_payload(
    writer: &mut impl Write,
    magic: [u8; 4],
    payload: &[u8],
    max_bytes: usize,
) -> Result<()> {
    ensure!(
        payload.len() <= max_bytes,
        "Persistent terminal control message exceeds its limit"
    );
    writer.write_all(&magic)?;
    writer.write_all(&VERSION.to_be_bytes())?;
    writer.write_all(&(payload.len() as u32).to_be_bytes())?;
    writer.write_all(payload)?;
    Ok(())
}

fn read_frame<T: DeserializeOwned>(reader: &mut impl Read) -> Result<T> {
    let (magic, len) = read_header(reader)?;
    ensure!(
        magic == MAGIC,
        "Invalid persistent terminal control message"
    );
    Ok(serde_json::from_slice(&read_payload(
        reader,
        len,
        MAX_FRAME_BYTES,
    )?)?)
}

fn read_header(reader: &mut impl Read) -> Result<([u8; 4], usize)> {
    let mut header = [0; 12];
    reader.read_exact(&mut header)?;
    let version = u32::from_be_bytes([header[4], header[5], header[6], header[7]]);
    ensure!(
        version == VERSION,
        "Persistent terminal protocol version {version} is incompatible with {VERSION}"
    );
    let len = u32::from_be_bytes([header[8], header[9], header[10], header[11]]) as usize;
    Ok(([header[0], header[1], header[2], header[3]], len))
}

fn read_payload(reader: &mut impl Read, len: usize, max_bytes: usize) -> Result<Vec<u8>> {
    ensure!(
        len <= max_bytes,
        "Persistent terminal control message exceeds its limit"
    );
    let mut payload = vec![0; len];
    reader.read_exact(&mut payload)?;
    Ok(payload)
}

fn checkpoint_codec() -> impl bincode::Options {
    bincode::options().with_limit(MAX_CHECKPOINT_BYTES as u64)
}

fn native_startup_codec() -> impl bincode::Options {
    bincode::options().with_limit(MAX_NATIVE_STARTUP_BYTES as u64)
}

fn write_startup(writer: &mut impl Write, startup: &Startup) -> Result<()> {
    match startup {
        Startup::Raw(startup) => write_frame(writer, startup),
        Startup::Native(startup) => {
            // JSON cannot represent non-UTF-8 Unix environment variables.
            let payload = native_startup_codec().serialize(startup)?;
            write_payload(
                writer,
                NATIVE_STARTUP_MAGIC,
                &payload,
                MAX_NATIVE_STARTUP_BYTES,
            )
        }
    }
}

fn read_startup(reader: &mut impl Read) -> Result<Startup> {
    let (magic, len) = read_header(reader)?;
    match magic {
        MAGIC => {
            let payload = read_payload(reader, len, MAX_FRAME_BYTES)?;
            Ok(Startup::Raw(serde_json::from_slice(&payload)?))
        }
        NATIVE_STARTUP_MAGIC => {
            let payload = read_payload(reader, len, MAX_NATIVE_STARTUP_BYTES)?;
            Ok(Startup::Native(
                native_startup_codec().deserialize(&payload)?,
            ))
        }
        _ => bail!("Invalid persistent terminal startup message"),
    }
}

#[cfg(test)]
#[path = "persistent_tests.rs"]
mod tests;
