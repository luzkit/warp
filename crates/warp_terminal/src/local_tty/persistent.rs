//! Local transport for PTYs whose lifetime is independent of a Warp window.

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
use command::blocking::Command;
use instant::Instant;
use nix::fcntl::{FlockArg, flock};
use nix::unistd::{geteuid, getpeereid};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::SizeInfo;

const MAGIC: [u8; 4] = *b"WPPT";
const VERSION: u32 = 1;
const MAX_FRAME_BYTES: usize = 16 * 1024;
const MAX_SESSIONS: usize = 8;
const DEFAULT_HISTORY_BYTES: usize = 1024 * 1024;
const MAX_HISTORY_BYTES: usize = 8 * 1024 * 1024;
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
}

/// A single writer lease. Dropping the stream releases it.
pub struct Attachment {
    pub stream: UnixStream,
    pub replay: Vec<u8>,
    pub pid: u32,
    pub truncated: bool,
}

impl fmt::Debug for Attachment {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Attachment")
            .field("pid", &self.pid)
            .field("replay_len", &self.replay.len())
            .field("truncated", &self.truncated)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug)]
pub struct Runtime {
    path: PathBuf,
}

#[derive(Deserialize, Serialize)]
struct Startup {
    shell: PathBuf,
    directory: Option<PathBuf>,
    name: Option<String>,
    size: SizeInfo,
    history_bytes: usize,
}

#[derive(Deserialize, Serialize)]
enum Request {
    Status,
    Attach,
    Rename(Option<String>),
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
        let history_bytes = match std::env::var("WARP_PERSISTENT_PTY_HISTORY_BYTES") {
            Ok(value) => value
                .parse()
                .context("Invalid persistent terminal history limit")?,
            Err(std::env::VarError::NotPresent) => DEFAULT_HISTORY_BYTES,
            Err(err) => return Err(err).context("Invalid persistent terminal history limit"),
        };
        validate_history_bytes(history_bytes)?;

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
        for id in self.session_ids()? {
            match self.connect(id) {
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

        let id = Uuid::new_v4();
        let startup = Startup {
            shell: shell.to_owned(),
            directory: directory.map(Path::to_owned),
            name,
            size: *size,
            history_bytes,
        };
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
            write_frame(&mut stdin, &startup)?;
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
                let _ = fs::remove_file(self.socket_path(id));
                Err(err)
            }
        }
    }

    pub fn list(&self) -> Result<Vec<Session>> {
        Ok(self
            .session_ids()?
            .into_iter()
            .map(|id| {
                self.status(id).unwrap_or(Session {
                    id,
                    name: None,
                    pid: None,
                    state: SessionState::Failed,
                })
            })
            .collect())
    }

    pub fn status(&self, id: Uuid) -> Result<Session> {
        self.request_status(id, Request::Status)
    }

    pub fn attach(&self, id: Uuid) -> Result<Attachment> {
        let (mut stream, response) = self.request(id, Request::Attach)?;
        let Response::Attached {
            pid,
            replay_len,
            truncated,
        } = response
        else {
            bail!("Unexpected persistent terminal attach response");
        };
        ensure!(
            replay_len <= MAX_HISTORY_BYTES,
            "Persistent terminal replay exceeds its limit"
        );
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
        })
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
        validate_directory(&self.path)?;
        let path = self.socket_path(id);
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

    fn session_ids(&self) -> Result<Vec<Uuid>> {
        validate_directory(&self.path)?;
        let mut ids = Vec::new();
        for entry in fs::read_dir(&self.path)? {
            let entry = entry?;
            let file_name = entry.file_name();
            let Some(name) = file_name.to_str() else {
                continue;
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
    let startup = read_frame(&mut io::stdin().lock())?;
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
    let payload = serde_json::to_vec(value)?;
    ensure!(
        payload.len() <= MAX_FRAME_BYTES,
        "Persistent terminal control message exceeds its limit"
    );
    writer.write_all(&MAGIC)?;
    writer.write_all(&VERSION.to_be_bytes())?;
    writer.write_all(&(payload.len() as u32).to_be_bytes())?;
    writer.write_all(&payload)?;
    Ok(())
}

fn read_frame<T: DeserializeOwned>(reader: &mut impl Read) -> Result<T> {
    let mut header = [0; 12];
    reader.read_exact(&mut header)?;
    ensure!(
        header[..4] == MAGIC,
        "Invalid persistent terminal control message"
    );
    let version = u32::from_be_bytes([header[4], header[5], header[6], header[7]]);
    ensure!(
        version == VERSION,
        "Persistent terminal protocol version {version} is incompatible with {VERSION}"
    );
    let len = u32::from_be_bytes([header[8], header[9], header[10], header[11]]) as usize;
    ensure!(
        len <= MAX_FRAME_BYTES,
        "Persistent terminal control message exceeds its limit"
    );
    let mut payload = vec![0; len];
    reader.read_exact(&mut payload)?;
    Ok(serde_json::from_slice(&payload)?)
}

#[cfg(test)]
#[path = "persistent_tests.rs"]
mod tests;
