//! `jj` CLI-backed [`Jj`] implementation.
//!
//! Remote-URL reads parse jj's embedded git store config directly (via
//! [`parse_remote_url`]), which works in both colocated and pure-jj repos
//! without a `git` binary on `PATH`.

use super::{CommitInfo, Jj};
use anyhow::{Context, Result, anyhow};
use std::path::PathBuf;
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
pub struct JjCli;

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
        let config_path = git_backend_dir().await?.join("config");
        let contents = match std::fs::read_to_string(&config_path) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => {
                return Err(e)
                    .with_context(|| format!("could not read `{}`", config_path.display()));
            }
        };
        Ok(parse_remote_url(&contents, name))
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

    async fn workspace_root(&self) -> Result<PathBuf> {
        workspace_root().await
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

    async fn pushed_bookmarks(&self) -> Result<Vec<String>> {
        let stdout = run_jj(&[
            "log",
            "--no-graph",
            "-r",
            r#"bookmarks() & remote_bookmarks(remote=exact:"origin")"#,
            "-T",
            r#"bookmarks.map(|b| b.name() ++ "\n")"#,
        ])
        .await?;
        let mut names: Vec<String> = std::str::from_utf8(&stdout)
            .context("jj log output is not UTF-8")?
            .lines()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect();
        names.sort();
        names.dedup();
        Ok(names)
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

/// Resolve the git directory that jj uses as its store.
///
/// jj writes the path to its git backend in `.jj/repo/store/git_target` as a
/// path relative to `.jj/repo/store/`. Colocated repos point at `../../../.git`;
/// pure-jj repos point to a git dir embedded under `.jj/`.
async fn git_backend_dir() -> Result<PathBuf> {
    let store_dir = workspace_root()
        .await?
        .join(".jj")
        .join("repo")
        .join("store");
    let target = std::fs::read_to_string(store_dir.join("git_target"))
        .context("could not read `.jj/repo/store/git_target`")?;
    let target = target.trim();
    Ok(store_dir.join(target))
}

/// Extract a `[remote "NAME"] url = ...` value from a git-config-format string.
///
/// Supports comments (`#`, `;`), whitespace, and optionally-quoted values. Returns
/// the last `url` value in the matching section.
fn parse_remote_url(contents: &str, remote: &str) -> Option<String> {
    let target_header = format!(r#"[remote "{remote}"]"#);
    contents
        .lines()
        .map(strip_comment)
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .fold((false, None), |(in_section, url), line| {
            if is_section_header(line) {
                (line == target_header, url)
            } else if in_section {
                (in_section, parse_key_value(line, "url").or(url))
            } else {
                (in_section, url)
            }
        })
        .1
}

fn is_section_header(line: &str) -> bool {
    line.starts_with('[') && line.ends_with(']')
}

fn strip_comment(line: &str) -> &str {
    let cut = line.find(['#', ';']).unwrap_or(line.len());
    &line[..cut]
}

fn parse_key_value(line: &str, key: &str) -> Option<String> {
    let rest = line.strip_prefix(key)?;
    let rest = rest.trim_start();
    let value = rest.strip_prefix('=')?.trim();
    let unquoted = value
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or(value);
    if unquoted.is_empty() {
        None
    } else {
        Some(unquoted.to_string())
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_url_from_simple_remote_section() {
        let cfg = r#"
[remote "origin"]
	url = git@github.com:o/r.git
	fetch = +refs/heads/*:refs/remotes/origin/*
"#;
        assert_eq!(
            parse_remote_url(cfg, "origin"),
            Some("git@github.com:o/r.git".into())
        );
    }

    #[test]
    fn picks_correct_remote_when_multiple_present() {
        let cfg = r#"
[remote "origin"]
	url = git@github.com:fork/r.git
[remote "upstream"]
	url = git@github.com:org/r.git
"#;
        assert_eq!(
            parse_remote_url(cfg, "upstream"),
            Some("git@github.com:org/r.git".into())
        );
    }

    #[test]
    fn returns_none_when_remote_missing() {
        let cfg = r#"
[remote "origin"]
	url = git@github.com:o/r.git
"#;
        assert!(parse_remote_url(cfg, "upstream").is_none());
    }

    #[test]
    fn returns_none_when_section_has_no_url() {
        let cfg = r#"
[remote "origin"]
	fetch = +refs/heads/*:refs/remotes/origin/*
"#;
        assert!(parse_remote_url(cfg, "origin").is_none());
    }

    #[test]
    fn ignores_comments() {
        let cfg = r#"
# leading comment
[remote "origin"]
	# inline comment
	url = git@github.com:o/r.git ; trailing
"#;
        assert_eq!(
            parse_remote_url(cfg, "origin"),
            Some("git@github.com:o/r.git".into())
        );
    }

    #[test]
    fn handles_quoted_url() {
        let cfg = r#"
[remote "origin"]
	url = "https://github.com/o/r.git"
"#;
        assert_eq!(
            parse_remote_url(cfg, "origin"),
            Some("https://github.com/o/r.git".into())
        );
    }

    #[test]
    fn last_url_wins_within_section() {
        let cfg = r#"
[remote "origin"]
	url = git@github.com:o/old.git
	url = git@github.com:o/new.git
"#;
        assert_eq!(
            parse_remote_url(cfg, "origin"),
            Some("git@github.com:o/new.git".into())
        );
    }

    #[test]
    fn ignores_other_keys_in_section() {
        let cfg = r#"
[remote "origin"]
	pushurl = git@github.com:o/wrong.git
	url = git@github.com:o/r.git
"#;
        assert_eq!(
            parse_remote_url(cfg, "origin"),
            Some("git@github.com:o/r.git".into())
        );
    }
}
