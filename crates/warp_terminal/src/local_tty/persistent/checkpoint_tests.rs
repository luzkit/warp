use serde_json::json;

use super::*;
use crate::shell::{ShellLaunchData, ShellType};

fn collector() -> Collector {
    Collector::new(&NativeShell {
        session_id: SessionId::from(7),
        shell_type: ShellType::Zsh,
        launch_data: ShellLaunchData::Executable {
            executable_path: "/bin/zsh".into(),
            shell_type: ShellType::Zsh,
        },
        pane_uuid: None,
    })
}

fn hook(name: &str, value: serde_json::Value) -> Vec<u8> {
    let data = serde_json::to_vec(&json!({"hook": name, "value": value})).unwrap();
    format!("\x1bP$d{}\x1b\\", hex::encode(data)).into_bytes()
}

fn bootstrap_value() -> serde_json::Value {
    json!({
        "session_id": 7, "shell": "zsh", "histfile": "", "home_dir": "", "path": "",
        "aliases": "", "abbreviations": "", "function_names": "", "env_var_names": "",
        "builtins": "", "keywords": "", "shell_version": ""
    })
}

fn bootstrapped() -> Collector {
    let mut collector = collector();
    collector.record(&hook("InitShell", json!({"session_id": 7, "shell": "zsh"})));
    collector.record(&hook("Bootstrapped", bootstrap_value()));
    collector.record(&hook(
        "CommandFinished",
        json!({"session_id": 7, "exit_code": 0, "next_block_id": "7-1"}),
    ));
    collector.record(&hook(
        "Precmd",
        json!({"session_id": 7, "pwd": "/tmp", "ps1": "$ "}),
    ));
    collector
}

#[test]
fn checkpoint_retains_bootstrap_after_unbounded_startup_noise() {
    let mut collector = collector();
    collector.record(&hook("InitShell", json!({"session_id": 7, "shell": "zsh"})));
    collector.record(&vec![b'x'; 400_000]);
    collector.record(&hook("Bootstrapped", bootstrap_value()));
    collector.record(&hook(
        "CommandFinished",
        json!({"session_id": 7, "exit_code": 0, "next_block_id": "7-1"}),
    ));
    collector.record(&hook(
        "Precmd",
        json!({"session_id": 7, "pwd": "/tmp", "ps1": "$ "}),
    ));
    collector.record(&vec![b'y'; 400_000]);

    let checkpoint = collector.snapshot().unwrap();
    assert_eq!(checkpoint.startup.len(), 4);
    assert!(checkpoint.startup[1].start > 400_000);
    assert!(!checkpoint.command_running);
    assert!(collector.retained_bytes() < 2048);
}

#[test]
fn checkpoint_requires_matching_shell_identity() {
    let mut collector = collector();
    collector.record(&hook("InitShell", json!({"session_id": 8, "shell": "zsh"})));
    collector.record(&hook(
        "InitShell",
        json!({"session_id": 7, "shell": "bash"}),
    ));
    collector.record(&hook("Bootstrapped", bootstrap_value()));
    collector.record(&hook(
        "CommandFinished",
        json!({"session_id": 7, "exit_code": 0, "next_block_id": "7-1"}),
    ));
    collector.record(&hook("Precmd", json!({"session_id": 7, "pwd": "/tmp"})));

    assert!(collector.snapshot().is_none());
}

#[test]
fn checkpoint_does_not_pair_inconsistent_block_ids() {
    let mut collector = bootstrapped();
    collector.record(&hook(
        "CommandFinished",
        json!({"session_id": 7, "exit_code": 0, "next_block_id": "7-2"}),
    ));
    collector.record(&hook(
        "Precmd",
        json!({"session_id": 7, "exit_code": 0, "next_block_id": "7-3", "pwd": "/tmp"}),
    ));

    assert!(collector.snapshot().unwrap().command_running);
}

#[test]
fn checkpoint_tracks_new_commands_without_overwriting_startup() {
    let mut collector = bootstrapped();
    collector.record(&hook(
        "Preexec",
        json!({"session_id": 7, "command": "sleep 20"}),
    ));
    assert!(collector.snapshot().unwrap().command_running);
    collector.record(&hook(
        "CommandFinished",
        json!({"session_id": 7, "exit_code": 0, "next_block_id": "7-2"}),
    ));
    collector.record(&hook(
        "Precmd",
        json!({"session_id": 7, "pwd": "/new/directory", "ps1": "$ "}),
    ));

    let checkpoint = collector.snapshot().unwrap();
    assert!(!checkpoint.command_running);
    assert!(checkpoint.prompt[0].start > checkpoint.startup[3].end);
    assert_eq!(checkpoint.prompt.len(), 2);
}

#[test]
fn checkpoint_treats_partial_preexec_as_running() {
    let mut collector = bootstrapped();
    collector.record(b"\x1bP$d7b22686f6f6b");

    assert!(collector.snapshot().unwrap().command_running);
}

#[test]
fn checkpoint_bounds_incomplete_payload_and_recovers_next_prompt() {
    let mut collector = bootstrapped();
    collector.record(b"\x1bP$d");
    collector.record(&vec![b'a'; MAX_CHECKPOINT_BYTES * 2]);
    assert!(collector.retained_bytes() < MAX_CHECKPOINT_BYTES);
    assert!(collector.snapshot().unwrap().command_running);
    collector.record(&hook(
        "CommandFinished",
        json!({"session_id": 7, "exit_code": 0, "next_block_id": "7-2"}),
    ));
    collector.record(&hook(
        "Precmd",
        json!({"session_id": 7, "pwd": "/tmp", "ps1": "$ "}),
    ));

    assert!(!collector.snapshot().unwrap().command_running);
}

#[test]
fn checkpoint_accepts_key_value_hooks_split_across_reads() {
    let mut collector = bootstrapped();
    collector.record(b"\x1b]9278;k;A;CommandFinished\x07\x1b]9278;k;B;session_id;7\x07");
    collector.record(
        b"\x1b]9278;k;B;exit_code;0\x07\x1b]9278;k;B;next_block_id;7-2\x07\x1b]9278;k;C\x07",
    );
    collector.record(b"\x1b]9278;k;A;Precmd\x07\x1b]9278;k;B;session_id;7\x07");
    collector.record(b"\x1b]9278;k;B;pwd;/new/directory\x07\x1b]9278;k;C\x07");

    let checkpoint = collector.snapshot().unwrap();
    assert!(!checkpoint.command_running);
    assert_eq!(checkpoint.prompt.len(), 2);
    assert!(
        checkpoint.prompt[1]
            .bytes
            .starts_with(b"\x1b]9278;k;A;Precmd")
    );
    assert_eq!(checkpoint.prompt[1].end, checkpoint.observed_offset);
}

#[test]
fn checkpoint_preserves_offsets_when_hooks_arrive_bytewise() {
    let mut collector = collector();
    let bytes = hook("InitShell", json!({"session_id": 7, "shell": "zsh"}));
    for byte in &bytes {
        collector.record(&[*byte]);
    }

    assert_eq!(collector.startup[0].start, 0);
    assert_eq!(collector.startup[0].bytes, bytes);
    assert_eq!(collector.startup[0].end, collector.offset);
}

#[test]
fn checkpoint_preserves_utf8_continuation_bytes_in_key_value_fields() {
    let mut collector = bootstrapped();
    collector.record(b"\x1b]9278;k;A;CommandFinished\x07\x1b]9278;k;B;session_id;7\x07");
    collector.record(b"\x1b]9278;k;B;exit_code;0\x07");
    collector.record("\x1b]9278;k;B;next_block_id;7-Ü-finished\x07\x1b]9278;k;C\x07".as_bytes());
    collector.record(&hook(
        "Precmd",
        json!({
            "session_id": 7, "exit_code": 0, "next_block_id": "7-Ü-finished", "pwd": "/tmp/Ü"
        }),
    ));

    assert!(!collector.snapshot().unwrap().command_running);
}
