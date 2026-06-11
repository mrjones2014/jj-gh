//! PR lookup helpers: resolve a revision or PR number into the
//! GitHub PR (and its remote target). Used by the `pr` handlers (`auto-merge`,
//! `edit`, `retry-failed`) and by `jj-gh debug pr-lookup`.

use super::{Gh, PrDetails, PrSummary, remote};
use crate::jj::Jj;
use anyhow::{Result, anyhow};

/// Resolved lookup state for a revision: the bookmark, the remote target, the
/// `owner:branch` head spec, the detected trunk bookmark, and the open PR (if
/// any) whose head matches.
///
/// Shared by `jj-gh pr auto-merge <rev>` and `jj-gh debug pr-lookup`.
#[derive(Debug)]
pub struct PrLookup {
    pub branch: String,
    pub target: remote::Target,
    pub head_spec: String,
    pub default_base: String,
    pub summary: Option<PrSummary>,
}

/// Lookup a PR by either a revision ID or PR number
///
/// # Errors
///
/// Returns an error if `rev` has no local bookmark, if the configured
/// `default_remote` is unset, if `trunk()` is empty, or if any underlying
/// jj/GH call fails.
pub async fn get_pr<J: Jj, G: Gh>(
    jj: &J,
    gh: &G,
    default_remote: &str,
    upstream_remote: &str,
    number_or_rev: &str,
) -> Result<PrDetails> {
    Ok(
        resolve_pr_with_target(jj, gh, default_remote, upstream_remote, number_or_rev)
            .await?
            .0,
    )
}

/// Same as [`get_pr`] but also returns the resolved [`remote::Target`] so
/// callers that need the owner/repo for further API calls don't have to
/// re-derive it from the remote URL.
///
/// # Errors
///
/// See [`get_pr`].
pub async fn resolve_pr_with_target<J: Jj, G: Gh>(
    jj: &J,
    gh: &G,
    default_remote: &str,
    upstream_remote: &str,
    number_or_rev: &str,
) -> Result<(PrDetails, remote::Target)> {
    if let Ok(num) = number_or_rev.parse::<u64>() {
        let origin_url = jj
            .remote_url(default_remote)
            .await?
            .ok_or_else(|| anyhow!("`{default_remote}` remote is not configured"))?;
        let upstream_url = jj.remote_url(upstream_remote).await?;
        let target = remote::target(&origin_url, upstream_url.as_deref())?;
        let pr = gh.get_pr(&target.owner, &target.repo, num).await?;
        Ok((pr, target))
    } else {
        let lookup =
            resolve_pr_for_rev(jj, gh, default_remote, upstream_remote, number_or_rev).await?;
        let summary = lookup.summary.ok_or_else(|| {
            anyhow!(
                "no open PR for revision `{number_or_rev}` (head `{}`)",
                lookup.head_spec,
            )
        })?;
        let pr = gh
            .get_pr(&lookup.target.owner, &lookup.target.repo, summary.number)
            .await?;
        Ok((pr, lookup.target))
    }
}

/// Resolve a revision into its PR-lookup context: bookmark, remote target,
/// head spec, trunk bookmark, and any existing open PR.
///
/// # Errors
///
/// Returns an error if `rev` has no local bookmark, if the configured
/// `default_remote` is unset, if `trunk()` is empty, or if any underlying
/// jj/GH call fails.
pub async fn resolve_pr_for_rev<J: Jj, G: Gh>(
    jj: &J,
    gh: &G,
    default_remote: &str,
    upstream_remote: &str,
    rev: &str,
) -> Result<PrLookup> {
    let info = jj.resolve_rev(rev).await?;
    let branch = info
        .bookmarks
        .first()
        .cloned()
        .ok_or_else(|| anyhow!("no local bookmark on `{rev}`; nothing to look up"))?;

    let origin_url = jj
        .remote_url(default_remote)
        .await?
        .ok_or_else(|| anyhow!("`{default_remote}` remote is not configured"))?;
    let upstream_url = jj.remote_url(upstream_remote).await?;
    let target = remote::target(&origin_url, upstream_url.as_deref())?;
    let head_spec = target.head_spec(&branch);

    let default_base = jj
        .trunk_branch()
        .await?
        .ok_or_else(|| anyhow!("could not detect trunk() bookmark"))?;

    let summary = gh
        .find_open_pr(&target.owner, &target.repo, &head_spec)
        .await?;

    Ok(PrLookup {
        branch,
        target,
        head_spec,
        default_base,
        summary,
    })
}
