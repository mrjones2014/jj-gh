//! Sole boundary for spawning external processes.
//!
//! Every `Command::new` in the crate lives here so call sites choose a run
//! *mode* instead of hand-wiring stdio:
//! - [`capture`] pipes and collects stdout for commands whose output we parse,
//!   normalizing a non-zero exit via [`subprocess_error`].
//! - [`stream`] inherits the parent's stdio so output streams live and keeps
//!   color/tty for display and progress commands. The child prints its own
//!   stderr, so a failure needs no captured message.
//! - [`interactive`] attaches to the controlling terminal, so interactive
//!   commands still work when the parent's stdout is piped.
//! - [`capture_sync`] is the synchronous variant for pre-runtime config
//!   assembly, before the async runtime exists.
//!
//! Token resolution needs the raw [`SpawnOutcome`] (timeout plus a fallback
//! chain) and a test fake, so it goes through the [`ProcessRunner`] trait.

use crate::util::subprocess_error;
use anyhow::{Context, Result, anyhow};
use std::ffi::OsStr;
use std::time::Duration;
use tokio::process::Command;
use tokio::time::timeout;

/// Outcome of a single process invocation through a [`ProcessRunner`].
#[derive(Debug, Clone)]
pub enum SpawnOutcome {
    /// Process ran to completion. `code` is `None` when terminated by a signal.
    Completed {
        code: Option<i32>,
        stdout: Vec<u8>,
        stderr: Vec<u8>,
    },
    /// Process could not be spawned (e.g. missing binary, permission denied).
    SpawnFailed(String),
    /// Process exceeded the configured timeout.
    TimedOut,
}

impl SpawnOutcome {
    pub(crate) fn status_display(code: Option<i32>) -> String {
        code.map_or_else(|| "signal".to_string(), |c| format!("exit code {c}"))
    }
}

/// External process boundary for capture-with-timeout. Implementations spawn
/// `argv[0]` with `argv[1..]` as arguments and return its outcome, applying any
/// timeout themselves. Faked in tests; production uses [`TokioProcessRunner`].
pub trait ProcessRunner {
    async fn run(&self, argv: &[impl AsRef<OsStr>], timeout: Duration) -> SpawnOutcome;
}

/// Production runner backed by `tokio::process` + `tokio::time::timeout`.
pub struct TokioProcessRunner;

impl ProcessRunner for TokioProcessRunner {
    async fn run(&self, argv: &[impl AsRef<OsStr>], dur: Duration) -> SpawnOutcome {
        let Some((prog, rest)) = argv.split_first() else {
            return SpawnOutcome::SpawnFailed("empty argv".into());
        };
        match timeout(dur, Command::new(prog).args(rest).output()).await {
            Ok(Ok(output)) => SpawnOutcome::Completed {
                code: output.status.code(),
                stdout: output.stdout,
                stderr: output.stderr,
            },
            Ok(Err(io)) => SpawnOutcome::SpawnFailed(io.to_string()),
            Err(_) => SpawnOutcome::TimedOut,
        }
    }
}

/// Run `argv` capturing stdout; on a non-zero exit, error with the normalized
/// stderr. For commands whose output we parse.
pub async fn capture(argv: &[&str]) -> Result<Vec<u8>> {
    let (prog, rest) = split(argv)?;
    let output = Command::new(prog)
        .args(rest)
        .output()
        .await
        .with_context(|| format!("failed to spawn `{prog}`"))?;
    if !output.status.success() {
        return Err(anyhow!("{}", subprocess_error(&output.stderr)));
    }
    Ok(output.stdout)
}

/// Run `argv` inheriting the parent's stdio so output streams live and keeps
/// color/tty. The child prints its own stderr, so a non-zero exit only needs a
/// generic message. For display, progress, and interactive commands.
pub async fn stream(argv: &[&str]) -> Result<()> {
    let (prog, rest) = split(argv)?;
    let status = Command::new(prog)
        .args(rest)
        .status()
        .await
        .with_context(|| format!("failed to spawn `{prog}`"))?;
    if !status.success() {
        return Err(anyhow!("`{prog}` exited with {status}"));
    }
    Ok(())
}

/// Run `argv` attached to the controlling terminal.
///
/// This keeps interactive programs usable when this process's stdout is piped.
/// If no controlling terminal or console is available, falls back to inheriting
/// the parent's stdio.
pub async fn interactive(argv: &[&str]) -> Result<()> {
    let (prog, rest) = split(argv)?;
    let mut command = Command::new(prog);
    command.args(rest);

    #[cfg(unix)]
    if let Ok(tty) = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")
    {
        use std::process::Stdio;

        let tty_stdout = tty
            .try_clone()
            .context("could not clone controlling terminal for stdout")?;
        let tty_stderr = tty
            .try_clone()
            .context("could not clone controlling terminal for stderr")?;
        command
            .stdin(Stdio::from(tty))
            .stdout(Stdio::from(tty_stdout))
            .stderr(Stdio::from(tty_stderr));
    }

    #[cfg(windows)]
    if let (Ok(stdin), Ok(stdout), Ok(stderr)) = (
        std::fs::OpenOptions::new().read(true).open("CONIN$"),
        std::fs::OpenOptions::new().write(true).open("CONOUT$"),
        std::fs::OpenOptions::new().write(true).open("CONOUT$"),
    ) {
        use std::process::Stdio;

        command
            .stdin(Stdio::from(stdin))
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr));
    }

    let status = command
        .status()
        .await
        .with_context(|| format!("failed to spawn `{prog}`"))?;
    if !status.success() {
        return Err(anyhow!("`{prog}` exited with {status}"));
    }
    Ok(())
}

/// Synchronous capture for pre-runtime config assembly, before the async
/// runtime exists. Returns `None` on any spawn failure or non-zero exit.
pub fn capture_sync(argv: &[&str]) -> Option<Vec<u8>> {
    let (prog, rest) = argv.split_first()?;
    let output = std::process::Command::new(prog).args(rest).output().ok()?;
    output.status.success().then_some(output.stdout)
}

/// Like [`capture_sync`] but for several commands at once: every child is
/// spawned before any is waited on, so independent cold-start subprocesses
/// overlap instead of running back-to-back. Results are positional; each entry
/// is `None` on spawn failure or non-zero exit.
pub fn capture_sync_batch(argvs: &[&[&str]]) -> Vec<Option<Vec<u8>>> {
    use std::process::{Command, Stdio};
    let children = argvs
        .iter()
        .map(|argv| {
            let (prog, rest) = argv.split_first()?;
            Command::new(prog)
                .args(rest)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .ok()
        })
        .collect::<Vec<Option<std::process::Child>>>();
    children
        .into_iter()
        .map(|child| {
            let output = child?.wait_with_output().ok()?;
            output.status.success().then_some(output.stdout)
        })
        .collect()
}

fn split<'a>(argv: &'a [&'a str]) -> Result<(&'a str, &'a [&'a str])> {
    argv.split_first()
        .map(|(prog, rest)| (*prog, rest))
        .ok_or_else(|| anyhow!("empty argv"))
}
