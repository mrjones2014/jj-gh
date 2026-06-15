//! GitHub token resolution.
//!
//! Source precedence (high to low), so flags and env vars override config:
//! 1. `--gh-askpass` flag, spawned with a configurable timeout.
//! 2. `$GH_ASKPASS` env var, shell-split and spawned.
//! 3. `$JJ_GH_TOKEN` env var.
//! 4. `$GH_TOKEN` env var (matches the `gh` CLI convention).
//! 5. `gh_askpass` from config files, spawned.
//! 6. `gh_token` from config files (plain text, less safe).
//! 7. `gh auth token`.
//!
//! If no source yields a token, [`resolve_token`] returns an error.
//!
//! Process spawning is abstracted via [`ProcessRunner`] and environment lookup
//! via [`EnvReader`] so tests can supply canned outcomes without touching the
//! real process environment or filesystem.

use crate::config::Config;
use crate::proc::{ProcessRunner, SpawnOutcome, TokioProcessRunner};
use anyhow::{Context, Result, anyhow};
use secrecy::SecretString;
use std::ffi::OsStr;
use std::time::Duration;

const ASKPASS_STDOUT_LIMIT: usize = 4 * 1024;

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

/// Resolve a [`SecretString`] using the production [`TokioProcessRunner`].
///
/// `flag_askpass` is the raw `--gh-askpass` CLI value (highest priority); the
/// remaining sources come from `config` and the environment.
///
/// # Errors
///
/// See [`resolve_token_with`].
pub async fn resolve_token(
    flag_askpass: Option<&[String]>,
    config: &Config,
) -> Result<SecretString> {
    resolve_token_with(flag_askpass, config, &TokioProcessRunner, &OsEnv).await
}

/// Resolve a [`SecretString`] using an explicit runner and env reader. Used in tests.
///
/// # Errors
///
/// Returns an error if a selected askpass helper fails (timeout, non-zero exit,
/// empty or oversize output) or if no source is configured. Empty askpass argvs
/// (flag, env, or config) are skipped rather than treated as an error.
async fn resolve_token_with<R: ProcessRunner, E: EnvReader>(
    flag_askpass: Option<&[String]>,
    config: &Config,
    runner: &R,
    env: &E,
) -> Result<SecretString> {
    let dur = Duration::from_secs(config.askpass_timeout_secs);

    // 1. `--gh-askpass` flag.
    if let Some(argv) = flag_askpass.filter(|a| !a.is_empty()) {
        return run_askpass(runner, argv, dur).await;
    }

    // 2. `$GH_ASKPASS` env var, shell-split.
    if let Some(raw) = env.get("GH_ASKPASS").filter(|s| !s.trim().is_empty()) {
        let argv = shell_words::split(&raw).context("could not split $GH_ASKPASS")?;
        if !argv.is_empty() {
            return run_askpass(runner, &argv, dur).await;
        }
    }

    // 3-4. Token env vars.
    if let Some(token) = env.get("JJ_GH_TOKEN") {
        return Ok(SecretString::new(token.into()));
    }
    if let Some(token) = env.get("GH_TOKEN") {
        return Ok(SecretString::new(token.into()));
    }

    // 5. `gh_askpass` from config files.
    if let Some(argv) = config.gh_askpass.as_deref().filter(|a| !a.is_empty()) {
        return run_askpass(runner, argv, dur).await;
    }

    // 6. Plain `gh_token` from config files.
    if let Some(token) = &config.gh_token {
        log::info!("using plain token from config; configure `gh_askpass` for a safer setup");
        return Ok(token.clone());
    }

    // 7. `gh auth token`.
    if let Ok(token) = run_token_command(runner, &["gh", "auth", "token"], dur).await {
        return Ok(token);
    }

    Err(anyhow!(
        "no GitHub token available: use `--gh-askpass`, configure `gh_token`, set `JJ_GH_TOKEN` or \
        `GH_TOKEN` environment variable, or run `gh auth login`"
    ))
}

/// Run an askpass argv and wrap failures with the rendered command for context.
async fn run_askpass<R: ProcessRunner>(
    runner: &R,
    argv: &[String],
    dur: Duration,
) -> Result<SecretString> {
    run_token_command(runner, argv, dur)
        .await
        .with_context(|| format!("askpass `{}` failed", shell_words::join(argv)))
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
    use crate::util::ShellCommand;
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
            gh_askpass: Some(ShellCommand(vec!["/fake/askpass".into()])),
            askpass_timeout_secs: 5,
            ..Config::default()
        }
    }

    fn ok_runner(stdout: &[u8]) -> FakeRunner {
        FakeRunner::new(SpawnOutcome::Completed {
            code: Some(0),
            stdout: stdout.to_vec(),
            stderr: vec![],
        })
    }

    #[tokio::test]
    async fn resolves_via_askpass_happy_path() {
        let runner = FakeRunner::new(SpawnOutcome::Completed {
            code: Some(0),
            stdout: b"ghp_from_askpass\n".to_vec(),
            stderr: vec![],
        });
        let token = resolve_token_with(None, &config_with_askpass(), &runner, &empty_env())
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
        let err = resolve_token_with(None, &config_with_askpass(), &runner, &empty_env())
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
        let err = resolve_token_with(None, &config_with_askpass(), &runner, &empty_env())
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
        let err = resolve_token_with(None, &config_with_askpass(), &runner, &empty_env())
            .await
            .unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("exceeds"), "msg: {msg}");
    }

    #[tokio::test]
    async fn errors_on_timeout() {
        let runner = FakeRunner::new(SpawnOutcome::TimedOut);
        let err = resolve_token_with(None, &config_with_askpass(), &runner, &empty_env())
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
        let err = resolve_token_with(None, &config_with_askpass(), &runner, &empty_env())
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
        let token = resolve_token_with(None, &config, &runner, &empty_env())
            .await
            .unwrap();
        assert_eq!(token.expose_secret(), "ghp_plain");
    }

    #[tokio::test]
    async fn errors_when_no_source_configured() {
        let runner = FakeRunner::new(SpawnOutcome::TimedOut);
        let err = resolve_token_with(None, &Config::default(), &runner, &empty_env())
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
        let token = resolve_token_with(None, &Config::default(), &runner, &empty_env())
            .await
            .unwrap();
        assert_eq!(token.expose_secret(), "ghp_from_gh_cli");
    }

    #[tokio::test]
    async fn empty_askpass_argv_is_skipped() {
        let runner = FakeRunner::new(SpawnOutcome::TimedOut); // gh-auth fallback unused
        let config = Config {
            gh_askpass: Some(ShellCommand(vec![])),
            ..Config::default()
        };
        // Empty askpass is skipped, not an error; with nothing else configured
        // we fall through to the "no token" error.
        let err = resolve_token_with(None, &config, &runner, &empty_env())
            .await
            .unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("no GitHub token"), "msg: {msg}");
    }

    #[tokio::test]
    async fn resolves_via_multi_arg_askpass() {
        let runner = FakeRunner::new(SpawnOutcome::Completed {
            code: Some(0),
            stdout: b"ghp_from_op\n".to_vec(),
            stderr: vec![],
        });
        let config = Config {
            gh_askpass: Some(ShellCommand(vec![
                "op".into(),
                "read".into(),
                "op://Vault/github/token".into(),
            ])),
            askpass_timeout_secs: 5,
            ..Config::default()
        };
        let token = resolve_token_with(None, &config, &runner, &empty_env())
            .await
            .unwrap();
        assert_eq!(token.expose_secret(), "ghp_from_op");
    }

    #[tokio::test]
    async fn resolves_via_jj_gh_token_env() {
        let runner = FakeRunner::new(SpawnOutcome::TimedOut); // unused
        let env = FakeEnv::with(&[("JJ_GH_TOKEN", "ghp_from_jj_env")]);
        let token = resolve_token_with(None, &Config::default(), &runner, &env)
            .await
            .unwrap();
        assert_eq!(token.expose_secret(), "ghp_from_jj_env");
    }

    #[tokio::test]
    async fn resolves_via_gh_token_env() {
        let runner = FakeRunner::new(SpawnOutcome::TimedOut); // unused
        let env = FakeEnv::with(&[("GH_TOKEN", "ghp_from_gh_env")]);
        let token = resolve_token_with(None, &Config::default(), &runner, &env)
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
        let token = resolve_token_with(None, &Config::default(), &runner, &env)
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
        let token = resolve_token_with(None, &config, &runner, &env)
            .await
            .unwrap();
        assert_eq!(token.expose_secret(), "ghp_from_env");
    }

    /// Runner that echoes the argv it was given as the token, so tests can
    /// assert *which* askpass command was selected.
    struct EchoRunner;

    impl ProcessRunner for EchoRunner {
        async fn run(&self, argv: &[impl AsRef<OsStr>], _: Duration) -> SpawnOutcome {
            let joined = argv
                .iter()
                .map(|a| a.as_ref().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join(" ");
            SpawnOutcome::Completed {
                code: Some(0),
                stdout: joined.into_bytes(),
                stderr: vec![],
            }
        }
    }

    #[tokio::test]
    async fn flag_askpass_beats_env_tokens() {
        let runner = ok_runner(b"ghp_from_flag_askpass\n");
        let env = FakeEnv::with(&[
            ("JJ_GH_TOKEN", "ghp_from_jj_env"),
            ("GH_TOKEN", "ghp_from_gh_env"),
        ]);
        let flag = vec!["op".to_string(), "read".into()];
        let token = resolve_token_with(Some(&flag), &Config::default(), &runner, &env)
            .await
            .unwrap();
        assert_eq!(token.expose_secret(), "ghp_from_flag_askpass");
    }

    #[tokio::test]
    async fn gh_askpass_env_beats_config_askpass() {
        let env = FakeEnv::with(&[("GH_ASKPASS", "env-helper")]);
        let token = resolve_token_with(None, &config_with_askpass(), &EchoRunner, &env)
            .await
            .unwrap();
        assert_eq!(token.expose_secret(), "env-helper");
    }

    #[tokio::test]
    async fn env_token_beats_config_askpass() {
        // The reported bug: a config-file `gh_askpass` must not outrank env tokens.
        let runner = ok_runner(b"ghp_from_askpass\n");
        let env = FakeEnv::with(&[("JJ_GH_TOKEN", "ghp_from_jj_env")]);
        let token = resolve_token_with(None, &config_with_askpass(), &runner, &env)
            .await
            .unwrap();
        assert_eq!(token.expose_secret(), "ghp_from_jj_env");
    }

    #[tokio::test]
    async fn config_askpass_beats_plain_token_and_gh_auth() {
        // No flag, no env: config-file askpass still outranks `gh_token` and
        // the `gh auth token` fallback.
        let runner = ok_runner(b"ghp_from_askpass\n");
        let config = Config {
            gh_token: Some(SecretString::from("ghp_plain".to_string())),
            ..config_with_askpass()
        };
        let token = resolve_token_with(None, &config, &runner, &empty_env())
            .await
            .unwrap();
        assert_eq!(token.expose_secret(), "ghp_from_askpass");
    }
}
