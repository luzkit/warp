# Persistent local terminal sessions

Draft for [#9416](https://github.com/warpdotdev/warp/issues/9416).

## Problem

Warp restores window layout, working directories, and recent terminal blocks after a restart. Closing the app can still interrupt the shell, SSH connection, build, or interactive program that was running in a pane. Users need to reconnect to the original process and see what happened while Warp was closed.

## Proof of concept

1. A user running the isolated macOS Warp OSS build can create and attach to a local persistent terminal. An ordinary shell can run arbitrary commands, including interactive applications and SSH. The user can tell a persistent terminal from an ordinary terminal.
2. Detaching, closing the persistent pane, or quitting Warp leaves the original process running. A forced GUI exit has the same process-continuity result. After reopening, the user can manually attach to the same live session. The proof compares the process ID and a monotonic counter before and after reconnecting.
3. A persistent terminal renders in Warp and accepts terminal input. Ctrl+C, terminal resizing, shell line editing, and alternate-screen input go to the same process while attached.
4. Reattach displays retained output in chronological order and continues with live output. If earlier output was discarded to enforce retention, the user can tell that the recovered history is incomplete.
5. A user can tell whether the selected session is live, already attached, or unavailable. An attach failure never creates a replacement process. An explicit termination action is separate from detach.
6. The proof uses standard shell I/O in the persistent pane. Shell history and shell completion are available when provided by the chosen shell. Structured Warp command blocks, Warp’s input editor and completion, and Warp shell integration are outside the first proof.

## Follow-up behavior

7. Users can create, list, name, rename, attach, and detach multiple sessions. Terminate requires confirmation. Session controls fit into the existing terminal, tab, or command palette UI, expose live, detached, ended, and failed states, and remain keyboard accessible.
8. An explicit preference enables automatic reconnection to surviving sessions. Restoration retains pane/session identities and layout where practical. Missing, ended, or incompatible sessions surface a recoverable state. Any new shell requires a separate user action.
9. Persistent terminals support Warp themes, scrollback, selection, normal shortcuts, and native tabs and splits. Rich input and structured block continuity depend on reliable shell integration and recovery of model state. Document any gaps that remain for each supported shell or restore case.
10. Output retention and live session count have finite limits. The user can see when a limit is reached. Session transport stays local to the owning account. A macOS reboot or loss of the process owning the PTY can end the session.

## Open questions

- Which controls belong in the palette, terminal context menu, or session list?
- Should an ended session retain its last output for inspection, and for how long?
- What shell-integration and display-state recovery guarantees are required before enabling automatic reconnection?
