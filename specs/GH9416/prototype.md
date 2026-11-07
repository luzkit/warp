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

The bundle identifier should be `dev.warp.WarpOss`. Native persistence is available in both debug and release macOS OSS builds. Debug builds additionally support isolated data profiles. Keep the test bundle in the isolated staging directory. The staged Cargo wrapper produces only the `.app`. The staged Metal wrapper reads the mounted, exported Apple toolchain. Remount it at `$stage/metal-mount` if needed.

## Launch and reconnect

An installed WarpOss app can be opened normally, without environment variables. It uses a private runtime under `/tmp`, namespaced by the effective user and application data domain. Named sessions show only their name in the attach picker; unnamed sessions show a short ID.

For an isolated development test, set these optional overrides in a dedicated launcher shell and keep the same values for subsequent launches. `WARP_DATA_PROFILE` applies only to debug builds. To restore the pane layout and reconnect on launch, enable both Warp’s ordinary session restoration and the explicit environment preference below.

```sh
export WARP_PERSISTENT_PTY_DIR="$(mktemp -d /tmp/wppty.XXXXXX)"
export WARP_DATA_PROFILE="$(basename "$WARP_PERSISTENT_PTY_DIR")"
# Optional. Manual attachment is available from Warp search.
# export WARP_PERSISTENT_PTY_AUTO_ATTACH=1
# Optional. Default 1048576, valid range 1 to 8388608 bytes per session.
# export WARP_PERSISTENT_PTY_HISTORY_BYTES=1048576
"$prototype_bin" &
prototype_gui_pid=$!
ps -p "$prototype_gui_pid" -o pid=,command=
```

- Open a regular terminal tab or split. Each supported local shell gets its own session and uses Warp’s editor, prompt, and command blocks.
- Use the pane’s **⋯** menu to **Rename Session…**, **Detach Session**, or **End Session…**. Naming is optional and helps you find the session again. Detach closes the pane while keeping the shell and its jobs running. You can also detach with **⌘⌥⇧D**, which you can change in Warp’s keyboard shortcuts. End asks for confirmation before stopping the session.
- Select **Attach to Session…** in Warp’s title bar search and choose a detached session. Warp reconnects a matching disconnected pane when available or opens a terminal tab for the session.
- Explicitly detach a new session to keep it running after closing Warp. After its first detach, the session remains retained on future disconnects until the shell exits or you end the session. With automatic reattachment disabled, saved retained panes can be reconnected through search.

For an end-to-end check, run the counter in [the validation plan](tech.md#testing-and-validation). Record its PID and tick, detach the session, then quit the isolated GUI with Cmd+Q. Check the workload PID while Warp is closed with `ps -p <workload-pid> -o pid=,command=`. Relaunch with the same exports, attach, and compare the original PID and the ticks produced during the gap. Ctrl+C should stop the counter and return to the same shell. For the forced-exit case, first detach and reattach to retain the session. Verify the recorded GUI PID resolves to the isolated bundle executable, then send SIGKILL to that exact PID.

## Limits and removal

The owner holds one input connection and keeps replay bytes in RAM. The limit defaults to 1 MiB per session, up to 8 MiB. Native sessions also keep up to 256 KiB of startup output and a separate shell-integration checkpoint with a 256 KiB retained-hook budget. The checkpoint retains validated bootstrap hooks and the latest complete prompt independently of scrollback; ordinary startup output can exceed the prefix without losing native rendering after a gap. It tracks running commands so restoring a prompt does not expose the editor over a running job. Individual integration hooks that exceed the checkpoint budget can still prevent complete restoration.

Manual attachment reports discarded output. Terminal state that predates the retained output may need a live repaint. Older owners remain attachable through protocol fallback and recover native rendering if the bootstrap hooks survive in the prefix or tail; start a new session with the rebuilt app to obtain checkpoint protection. App-integrated SSH recovery and Warp's PTY-based password prompt detection have limited support. While detached, a program waiting for a terminal query response can stall or time out. A reboot or loss of the owner can end the session. A naturally exited session reports its status for 60 seconds. Confirmed termination removes its socket immediately. Jobs that detach from the shell or move into independent process groups can outlive shell termination.

To remove the prototype, reconnect with its original runtime and profile, terminate only its sessions, then quit the OSS GUI. Check for worker processes that name the exact private runtime before removing it. For example, use `ps -axo pid=,command= | grep -F -- "--persistent-pty-worker $WARP_PERSISTENT_PTY_DIR "` and disregard the search command itself. Remove that specific private runtime directory and, if no longer needed, the isolated clone and staged build dependencies. Optional profile cleanup is limited to `~/.warp-oss-<profile>`, the matching OSS preferences domain, and the exact `dev.warp.WarpOss-<profile>` state directory. macOS can store that directory under `~/Library/Application Support` or under `~/Library/Group Containers/2BBY89MBSN.dev.warp/Library/Application Support`. If removing the staged Metal tools, detach the read-only image with `hdiutil detach "$stage/metal-mount"` first. Keep the installed Warp app and its data directories separate.

## Noisy-startup replay validation

The rebuilt app is staged at `/private/tmp/warp-persistence.o3brlHz5/checkpoint-fix/WarpOss.app`. After detaching sessions to retain and quitting the earlier prototype, `/private/tmp/warp-persistence.o3brlHz5/checkpoint-fix/start` launches it with the existing `warp-persistence-demo` profile and runtime. Existing owners continue running their original binaries; checkpoint protection starts with sessions created by this build.

- All 153 focused transport, checkpoint, and ANSI tests passed. The 19 app replay, restoration, and lifecycle checks passed. Targeted Clippy with warnings denied passed.
- Regression coverage includes bootstrap hooks surviving only in the tail, complete hook eviction, partial overlaps at the tail boundary, idle and running sessions, Unicode key-value hooks, bounded incomplete payloads, older-owner fallback, split historical escape sequences, and restoring metadata without fresh bootstrap actions.
- Real zsh tests passed with 64 KiB and 64-byte histories. Bootstrap first appeared at bytes 422409 and 422400, beyond the startup prefix, then was evicted from both raw buffers. Shell PIDs `88069` and `88294`, a shell variable, and a single `.zshrc` execution survived attachment cycles. Foreground jobs survived detachment and Ctrl+C restored native shell hooks. Test owners terminated and removed their sockets. One initial owner launch exceeded the harness startup deadline; the complete retry passed both cases with empty worker error logs. Evidence: `/private/tmp/wpcp.s4Uzvt/headless-verification.log`.

- Visual verification in a separate bundle/profile showed the native editor and command blocks after both noisy-startup and full history gaps. `Noisy checkpoint` kept its name in the picker and pane. Shell PID `91617` and startup count `1` survived all cycles. Foreground job `92320` resumed through tick 21 after attachment; Ctrl+C restored the native editor and the next command confirmed the original shell. Only the disposable GUI and owners were stopped afterward.

## Native workflow validation

- On macOS arm64 with zsh, a regular pane displayed the Warp editor and command blocks. The pane menu saved an optional name, and Warp search found the detached session by that name. A foreground counter kept advancing with Warp closed.
- In the rebuilt app, the detach shortcut closed the pane and manual attachment after a GUI restart restored shell PID `17231`, its previous command block, and an editable Warp prompt. With a 64 KiB history limit, about 400 KB of later output forced a gap. Reattachment showed the retained block and the same running counter PID `21366`. Ctrl+C stopped it, and a new Warp command block confirmed shell PID `17231` again.
- Confirmed **End Session** stopped the shell and its foreground job and left the other terminal open. The disposable runtime contained no owner sockets after quitting the test app.
- The 97 focused transport and ANSI tests passed. The app's split-replay and SQLite persistent-pane tests also passed.

## Validation evidence for the original prototype

- macOS arm64, November 6, 2026. The ad hoc signed debug app identified itself as `dev.warp.WarpOss`.
- Clean quit. GUI PID `39608` exited. At 14:08:13 EST, the PTY owner PID `41385` was parented by launchd, and shell PID `41386` and workload PID `41450` were still alive. After GUI PID `41909` relaunched, the pane replayed the same workload PID and showed ticks up to 95, then live ticks up to 105. Ctrl+C interrupted it, and the next shell command reported the same shell PID `41386`.
- Native renderer. Red ANSI text displayed in color. Vim 9.1 entered the alternate screen, accepted `:q`, and returned to the original scrollback. `stty size` reported 47×124 in the full pane and 45×60 in a split.
- Forced exit and automatic reconnect. SQLite contained both saved persistent pane UUIDs before the test. GUI PID `54592` was killed, and workload PIDs `56008` and `56123` stayed alive. GUI PID `56534` restored the same split layout and both running processes, with increasing ticks in each pane. The menu showed both attached and retained the optional name `Auto right test`.
- Preference off and termination. After a clean quit and relaunch with automatic attachment disabled, both saved panes showed disconnected. Manual attachment reopened the named session in its original split with workload PID `56123` and the retained output. Ctrl+C followed by shell exit made the menu report terminated. The other session stopped after confirmed termination.
- During the preliminary run, the existing SQLite foreign key rejected a separate pane kind and an earlier owner truncated a replay larger than the socket buffer. The persistence marker now lives on the existing terminal record. The replay regression reads 200,000 bytes through the 8 KiB socket buffer and verifies live input afterward.
- Owner crash. Killing a disposable owner closed the attached socket. Its stale endpoint refused connections. Explicit creation started a different owner and shell, and explicit termination removed that owner’s socket.
- Automated checks. The full `warp_terminal` suite passed all 569 selected tests with two skipped, including 13 focused PTY and ANSI checks. Two SQLite round-trip tests and targeted Clippy with warnings denied passed.
- Verification scope. Native testing used the isolated ad hoc signed macOS arm64 bundle. Production autoupdate, other platforms, and a desktop logout or reboot were outside this run.
