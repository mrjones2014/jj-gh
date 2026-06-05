//! Resolve PR target from configured remotes.
//!
//! Rule: with an `upstream` remote, the PR goes to upstream's repo (fork workflow);
//! without, the PR goes to origin. In both cases `head_spec` is `<origin-owner>:<branch>`
//! because GitHub's list-PRs `head` filter is silently ignored without the owner prefix.

use crate::git::url::parse_owner_repo;
use anyhow::{Result, anyhow};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub owner: String,
    pub repo: String,
    origin_owner: String,
    upstream_remote: bool,
}

impl Target {
    /// Compose the GitHub `head` filter for `branch` in this target context.
    #[must_use]
    pub fn head_spec(&self, branch: &str) -> String {
        format!("{}:{branch}", self.origin_owner)
    }

    /// Compose the editor/template-facing base display for `branch`.
    ///
    /// Cross-fork PRs benefit from showing the base owner (`upstream:main`) so
    /// users can distinguish the target repo from their fork. Same-repo PRs
    /// keep the older bare branch display.
    #[must_use]
    pub fn base_spec(&self, branch: &str) -> String {
        if self.upstream_remote {
            format!("{}:{branch}", self.owner)
        } else {
            branch.to_string()
        }
    }
}

/// Convert an editor/template-facing base value into the branch-only value
/// GitHub GraphQL expects for `baseRefName`.
///
/// Accepts either `branch` or `<target-owner>:branch`. Rejects other owners so
/// we don't silently create/update against the wrong target repo.
pub fn branch_from_base_spec(target_owner: &str, base: &str) -> Result<String> {
    let base = base.trim();
    if base.is_empty() {
        return Err(anyhow!("base is empty"));
    }
    let Some((owner, branch)) = base.split_once(':') else {
        return Ok(base.to_string());
    };
    if owner != target_owner {
        return Err(anyhow!(
            "base owner `{owner}` does not match PR target owner `{target_owner}`"
        ));
    }
    let branch = branch.trim();
    if branch.is_empty() {
        return Err(anyhow!("base branch is empty"));
    }
    Ok(branch.to_string())
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
                upstream_remote: true,
            })
        }
        None => Ok(Target {
            owner: origin_owner.clone(),
            repo: origin_repo,
            origin_owner,
            upstream_remote: false,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origin_only_uses_origin_owner_repo_with_qualified_head() {
        let t = target("git@github.com:o/r.git", None).unwrap();
        assert_eq!(t.owner, "o");
        assert_eq!(t.repo, "r");
        assert_eq!(t.head_spec("feature"), "o:feature");
        assert_eq!(t.base_spec("main"), "main");
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
        assert_eq!(t.base_spec("master"), "upstream-owner:master");
    }

    #[test]
    fn handles_https_origin() {
        let t = target("https://github.com/o/r", None).unwrap();
        assert_eq!(t.owner, "o");
        assert_eq!(t.repo, "r");
        assert_eq!(t.head_spec("x"), "o:x");
        assert_eq!(t.base_spec("main"), "main");
    }

    #[test]
    fn upstream_remote_qualifies_base_even_when_owner_matches() {
        let t = target(
            "git@github.com:o/forked-name.git",
            Some("git@github.com:o/upstream-name.git"),
        )
        .unwrap();
        assert_eq!(t.base_spec("main"), "o:main");
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
        assert_eq!(t.base_spec("main"), "org:main");
    }

    #[test]
    fn unparseable_origin_errors() {
        let err = target("not a url", None).unwrap_err();
        assert!(err.to_string().contains("could not parse"));
    }

    #[test]
    fn branch_from_base_spec_accepts_bare_branch() {
        assert_eq!(
            branch_from_base_spec("upstream", "main").unwrap(),
            "main".to_string()
        );
    }

    #[test]
    fn branch_from_base_spec_strips_matching_owner() {
        assert_eq!(
            branch_from_base_spec("upstream", "upstream:master").unwrap(),
            "master".to_string()
        );
    }

    #[test]
    fn branch_from_base_spec_rejects_wrong_owner() {
        let err = branch_from_base_spec("upstream", "other:master").unwrap_err();
        assert!(err.to_string().contains("does not match"));
    }
}
