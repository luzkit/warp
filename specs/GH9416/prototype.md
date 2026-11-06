# macOS persistent terminal prototype

## Build the isolated app

From the isolated feature branch, use the repository’s pinned Rust toolchain, `cargo-bundle`, `jq`, Protobuf, and the Xcode macOS SDK and Metal tools. Use project-private dependency staging or the tools already installed on the Mac. For this checkout, the staged Cargo, Protobuf, and Metal wrapper binaries are under the clone’s parent directory.

```sh
stage="$(cd .. && pwd -P)"
export CARGO_TARGET_DIR="$PWD/target"
export PATH="$stage/build-tools:$stage/protoc/bin:$stage/cargo/bin:$PATH"
export CARGO_HOME="$stage/cargo"
export RUSTUP_HOME="$stage/rustup"
export PROTOC="$stage/protoc/bin/protoc"
export CARGO_PROFILE_DEV_DEBUG=0
WARP_BIN_NAME=warp-oss WARP_CHANNEL=oss FEATURES=gui \
  ./script/macos/run --dont-open
prototype_app="$CARGO_TARGET_DIR/debug/bundle/osx/WarpOss.app"
prototype_bin="$prototype_app/Contents/MacOS/warp-oss"
/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$prototype_app/Contents/Info.plist"
```

The bundle identifier should be `dev.warp.WarpOss`. Use a debug build because profile isolation and the prototype entry points are gated to that configuration. Keep the bundle in the isolated staging directory. The staged Cargo wrapper produces only the `.app`. The staged Metal wrapper reads the mounted, exported Apple toolchain. Remount it at `$stage/metal-mount` if needed.

## Launch and reconnect

Set these in a dedicated launcher shell and keep the same values for subsequent launches. Use a fresh private runtime directory for the first run. Pass the same runtime and profile to each subsequent launch. To restore the pane layout and reconnect on launch, enable both Warp’s ordinary session restoration and the explicit environment preference below.

```sh
export WARP_PERSISTENT_PTY_DIR="$(mktemp -d /tmp/wppty.XXXXXX)"
export WARP_DATA_PROFILE="$(basename "$WARP_PERSISTENT_PTY_DIR")"
# Optional. Manual attachment is available from the new-session menu.
# export WARP_PERSISTENT_PTY_AUTO_ATTACH=1
# Optional. Default 1048576, valid range 1 to 8388608 bytes per session.
# export WARP_PERSISTENT_PTY_HISTORY_BYTES=1048576
"$prototype_bin" &
prototype_gui_pid=$!
ps -p "$prototype_gui_pid" -o pid=,command=
```

- Open the chevron beside the new-session button and choose **New Persistent Terminal** or **New Persistent Terminal Split**.
- Use the same menu to discover and attach to detached sessions. Focus the persistent pane before using **Rename Persistent Terminal** or **Detach Persistent Terminal**. The menu shows when a session is attached, ended, or unavailable.
- Closing the persistent pane or quitting the GUI detaches. Warp’s existing quit warning can still count a persistent process. With automatic reattachment disabled, a restored pane stays disconnected until you attach from the menu. The **Terminate …** item opens a confirmation and stops the selected session. For multiple sessions, confirm each one separately when cleaning up.

For an end-to-end check, run the counter in [the validation plan](tech.md#testing-and-validation). Record its PID and tick, quit the isolated GUI with Cmd+Q, and check the workload PID while Warp is closed with `ps -p <workload-pid> -o pid=,command=`. Relaunch with the same exports, attach, and compare the original PID and the ticks produced during the gap. Ctrl+C should stop the counter and return to the same shell. For the forced-exit case, verify the recorded PID resolves to the isolated bundle executable, then send SIGKILL to that exact GUI PID.

## Limits and removal

The owner holds one input connection and keeps replay bytes in RAM. The limit defaults to 1 MiB per session, up to 8 MiB, with up to eight owners per runtime directory. Manual attachment reports discarded output. The first implementation uses the native terminal renderer and shell line editing. Structured Warp blocks, the Warp editor and completion, and shell integration are outside the prototype. Terminal state that predates the retained output may need a live repaint. While detached, a program waiting for a terminal query response can stall or time out. Replayed queries have no side effects, so some programs may need to retry after reattachment. A reboot or loss of the owner can end the session. A naturally exited session reports its status for 60 seconds. Confirmed termination removes its socket immediately. Jobs that detach from the shell or move into independent process groups can outlive shell termination.

To remove the prototype, reconnect with its original runtime and profile, terminate only its sessions, then quit the OSS GUI. Check for worker processes that name the exact private runtime before removing it. For example, use `ps -axo pid=,command= | grep -F -- "--persistent-pty-worker $WARP_PERSISTENT_PTY_DIR "` and disregard the search command itself. Remove that specific private runtime directory and, if no longer needed, the isolated clone and staged build dependencies. Optional profile cleanup is limited to `~/.warp-oss-<profile>`, the matching OSS preferences domain, and the exact `dev.warp.WarpOss-<profile>` state directory. macOS can store that directory under `~/Library/Application Support` or under `~/Library/Group Containers/2BBY89MBSN.dev.warp/Library/Application Support`. If removing the staged Metal tools, detach the read-only image with `hdiutil detach "$stage/metal-mount"` first. Keep the installed Warp app and its data directories separate.

## Validation evidence

- macOS arm64, November 6, 2026. The ad hoc signed debug app identified itself as `dev.warp.WarpOss`.
- Clean quit. GUI PID `39608` exited. At 14:08:13 EST, the PTY owner PID `41385` was parented by launchd, and shell PID `41386` and workload PID `41450` were still alive. After GUI PID `41909` relaunched, the pane replayed the same workload PID and showed ticks up to 95, then live ticks up to 105. Ctrl+C interrupted it, and the next shell command reported the same shell PID `41386`.
- Native renderer. Red ANSI text displayed in color. Vim 9.1 entered the alternate screen, accepted `:q`, and returned to the original scrollback. `stty size` reported 47×124 in the full pane and 45×60 in a split.
- Forced exit and automatic reconnect. SQLite contained both saved persistent pane UUIDs before the test. GUI PID `54592` was killed, and workload PIDs `56008` and `56123` stayed alive. GUI PID `56534` restored the same split layout and both running processes, with increasing ticks in each pane. The menu showed both attached and retained the optional name `Auto right test`.
- Preference off and termination. After a clean quit and relaunch with automatic attachment disabled, both saved panes showed disconnected. Manual attachment reopened the named session in its original split with workload PID `56123` and the retained output. Ctrl+C followed by shell exit made the menu report terminated. The other session stopped after confirmed termination.
- During the preliminary run, the existing SQLite foreign key rejected a separate pane kind and an earlier owner truncated a replay larger than the socket buffer. The persistence marker now lives on the existing terminal record. The replay regression reads 200,000 bytes through the 8 KiB socket buffer and verifies live input afterward.
- Owner crash. Killing a disposable owner closed the attached socket. Its stale endpoint refused connections. Explicit creation started a different owner and shell, and explicit termination removed that owner’s socket.
- Automated checks. The full `warp_terminal` suite passed all 569 selected tests with two skipped, including 13 focused PTY and ANSI checks. Two SQLite round-trip tests and targeted Clippy with warnings denied passed.
- Verification scope. Native testing used the isolated ad hoc signed macOS arm64 bundle. Production autoupdate, other platforms, and a desktop logout or reboot were outside this run.
