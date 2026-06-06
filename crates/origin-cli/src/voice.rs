// SPDX-License-Identifier: Apache-2.0
//! `origin dictate` — speech-to-text dictation (aider `/voice`, jcode
//! `dictate`).
//!
//! Spawns an external STT engine (from `$ORIGIN_STT_CMD`, default `whisper`),
//! reads its stdout line-by-line, and applies the queue/interleave policy from
//! the pure [`origin_voice`] crate to assemble a submittable prompt, which is
//! printed when the engine exits.

use std::io::{BufRead as _, BufReader};
use std::process::{Command, Stdio};

use anyhow::Result;
use origin_voice::{build_command, validate, DictationConfig, DictationMode, DictationSession, Transcript};

/// Run `origin dictate`: drive an STT engine and print the assembled prompt.
///
/// `interleave` selects [`DictationMode::Interleave`] (each line is emitted
/// eagerly) instead of the default [`DictationMode::Queue`]. `language` and
/// `device` are injected as `--language`/`--device` flags.
///
/// # Errors
/// Returns when the configuration is invalid or the STT process fails to run.
pub fn run(interleave: bool, language: Option<String>, device: Option<String>) -> Result<()> {
    let command = std::env::var("ORIGIN_STT_CMD").unwrap_or_else(|_| "whisper".to_owned());
    let cfg = DictationConfig {
        command,
        args: Vec::new(),
        language,
        device,
    };
    if let Err(e) = validate(&cfg) {
        anyhow::bail!("invalid dictation config: {e}");
    }
    let (prog, args) = build_command(&cfg);

    let mode = if interleave {
        DictationMode::Interleave
    } else {
        DictationMode::Queue
    };
    let mut session = DictationSession::new(mode);
    let mut chunks: Vec<String> = Vec::new();

    let child = Command::new(&prog).args(&args).stdout(Stdio::piped()).spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(e) => {
            anyhow::bail!(
                "could not start speech-to-text engine `{prog}` ({e}); set $ORIGIN_STT_CMD to a working STT command"
            );
        }
    };

    let Some(stdout) = child.stdout.take() else {
        anyhow::bail!("STT engine stdout unavailable");
    };
    let reader = BufReader::new(stdout);
    for line in reader.lines() {
        let line = line.map_err(|e| anyhow::anyhow!("reading STT output: {e}"))?;
        if line.trim().is_empty() {
            continue;
        }
        // The default engine emits one final transcript per line.
        session.push(Transcript::new(line, true));
        if let Some(chunk) = session.take_ready() {
            chunks.push(chunk);
        }
    }
    let _ = child.wait();
    // Flush any tail buffered in queue mode.
    if let Some(chunk) = session.take_ready() {
        chunks.push(chunk);
    }

    if chunks.is_empty() {
        println!("(no speech transcribed)");
    } else {
        println!("{}", chunks.join(" "));
    }
    Ok(())
}
