use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use warp_core::SessionId;

use super::{MAX_CHECKPOINT_BYTES, NativeShell};
use crate::model::ansi::{DProtoHook, PendingHook, PrecmdHookValue};
use crate::model::block::BlockId;

/// A complete shell hook with its location in the original PTY output.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CheckpointFrame {
    pub start: u64,
    pub end: u64,
    pub bytes: Vec<u8>,
}

/// Native shell state retained independently of the output history limit.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct NativeCheckpoint {
    pub startup: Vec<CheckpointFrame>,
    pub prompt: Vec<CheckpointFrame>,
    pub command_running: bool,
    pub observed_offset: u64,
}

impl NativeCheckpoint {
    pub(super) fn validate(&self, replay_offset: Option<u64>, replay_len: usize) -> Result<()> {
        ensure!(
            replay_offset.and_then(|offset| offset.checked_add(replay_len as u64))
                == Some(self.observed_offset),
            "Persistent terminal checkpoint does not match replay"
        );
        ensure!(
            (3..=4).contains(&self.startup.len()) && self.prompt.len() <= 2,
            "Invalid persistent terminal checkpoint"
        );
        for frames in [&self.startup, &self.prompt] {
            let mut previous_end = 0;
            for frame in frames {
                ensure!(
                    frame.start >= previous_end
                        && frame.end <= self.observed_offset
                        && frame.start.checked_add(frame.bytes.len() as u64) == Some(frame.end)
                        && !frame.bytes.is_empty(),
                    "Invalid persistent terminal checkpoint frame"
                );
                previous_end = frame.end;
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy)]
enum CaptureState {
    Ground,
    Escape,
    Dcs,
    Osc,
    StringEscape,
}

struct PendingCapture {
    frame: CheckpointFrame,
    hook: PendingHook,
}

pub(super) struct Collector {
    session_id: SessionId,
    shell: &'static str,
    startup: Vec<CheckpointFrame>,
    prompt: Vec<CheckpointFrame>,
    completion: Option<(BlockId, CheckpointFrame)>,
    startup_complete: bool,
    command_running: bool,
    offset: u64,
    state: CaptureState,
    frame: CheckpointFrame,
    pending: Option<PendingCapture>,
}

impl Collector {
    pub(super) fn new(shell: &NativeShell) -> Self {
        Self {
            session_id: shell.session_id,
            shell: shell.shell_type.name(),
            startup: Vec::new(),
            prompt: Vec::new(),
            completion: None,
            startup_complete: false,
            command_running: true,
            offset: 0,
            state: CaptureState::Ground,
            frame: CheckpointFrame {
                start: 0,
                end: 0,
                bytes: Vec::new(),
            },
            pending: None,
        }
    }

    fn retained_bytes(&self) -> usize {
        self.startup
            .iter()
            .chain(&self.prompt)
            .map(|frame| frame.bytes.len())
            .sum::<usize>()
            + self
                .completion
                .as_ref()
                .map_or(0, |(_, frame)| frame.bytes.len())
            + self
                .pending
                .as_ref()
                .map_or(0, |pending| pending.frame.bytes.len())
            + self.frame.bytes.len()
    }

    fn abandon_capture(&mut self) {
        self.frame.bytes.clear();
        self.pending = None;
        self.completion = None;
        self.state = CaptureState::Ground;
        self.command_running = true;
    }

    pub(super) fn record(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            let start = self.offset;
            self.offset = self.offset.saturating_add(1);
            // Include the frame under construction in the same budget as retained hooks.
            if self.retained_bytes() + 2 > MAX_CHECKPOINT_BYTES - 1024 {
                self.abandon_capture();
            }
            if let Some(pending) = &mut self.pending {
                pending.frame.bytes.push(byte);
                pending.frame.end = self.offset;
            }
            match self.state {
                CaptureState::Ground => {
                    if byte == 0x1b {
                        self.frame = CheckpointFrame {
                            start,
                            end: self.offset,
                            bytes: vec![byte],
                        };
                        self.state = CaptureState::Escape;
                    }
                }
                CaptureState::Escape => match byte {
                    b'P' | b']' => {
                        self.frame.bytes.push(byte);
                        self.state = if byte == b'P' {
                            CaptureState::Dcs
                        } else {
                            CaptureState::Osc
                        };
                    }
                    0x1b => self.frame.start = start,
                    _ => {
                        self.frame.bytes.clear();
                        self.state = CaptureState::Ground;
                    }
                },
                CaptureState::Dcs | CaptureState::Osc => {
                    self.frame.bytes.push(byte);
                    if (byte == 0x9c && matches!(self.state, CaptureState::Dcs))
                        || (byte == 0x07 && matches!(self.state, CaptureState::Osc))
                    {
                        self.finish_frame();
                    } else if byte == 0x1b {
                        self.state = CaptureState::StringEscape;
                    } else if matches!(byte, 0x18 | 0x1a) {
                        self.abandon_capture();
                    }
                }
                CaptureState::StringEscape => {
                    if byte == b'\\' {
                        self.frame.bytes.push(byte);
                        self.finish_frame();
                    } else {
                        self.command_running = true;
                        self.pending = None;
                        self.frame = CheckpointFrame {
                            start: start - 1,
                            end: self.offset,
                            bytes: vec![0x1b, byte],
                        };
                        self.state = match byte {
                            b'P' => CaptureState::Dcs,
                            b']' => CaptureState::Osc,
                            _ => CaptureState::Ground,
                        };
                    }
                }
            }
        }
    }

    fn finish_frame(&mut self) {
        self.state = CaptureState::Ground;
        self.frame.end = self.offset;
        let frame = std::mem::replace(
            &mut self.frame,
            CheckpointFrame {
                start: 0,
                end: 0,
                bytes: Vec::new(),
            },
        );
        let body = frame
            .bytes
            .strip_suffix(b"\x1b\\")
            .or_else(|| frame.bytes.strip_suffix(b"\x07"))
            .or_else(|| frame.bytes.strip_suffix(b"\x9c"))
            .unwrap_or(&frame.bytes);
        if let Some(payload) = body.strip_prefix(b"\x1b]9278;k;") {
            self.receive_key_value(payload, &frame);
            return;
        }
        let encoded = body
            .strip_prefix(b"\x1bP$d")
            .or_else(|| body.strip_prefix(b"\x1b]9278;d;"));
        let plain = body
            .strip_prefix(b"\x1bP$f")
            .or_else(|| body.strip_prefix(b"\x1b]9278;f;"));
        let hook = if let Some(payload) = encoded {
            hex::decode(payload)
                .ok()
                .and_then(|decoded| serde_json::from_slice(&decoded).ok())
        } else if let Some(payload) = plain {
            serde_json::from_slice(payload)
                .ok()
                .filter(|hook| matches!(hook, DProtoHook::InitShell { .. }))
        } else {
            return;
        };
        self.pending = None;
        match hook {
            Some(hook) => self.receive_hook(hook, frame),
            None => {
                self.command_running = true;
                self.completion = None;
            }
        }
    }

    fn receive_key_value(&mut self, payload: &[u8], frame: &CheckpointFrame) {
        let mut fields = payload.splitn(3, |byte| *byte == b';');
        match fields.next() {
            Some(b"A") => {
                self.pending = fields
                    .next()
                    .and_then(|name| std::str::from_utf8(name).ok())
                    .and_then(PendingHook::create)
                    .map(|hook| PendingCapture {
                        frame: frame.clone(),
                        hook,
                    });
            }
            Some(b"B") => {
                if let (Some(pending), Some(key), Some(value)) =
                    (&mut self.pending, fields.next(), fields.next())
                {
                    pending.hook.update(
                        String::from_utf8_lossy(key).into_owned(),
                        String::from_utf8_lossy(value).into_owned(),
                    );
                }
            }
            Some(b"C") => {
                if let Some(pending) = self.pending.take() {
                    match pending.hook.finish() {
                        Ok(hook) => self.receive_hook(hook, pending.frame),
                        Err(_) => self.command_running = true,
                    }
                }
            }
            _ => self.abandon_capture(),
        }
    }

    fn receive_hook(&mut self, hook: DProtoHook, frame: CheckpointFrame) {
        if hook.session_id() != Some(self.session_id) {
            self.command_running = true;
            self.completion = None;
            return;
        }
        match hook {
            DProtoHook::InitShell { value } => {
                if self.startup.is_empty() && value.shell == self.shell && !value.is_subshell {
                    self.startup.push(frame);
                }
            }
            DProtoHook::Bootstrapped { value } => {
                if self.startup.len() == 1 && value.shell == self.shell {
                    self.startup.push(frame);
                }
            }
            DProtoHook::CommandFinished { value } => {
                self.command_running = true;
                if self.startup.len() >= 2 {
                    self.completion = Some((value.completion_metadata.next_block_id, frame));
                }
            }
            DProtoHook::Precmd { value } => {
                let combined_id = match value {
                    PrecmdHookValue::WithCompletionMetadata(value) => {
                        Some(value.completion_metadata.next_block_id)
                    }
                    PrecmdHookValue::PromptOnly(_) => None,
                };
                let mut prompt = match self.completion.take() {
                    Some((block_id, completion))
                        if combined_id.as_ref().is_none_or(|id| id == &block_id) =>
                    {
                        vec![completion]
                    }
                    None if combined_id.is_some() => Vec::new(),
                    _ => {
                        self.command_running = true;
                        return;
                    }
                };
                prompt.push(frame);
                if self.startup.len() >= 2 {
                    self.prompt = prompt;
                    if !self.startup_complete {
                        let prompt_bytes = self
                            .prompt
                            .iter()
                            .map(|frame| frame.bytes.len())
                            .sum::<usize>();
                        if self.retained_bytes() + prompt_bytes > MAX_CHECKPOINT_BYTES - 1024 {
                            self.startup.append(&mut self.prompt);
                            self.startup_complete = true;
                            self.command_running = true;
                            return;
                        }
                        self.startup.extend(self.prompt.clone());
                        self.startup_complete = true;
                    }
                    if self.retained_bytes() > MAX_CHECKPOINT_BYTES - 1024 {
                        self.prompt.clear();
                        self.command_running = true;
                    } else {
                        self.command_running = false;
                    }
                }
            }
            DProtoHook::Preexec { .. }
            | DProtoHook::InitSubshell { .. }
            | DProtoHook::SSH { .. }
            | DProtoHook::PreInteractiveSSHSession { .. }
            | DProtoHook::ExitShell { .. } => {
                self.command_running = true;
                self.completion = None;
            }
            DProtoHook::InputBuffer { .. }
            | DProtoHook::ExternalShellWidgetSelection { .. }
            | DProtoHook::Clear { .. }
            | DProtoHook::SourcedRcFileForWarp { .. }
            | DProtoHook::FinishUpdate { .. } => (),
        }
    }

    pub(super) fn snapshot(&self) -> Option<NativeCheckpoint> {
        self.startup_complete.then(|| NativeCheckpoint {
            startup: self.startup.clone(),
            prompt: self.prompt.clone(),
            command_running: self.command_running
                || self.pending.is_some()
                || !matches!(self.state, CaptureState::Ground),
            observed_offset: self.offset,
        })
    }
}

#[cfg(test)]
#[path = "checkpoint_tests.rs"]
mod tests;
