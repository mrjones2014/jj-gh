//! Extract `(owner, repo)` from a git remote URL.

use anyhow::{Context, Result, anyhow};

/// Parse a git remote URL into `(owner, repo)`.
///
/// Supports https, ssh, and the `git@host:owner/repo` shorthand. The trailing
/// `.git` suffix is stripped when present.
///
/// # Errors
///
/// Returns an error if the URL is unparseable or does not contain an
/// `owner/repo` path component.
pub fn parse_owner_repo(remote_url: &str) -> Result<(String, String)> {
    let path = extract_path(remote_url)?;
    let path = path.strip_suffix(".git").unwrap_or(&path);
    let mut parts = path.splitn(2, '/');
    let owner = parts
        .next()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow!("missing owner in url: {remote_url}"))?
        .to_string();
    let repo = parts
        .next()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow!("missing repo in url: {remote_url}"))?
        .to_string();
    Ok((owner, repo))
}

fn extract_path(remote_url: &str) -> Result<String> {
    if let Some(rest) = remote_url.strip_prefix("git@") {
        // ssh shorthand: git@host:owner/repo[.git]
        let (_, path) = rest
            .split_once(':')
            .ok_or_else(|| anyhow!("invalid ssh shorthand: {remote_url}"))?;
        return Ok(path.to_string());
    }
    let parsed = ::url::Url::parse(remote_url)
        .with_context(|| format!("could not parse url: {remote_url}"))?;
    Ok(parsed.path().trim_start_matches('/').to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssh_shorthand_with_dot_git() {
        assert_eq!(
            parse_owner_repo("git@github.com:o/r.git").unwrap(),
            ("o".into(), "r".into())
        );
    }

    #[test]
    fn ssh_shorthand_without_dot_git() {
        assert_eq!(
            parse_owner_repo("git@github.com:o/r").unwrap(),
            ("o".into(), "r".into())
        );
    }

    #[test]
    fn https_with_dot_git() {
        assert_eq!(
            parse_owner_repo("https://github.com/o/r.git").unwrap(),
            ("o".into(), "r".into())
        );
    }

    #[test]
    fn https_without_dot_git() {
        assert_eq!(
            parse_owner_repo("https://github.com/o/r").unwrap(),
            ("o".into(), "r".into())
        );
    }

    #[test]
    fn ssh_url_form() {
        assert_eq!(
            parse_owner_repo("ssh://git@github.com/o/r.git").unwrap(),
            ("o".into(), "r".into())
        );
    }

    #[test]
    fn empty_path_errors() {
        let err = parse_owner_repo("https://github.com/").unwrap_err();
        assert!(err.to_string().contains("missing owner"), "msg: {err}");
    }

    #[test]
    fn missing_repo_errors() {
        let err = parse_owner_repo("https://github.com/o").unwrap_err();
        assert!(err.to_string().contains("missing repo"), "msg: {err}");
    }

    #[test]
    fn unparseable_url_errors() {
        let err = parse_owner_repo("not a url").unwrap_err();
        assert!(err.to_string().contains("could not parse"), "msg: {err}");
    }
}
