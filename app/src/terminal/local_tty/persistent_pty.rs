use std::io::{self, Read, Write};
use std::net::Shutdown;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::sync::Arc;

use mio::unix::SourceFd;
use uuid::Uuid;
use warp_terminal::SizeInfo;
use warp_terminal::event::{ExitReason, ObservedExitStatus};
use warp_terminal::local_tty::event_loop::{PTY_TOKEN, SIGNALS_TOKEN};
use warp_terminal::local_tty::persistent::{Runtime, SessionState};
use warp_terminal::local_tty::{ChildEvent, EventedPty, EventedReadWrite};

pub(super) struct PersistentStream(pub(super) Arc<UnixStream>);

impl Read for PersistentStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.0.as_ref().read(buf)
    }
}

impl Write for PersistentStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.as_ref().write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0.as_ref().flush()
    }
}

pub(super) struct PersistentPty {
    pub(super) runtime: Runtime,
    pub(super) id: Uuid,
    pub(super) stream: PersistentStream,
}

impl Drop for PersistentPty {
    fn drop(&mut self) {
        let _ = self.stream.0.shutdown(Shutdown::Both);
    }
}

impl EventedReadWrite for PersistentPty {
    type Reader = PersistentStream;
    type Writer = PersistentStream;

    fn register(&mut self, poll: &mio::Poll, interest: mio::Interest) -> io::Result<()> {
        poll.registry().register(
            &mut SourceFd(&self.stream.0.as_raw_fd()),
            PTY_TOKEN,
            interest,
        )
    }

    fn reregister(&mut self, poll: &mio::Poll, interest: mio::Interest) -> io::Result<()> {
        poll.registry().reregister(
            &mut SourceFd(&self.stream.0.as_raw_fd()),
            PTY_TOKEN,
            interest,
        )
    }

    fn deregister(&mut self, poll: &mio::Poll) -> io::Result<()> {
        poll.registry()
            .deregister(&mut SourceFd(&self.stream.0.as_raw_fd()))
    }

    fn reader(&mut self) -> &mut Self::Reader {
        &mut self.stream
    }

    fn read_token(&self) -> mio::Token {
        PTY_TOKEN
    }

    fn writer(&mut self) -> &mut Self::Writer {
        &mut self.stream
    }

    fn write_token(&self) -> mio::Token {
        PTY_TOKEN
    }
}

impl EventedPty for PersistentPty {
    fn child_event_token(&self) -> mio::Token {
        SIGNALS_TOKEN
    }

    fn next_child_event(&mut self) -> Option<ChildEvent> {
        None
    }

    fn on_resize(&mut self, size: &SizeInfo) {
        let _ = self.runtime.resize(self.id, size);
    }

    fn disconnect_reason(&self) -> ExitReason {
        if self
            .runtime
            .status(self.id)
            .is_ok_and(|session| session.state == SessionState::Terminated)
        {
            ExitReason::ShellProcessExited {
                status: ObservedExitStatus::Unavailable,
            }
        } else {
            ExitReason::PtyDisconnected
        }
    }

    fn kill(self) -> anyhow::Result<()> {
        self.stream.0.shutdown(Shutdown::Both)?;
        Ok(())
    }
}
