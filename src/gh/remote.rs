//! Resolve PR target from configured remotes.
//!
//! Rule: with an `upstream` remote, the PR goes to upstream's repo and `head_spec`
//! is `<origin-owner>:<branch>` (fork workflow). Without, the PR goes to origin
//! and `head_spec` is just `<branch>`.

use crate::git::url::parse_owner_repo;
use anyhow::Result;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub owner: String,
    pub repo: String,
    origin_owner: String,
    is_fork: bool,
}

impl Target {
    /// Compose the GitHub `head` filter for `branch` in this target context.
    #[must_use]
    pub fn head_spec(&self, branch: &str) -> String {
        if self.is_fork {
            format!("{}:{branch}", self.origin_owner)
        } else {
            branch.to_string()
        }
    }
}

/// Compute the PR [`Target`] for the given remote URLs.
///
/// # Errors
///
/// Returns an error if either URL fails to parse into `(owner, repo)`.
pub fn target(origin_url: &str, upstream_url: Option<&str>) -> Result<Target> {
    let (origin_owner, origin_repo) = parse_owner_repo(origin_url)?;
    match upstream_url {
        Some(upstream) => {
            let (owner, repo) = parse_owner_repo(upstream)?;
            Ok(Target {
                owner,
                repo,
                origin_owner,
                is_fork: true,
            })
        }
        None => Ok(Target {
            owner: origin_owner.clone(),
            repo: origin_repo,
            origin_owner,
            is_fork: false,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origin_only_uses_origin_owner_repo_and_plain_branch() {
        let t = target("git@github.com:o/r.git", None).unwrap();
        assert_eq!(t.owner, "o");
        assert_eq!(t.repo, "r");
        assert_eq!(t.head_spec("feature"), "feature");
    }

    #[test]
    fn fork_routes_to_upstream_with_owner_prefixed_head() {
        let t = target(
            "git@github.com:fork-owner/r.git",
            Some("git@github.com:upstream-owner/r.git"),
        )
        .unwrap();
        assert_eq!(t.owner, "upstream-owner");
        assert_eq!(t.repo, "r");
        assert_eq!(t.head_spec("feature"), "fork-owner:feature");
    }

    #[test]
    fn handles_https_origin() {
        let t = target("https://github.com/o/r", None).unwrap();
        assert_eq!(t.owner, "o");
        assert_eq!(t.repo, "r");
        assert_eq!(t.head_spec("x"), "x");
    }

    #[test]
    fn handles_dot_git_variations() {
        let a = target("https://github.com/o/r.git", None).unwrap();
        let b = target("https://github.com/o/r", None).unwrap();
        assert_eq!(a, b);
        assert_eq!(a.head_spec("x"), b.head_spec("x"));
    }

    #[test]
    fn fork_with_different_repo_names_uses_upstream_repo() {
        let t = target(
            "git@github.com:me/forked-name.git",
            Some("git@github.com:org/canonical-name.git"),
        )
        .unwrap();
        assert_eq!(t.owner, "org");
        assert_eq!(t.repo, "canonical-name");
        assert_eq!(t.head_spec("feature"), "me:feature");
    }

    #[test]
    fn unparseable_origin_errors() {
        let err = target("not a url", None).unwrap_err();
        assert!(err.to_string().contains("could not parse"));
    }
}
