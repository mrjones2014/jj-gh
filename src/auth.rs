//! GitHub token resolution.
//!
//! Source precedence:
//! 1. `gh_askpass` helper, spawned with a configurable timeout.
//! 2. `JJ_GH_TOKEN` environment variable.
//! 3. `GH_TOKEN` environment variable (matches the `gh` CLI convention).
//! 4. `gh_token` field in the merged config (plain text, less safe).
//!
//! If no source yields a token, [`resolve_token`] returns an error.
//!
//! Process spawning is abstracted via [`ProcessRunner`] and environment lookup
//! via [`EnvReader`] so tests can supply canned outcomes without touching the
//! real process environment or filesystem.

use crate::config::Config;
use anyhow::{Context, Result, anyhow};
use secrecy::SecretString;
use std::{ffi::OsStr, time::Duration};
use tokio::{process::Command, time::timeout};

const ASKPASS_STDOUT_LIMIT: usize = 4 * 1024;

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
    fn status_display(code: Option<i32>) -> String {
        code.map_or_else(|| "signal".to_string(), |c| format!("exit code {c}"))
    }
}

/// External process boundary. Implementations spawn `argv[0]` with `argv[1..]`
/// as arguments and return its outcome, applying any timeout themselves.
pub trait ProcessRunner {
    async fn run(&self, argv: &[impl AsRef<OsStr>], timeout: Duration) -> SpawnOutcome;
}

/// Environment lookup boundary. Implementations return the value of `key` or
/// `None` if unset.
pub trait EnvReader {
    fn get(&self, key: &str) -> Option<String>;
}

/// Production reader backed by `std::env::var`.
pub struct OsEnv;

impl EnvReader for OsEnv {
    fn get(&self, key: &str) -> Option<String> {
        std::env::var(key).ok()
    }
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

/// Resolve a [`SecretString`] using the production [`TokioProcessRunner`]. CLI
/// `--gh-askpass` / `--askpass-timeout` flags are folded into `config` by the
/// figment overlay in `pr::dispatch`.
///
/// # Errors
///
/// See [`resolve_token_with`].
pub async fn resolve_token(config: &Config) -> Result<SecretString> {
    resolve_token_with(config, &TokioProcessRunner, &OsEnv).await
}

/// Resolve a [`SecretString`] using an explicit runner and env reader. Used in tests.
///
/// # Errors
///
/// Returns an error if the askpass helper fails (timeout, non-zero exit, empty
/// or oversize output) or if no source is configured.
async fn resolve_token_with<R: ProcessRunner, E: EnvReader>(
    config: &Config,
    runner: &R,
    env: &E,
) -> Result<SecretString> {
    if let Some(argv) = config.gh_askpass.as_deref() {
        if argv.is_empty() {
            return Err(anyhow!("`gh_askpass` is set but empty"));
        }
        let dur = Duration::from_secs(config.askpass_timeout_secs);
        return run_token_command(runner, argv, dur)
            .await
            .with_context(|| format!("askpass `{}` failed", shell_words::join(argv)));
    }

    if let Some(token) = env.get("JJ_GH_TOKEN") {
        return Ok(SecretString::new(token.into()));
    }

    if let Some(token) = env.get("GH_TOKEN") {
        return Ok(SecretString::new(token.into()));
    }

    if let Some(token) = &config.gh_token {
        log::info!("using plain token from config; configure `gh_askpass` for a safer setup");
        return Ok(token.clone());
    }

    if let Ok(token) = run_token_command(
        runner,
        &["gh", "auth", "token"],
        Duration::from_secs(config.askpass_timeout_secs),
    )
    .await
    {
        return Ok(token);
    }

    Err(anyhow!(
        "no GitHub token available: use `--gh_askpass`, configure `gh_token`, set `JJ_GH_TOKEN` or \
        `GH_TOKEN` environment variable, or run `gh auth login`"
    ))
}

async fn run_token_command<R: ProcessRunner>(
    runner: &R,
    argv: &[impl AsRef<OsStr>],
    dur: Duration,
) -> Result<SecretString> {
    match runner.run(argv, dur).await {
        SpawnOutcome::TimedOut => Err(anyhow!("command timed out after {}s", dur.as_secs())),
        SpawnOutcome::SpawnFailed(msg) => Err(anyhow!("failed to spawn command: {msg}")),
        SpawnOutcome::Completed {
            code,
            stdout,
            stderr,
        } => parse_completed(code, &stdout, &stderr),
    }
}

fn parse_completed(code: Option<i32>, stdout: &[u8], stderr: &[u8]) -> Result<SecretString> {
    if code != Some(0) {
        let stderr = String::from_utf8_lossy(stderr);
        let stderr = stderr.trim();
        let status = SpawnOutcome::status_display(code);
        return Err(if stderr.is_empty() {
            anyhow!("command exited with {status}")
        } else {
            anyhow!("command exited with {status}: {stderr}")
        });
    }

    if stdout.len() > ASKPASS_STDOUT_LIMIT {
        return Err(anyhow!(
            "stdout exceeds {ASKPASS_STDOUT_LIMIT} bytes; refusing to treat as token"
        ));
    }

    let raw = std::str::from_utf8(stdout).map_err(|_| anyhow!("stdout is not UTF-8"))?;
    let trimmed = raw.trim_end_matches(['\r', '\n']);
    if trimmed.is_empty() {
        return Err(anyhow!("produced no token on stdout"));
    }

    Ok(SecretString::from(trimmed.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use secrecy::ExposeSecret;
    use std::collections::HashMap;

    struct FakeRunner {
        outcome: SpawnOutcome,
    }

    impl FakeRunner {
        fn new(outcome: SpawnOutcome) -> Self {
            Self { outcome }
        }
    }

    impl ProcessRunner for FakeRunner {
        async fn run(&self, _: &[impl AsRef<OsStr>], _: Duration) -> SpawnOutcome {
            self.outcome.clone()
        }
    }

    #[derive(Default)]
    struct FakeEnv(HashMap<String, String>);

    impl FakeEnv {
        fn with(pairs: &[(&str, &str)]) -> Self {
            Self(
                pairs
                    .iter()
                    .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                    .collect(),
            )
        }
    }

    impl EnvReader for FakeEnv {
        fn get(&self, key: &str) -> Option<String> {
            self.0.get(key).cloned()
        }
    }

    fn empty_env() -> FakeEnv {
        FakeEnv::default()
    }

    fn config_with_askpass() -> Config {
        Config {
            gh_askpass: Some(vec!["/fake/askpass".into()]),
            askpass_timeout_secs: 5,
            ..Config::default()
        }
    }

    #[tokio::test]
    async fn resolves_via_askpass_happy_path() {
        let runner = FakeRunner::new(SpawnOutcome::Completed {
            code: Some(0),
            stdout: b"ghp_from_askpass\n".to_vec(),
            stderr: vec![],
        });
        let token = resolve_token_with(&config_with_askpass(), &runner, &empty_env())
            .await
            .unwrap();
        assert_eq!(token.expose_secret(), "ghp_from_askpass");
    }

    #[tokio::test]
    async fn errors_on_non_zero_exit() {
        let runner = FakeRunner::new(SpawnOutcome::Completed {
            code: Some(1),
            stdout: vec![],
            stderr: b"something went wrong".to_vec(),
        });
        let err = resolve_token_with(&config_with_askpass(), &runner, &empty_env())
            .await
            .unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("askpass"), "msg: {msg}");
        assert!(msg.contains("something went wrong"), "msg: {msg}");
    }

    #[tokio::test]
    async fn errors_on_empty_stdout() {
        let runner = FakeRunner::new(SpawnOutcome::Completed {
            code: Some(0),
            stdout: vec![],
            stderr: vec![],
        });
        let err = resolve_token_with(&config_with_askpass(), &runner, &empty_env())
            .await
            .unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("no token on stdout"), "msg: {msg}");
    }

    #[tokio::test]
    async fn errors_on_oversize_stdout() {
        let runner = FakeRunner::new(SpawnOutcome::Completed {
            code: Some(0),
            stdout: vec![b'a'; ASKPASS_STDOUT_LIMIT + 1],
            stderr: vec![],
        });
        let err = resolve_token_with(&config_with_askpass(), &runner, &empty_env())
            .await
            .unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("exceeds"), "msg: {msg}");
    }

    #[tokio::test]
    async fn errors_on_timeout() {
        let runner = FakeRunner::new(SpawnOutcome::TimedOut);
        let err = resolve_token_with(&config_with_askpass(), &runner, &empty_env())
            .await
            .unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("timed out"), "msg: {msg}");
    }

    #[tokio::test]
    async fn errors_on_spawn_failure() {
        let runner = FakeRunner::new(SpawnOutcome::SpawnFailed(
            "no such file or directory".into(),
        ));
        let err = resolve_token_with(&config_with_askpass(), &runner, &empty_env())
            .await
            .unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("failed to spawn command"), "msg: {msg}");
    }

    #[tokio::test]
    async fn falls_back_to_plain_token() {
        let runner = FakeRunner::new(SpawnOutcome::TimedOut); // unused, no askpass set
        let config = Config {
            gh_token: Some(SecretString::from("ghp_plain".to_string())),
            ..Config::default()
        };
        let token = resolve_token_with(&config, &runner, &empty_env())
            .await
            .unwrap();
        assert_eq!(token.expose_secret(), "ghp_plain");
    }

    #[tokio::test]
    async fn errors_when_no_source_configured() {
        let runner = FakeRunner::new(SpawnOutcome::TimedOut);
        let err = resolve_token_with(&Config::default(), &runner, &empty_env())
            .await
            .unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("no GitHub token"), "msg: {msg}");
    }

    #[tokio::test]
    async fn resolves_via_gh_auth_token_fallback() {
        let runner = FakeRunner::new(SpawnOutcome::Completed {
            code: Some(0),
            stdout: b"ghp_from_gh_cli\n".to_vec(),
            stderr: vec![],
        });
        let token = resolve_token_with(&Config::default(), &runner, &empty_env())
            .await
            .unwrap();
        assert_eq!(token.expose_secret(), "ghp_from_gh_cli");
    }

    #[tokio::test]
    async fn errors_when_askpass_argv_is_empty() {
        let runner = FakeRunner::new(SpawnOutcome::TimedOut); // unused
        let config = Config {
            gh_askpass: Some(vec![]),
            ..Config::default()
        };
        let err = resolve_token_with(&config, &runner, &empty_env())
            .await
            .unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("`gh_askpass` is set but empty"), "msg: {msg}");
    }

    #[tokio::test]
    async fn resolves_via_multi_arg_askpass() {
        let runner = FakeRunner::new(SpawnOutcome::Completed {
            code: Some(0),
            stdout: b"ghp_from_op\n".to_vec(),
            stderr: vec![],
        });
        let config = Config {
            gh_askpass: Some(vec![
                "op".into(),
                "read".into(),
                "op://Vault/github/token".into(),
            ]),
            askpass_timeout_secs: 5,
            ..Config::default()
        };
        let token = resolve_token_with(&config, &runner, &empty_env())
            .await
            .unwrap();
        assert_eq!(token.expose_secret(), "ghp_from_op");
    }

    #[tokio::test]
    async fn resolves_via_jj_gh_token_env() {
        let runner = FakeRunner::new(SpawnOutcome::TimedOut); // unused
        let env = FakeEnv::with(&[("JJ_GH_TOKEN", "ghp_from_jj_env")]);
        let token = resolve_token_with(&Config::default(), &runner, &env)
            .await
            .unwrap();
        assert_eq!(token.expose_secret(), "ghp_from_jj_env");
    }

    #[tokio::test]
    async fn resolves_via_gh_token_env() {
        let runner = FakeRunner::new(SpawnOutcome::TimedOut); // unused
        let env = FakeEnv::with(&[("GH_TOKEN", "ghp_from_gh_env")]);
        let token = resolve_token_with(&Config::default(), &runner, &env)
            .await
            .unwrap();
        assert_eq!(token.expose_secret(), "ghp_from_gh_env");
    }

    #[tokio::test]
    async fn jj_gh_token_beats_gh_token() {
        let runner = FakeRunner::new(SpawnOutcome::TimedOut); // unused
        let env = FakeEnv::with(&[
            ("JJ_GH_TOKEN", "ghp_from_jj_env"),
            ("GH_TOKEN", "ghp_from_gh_env"),
        ]);
        let token = resolve_token_with(&Config::default(), &runner, &env)
            .await
            .unwrap();
        assert_eq!(token.expose_secret(), "ghp_from_jj_env");
    }

    #[tokio::test]
    async fn env_beats_plain_config_token() {
        let runner = FakeRunner::new(SpawnOutcome::TimedOut); // unused
        let config = Config {
            gh_token: Some(SecretString::from("ghp_plain".to_string())),
            ..Config::default()
        };
        let env = FakeEnv::with(&[("GH_TOKEN", "ghp_from_env")]);
        let token = resolve_token_with(&config, &runner, &env).await.unwrap();
        assert_eq!(token.expose_secret(), "ghp_from_env");
    }

    #[tokio::test]
    async fn askpass_beats_env() {
        let runner = FakeRunner::new(SpawnOutcome::Completed {
            code: Some(0),
            stdout: b"ghp_from_askpass\n".to_vec(),
            stderr: vec![],
        });
        let env = FakeEnv::with(&[
            ("JJ_GH_TOKEN", "ghp_from_jj_env"),
            ("GH_TOKEN", "ghp_from_gh_env"),
        ]);
        let token = resolve_token_with(&config_with_askpass(), &runner, &env)
            .await
            .unwrap();
        assert_eq!(token.expose_secret(), "ghp_from_askpass");
    }
}
