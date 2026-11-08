# Enhanced pane management

The first milestone moves a live terminal from a single-terminal tab onto an edge of a terminal pane in the active tab. The workspace previews the resulting split while the pointer moves and commits on drop. The move must preserve the pane, shell, running jobs, scrollback, command history, native input and interactive terminal state. A preview or canceled operation cannot change the layout or create a shell.

## Development base

| Checkout or reference | Baseline |
| --- | --- |
| Docking worktree | `/private/tmp/warp-enhanced-pane-management`, branch `feature/enhanced-pane-management`, started clean at fork master `26dcb4625afbf0672ca6e39a64fa0503c7e6a703`. |
| Upstream | `upstream/master` refreshed to `abb3fe33`. The merge base with the fork is `f7fc2f2a`. |
| Persistent-session project | The original standalone clone at `/private/tmp/warp-persistence.o3brlHz5/warp` was clean on `codex/persistent-terminal-sessions` at `208320d3`. The shared repository's branch of that name points to `b412c952`. Fork master already includes the delivered feature in `5233ecde`. [Persistent terminal design and verification](../GH9416/tech.md) |
| Other linked worktrees | The existing `codex/warp-self-hosted-release` worktree is an independent CI audit. |

The docking branch uses the delivered fork base. Its workspace changes need to remain portable to ordinary terminals and upstream code. Keep build targets, profiles and disposable processes separate from the installed application and the persistence prototype.

## Upstream overlap

GitHub state checked November 6, 2026. Open proposals and closed prototypes are references for design and failure cases.

| Work | State | Relevance |
| --- | --- | --- |
| [PR #16610](https://github.com/warpdotdev/warp/pull/16610) | Open draft | Moves terminal panes through a same-window tab menu, always as right splits, and drops the emptied tab. Excludes child-agent owners. The eligibility checks and live-transfer tests are useful references. Pointer docking and preservation of nested layouts are outside its scope. |
| [PR #13461](https://github.com/warpdotdev/warp/pull/13461) | Open | Proposes refreshing remaining panes when their source tab becomes inactive during a transfer. Relevant when moving a pane out of a multi-pane group. Use the existing terminal layout and PTY resize path to deliver size changes. |
| [PR #13237](https://github.com/warpdotdev/warp/pull/13237) | Open | Proposes migrating the off-tree child panes and their registrations with a parent agent. The first milestone excludes this ownership case. |
| [PR #14164](https://github.com/warpdotdev/warp/pull/14164) | Open | Proposes transferring complete tab groups across windows while retaining group metadata and member pane-group state. The later [PR #14415](https://github.com/warpdotdev/warp/pull/14415) closed unmerged and provides another reference for identity-based cleanup and atomic group handoff. |
| [Issue #8959](https://github.com/warpdotdev/warp/issues/8959) | Open, `needs-info` | Tracks the requested tab-to-split gesture. The July 2026 readiness change left the issue without a spec or implementation readiness label. |
| [Issue #5303](https://github.com/warpdotdev/warp/issues/5303) | Open | Tracks broader split rearrangement, tab-to-pane and pane-to-tab conversion, and divider reset. |
| [PR #11404](https://github.com/warpdotdev/warp/pull/11404) | Closed unmerged | Closest prototype. It documents active-tab hit testing, half-pane previews, vertical drag constraints, and the reorder conflict when a nonactive tab is dragged over pane content. Its four-direction vertical-tab implementation is absent from the base. |
| [PR #13401](https://github.com/warpdotdev/warp/pull/13401) | Closed unmerged | Selected-tab context menu merge into right splits. Review discussed the gap between dragging existing pane headers and moving a single-pane tab. |
| [Issue #16628](https://github.com/warpdotdev/warp/issues/16628) | Open | Moving a sole pane through `remove_pane_for_move` attempts to focus a remaining pane and remove the sole tree leaf. Both attempts report errors. Its `Exited` event is another reason to use a dedicated sole-pane extraction. |
| [PR #9275](https://github.com/warpdotdev/warp/pull/9275) and [PR #14432](https://github.com/warpdotdev/warp/pull/14432) | Merged, present in base | Existing whole-tab cross-window move and a fix for stale window routing of view streams during the transfer. These cover a different path from same-window pane docking. |
| [PR #14347](https://github.com/warpdotdev/warp/pull/14347) | Closed unmerged | Describes a cross-window transfer that can remove the wrong tab after the tab list shifts. Use stable group identities and resolve current tab indices at commit and cleanup. |

## Current architecture

| Responsibility | Existing implementation and contract |
| --- | --- |
| Window, tab and tab-group ownership | `WorkspaceRegistry` resolves workspace handles by window. `Workspace` owns ordered `TabData` records, each with a live `PaneGroup` view handle. Tab metadata, pinning, groups, MRU and active tab belong to the workspace and tab bar. Tab indices describe the current order and can change during a gesture. [Registry](../../app/src/workspace/registry.rs) · [Workspace](../../app/src/workspace/view.rs) · [Tab data](../../app/src/tab.rs) |
| Pane identity and layout | `PaneId` identifies a pane view in the running app. A `PaneGroup` owns content objects and a pane tree with visible splits and hidden content. Split direction and flex proportions drive layout. Saved pane UUIDs identify persisted pane records across restoration. [Pane interfaces](../../app/src/pane_group/pane/mod.rs) · [Tree](../../app/src/pane_group/tree.rs) |
| Terminal ownership | `TerminalPane` retains its pane view and the live terminal stack. Terminal managers retain the model, controllers, and PTY or connection. The model retains live grid and block state. Moving pane content with its backing handles keeps this graph alive. [Terminal pane](../../app/src/pane_group/pane/terminal_pane.rs) · [Terminal manager](../../app/src/terminal/local_tty/terminal_manager.rs) |
| Existing pane drag and transfer | Within a tab the drag uses the tree's hidden pane preview. To move a split pane to another tab, `SwitchTabFocusAndMovePane` calls `remove_pane_for_move`, switches to the target tab and calls `add_pane_as_hidden` for continued placement. A tab-bar tear-out can pass the existing pane to `add_tab_from_existing_pane`. Both rely on normal detach/attach registrations. [Pane group](../../app/src/pane_group/mod.rs) · [Workspace events](../../app/src/workspace/view.rs) |
| Existing tab drag | Horizontal and vertical tab rows share drag state and workspace actions for tab reordering. `CrossWindowTabDrag` handles floating windows and whole-tab handoff, including platform-specific behavior. Same-window directional docking uses the normal tab row as a drag source and keeps its preview local to the existing window. [Vertical tabs](../../app/src/workspace/view/vertical_tabs.rs) · [Cross-window drag](../../app/src/workspace/cross_window_tab_drag.rs) |
| Focus and resize | The workspace's active tab and the group's focus state coordinate pane selection and terminal focus. Terminal view layout calculates the grid size and emits a resize through the existing PTY controller. The group already defines minimum pane size, including the larger requirement when Universal Developer Input is enabled. [Group focus](../../app/src/pane_group/focus_state.rs) · [Terminal view](../../app/src/terminal/view.rs) · [PTY controller](../../app/src/terminal/writeable_pty/pty_controller.rs) |
| Dependent views | Pane content implements attach/detach hooks. Terminal hooks update the workspace subscriptions and active-agent registrations. A group also owns hidden child-agent panes and editor-specific state. A future capability check must account for these relationships and preserve ownership through a move. [Pane interfaces](../../app/src/pane_group/pane/mod.rs) · [Pane group](../../app/src/pane_group/mod.rs) |
| Restoration and display topology | Workspace snapshots feed SQLite restoration. Platform windows expose fullscreen state, and the application receives screen-change notifications. Pane maximization, app fullscreen and monitor topology are separate inputs to future planning. [Persistence](../../app/src/persistence/README.md) · [App callbacks](../../app/src/lib.rs) · [macOS window](../../crates/warpui/src/platform/mac/window.rs) |
| Keyboard and commands | Workspace and pane actions already dispatch through WarpUI. Future keyboard docking enters an explicit mode through a configurable binding and Command Palette action. [Workspace actions](../../app/src/workspace/action.rs) · [Pane actions](../../app/src/pane_group/mod.rs) |

## Milestone 1 design

### Planning and presentation

Activate the destination tab first, then drag the inactive source tab from the tab bar. The gesture records both pane-group identities and resolves them against current workspace state. The destination pane is addressed by its `PaneId`. Inactive groups retain previously laid out bounds, so use only the active group's current visible pane geometry for hit testing.

Choose Left, Right, Top or Bottom from the pointer position and calculate the prospective new pane rectangle using the same direction and split proportions as commit. The available extent must support both panes under the current minimum-size policy. A preview is presentation state. It never changes the pane tree, opens a shell or takes ownership of content. A stale pane, a tab closing mid-drag, or Escape clears or invalidates the preview. Pointer-up revalidates the current source, destination and geometry. The center stays neutral until Swap and Stack are available. A window-scoped input guard keeps drag events and the cancellation keystroke from reaching a running terminal application.

### Transaction

1. Confirm both tabs are present in the same workspace, the source and destination identities are distinct, and the destination is visible. The first milestone requires one visible, connected terminal in each tab with no additional pane content. Special ownership and state such as dependent child agents, shared sessions, synchronized input and stacked content require additional transfer rules.
2. Validate that the destination tree can accept the specific directional split and retain the planned extent. The layout change and transfer run in one workspace update, with normal view events processed after the operation.
3. Extract the source's original pane content, stage the destination split and restore the source box if the split fails. A sole-leaf extraction avoids the normal multi-pane path's focus fallback and sole-leaf tree removal. Moving the same boxed content keeps the pane ID, terminal view, manager, model and session identity. After the split succeeds, `DetachType::Moved` and the normal attach hook update group subscriptions and registrations.
4. Attach into the validated split and apply normal pane focus. Remove the now-empty source tab through existing workspace cleanup, with no close-tab undo entry. Standard layout drives subsequent grid and PTY resize.
5. Any rejected operation leaves both layouts untouched. Later multi-pane and cross-window operations need explicit plans for restoring ownership if a transfer has fallible steps. Layout history must store bounded reversible layout operations keyed to live identities. If a referenced process terminates, undo must handle the missing reference without respawning it or replaying its commands.

For later milestones, the planner should remain independent of live mutation. Templates describe geometry and constraints. Snapshots may additionally identify sessions. The transaction owns revalidation, transfer, registration and focus. Workspace snapshots reflect committed layouts and layout history holds only the references needed to reverse live moves.

### Current scope

This milestone covers the inactive-tab gesture when both tabs contain one eligible terminal. Dragging an eligible inactive tab reserves the gesture for same-window docking until release. A release in the tab bar can still reorder the tab using the existing adjacent-move and grouping rules. Full drag-through reordering and cross-window handoff for this gesture are deferred. Tabs with other content continue to use existing drag behavior. The source tab must have an individual draggable row.

Center Swap and Stack, multiple panes, multiple windows, layout undo, and specialized content remain tracked in later milestones. The experimental feature is disabled by default and explicitly enabled in the isolated development build.

## Milestone 2 stack behavior

A stack shares one visible layout slot. Each member remains a separate pane with its original view, terminal manager, session, and saved pane UUID. The active pane's overflow menu lists the other members. Selecting a member puts that pane into the same slot and focuses it. Closing a member removes only that pane and promotes a sibling if needed. The pane-to-tab action extracts the active member and keeps the remaining members in the original slot.

The existing workspace snapshot format records each stack member once as a regular split. After restart those members appear as splits. Retained persistent terminals follow their existing reattachment or disconnected-placeholder behavior. Ordinary terminals follow Warp's normal workspace restoration behavior. Preserving stacked layout across restart and applying saved layouts to existing sessions belong to the layout snapshot work.

## Persistent-session integration contract

| Contract | Responsibility |
| --- | --- |
| Terminal session identity | The live terminal manager/model own the current runtime session identity. An optional persistent owner UUID identifies the attachable PTY across GUI restarts. A move carries existing handles and preserves both forms of identity when present. A process PID is acceptance evidence and may change only through an independent lifecycle event. |
| Pane identity | Docking keeps the same live `PaneId` and persisted pane UUID while changing workspace ownership. View handles and `EntityId` values address live objects. A snapshot that crosses restarts needs a stable saved identity and may resolve to a missing pane. |
| Lifecycle | The terminal/session side owns started, connected, disconnected, exited and retained state. Docking checks capability against that state and observes normal terminal events. A terminal exiting between preview and drop invalidates the plan. |
| Attach and detach | `DetachType::Moved` is a workspace ownership transition. Persistent detach/reattach refers to the process transport and its writer lease. Docking neither disconnects a healthy transport nor initiates reconnect, replay or owner creation. |
| View ownership and metadata | Group transfer moves the existing view with pane metadata, block history, editor input, PTY controllers and dependent registrations that the operation supports. Session labels, owner identity, retention and transport state remain with the terminal manager. |
| Layout ownership | The workspace and pane tree own ordering, split proportions, active pane, layout history and snapshots. The persistent transport has no dependency on docking and owns no workspace layout. |
| Optional compatibility | Run the same transfer with an ordinary PTY. Additional capability adapters may later support existing disconnected placeholders while keeping explicit session recovery separate. A failed or missing attachment must never trigger a replacement shell during docking, undo or template application. |

The persistent-session code in the fork base changes `TerminalManager`, `TerminalView`, `PaneGroup`, workspace actions and snapshots, and SQLite terminal records. These are the overlap points to inspect during integration. Keep transport and persistence-schema changes on their owning branch. Validate docking and persistence independently, then use a temporary integration branch for any unshipped compatibility change. Verify ordinary and persistent terminals, attachment failure, restoration, metadata, focus, no duplicate live owners, and a single PTY consumer for each session.

## Milestones and approved decisions

| Milestone | Planned work | Status |
| --- | --- | --- |
| 1. Existing tab to split | Same-window single-terminal tab source, four directional targets, live split preview, cancel, live identity transfer, normal source-tab cleanup. | Implemented and verified in the isolated macOS development build. See the evidence log. |
| 2. Rearrangement and docking | Intra-tab and inter-tab moves, swap, center Swap/Stack, pane-to-tab, atomic multi-pane layout moves, nested structure and optional flattening. | Implemented and verified with focused macOS integration and unit tests. See the evidence log. |
| 3. Resizing and undo/redo | Adaptive proportions, minimum sizes, manual rebalance and divider reset, normal PTY resize, bounded layout undo and redo with explicit layout commands. | Implemented and verified with focused macOS tests. See the evidence log. |
| 4. Windows and monitors | Cross-window pane and tab-layout moves, preview and tear-out, configurable empty-window cleanup, focus restoration and fullscreen-compatible alternatives. | Implemented and verified for two-window macOS workflows. Physical display and fullscreen checks are pending. |
| 5. Intelligent layouts | Named templates, workspace snapshots, import/export, session reassignment, overflow alternatives, display profiles and recovery on display changes. | Pending |
| 6. Advanced interaction | Multi-selection and temporary groups, drag handles, explicit keyboard docking, searchable Pane and Workspace Navigator, focus options and accessibility. | Pending |
| 7. Specialized content | Capability-aware movement of agent and editor views, unsaved state and parent/child dependencies, fullscreen and maximized states, and optional persistent-session lifecycle integration. | Pending |
| 8. Hardening and rollout | Bounded preview work, measured frame and resource budgets, diagnostics, stress and session integrity testing, accessibility and platform validation, staged rollout. | Pending |

| ID | Approved product decision | Milestones |
| --- | --- | --- |
| D1 | Unified visual drag-and-drop system | 1, 2, 4, 6 |
| D2 | Smart docking with live layout previews | 1, 2, 4, 5 |
| D3 | Smart center docking with Swap and Stack | 2 |
| D4 | Smart multi-pane group movement | 2, 4, 6 |
| D5 | Adaptive resizing with manual control | 3 |
| D6 | Transactional layout history with recovery | 3 |
| D7 | Full cross-window docking with configurable cleanup | 4 |
| D8 | Full keyboard docking mode | 6 |
| D9 | Intelligent layout management | 5 |
| D10 | Intelligent overflow management | 5 |
| D11 | Capability-aware universal docking | 1, 7 |
| D12 | Adaptive, customizable drag controls | 6 |
| D13 | Predictable, transactional drag-and-drop | 1, 2, 3, 4 |
| D14 | Context-aware focus management | 1, 3, 4, 6 |
| D15 | Fullscreen-aware smart docking | 4, 7 |
| D16 | Full lifecycle-aware docking | 7 |
| D17 | Advanced multi-pane selection and grouping | 6 |
| D18 | Adaptive layouts with display-specific profiles | 5 |
| D19 | Intelligent display topology management | 4, 5 |
| D20 | Unified Pane and Workspace Navigator | 6 |
| D21 | Comprehensive accessible interaction system | 6, 8 |
| D22 | Adaptive high-performance docking | 8 |
| D23 | Modular docking architecture with upstream compatibility | 1–8 |
| D24 | Production-grade validation with staged rollout | 1–8 |
| D25 | Coordinated parallel development with staged integration | 1–8 |

### Rearrangement design

Directional docking replaces the target leaf with a nested branch and preserves the leaf's flex in its enclosing branch. Whole-tab transfer uses the incoming tree and its flex ratios. Flattening explicitly arranges the incoming panes in visual order along the dock axis. Same-tab moves and swaps edit the tree while each terminal stays attached to its pane group.

Transfers rebind focus observers to the new pane group. After detaching from a tab that still has other terminals, the transaction restores their shared-session and AI history subscriptions. Cross-tab hover computes geometry from pane-tree flex and divider widths, then paints an outline over the mounted source. Only the commit attaches a terminal to its new group and activates the destination.

Stack members retain their original `PaneId`, `TerminalPane`, metadata, and lifecycle subscriptions. One member occupies the visible slot. Workspace snapshots save all members as ordinary splits, so a restored stack uses split layout. Persistent members use the existing persistent owner restore behavior. An attached hidden member can be found and revealed by its owner UUID.

### Moving a split pane back to a tab

Open the terminal pane's header overflow and choose **Move to own tab**. The command moves the pane whose menu you opened, preserves its terminal view and process, and focuses the new tab. The existing drag back to the tab list remains available. The menu action uses the opt-in enhanced pane management flag.

Docking into existing splits has no fixed pane count. Available space and the minimum usable size of each pane determine whether a layout fits.

### Rearranging terminals

- Drag an inactive tab to an edge of a visible terminal pane. If the source tab has multiple eligible terminals, the move keeps its nested splits and proportions. Hold **Option** on macOS or **Alt** on other platforms during the drag to flatten the incoming layout along the drop direction.
- Drag a pane title to another pane in the same tab. To target a different tab, hover over its row first, then continue onto an edge in the layout outline. Hover shows pane geometry without rendering the inactive terminals or changing the active tab.
- Drop on **Swap** or **Stack** in the center for two eligible terminals. A swap keeps both slots. A stack shares one slot and lists the original terminals in the pane menu. Close the visible member to reveal another, or choose **Move to own tab** to extract it.
- Drop a pane into a gap in the tab list to move it to its own tab. The pane menu provides the same operation.

Cross-tab previews require an inactive destination whose pane area is clear of open side panels. Docking checks the size of each affected slot. Existing stacks can accept eligible terminals. For split or swap involving a stack member, move the member into its own tab first.

### Resizing and layout history

- Drag a divider to preview its new position. Release to commit, or press **Escape** to cancel. Double-click the divider to reset the proportions of its two neighbors.
- Run **Balance Panes** from the Command Palette to rebalance the active tab. Use **Undo Pane Layout Change** and **Redo Pane Layout Change** to step through supported layout edits. All three commands support custom keybindings.
- Each window keeps up to 32 recent layout edits until restart. History checks the tab order and the affected pane and session IDs before applying a change. A tab or session change can make an entry stale. If the layout needs a larger window, the entry remains available to try again.
- History can replay docking, stacking, swapping, moving panes to tabs, and resizing. It moves or resizes existing panes and preserves their processes.

### Moving panes between windows

- Drag a pane header or a whole tab to a split edge or the tab strip of another Warp window. The destination previews the drop. Escape cancels it.
- Drag a pane or tab outside Warp windows to open it in a new window on the display where you dropped it. An entire tab keeps its split tree when moved as a tab.
- The pane overflow and tab context menus list eligible windows and offer a new-window action. The Command Palette offers **Move Pane to New Window** and **Move Tab Layout to New Window**. These actions provide a route when a window manager or fullscreen space limits pointer dragging.
- The source window closes by default when its last tab leaves. To leave it open with a fresh tab, run **Keep Empty Windows After Moving Panes**. **Close Empty Windows After Moving Panes** restores the default.
- **Swap** and **Stack** are available within a window. Window moves operate on live terminal views and preserve the shell and running jobs. Cross-window transfers are outside the per-window layout undo history.

## Build and validation

Build from the isolated worktree with the pinned Rust toolchain, Protobuf, Xcode and Metal tools, and the repository's macOS bundling prerequisites. This invokes the macOS runner directly to select the OSS binary and keep the debug bundle in the worktree.

```sh
export CARGO_TARGET_DIR="$PWD/target"
export WARP_PERSISTENT_PTY_DIR="$(mktemp -d /tmp/warpdock.XXXXXX)"
export WARP_DATA_PROFILE="$(basename "$WARP_PERSISTENT_PTY_DIR")"
WARP_BIN_NAME=warp-oss WARP_CHANNEL=oss FEATURES=gui,enhanced_pane_management \
  ./script/macos/run --dont-open
docking_app="$CARGO_TARGET_DIR/debug/bundle/osx/WarpOss.app"
/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$docking_app/Contents/Info.plist"
"$docking_app/Contents/MacOS/warp-oss"
```

Verify the bundle identifies itself as `dev.warp.WarpOss` before launching it. Set `HOME` and `ZDOTDIR` to a disposable directory for the binary launch to separate its shell startup and preferences from the production profile. For the feature-off control, omit `enhanced_pane_management` from `FEATURES` and use a distinct target and debug profile. An OSS build uses a persistent runtime by default on eligible macOS paths. For the ordinary-PTY control, set `WARP_PERSISTENT_PTY_DIR` to an existing regular file in the disposable runtime directory before launch and verify that the app logs its direct local PTY fallback. Turning off auto-reattach alone leaves the default runtime behavior in place.

Launch the signed Milestone 4 development build from SANDISK SSD.

```sh
"/Volumes/SANDISK SSD/git/warp-pane-management-m4/target/start"
```

The M4 launcher uses its own SSD profile and private PTY runtime. The M3 SSD
launcher is at `/Volumes/SANDISK SSD/git/warp-pane-management/target/start`.

The following Milestone 3 temporary launcher is retained as historical evidence.

```sh
/private/tmp/warp-docking-m3.GPjX9s/start
```

The launcher reuses the same private runtime directory across launches. The earlier
Milestone 2 build has a separate profile and runtime.

```sh
dock_stage=/private/tmp/warp-docking-m2.ea53i9
dock_runtime="$(mktemp -d /tmp/wpdock.XXXXXX)"
env HOME="$dock_stage/home" ZDOTDIR="$dock_stage/home" \
  WARP_DATA_PROFILE=docking-m2 WARP_PERSISTENT_PTY_DIR="$dock_runtime" \
  WARP_SHELL_PATH=/bin/zsh "$dock_stage/WarpOss.app/Contents/MacOS/warp-oss"
```

For Milestone 1, start two disposable long-running commands that print the workload PID and a monotonic counter. Record both workload and shell PIDs before drag. Compare the same PIDs and continuously increasing output after the move. Check existing scrollback and typed input, all four directions, Escape, same-tab and invalid target rejection, small targets, source-tab cleanup, and flag-off tab drag behavior. Cover zsh and bash and run focused GUI regression tests through the repository integration harness. Expand session integrity coverage to SSH, interactive applications and simultaneous output in later validation.

The macOS integration test compares both shell PIDs and child workload PIDs with their process start times and checks the same terminal view and session identities. It covers previews on all four edges, a rejected center drop, Escape, and a committed right split. Run the focused test with the local AppKit display and the integration harness's disposable home.

```sh
WARPUI_USE_REAL_DISPLAY_IN_INTEGRATION_TESTS=1 WARP_SHELL_PATH=/bin/zsh \
  cargo run --locked -p integration --bin integration -- test_dock_tab_preserves_running_processes
```

The final test candidate also ran through the normal nextest registration. Use a real-display session for a native smoke test or the ordinary integration backend for repeatable process checks.

```sh
WARP_SHELL_PATH=/bin/zsh cargo nextest run --locked -p integration \
  --features drag_tabs_to_windows \
  -E 'test(test_dock_tab_preserves_running_processes) | test(test_reorder_tabs_with_drag)'
WARP_SHELL_PATH=/bin/bash cargo nextest run --locked -p integration \
  --features drag_tabs_to_windows -E 'test(test_dock_tab_preserves_running_processes)'
```

The Milestone 2 regression suite exercises the drag and menu entry points. The tree tests also
check flattening without changing the source pane order.

```sh
docking_tests='test(test_dock_) | test(test_move_split_pane_to_new_tab_from_overflow) | test(test_stack_terminal_tabs_and_extract_member) | test(test_swap_panes_from_header_drag) | test(test_reorder_tabs_with_drag)'
for warp_shell in /bin/zsh /bin/bash; do
  WARP_SHELL_PATH="$warp_shell" cargo nextest run --locked -p integration \
    --features drag_tabs_to_windows -E "$docking_tests"
done
cargo nextest run --locked -p warp --lib --features gui,enhanced_pane_management \
  -E 'test(pane_group::tree::tests) | test(docked_stack) | test(persistent_close_completion_targets)'
cargo clippy --locked -p warp --lib --features gui,enhanced_pane_management -- -D warnings
cargo clippy --locked -p integration --lib --bin integration \
  --features drag_tabs_to_windows -- -D warnings
```

Milestone 3 adds the `test(test_pane_layout_)` filter to the integration run and
`test(docking_layout_history)` to the app unit run. The stack regression also undoes
the member extraction and the original stack. The resize test checks the actual
divider gesture, terminal column count, and the three Command Palette actions.

Milestone 4 adds two tests to the shell integration runner. The pointer test uses
real AppKit windows so it can exercise native window ordering. The Command
Palette tear-out test uses the standard offscreen integration mode.

```sh
WARP_INTEGRATION=1 WARPUI_USE_REAL_DISPLAY_IN_INTEGRATION_TESTS=1 WARP_SHELL_PATH=/bin/zsh \
  cargo run --locked -p integration --bin integration --features drag_tabs_to_windows -- \
  test_move_pane_between_windows_preserves_processes
WARP_INTEGRATION=1 WARP_SHELL_PATH=/bin/zsh \
  cargo run --locked -p integration --bin integration --features drag_tabs_to_windows -- \
  test_tear_out_tab_layout_preserves_processes
cargo test --locked -p warpui_core --lib transfer_
```

Run focused tests, then relevant lint checks, then format, in the order in [AGENTS.md](../../AGENTS.md). Keep one record per milestone with the exact commands, build and commit, platform, process-identity evidence, failures or unavailable tests, and the next milestone. Production installation and profiles stay separate from the development run.

### Evidence log

| Item | Result |
| --- | --- |
| Milestone 4 integration checks | Eleven tests passed with zsh and Bash on Apple Silicon macOS. The native pointer case verified split preview, Escape and retry, moving a pane to a second window, and moving the active split layout back through the destination tab strip. The offscreen Command Palette case tore out an entire split twice and verified the close and keep source-window policies. The tests compared view IDs, session IDs, shell and foreground process IDs, start times, and focus. |
| Milestone 4 WarpUI transfer tests | All 16 focused tests passed, including reparenting a view before transferring its former and current parents. |
| Milestone 4 app checks and SSD build | All 18 focused app unit tests passed, the registered tear-out test passed through the normal test harness, and Clippy passed for the app, WarpUI core, and integration runner with warnings denied. The signed `dev.warp.WarpOss` bundle on SSD passed strict signature verification with CDHash `71d6e3a67427866598d06f9d6741cf1bff1b736e`. |
| Milestone 4 platform coverage | Real AppKit windows were exercised. A physical second-display run, fullscreen transitions, Linux, and Windows are pending. |
| Milestone 3 integration checks | All 10 focused tests passed on Apple Silicon macOS with zsh and Bash. They verified the **Move to own tab** menu item, docking and stack undo, redo, stale history after shell exit, divider preview and Escape, committed terminal dimensions, divider reset, Balance Panes, and feature-off tab reordering. Process checks compared terminal view and session IDs and shell and foreground process IDs and start times. |
| Milestone 3 app unit checks | All 44 focused tests passed. Coverage included nested pane minimums, maintaining preferred proportions as space returns, clamped divider movement, layout history limits, redo invalidation, and persistent stack routing. The app fixtures ran with native macOS system access. |
| Milestone 3 WarpUI checks | All 25 focused Flex and wrap tests passed with `cargo nextest run --locked -p warpui_core --lib -E 'test(elements::gui::flex)'`. |
| Milestone 3 lint and build | Targeted Clippy passed for the app, WarpUI core, and integration harness with warnings denied. The `dev.warp.WarpOss` development bundle passed strict code signature verification. The bundle CDHash is `78942cad369034288924248a12b29cd3e2afb69e`. The gestures were verified through the integration harness. |
| Move to own tab | The macOS integration test opened the unfocused pane's menu, checked its placement below **Maximize pane**, moved it, and verified both original terminal view and session IDs and the shell and foreground process IDs and start times. The new test and the tab docking regression passed on zsh and Bash. Both targeted Clippy checks passed. |
| Milestone 2 integration checks | All eight focused tests passed on Apple Silicon macOS with zsh and Bash 3.2. The tests covered directional docking, nested destinations, cross-tab preview and move, a whole nested layout, Swap, Stack and member extraction, the pane menu, and feature-off tab reorder. They compared live terminal views, sessions, shell and foreground process IDs, and process start times across the moves. |
| Milestone 2 macOS development build | `/private/tmp/warp-docking-m2.ea53i9/WarpOss.app` was built as `dev.warp.WarpOss` with the enhanced pane management feature, signed and verified with strict code signature checks. The bundle CDHash is `6aad2dc6909d1a3dcbe599a23c17fe3f33755c34`. The Milestone 2 gestures were exercised through the integration harness. Manual checks of this bundle are pending. |
| Milestone 2 unit checks | All 35 focused tests passed, covering the split tree, including layout flattening and minimum sizes, stack switching, closing and snapshot projection, and persistent close routing. The app fixtures required access to the macOS system configuration outside the filesystem sandbox. |
| Milestone 1 automated checks | Final macOS nextest runs passed for zsh and Bash 3.2. `test_dock_tab_preserves_running_processes` covered all four previews, center rejection, Escape, and shell, workload, terminal view and session identity after a right split. `test_reorder_tabs_with_drag` passed with enhanced docking disabled. |
| Milestone 1 macOS development build and process-identity comparison | Signed debug `WarpOss.app` on Apple Silicon. Native vertical-tab drag moved the source into the right split and focused it. Both command blocks retained their pre-move output and continued printing. Destination shell/workload PIDs were `84311` and `84496`. Source shell/workload PIDs were `84611` and `84746`. `ps` snapshots of all four PIDs, parent PIDs and start times were byte-identical before and after. |
| Targeted lint and formatting | App and integration Clippy passed with warnings denied. Repository Rust formatting and the whitespace diff check passed. |
| Other platforms | Linux, Windows and the remaining platform matrix are pending. |
| Compatibility with the persistent-session backend and ordinary PTY | The native run used the disposable runtime `/tmp/wpdock.9YetIM`, which retained both owner sockets after the move. The integration runner verified ordinary local PTYs under its isolated Integration channel. |
| Performance | No measurements yet. Before rollout, measure busy workspaces at 10, 50 and 100 panes, recording preview frame time, planner cost, allocation or memory data, CPU overhead and input/output latency. Derive budgets from a feature-off baseline on the same hardware. |
| Implementation and validation commit references | Design checkpoint `93aa258f`. Milestone 1 implementation and regression coverage `2823392e`. Pane menu `eb2a02d3`. Milestone 2 implementation and regression coverage `4d2f3fb0`. Milestone 3 implementation and regression coverage `8bf2a058`. Milestone 4 implementation and regression coverage `4599287a`. |

## Contribution boundaries

The fork's first review boundary is the default-off same-window transfer and its regression evidence. Separate rearrangement, layout history, cross-window handoff and specialized content changes so each can be validated on its own. Any upstream proposal should begin with updated searches and maintainer agreement on the feature issue. Once the issue is ready for a spec, prepare separate `specs/GH8959/product.md` and `tech.md` files with testable behavior, architecture and manual evidence. Attribute design references and check the applicable repository license and contribution requirements before carrying any external code forward. [Contribution guide](../../CONTRIBUTING.md)
