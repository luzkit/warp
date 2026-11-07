# Fork macOS installer

Versioned releases of `luzkit/warp` contain **WarpOss.app** for Apple Silicon with
native persistent terminal sessions. The release pipeline signs with our Developer
ID, obtains Apple's notarization, and staples tickets to both the app and installer.
This is an independently distributed fork, not an upstream Warp release.

## Download and install

Download the DMG from a `fork-v*` release, open it, and drag **WarpOss** into
**Applications**. Launch it from Applications. This installs the OSS app separately
from Warp Stable. It replaces any existing WarpOss installation, so detach sessions
you want to retain and quit the old WarpOss before replacing it.

Launch normally from Applications; no terminal command or environment variable is
required. New terminals use the installed app's default session runtime. Verify
the downloaded file using its accompanying checksum:

```sh
shasum -a 256 -c WarpOss-macos-arm64-fork-v0.1.0.dmg.sha256
```

The build metadata JSON records the exact commit, Rust toolchain, signing team,
notarization submission IDs, and signing workflow run. The app and DMG must pass
signature, ticket, and Gatekeeper verification before becoming a release artifact.
This installer does not enable automatic updates; install a later fork release to
update it.

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

Earlier prototype launchers used an explicit `WARP_PERSISTENT_PTY_DIR`. Their
sessions remain in that separate runtime; installation does not migrate or stop
them. Start normally to use the installed runtime. Existing WarpOss preferences
are not erased during installation.

## Build and publish

The private `luzkit/nab-release-app` repository coordinates notarized builds using
its existing Mac runner and Keychain. The reviewed workflow template is
[`script/ci/fork-notarized-release.yml`](../../script/ci/fork-notarized-release.yml);
deploy it there as `.github/workflows/fork-notarized-release.yml`. It runs only by
manual dispatch from that repository's `main` branch. The public fork's PRs never
receive access to the signing runner.

1. Merge the release changes into `luzkit/warp:master`, then create an immutable tag
   such as `fork-v0.1.0` at that exact commit. Tags use `fork-vMAJOR.MINOR.PATCH`, with
   major at most 9999 and minor/patch at most 99 for Apple's bundle version format.
2. In the private repository, run **Notarized fork macOS release** from `main` with
   `release_tag` set to that tag. `preflight_only` validates the source and signing
   setup without starting a build. A full release also requires
   `allow_mac_build=true`, explicitly approving compilation on the personal Mac,
   Developer ID signing, and submission to Apple for notarization. Leaving it false
   stops before a Mac build. There is no hosted macOS fallback.
3. The workflow resolves the tag to the current fork `master` SHA and runs signing
   regression tests and packaging shell checks on the homelab Linux pool. It checks
   the existing Mac signing identity and notarization profile, compiles the reviewed
   source on the personal Mac, and passes that run's exact unsigned artifact to the
   signing job. It validates source metadata, checksum, bundle identity, version,
   and architecture without executing the downloaded app or rebuilding Rust.
4. After the final verification job succeeds, download its exact signed artifact.
   Verify the final checksum and metadata against the resolved source, tag, signing
   team, and run. Create the fork's draft release with the DMG, checksum, metadata,
   and both accepted notarization receipts; publish only after verification.

The controller intentionally has no cross-repository write credential. Publishing
uses the release operator's existing GitHub authorization for `luzkit/warp`. This
keeps the existing runner groups and other Mac projects unchanged. The Linux
resolve/validation and final verification jobs use `[self-hosted, Linux]`, the
labels of the existing `homelab-ci-01`, `homelab-ci-02`, and `homelab-ci-03` pool.
Their tools are installed inside Ubuntu job containers, not on the persistent host.
Only the private controller's manual `main` dispatch can schedule these jobs; public
PR code never reaches the homelab or personal Mac.
Do not overwrite a version tag or existing release assets; use a new version for
changed source. The final publication must recheck the tag SHA and existing drafts,
which the controller's read-only token cannot necessarily see.

The existing private repository variables are `MAC_SIGNING_IDENTITY` and
`APPLE_KEYCHAIN_PROFILE`. The workflow derives the team and full Developer ID
selector from that identity; certificates and notarization credentials stay in the
runner account's Keychain. It never exports keys, imports certificates, or unlocks
the Keychain with a password. An unavailable identity/profile stops before the
costly build. A failed or timed-out Apple submission retains its ID in a diagnostic
artifact; inspect that submission before retrying, rather than blindly resubmitting.

Builds use the existing `[self-hosted, macOS, ARM64, personal-mac]` runner, two Cargo
jobs, the repository's pinned Rust toolchain, and an optimized release profile
without LTO or debug symbols. Each run uses its own source checkout, Cargo target
directory, and installer output. The target directory stays within that checkout
for the DockTilePlugin build and is removed with the checkout after the job. The
installer is under `RUNNER_TEMP`; Cargo registry downloads remain in the runner
account's Cargo home.
The workflow checks preinstalled tools and does not install packages or change the
global Xcode selection. It scopes Cargo paths, `DEVELOPER_DIR`, and Metal
`TOOLCHAINS` to the build job. It does not restore an Actions Rust cache onto the
persistent Mac, preserving the installed Cargo tools. Public PR snapshots retain
their separate hosted-runner cache.

The build generates license attribution and settings schema, verifies the ad-hoc
app signature and architecture, and verifies the disk image. Linux handles source
and artifact verification but cannot replace the native macOS compiler or Apple's
signing tools.

### Mac runner readiness and Keychain recovery

A signing-ready Mac is not necessarily a Warp build-ready Mac. Its `github-runner`
account must have the repository's pinned Rust toolchain, Protobuf, jq, pkg-config,
Git LFS, Python 3.11+, cargo-binstall 1.18.1, cargo-bundle 0.11.0, cargo-about 0.8.4, and Xcode's
Metal Toolchain. The current build job selects Xcode 26.3 from
`/Applications/Xcode-26.3.0.app` and Metal
`com.apple.dt.toolchain.Metal.32023.864`; update these job-scoped selections when
provisioning a different approved Xcode installation. Install approved system
prerequisites from the administrator account and account-local Cargo tools as `github-runner`;
do not change Homebrew ownership or grant the runner administrator access. Keep
enough free disk space for the source, dependency cache, build output, and installer.

The existing runner service uses `SessionCreate=false` in the runner account's GUI
domain. Preserve that reviewed session configuration. `notarytool` targets the data
protection Keychain unless given an explicit Keychain file, so an unlocked login
Keychain file in another security session does not establish that the service can
use its notarization profile. Test access with `preflight_only=true` from the actual
runner. If it reports `keychainLocked`, stop and investigate the runner's login and
service context before changing settings or credentials. Coordinate service
maintenance with the operator; do not switch desktop users or restart a busy runner
as an automated recovery step. Do not send passwords in chat or save them in
repository variables, Actions secrets, or logs. No certificate export, new Apple
credential, permission broadening, or full build is needed for this check. Apple's
[notarization guidance](https://developer.apple.com/documentation/technotes/tn3147-migrating-to-the-latest-notarization-tool)
describes the profile's default Keychain storage.

## Development snapshots

**Actions → Fork macOS installer → Run workflow** in the public fork produces a
downloadable **ad-hoc signed, unnotarized** snapshot for development, retained for
7 days. These are distinct from notarized versioned releases and never publish
release assets. macOS may require first-launch approval for these snapshots.

Non-draft source PRs build a proposed-merge snapshot with read-only permissions.
Packaging-only PRs run signing/artifact regression tests, shell checks, and workflow
lint; the notarized release run performs the full native build and real Apple
verification before distribution. Tags do not trigger ad-hoc release publication.

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

For local Developer ID packaging, set `WARP_FORK_SIGNING_MODE=developer-id` plus
`MAC_SIGNING_IDENTITY` (the full `Developer ID Application: …` selector),
`APPLE_TEAM_ID`, and `APPLE_KEYCHAIN_PROFILE` for an existing Keychain setup. No
credentials should be put in the repository. To sign an already-built installer:

```sh
python3 script/macos/notarize_fork.py /path/to/unsigned /path/to/new-signed-output \
  --expected-source-sha FULL_COMMIT_SHA --expected-version fork-v0.1.0
```

The input directory must contain exactly that version's DMG, checksum, and JSON;
the output directory must not already exist. The original artifact is preserved.
