//! `jj` CLI-backed [`Jj`] implementation.
//!
//! Remote-URL reads go through `gix` against the colocated git store
//! discovered at the workspace root. The repository is discovered once at
//! [`JjCli::new`] and reused for every subsequent gix operation.

use super::{CommitInfo, Jj, PushedBookmark};
use anyhow::{Context, Result, anyhow};
use std::path::{Path, PathBuf};
use tokio::process::Command;

/// Build a jj template that emits a JSON object: each `(key, expr)` becomes
/// `"key": json(expr)`.
fn json_object_template(fields: &[(&str, &str)]) -> String {
    let body = fields
        .iter()
        .map(|(name, expr)| format!(r#""\"{name}\":" ++ json({expr})"#))
        .collect::<Vec<_>>()
        .join(r#" ++ "," ++ "#);
    format!(r#""{{" ++ {body} ++ "}}""#)
}

/// Production [`Jj`] impl that shells out to the system `jj` binary.
///
/// Caches the workspace root and a `gix::Repository` discovered at
/// construction so all gix-backed reads share one handle.
pub struct JjCli {
    repo: gix::Repository,
    workspace_root: PathBuf,
}

impl JjCli {
    /// Resolve the workspace root via `jj` and discover its colocated git
    /// store. Subsequent gix operations reuse the cached [`gix::Repository`].
    ///
    /// # Errors
    ///
    /// Propagates failures from `jj workspace root` or gix discovery.
    pub async fn new() -> Result<Self> {
        let workspace_root = workspace_root().await?;
        let repo = gix::discover(&workspace_root)?;
        Ok(Self {
            repo,
            workspace_root,
        })
    }

    /// Shared handle to the discovered git repository.
    #[must_use]
    pub fn repo(&self) -> &gix::Repository {
        &self.repo
    }
}

impl Jj for JjCli {
    async fn resolve_rev(&self, rev: &str) -> Result<CommitInfo> {
        let template = json_object_template(&[
            ("change_id", "change_id"),
            ("commit_id", "commit_id"),
            ("description", "description"),
            ("bookmarks", "bookmarks.map(|b| b.name())"),
        ]);
        let stdout = run_jj(&[
            "log",
            "--no-graph",
            "-r",
            rev,
            "--limit",
            "1",
            "-T",
            &template,
        ])
        .await?;
        serde_json::from_slice(&stdout)
            .with_context(|| format!("could not parse jj log output for `{rev}`"))
    }

    async fn stacked_ancestor_bookmark(&self, rev: &str) -> Result<Option<String>> {
        let revset = format!("ancestors(({rev})-) & bookmarks()");
        let stdout = run_jj(&[
            "log",
            "--no-graph",
            "-r",
            &revset,
            "--limit",
            "1",
            "-T",
            "json(bookmarks.map(|b| b.name()))",
        ])
        .await?;
        if stdout.is_empty() {
            return Ok(None);
        }
        let bookmarks: Vec<String> =
            serde_json::from_slice(&stdout).context("could not parse jj bookmarks output")?;
        Ok(bookmarks.into_iter().next())
    }

    async fn first_commit_description(&self, revset: &str) -> Result<String> {
        let stdout = run_jj(&[
            "log",
            "--no-graph",
            "--reversed",
            "-r",
            revset,
            "--limit",
            "1",
            "-T",
            "description.first_line()",
        ])
        .await?;
        Ok(std::str::from_utf8(&stdout)
            .context("jj log output is not UTF-8")?
            .trim()
            .to_string())
    }

    async fn remote_url(&self, name: &str) -> Result<Option<String>> {
        let remote = self.repo.find_remote(name).ok();
        Ok(remote.and_then(|remote| {
            remote
                .url(gix::remote::Direction::Fetch)
                .map(ToString::to_string)
        }))
    }

    async fn push(&self, rev: &str) -> Result<()> {
        let status = Command::new("jj")
            .args(["git", "push", "-c", rev])
            .status()
            .await
            .context("failed to spawn `jj`")?;
        if !status.success() {
            return Err(anyhow!("`jj git push -c {rev}` failed with {status}"));
        }
        Ok(())
    }

    async fn trunk_branch(&self) -> Result<Option<String>> {
        let stdout = run_jj(&[
            "log",
            "--no-graph",
            "-r",
            "trunk()",
            "--limit",
            "1",
            "-T",
            "json(bookmarks.map(|b| b.name()))",
        ])
        .await?;
        if stdout.is_empty() {
            return Ok(None);
        }
        let names: Vec<String> =
            serde_json::from_slice(&stdout).context("could not parse jj bookmarks output")?;
        Ok(names.into_iter().next())
    }

    async fn remote_bookmark_sha(&self, bookmark: &str, remote: &str) -> Result<Option<String>> {
        let revset = format!("remote_bookmarks(exact:{bookmark:?}, remote=exact:{remote:?})");
        let stdout = run_jj(&[
            "log",
            "--no-graph",
            "-r",
            &revset,
            "--limit",
            "1",
            "-T",
            "commit_id",
        ])
        .await?;
        let sha = std::str::from_utf8(&stdout)
            .context("jj log output is not UTF-8")?
            .trim()
            .to_string();
        Ok(Some(sha).filter(|s| !s.is_empty()))
    }

    async fn workspace_root(&self) -> Result<&PathBuf> {
        Ok(&self.workspace_root)
    }

    async fn git_import(&self) -> Result<()> {
        let status = Command::new("jj")
            .args(["git", "import"])
            .status()
            .await
            .context("failed to spawn `jj`")?;
        if !status.success() {
            return Err(anyhow!("`jj git import` failed with {status}"));
        }
        Ok(())
    }

    async fn eval_template(
        &self,
        revset: &str,
        template: &str,
        config_file: Option<&Path>,
        reversed: bool,
    ) -> Result<String> {
        let args = eval_template_argv(revset, template, config_file, reversed);
        String::from_utf8(run_jj_strs(&args).await?).context("jj log output is not UTF-8")
    }

    async fn pushed_bookmarks(&self, remote: &str) -> Result<Vec<PushedBookmark>> {
        // `jj bookmark list --tracked --remote <remote>` emits one entry per
        // local/remote side of each tracked bookmark; filtering on
        // `if(remote, ...)` keeps only the local-side row, whose
        // `normal_target.commit_id()` is the local commit (which may diverge
        // from the remote target, e.g. local rebase without push).
        //
        // Emit NDJSON: one `PushedBookmark` per line, parsed via serde so
        // bookmark names with unusual characters round-trip safely.
        let record = json_object_template(&[
            ("name", "name"),
            ("local_commit_id", "normal_target.commit_id()"),
        ]);
        let template = format!(r#"if(remote, "", {record} ++ "\n")"#);
        let stdout = run_jj(&[
            "bookmark",
            "list",
            "--tracked",
            "--remote",
            remote,
            "-T",
            &template,
        ])
        .await?;
        let mut bookmarks: Vec<PushedBookmark> = std::str::from_utf8(&stdout)
            .context("jj bookmark list output is not UTF-8")?
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| {
                serde_json::from_str(l).with_context(|| format!("parsing bookmark record `{l}`"))
            })
            .collect::<Result<_>>()?;
        bookmarks.sort_by(|a, b| a.name.cmp(&b.name));
        bookmarks.dedup_by(|a, b| a.name == b.name);
        Ok(bookmarks)
    }
}

async fn workspace_root() -> Result<PathBuf> {
    let stdout = run_jj(&["workspace", "root"]).await?;
    let path = std::str::from_utf8(&stdout)
        .context("jj workspace root output is not UTF-8")?
        .trim()
        .to_string();
    if path.is_empty() {
        return Err(anyhow!("jj workspace root returned an empty path"));
    }
    Ok(PathBuf::from(path))
}

async fn run_jj(args: &[&str]) -> Result<Vec<u8>> {
    let output = Command::new("jj")
        .arg("--ignore-working-copy")
        .args(args)
        .output()
        .await
        .context("failed to spawn `jj`")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!("`jj {}` failed: {}", args.join(" "), stderr.trim()));
    }
    Ok(output.stdout)
}

async fn run_jj_strs(args: &[String]) -> Result<Vec<u8>> {
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    run_jj(&refs).await
}

/// Build the argv passed to `run_jj` for [`Jj::eval_template`]. Pure so it
/// can be unit tested without spawning.
fn eval_template_argv(
    revset: &str,
    template: &str,
    config_file: Option<&Path>,
    reversed: bool,
) -> Vec<String> {
    let mut argv: Vec<String> = Vec::with_capacity(10);
    if let Some(path) = config_file {
        argv.push("--config-file".into());
        argv.push(path.to_string_lossy().into_owned());
    }
    argv.push("log".into());
    argv.push("-r".into());
    argv.push(revset.into());
    argv.push("--no-graph".into());
    argv.push("--color=never".into());
    if reversed {
        argv.push("--reversed".into());
    }
    argv.push("-T".into());
    argv.push(template.into());
    argv
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eval_template_argv_minimal() {
        let argv = eval_template_argv("@", "description", None, false);
        assert_eq!(
            argv,
            vec![
                "log",
                "-r",
                "@",
                "--no-graph",
                "--color=never",
                "-T",
                "description"
            ]
        );
    }

    #[test]
    fn eval_template_argv_with_config_file_and_reversed() {
        let argv = eval_template_argv(
            "trunk()..@",
            "description.first_line()",
            Some(Path::new("/tmp/x.toml")),
            true,
        );
        assert_eq!(
            argv,
            vec![
                "--config-file",
                "/tmp/x.toml",
                "log",
                "-r",
                "trunk()..@",
                "--no-graph",
                "--color=never",
                "--reversed",
                "-T",
                "description.first_line()",
            ]
        );
    }

    #[test]
    fn eval_template_argv_config_file_precedes_subcommand() {
        let argv = eval_template_argv("@", "x", Some(Path::new("/c.toml")), false);
        let log_idx = argv.iter().position(|s| s == "log").unwrap();
        let cfg_idx = argv.iter().position(|s| s == "--config-file").unwrap();
        assert!(cfg_idx < log_idx, "--config-file must precede `log`");
    }
}
