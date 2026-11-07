# Fork macOS installer

The **Fork macOS installer** workflow builds `WarpOss.app` for Apple Silicon from
`luzkit/warp`. It includes native persistent terminal sessions and runs independently
of upstream release infrastructure. It does not require upstream credentials or an
Apple Developer account.

## Download and install

Download the DMG from a `fork-v*` prerelease, open it, and drag **WarpOss** into
**Applications**. Launch it from Applications. This installs the OSS app separately
from Warp Stable. It replaces any existing WarpOss installation, so detach sessions
you want to retain and quit the old WarpOss before replacing it.

The app is ad hoc signed, not notarized by Apple. macOS may require approval in
**System Settings → Privacy & Security** on first launch. Verify the downloaded
file using its accompanying checksum:

```sh
shasum -a 256 -c WarpOss-macos-arm64-fork-v0.1.0.dmg.sha256
```

The build metadata JSON records the exact commit and Rust toolchain. This installer
does not enable automatic updates; install a later fork release to update it.

Open a normal local terminal, then use its pane menu to **Rename Session…** or
**Detach Session**. Find retained terminals with **Attach to Session…** in Warp
search. **End Session…** stops the shell and its foreground job. Persistence survives
closing Warp after retaining the session; it does not survive a reboot or loss of
the owner process. Output history is bounded. Sessions created by older builds
remain owned by their original worker until ended; create a new session to use
updated worker behavior.

The PTY owner keeps the shell and its jobs, not active AI execution. Pane input,
model/profile preferences, and conversation references are restored, but historical
AI command-output blocks outside retained terminal history are not fully reconstructed
by this prototype.

Earlier prototype launchers used an explicit `WARP_PERSISTENT_PTY_DIR`. Those
retained sessions stay in that runtime and do not appear in the installed app's
default runtime. To reconnect them, detach sessions you want to keep, quit WarpOss,
and launch the installed executable with the **same previous runtime path**:

```sh
WARP_PERSISTENT_PTY_DIR=/path/to/previous/runtime \
  /Applications/WarpOss.app/Contents/MacOS/warp-oss
```

Replace the example path with the value from your old launcher. The old
`WARP_DATA_PROFILE` setting applies only to debug builds; it does not select a data
profile in this release installer. Installing does not move or stop existing owners.

## Build and publish

Run **Actions → Fork macOS installer → Run workflow** to produce a downloadable
workflow artifact without creating a release. The artifact contains a DMG, checksum,
and build metadata and is retained for 7 days.

Push a tag beginning with `fork-v`, such as `fork-v0.1.0`, to build and attach those
files to a **draft prerelease**. Review the draft on GitHub and publish it when ready.
The workflow fails if a release for that tag already exists; it never overwrites
existing release assets. Use a new version tag for a changed build.
Non-draft pull requests into `master` that change source code or packaging
also build an installer artifact from the proposed merge, with read-only repository
permissions. Pull requests and manual builds never publish releases.

Builds use GitHub's `macos-15` Apple Silicon runner, two Cargo jobs, the repository's
pinned Rust toolchain, an optimized release profile without LTO or debug symbols,
and dependency caching. The workflow installs Protobuf, the Metal toolchain,
`cargo-bundle`, and `cargo-about`. It generates license attribution and settings
schema, verifies the app signature and architecture, and verifies the disk image.
Cache saves are enabled for trusted branch/tag runs and same-repository pull
requests. GitHub scopes pull-request and tag caches to their refs; run the workflow
manually on `master` to warm the default-branch cache that later branches and tags
can reuse. Workspace crates are rebuilt rather than stored in the dependency cache.

The existing Linux homelab runners cannot build this native macOS app. This workflow
does not change their repository access policy or require a self-hosted runner.

## Local build

On an Apple Silicon Mac with Xcode, Rust, Homebrew, and Python 3:

```sh
brew install protobuf jq pkgconf
xcodebuild -downloadComponent MetalToolchain
./script/install_cargo_bundle
./script/install_cargo_release_deps --no-build-deps
./script/macos/bundle-fork fork-v0.1.0
```

The result is written to `dist/fork/`. An optional second argument selects another
output directory. The script builds the release app and never opens or installs it.
It uses existing resource and plist helpers, strips the upstream Apple app-group
entitlement, and uses `hdiutil` directly so packaging needs no Finder automation.
