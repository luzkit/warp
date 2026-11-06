use std::ffi::OsStr;
use std::path::PathBuf;

use warp_core::channel::{Channel, ChannelState};

const RUNTIME_DIR: &str = "WARP_PERSISTENT_PTY_DIR";

pub(crate) fn runtime_dir() -> Option<PathBuf> {
    if ChannelState::channel() != Channel::Oss
        || ChannelState::data_profile().is_none_or(|profile| profile.is_empty())
    {
        return None;
    }
    std::env::var_os(RUNTIME_DIR).map(PathBuf::from)
}

pub(crate) fn auto_reattach_path() -> Option<PathBuf> {
    (std::env::var_os("WARP_PERSISTENT_PTY_AUTO_ATTACH").as_deref() == Some(OsStr::new("1")))
        .then(runtime_dir)
        .flatten()
}
