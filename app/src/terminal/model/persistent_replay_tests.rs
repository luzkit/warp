use serde_json::{Value, json};
use warp_terminal::local_tty::persistent::{CheckpointFrame, NativeCheckpoint};

use super::*;
use crate::terminal::model::test_utils::block_size;

fn unbootstrapped_model() -> TerminalModel {
    let mut model = TerminalModel::new(
        None,
        block_size(),
        color::List::from(&color::Colors::default()),
        ChannelEventListener::new_for_test(),
        Arc::new(Background::default()),
        false,
        false,
        false,
        false,
        false,
        ObfuscateSecrets::No,
        false,
        None,
        ShellLaunchState::ShellSpawned {
            available_shell: None,
            display_name: ShellName::blank(),
            shell_type: ShellType::Zsh,
        },
    );
    model.register_session_id(123.into());
    model
}

fn hook(name: &str, value: Value, start: u64) -> CheckpointFrame {
    let encoded = hex::encode(serde_json::to_vec(&json!({"hook": name, "value": value})).unwrap());
    let bytes = format!("\x1bP$d{encoded}\x1b\\").into_bytes();
    CheckpointFrame {
        start,
        end: start + bytes.len() as u64,
        bytes,
    }
}

fn prompt(start: u64, block: &str, pwd: &str) -> Vec<CheckpointFrame> {
    let finished = hook(
        "CommandFinished",
        json!({"session_id":123,"exit_code":0,"next_block_id":block}),
        start,
    );
    let precmd = hook(
        "Precmd",
        json!({"session_id":123,"exit_code":0,"next_block_id":block,"pwd":pwd}),
        finished.end,
    );
    vec![finished, precmd]
}

fn checkpoint(command_running: bool) -> NativeCheckpoint {
    let init = hook("InitShell", json!({"session_id":123,"shell":"zsh"}), 0);
    let bootstrapped = hook(
        "Bootstrapped",
        json!({
            "session_id":123,"shell":"zsh","histfile":"","home_dir":"/tmp","path":"/bin",
            "aliases":"","abbreviations":"","function_names":"","env_var_names":"",
            "builtins":"","keywords":"","shell_version":"5.9"
        }),
        422_302,
    );
    let mut startup = vec![init, bootstrapped];
    startup.extend(prompt(425_000, "first", "/first"));
    NativeCheckpoint {
        startup,
        prompt: prompt(800_000, "latest", "/latest"),
        command_running,
        observed_offset: 900_000,
    }
}

#[test]
fn legacy_gap_recovers_bootstrap_hooks_retained_only_in_tail() {
    let mut model = unbootstrapped_model();
    let mut parser = ansi::Processor::new();
    let checkpoint = checkpoint(false);
    let prefix = checkpoint.startup[0].bytes.clone();
    let tail = checkpoint.startup[1..]
        .iter()
        .flat_map(|frame| frame.bytes.clone())
        .collect::<Vec<_>>();

    model.replay_persistent_pty(&mut parser, &tail, prefix, true, None, None);

    assert!(!model.is_persistent_pty());
    assert!(model.block_list.is_bootstrapping_precmd_done());
    assert!(model.block_list.active_block().has_received_precmd());
    assert!(!model.block_list.active_block().started());
}

#[test]
fn checkpoint_restores_native_prompt_after_bootstrap_and_prompt_are_evicted() {
    let mut model = unbootstrapped_model();
    let mut parser = ansi::Processor::new();
    let checkpoint = checkpoint(false);

    model.replay_persistent_pty(
        &mut parser,
        b"partial line\n",
        vec![],
        true,
        Some(&checkpoint),
        Some(899_987),
    );

    assert!(!model.is_persistent_pty());
    assert!(model.block_list.is_bootstrapping_precmd_done());
    assert!(model.block_list.active_block().has_received_precmd());
    assert!(!model.block_list.active_block().started());
    assert_eq!(model.block_list.active_block().id().to_string(), "latest");
    assert_eq!(
        model.block_list.active_block().session_id(),
        Some(123.into())
    );
}

#[test]
fn checkpoint_keeps_running_command_active_without_preexec_in_tail() {
    let mut model = unbootstrapped_model();
    let mut parser = ansi::Processor::new();
    let checkpoint = checkpoint(true);

    model.replay_persistent_pty(
        &mut parser,
        b"partial\njob output\n",
        vec![],
        true,
        Some(&checkpoint),
        Some(899_981),
    );

    assert!(!model.is_persistent_pty());
    assert!(model.block_list.active_block().started());
    let completion = prompt(900_000, "after-interrupt", "/latest");
    for frame in completion {
        parser.parse_bytes(&mut model, &frame.bytes, &mut Vec::new());
    }
    assert!(!model.block_list.active_block().started());
    assert!(model.block_list.active_block().has_received_precmd());
    assert_eq!(
        model.block_list.active_block().id().to_string(),
        "after-interrupt"
    );
}

#[test]
fn checkpoint_deduplicates_prompt_crossing_tail_boundary() {
    let mut model = unbootstrapped_model();
    let mut parser = ansi::Processor::new();
    let mut checkpoint = checkpoint(false);
    checkpoint.observed_offset = checkpoint.prompt.last().unwrap().end;
    let last_prompt = checkpoint.prompt.last().unwrap();
    let tail = &last_prompt.bytes[15..];

    model.replay_persistent_pty(
        &mut parser,
        tail,
        vec![],
        true,
        Some(&checkpoint),
        Some(last_prompt.start + 15),
    );

    assert!(!model.is_persistent_pty());
    assert!(!model.block_list.active_block().started());
    assert_eq!(model.block_list.active_block().id().to_string(), "latest");
    assert_eq!(
        model
            .block_list
            .blocks()
            .iter()
            .filter(|block| block.id().to_string() == "latest")
            .count(),
        1
    );
}

#[test]
fn checkpoint_leaves_complete_tail_hooks_for_native_replay() {
    let mut model = unbootstrapped_model();
    let mut parser = ansi::Processor::new();
    let mut checkpoint = checkpoint(false);
    checkpoint.observed_offset = checkpoint.prompt.last().unwrap().end;
    let tail = checkpoint
        .prompt
        .iter()
        .flat_map(|frame| frame.bytes.clone())
        .collect::<Vec<_>>();

    model.replay_persistent_pty(
        &mut parser,
        &tail,
        vec![],
        true,
        Some(&checkpoint),
        Some(800_000),
    );

    assert!(model.block_list.is_bootstrapping_precmd_done());
    assert!(!model.block_list.active_block().started());
    assert_eq!(model.block_list.active_block().id().to_string(), "latest");
}

#[test]
fn checkpoint_completes_partial_command_finished_before_tail_precmd() {
    let mut model = unbootstrapped_model();
    let mut parser = ansi::Processor::new();
    let mut checkpoint = checkpoint(false);
    checkpoint.observed_offset = checkpoint.prompt.last().unwrap().end;
    let tail = checkpoint
        .prompt
        .iter()
        .flat_map(|frame| frame.bytes.clone())
        .skip(15)
        .collect::<Vec<_>>();

    model.replay_persistent_pty(
        &mut parser,
        &tail,
        vec![],
        true,
        Some(&checkpoint),
        Some(800_015),
    );

    assert!(model.block_list.is_bootstrapping_precmd_done());
    assert!(!model.block_list.active_block().started());
    assert_eq!(model.block_list.active_block().id().to_string(), "latest");
}

#[test]
fn checkpoint_preserves_running_command_received_in_tail() {
    let mut model = unbootstrapped_model();
    let mut parser = ansi::Processor::new();
    let mut checkpoint = checkpoint(true);
    let preexec = hook(
        "Preexec",
        json!({"session_id":123,"command":"sleep 100"}),
        850_000,
    );
    checkpoint.observed_offset = preexec.end;

    model.replay_persistent_pty(
        &mut parser,
        &preexec.bytes,
        vec![],
        true,
        Some(&checkpoint),
        Some(preexec.start),
    );

    assert!(model.block_list.active_block().started());
    assert_eq!(
        model.block_list.active_block().state(),
        BlockState::Executing
    );
    assert_eq!(model.block_list.active_block().id().to_string(), "latest");
}
