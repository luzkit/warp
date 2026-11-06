use std::os::unix::ffi::OsStrExt;

use super::*;

#[test]
fn persistent_runtime_default_is_stable_without_a_development_profile() {
    assert_eq!(
        default_runtime_dir(501, "dev.warp.WarpOss"),
        PathBuf::from("/tmp/warp-pty-501-f01f82bc6f391df0759f9e678d0bc4e4")
    );
}

#[test]
fn persistent_runtime_default_separates_users_and_profiles() {
    let installed = default_runtime_dir(501, "dev.warp.WarpOss");

    assert_ne!(installed, default_runtime_dir(502, "dev.warp.WarpOss"));
    assert_ne!(installed, default_runtime_dir(501, "dev.warp.WarpOss-demo"));
    assert_ne!(
        installed,
        default_runtime_dir(501, "dev.warp.WarpOss-other")
    );
}

#[test]
fn persistent_runtime_default_fits_darwin_sockets_with_long_profile_names() {
    let data_domain = "dev.warp.WarpOss-/Ü/".repeat(4096);
    let directory = default_runtime_dir(u32::MAX, &data_domain);
    let socket = directory.join("n0123456789abcdef0123456789abcdef");

    assert_eq!(directory.parent(), Some(std::path::Path::new("/tmp")));
    assert!(socket.as_os_str().as_bytes().len() <= 103);
}
