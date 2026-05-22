//! `jj` CLI-backed [`Jj`] implementation.
//!
//! Remote-URL reads go through `git config` against jj's embedded git store
//! (`<workspace>/.jj/repo/store/git`) so the same code path works for both
//! colocated and pure-jj repos.

use super::{CommitInfo, Jj};
use anyhow::{Context, Result, anyhow};
use std::{path::PathBuf, process::Command};

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
pub struct JjCli;

impl Jj for JjCli {
    fn resolve_rev(&self, rev: &str) -> Result<CommitInfo> {
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
        ])?;
        serde_json::from_slice(&stdout)
            .with_context(|| format!("could not parse jj log output for `{rev}`"))
    }

    fn stacked_ancestor_bookmark(&self, rev: &str) -> Result<Option<String>> {
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
        ])?;
        if stdout.is_empty() {
            return Ok(None);
        }
        let bookmarks: Vec<String> =
            serde_json::from_slice(&stdout).context("could not parse jj bookmarks output")?;
        Ok(bookmarks.into_iter().next())
    }

    fn first_commit_description(&self, revset: &str) -> Result<String> {
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
        ])?;
        Ok(std::str::from_utf8(&stdout)
            .context("jj log output is not UTF-8")?
            .trim()
            .to_string())
    }

    fn remote_url(&self, name: &str) -> Result<Option<String>> {
        let git_dir = git_backend_dir()?;
        let output = Command::new("git")
            .arg("-C")
            .arg(&git_dir)
            .args(["config", "--get", &format!("remote.{name}.url")])
            .output()
            .context("failed to spawn `git`")?;
        if !output.status.success() {
            // `git config --get` exits 1 when the key is missing.
            if output.status.code() == Some(1) {
                return Ok(None);
            }
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow!("`git config` failed: {}", stderr.trim()));
        }
        let url = String::from_utf8(output.stdout)
            .context("`git config` output is not UTF-8")?
            .trim()
            .to_string();
        Ok(Some(url).filter(|s| !s.is_empty()))
    }

    fn remote_bookmark_sha(&self, bookmark: &str, remote: &str) -> Result<Option<String>> {
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
        ])?;
        let sha = std::str::from_utf8(&stdout)
            .context("jj log output is not UTF-8")?
            .trim()
            .to_string();
        Ok(Some(sha).filter(|s| !s.is_empty()))
    }
}

fn workspace_root() -> Result<PathBuf> {
    let stdout = run_jj(&["workspace", "root"])?;
    let path = std::str::from_utf8(&stdout)
        .context("jj workspace root output is not UTF-8")?
        .trim()
        .to_string();
    if path.is_empty() {
        return Err(anyhow!("jj workspace root returned an empty path"));
    }
    Ok(PathBuf::from(path))
}

/// Resolve the git directory that jj uses as its store.
///
/// jj writes the path to its git backend in `.jj/repo/store/git_target` as a
/// path relative to `.jj/repo/store/`. Colocated repos point at `../../../.git`;
/// pure-jj repos point to a git dir embedded under `.jj/`.
fn git_backend_dir() -> Result<PathBuf> {
    let store_dir = workspace_root()?.join(".jj").join("repo").join("store");
    let target = std::fs::read_to_string(store_dir.join("git_target"))
        .context("could not read `.jj/repo/store/git_target`")?;
    let target = target.trim();
    Ok(store_dir.join(target))
}

fn run_jj(args: &[&str]) -> Result<Vec<u8>> {
    let output = Command::new("jj")
        .arg("--ignore-working-copy")
        .args(args)
        .output()
        .context("failed to spawn `jj`")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!("`jj {}` failed: {}", args.join(" "), stderr.trim()));
    }
    Ok(output.stdout)
}
