//! Small reusable helpers.

use anyhow::{Result, anyhow};
use std::future::Future;

/// Normalize stderr from a subprocess for inclusion in a user-facing error.
#[must_use]
pub fn subprocess_error(stderr: &[u8]) -> String {
    let message = String::from_utf8_lossy(stderr);
    let message = message.trim();
    let message = message.strip_prefix("Error: ").unwrap_or(message);
    if message.is_empty() {
        "command failed without error output".to_string()
    } else {
        message.to_string()
    }
}

/// Pair of an explicit override (e.g. CLI flag) and a config-supplied
/// last-resort default. Used by `subcommand_args!` for
/// `#[config(fallback = "...")]` fields where intermediate runtime sources
/// (async jj calls, ancestor bookmarks, etc.) sit between the two ends of the
/// precedence chain.
///
/// Resolution order:
/// 1. CLI override (if `Some`, the closure is never called);
/// 2. result of the async closure passed to [`Self::resolve`] / [`Self::resolve_or`];
/// 3. config fallback.
#[derive(Debug, Clone)]
pub struct EvalWithCfgFallback<T> {
    cli: Option<T>,
    fallback: Option<T>,
}

impl<T: Clone> EvalWithCfgFallback<T> {
    /// Used by macro-emitted code; not part of the public ergonomic API.
    #[must_use]
    pub fn new(cli: Option<T>, fallback: Option<T>) -> Self {
        Self { cli, fallback }
    }

    /// Resolve CLI first, then the closure result, then the config fallback.
    /// The closure is only awaited when CLI is `None`. Returns `None` if every
    /// source is empty.
    pub async fn resolve<F, Fut>(&self, f: F) -> Option<T>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Option<T>>,
    {
        if let Some(v) = self.cli.clone() {
            return Some(v);
        }
        if let Some(v) = f().await {
            return Some(v);
        }
        self.fallback.clone()
    }

    /// Like [`Self::resolve`] but errors with `err` if every source is `None`.
    pub async fn resolve_or<F, Fut, E>(&self, f: F, err: E) -> Result<T>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Option<T>>,
        E: Into<String>,
    {
        self.resolve(f)
            .await
            .ok_or_else(|| anyhow!("{}", err.into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn subprocess_error_removes_rust_style_prefix() {
        assert_eq!(
            subprocess_error(b"Error: revision does not exist\nCaused by:\nother"),
            "revision does not exist\nCaused by:\nother"
        );
    }

    #[test]
    fn subprocess_error_preserves_other_messages() {
        assert_eq!(
            subprocess_error(b"fatal: bad revision\n"),
            "fatal: bad revision"
        );
    }

    #[test]
    fn subprocess_error_handles_empty_stderr() {
        assert_eq!(subprocess_error(b""), "command failed without error output");
    }

    fn pair(cli: Option<&str>, fallback: Option<&str>) -> EvalWithCfgFallback<String> {
        EvalWithCfgFallback::new(cli.map(str::to_string), fallback.map(str::to_string))
    }

    #[tokio::test]
    async fn cli_wins_and_closure_never_runs() {
        let calls = Cell::new(0);
        let r = pair(Some("from-cli"), Some("from-fallback"))
            .resolve_or(
                || async {
                    calls.set(calls.get() + 1);
                    Some("from-closure".into())
                },
                "should not error",
            )
            .await;
        assert_eq!(r.unwrap(), "from-cli");
        assert_eq!(calls.get(), 0, "closure must not run when CLI is Some");
    }

    #[tokio::test]
    async fn closure_result_wins_over_fallback() {
        let r = pair(None, Some("from-fallback"))
            .resolve_or(|| async { Some("from-closure".into()) }, "should not error")
            .await;
        assert_eq!(r.unwrap(), "from-closure");
    }

    #[tokio::test]
    async fn fallback_used_when_cli_and_closure_both_empty() {
        let r = pair(None, Some("from-fallback"))
            .resolve_or(|| async { Option::<String>::None }, "should not error")
            .await;
        assert_eq!(r.unwrap(), "from-fallback");
    }

    #[tokio::test]
    async fn error_when_every_source_is_none() {
        let r = pair(None, None)
            .resolve_or(|| async { Option::<String>::None }, "all sources empty")
            .await;
        let err = r.unwrap_err();
        assert!(err.to_string().contains("all sources empty"));
    }

    #[tokio::test]
    async fn resolve_without_err_returns_none_when_all_empty() {
        let r = pair(None, None)
            .resolve(|| async { Option::<String>::None })
            .await;
        assert!(r.is_none());
    }
}
