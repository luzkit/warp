use std::ffi::OsStr;
use std::path::PathBuf;

use nix::unistd::geteuid;
use sha2::{Digest, Sha256};
use warp_core::channel::{Channel, ChannelState};

const RUNTIME_DIR: &str = "WARP_PERSISTENT_PTY_DIR";

pub(crate) fn runtime_dir() -> Option<PathBuf> {
    if ChannelState::channel() != Channel::Oss {
        return None;
    }
    Some(
        std::env::var_os(RUNTIME_DIR)
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                default_runtime_dir(geteuid().as_raw(), &ChannelState::data_domain())
            }),
    )
}

fn default_runtime_dir(user_id: u32, data_domain: &str) -> PathBuf {
    // Darwin's per-user temp directory can already exceed the Unix socket path budget.
    let domain_hash = hex::encode(&Sha256::digest(data_domain.as_bytes())[..16]);
    PathBuf::from(format!("/tmp/warp-pty-{user_id}-{domain_hash}"))
}

pub(crate) fn auto_reattach_path() -> Option<PathBuf> {
    (std::env::var_os("WARP_PERSISTENT_PTY_AUTO_ATTACH").as_deref() == Some(OsStr::new("1")))
        .then(runtime_dir)
        .flatten()
}

#[cfg(test)]
#[path = "persistent_terminals_tests.rs"]
mod tests;
