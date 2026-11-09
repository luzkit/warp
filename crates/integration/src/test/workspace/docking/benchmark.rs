use std::cell::Cell;
use std::io;
use std::mem::MaybeUninit;
use std::rc::Rc;
use std::time::Duration;

use instant::Instant;
use pathfinder_geometry::vector::vec2f;
use serde_json::json;
use sysinfo::{Pid, ProcessesToUpdate, System};
use warp::features::FeatureFlag;
use warp::integration_testing::terminal::assert_terminal_bootstrapped;
use warp::integration_testing::view_getters::{terminal_view, workspace_view};
use warp::integration_testing::workspace::{assert_focused_tab_index, assert_tab_count};
use warp::workspace::WorkspaceAction;
use warp::workspace::tab_settings::VerticalTabsDisplayGranularity;
use warpui_core::event::{Event, ModifiersState};
use warpui_core::integration::{AssertionOutcome, StepDataMap, TestStep};
use warpui_core::{App, TypedActionView, WindowId, async_assert_eq};

use super::super::enable_vertical_tabs;
use super::{
    PREVIEW_POSITION, PROCESS_SNAPSHOT, TerminalProcesses, drag_tab_to_right_edge, no_preview,
    pane_bounds, processes_at, release_tab, saved_bounds, shell_supports_process_tests,
    with_preview_assertion,
};
use crate::Builder;

const WORKLOAD: &str = r#"/bin/bash -c 'printf "dock-child=%s\n" "$$"; for ((i=0; i<900; i++)); do if IFS= read -r -t 1 line; then printf "dock-reply=%s\n" "$line"; else printf "dock-tick=%s\n" "$i"; fi; done'"#;
const BENCHMARK: &str = "docking benchmark";
const SAMPLES: usize = 72;
const SAMPLE_INTERVAL: Duration = Duration::from_millis(40);
const ROUND_TRIPS: usize = 5;
const BOOTSTRAP_TIMEOUT: Duration = Duration::from_secs(180);
const WORKSPACE_STEP_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Default)]
struct Benchmark {
    ticks: Vec<u32>,
    pointer: Vec<Duration>,
    pointer_to_scene: Vec<Duration>,
    round_trips: Vec<Duration>,
    output_lines: u32,
    usage_start: Option<Usage>,
    usage_end: Option<Usage>,
}

struct Usage {
    at: Instant,
    cpu: Duration,
    rss_bytes: u64,
    peak_rss_bytes: u64,
}

impl Usage {
    fn read() -> io::Result<Self> {
        let pid = Pid::from_u32(std::process::id());
        let mut system = System::new();
        system.refresh_processes(ProcessesToUpdate::Some(&[pid]), true);
        let rss_bytes = system
            .process(pid)
            .ok_or_else(|| io::Error::other("Could not read application RSS"))?
            .memory();
        let mut usage = MaybeUninit::<nix::libc::rusage>::uninit();
        // The pointer provides enough aligned storage for getrusage to fill.
        let result = unsafe { nix::libc::getrusage(nix::libc::RUSAGE_SELF, usage.as_mut_ptr()) };
        if result != 0 {
            return Err(io::Error::last_os_error());
        }
        // getrusage successfully initialized the output struct.
        let usage = unsafe { usage.assume_init() };
        let cpu_time = |time: nix::libc::timeval| {
            Duration::new(time.tv_sec as u64, time.tv_usec as u32 * 1_000)
        };
        let rss_unit = if cfg!(target_vendor = "apple") {
            1
        } else {
            1024
        };
        Ok(Self {
            at: Instant::now(),
            cpu: cpu_time(usage.ru_utime) + cpu_time(usage.ru_stime),
            rss_bytes,
            peak_rss_bytes: usage.ru_maxrss as u64 * rss_unit,
        })
    }
}

fn workload_output(app: &App, window: WindowId, tab: usize) -> String {
    terminal_view(app, window, tab, 0).read(app, |view, _| {
        view.model
            .lock()
            .block_list()
            .active_block()
            .output_to_string()
    })
}

fn output_ticks(app: &App, window: WindowId, pane_count: usize) -> Option<Vec<u32>> {
    (0..pane_count)
        .map(|tab| {
            workload_output(app, window, tab)
                .lines()
                .rev()
                .find_map(|line| line.trim().strip_prefix("dock-tick=")?.parse().ok())
        })
        .collect()
}

fn assert_original_processes(
    app: &App,
    window: WindowId,
    data: &StepDataMap,
    layout: &[(usize, usize)],
) -> AssertionOutcome {
    let expected = data
        .get::<_, Vec<TerminalProcesses>>(PROCESS_SNAPSHOT)
        .expect("The original sessions and processes were recorded");
    match processes_at(app, window, layout) {
        Ok(actual) => async_assert_eq!(&actual, expected),
        Err(error) => AssertionOutcome::failure(error.to_string()),
    }
}

fn sample_frame(
    app: &mut App,
    window_id: WindowId,
    index: usize,
    preview: bool,
) -> (Duration, Duration) {
    let at = match index % 4 {
        0 => vec2f(0.10, 0.5),
        1 => vec2f(0.90, 0.5),
        2 => vec2f(0.5, 0.10),
        _ => vec2f(0.5, 0.90),
    };
    let pane = pane_bounds(app, window_id, 0, 0);
    let position = pane.origin() + pane.size() * at;
    let event = if preview {
        Event::LeftMouseDragged {
            position,
            modifiers: ModifiersState::default(),
        }
    } else {
        Event::MouseMoved {
            position,
            cmd: false,
            shift: false,
            is_synthetic: false,
        }
    };
    let window = app.read(|ctx| {
        ctx.windows()
            .platform_window(window_id)
            .expect("Window exists")
    });
    let workspace = workspace_view(app, window_id);
    let start = Instant::now();
    let pointer = app.update(|ctx| {
        (window.callbacks().event_callback)(event, ctx);
        let pointer = start.elapsed();
        // Both modes build one scene via the integration runner's eager invalidation handler.
        workspace.update(ctx, |_, ctx| ctx.notify());
        pointer
    });
    let pointer_to_scene = start.elapsed();
    assert_eq!(
        saved_bounds(app, window_id, PREVIEW_POSITION).is_some(),
        preview,
        "The scene should match the benchmark mode",
    );
    (pointer, pointer_to_scene)
}

fn round_trip(index: usize) -> TestStep {
    let sent = Rc::new(Cell::new(None::<Instant>));
    let started = Rc::clone(&sent);
    let request = format!("dock-request-{index}");
    let response = format!("dock-reply={request}");
    TestStep::new("Measure keyboard input through the busy PTY")
        .set_timeout(WORKSPACE_STEP_TIMEOUT)
        .with_event_fn(move |_, _| {
            started.set(Some(Instant::now()));
            Event::TypedCharacters {
                chars: request.clone(),
            }
        })
        .with_keystrokes(&["enter"])
        .add_named_assertion_with_data_from_prior_step(
            "The foreground job echoed the application input",
            move |app, window, data| {
                if !workload_output(app, window, 0)
                    .lines()
                    .any(|line| line.trim() == response)
                {
                    return AssertionOutcome::failure("Waiting for the PTY reply".to_owned());
                }
                data.get_mut::<_, Benchmark>(BENCHMARK)
                    .expect("Benchmark started")
                    .round_trips
                    .push(sent.get().expect("Input was dispatched").elapsed());
                AssertionOutcome::Success
            },
        )
}

fn milliseconds(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.
}

fn percentiles(samples: &mut [Duration]) -> serde_json::Value {
    samples.sort_unstable();
    let rank = |percent: usize| milliseconds(samples[(samples.len() * percent).div_ceil(100) - 1]);
    json!({ "p50": rank(50), "p95": rank(95) })
}

/// Opt-in because the largest case starts 100 local terminal sessions.
pub fn benchmark_enhanced_pane_management() -> Builder {
    let pane_count = std::env::var("WARP_DOCKING_BENCH_PANES")
        .expect("Set WARP_DOCKING_BENCH_PANES to 10, 50, or 100")
        .parse::<usize>()
        .expect("Pane count must be numeric");
    assert!(
        matches!(pane_count, 10 | 50 | 100),
        "Use 10, 50, or 100 panes"
    );
    let mode = std::env::var("WARP_DOCKING_BENCH_MODE")
        .expect("Set WARP_DOCKING_BENCH_MODE to baseline or preview");
    let preview = match mode.as_str() {
        "baseline" => false,
        "preview" => true,
        _ => panic!("Use baseline or preview mode"),
    };
    assert!(
        shell_supports_process_tests(),
        "Run this benchmark with zsh or Bash"
    );
    FeatureFlag::EnhancedPaneManagement.set_enabled(preview);

    let initial_layout: Vec<_> = (0..pane_count).map(|tab| (tab, 0)).collect();
    let layout = initial_layout.clone();
    let mut builder = Builder::new()
        .with_timeout(Duration::from_secs(600))
        .with_step(enable_vertical_tabs(VerticalTabsDisplayGranularity::Tabs));
    for first in (0..pane_count).step_by(10) {
        let end = (first + 10).min(pane_count);
        let mut bootstrapped: Vec<_> = (first..end)
            .map(|tab| assert_terminal_bootstrapped(tab, 0))
            .collect();
        builder = builder.with_step(
            TestStep::new("Open and bootstrap a batch of terminal tabs")
                .set_timeout(BOOTSTRAP_TIMEOUT)
                .with_action(move |app, window, _| {
                    workspace_view(app, window).update(app, |workspace, ctx| {
                        for _ in first.max(1)..end {
                            workspace.handle_action(&WorkspaceAction::AddDefaultTab, ctx);
                        }
                    });
                })
                .add_assertion(assert_tab_count(end))
                .add_named_assertion(
                    "Every terminal in the batch has bootstrapped",
                    move |app, window| {
                        bootstrapped
                            .iter_mut()
                            .map(|assertion| assertion(app, window))
                            .find(|result| !matches!(result, AssertionOutcome::Success))
                            .unwrap_or(AssertionOutcome::Success)
                    },
                ),
        );
    }
    builder = builder
        .with_step(
            TestStep::new("Start the workloads and focus the destination")
                .set_timeout(WORKSPACE_STEP_TIMEOUT)
                .with_action(move |app, window, _| {
                    let workspace = workspace_view(app, window);
                    let terminals: Vec<_> = (0..pane_count)
                        .map(|tab| terminal_view(app, window, tab, 0))
                        .collect();
                    app.update(|ctx| {
                        for terminal in terminals {
                            terminal.update(ctx, |view, ctx| {
                                view.execute_command_or_set_pending(WORKLOAD, ctx);
                            });
                        }
                        workspace.update(ctx, |workspace, ctx| {
                            workspace.handle_action(&WorkspaceAction::ActivateTab(0), ctx);
                        });
                    });
                })
                .add_assertion(assert_focused_tab_index(0))
                .add_assertion(assert_tab_count(pane_count)),
        )
        .with_step(
            TestStep::new("Record the running sessions")
                .set_timeout(WORKSPACE_STEP_TIMEOUT)
                .add_named_assertion_with_data_from_prior_step(
                    "Every pane has a live shell and foreground job",
                    move |app, window, data| match processes_at(app, window, &layout) {
                        Ok(processes) => {
                            data.insert(PROCESS_SNAPSHOT, processes);
                            AssertionOutcome::Success
                        }
                        Err(error) => AssertionOutcome::failure(error.to_string()),
                    },
                ),
        );
    let preview_right = move || {
        with_preview_assertion(
            TestStep::new("Preview docking the second tab on the right")
                .with_action(drag_tab_to_right_edge)
                .add_assertion(assert_focused_tab_index(0))
                .add_assertion(assert_tab_count(pane_count)),
            vec2f(0.75, 0.5),
            vec2f(0.5, 1.0),
        )
    };
    if preview {
        builder = builder.with_step(preview_right());
    }

    builder = builder.with_step(
        TestStep::new("Warm up the scene with four pointer positions")
            .set_timeout(WORKSPACE_STEP_TIMEOUT)
            .with_action(move |app, window, _| {
                for index in 0..4 {
                    sample_frame(app, window, index, preview);
                }
            })
            .add_named_assertion_with_data_from_prior_step(
                "Every job is producing output",
                move |app, window, data| match output_ticks(app, window, pane_count) {
                    Some(ticks) => {
                        let start = Usage::read().expect("Read application resource usage");
                        data.insert(
                            BENCHMARK,
                            Benchmark {
                                ticks,
                                usage_start: Some(start),
                                ..Benchmark::default()
                            },
                        );
                        AssertionOutcome::Success
                    }
                    None => AssertionOutcome::failure("Waiting for terminal output".to_owned()),
                },
            ),
    );
    for index in 0..SAMPLES {
        builder = builder.with_step(
            TestStep::new("Sample the pointer and scene")
                .with_action(move |app, window, data| {
                    let (pointer, pointer_to_scene) = sample_frame(app, window, index, preview);
                    let result = data
                        .get_mut::<_, Benchmark>(BENCHMARK)
                        .expect("Benchmark started");
                    result.pointer.push(pointer);
                    result.pointer_to_scene.push(pointer_to_scene);
                })
                .set_post_step_pause(SAMPLE_INTERVAL),
        );
    }
    builder = builder.with_step(
        TestStep::new("Verify terminal output during pointer movement")
            .set_timeout(WORKSPACE_STEP_TIMEOUT)
            .with_action(|_, _, data| {
                let result = data
                    .get_mut::<_, Benchmark>(BENCHMARK)
                    .expect("Benchmark started");
                result.usage_end = Some(Usage::read().expect("Read application resource usage"));
            })
            .add_named_assertion_with_data_from_prior_step(
                "Every pane advanced its output while the workspace was busy",
                move |app, window, data| {
                    let Some(ticks) = output_ticks(app, window, pane_count) else {
                        return AssertionOutcome::failure("Waiting for terminal output".to_owned());
                    };
                    let result = data
                        .get_mut::<_, Benchmark>(BENCHMARK)
                        .expect("Benchmark started");
                    let deltas: Vec<_> = ticks
                        .into_iter()
                        .zip(&result.ticks)
                        .map(|(end, start)| end.saturating_sub(*start))
                        .collect();
                    if deltas.contains(&0) {
                        return AssertionOutcome::failure(
                            "Some panes have not produced a new tick".to_owned(),
                        );
                    }
                    result.output_lines = deltas.into_iter().sum();
                    AssertionOutcome::Success
                },
            ),
    );
    if preview {
        builder = builder
            .with_step(
                TestStep::new("Cancel the preview")
                    .with_keystrokes(&["escape"])
                    .add_named_assertion("The preview closed", no_preview),
            )
            .with_step(
                TestStep::new("Release after canceling")
                    .set_timeout(WORKSPACE_STEP_TIMEOUT)
                    .with_action(release_tab)
                    .add_assertion(assert_tab_count(pane_count))
                    .add_named_assertion_with_data_from_prior_step(
                        "Cancel preserved every original process",
                        move |app, window, data| {
                            assert_original_processes(app, window, data, &initial_layout)
                        },
                    ),
            );
    }

    for index in 0..ROUND_TRIPS {
        builder = builder.with_step(round_trip(index));
    }

    let mut final_layout: Vec<_> = (0..pane_count).map(|tab| (tab, 0)).collect();
    if preview {
        final_layout = std::iter::once((0, 0))
            .chain(std::iter::once((0, 1)))
            .chain((1..pane_count - 1).map(|tab| (tab, 0)))
            .collect();
        builder = builder.with_step(preview_right()).with_step(
            TestStep::new("Commit the live tab into a split")
                .set_timeout(WORKSPACE_STEP_TIMEOUT)
                .with_action(release_tab)
                .add_assertion(assert_tab_count(pane_count - 1)),
        );
    }

    builder.with_step(
        TestStep::new("Report the benchmark")
            .set_timeout(WORKSPACE_STEP_TIMEOUT)
            .add_named_assertion_with_data_from_prior_step(
                "Every shell, job, view, and session still matches",
                move |app, window, data| {
                    assert_original_processes(app, window, data, &final_layout)
                },
            )
            .add_named_assertion_with_data_from_prior_step(
                "Emit timing and resource measurements",
                move |_, _, data| {
                    let result = data
                        .get_mut::<_, Benchmark>(BENCHMARK)
                        .expect("Benchmark started");
                    let start = result
                        .usage_start
                        .take()
                        .expect("Initial usage was measured");
                    let end = result.usage_end.take().expect("Final usage was measured");
                    let summary = json!({
                        "mode": mode,
                        "panes": pane_count,
                        "samples": SAMPLES,
                        "pointer_ms": percentiles(&mut result.pointer),
                        "pointer_to_scene_ms": percentiles(&mut result.pointer_to_scene),
                        "pty_round_trip_observed_ms": percentiles(&mut result.round_trips),
                        "elapsed_ms": milliseconds(end.at.duration_since(start.at)),
                        "app_cpu_ms": milliseconds(end.cpu - start.cpu),
                        "app_rss_bytes_before": start.rss_bytes,
                        "app_rss_bytes_after": end.rss_bytes,
                        "app_peak_rss_bytes_before": start.peak_rss_bytes,
                        "app_peak_rss_bytes_after": end.peak_rss_bytes,
                        "output_ticks_during_preview_or_control": result.output_lines,
                        "preserved_live_process_pairs": pane_count,
                    });
                    println!("WARP_DOCKING_BENCHMARK {summary}");
                    AssertionOutcome::Success
                },
            ),
    )
}
