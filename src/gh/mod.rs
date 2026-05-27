//! GitHub API abstraction.
//!
//! All write-side calls go through [`Gh`]. The production impl wraps `octocrab`;
//! tests use a fake.

use crate::config::AutoMergeMethod;
use anyhow::Result;

mod prs_with_ci_status;

pub mod real;
pub mod remote;
pub use prs_with_ci_status::{CiStatus, PrWithCiStatus};

/// Summary of an existing pull request. Just the fields we render.
#[derive(Debug, Clone)]
pub struct PrSummary {
    pub number: u64,
    pub html_url: String,
    pub title: String,
    pub state: String,
}

/// Full PR metadata used to render bookmark templates and stderr hints in
/// `pr fetch`. `head_user_login` / `head_repo_name` are `None` when GitHub
/// returns a null user/repo (e.g. the source fork has been deleted).
#[derive(Debug, Clone)]
pub struct PrDetails {
    pub number: u64,
    pub title: String,
    pub html_url: String,
    pub head_ref: String,
    pub head_sha: String,
    pub head_user_login: Option<String>,
    pub head_repo_name: Option<String>,
    pub graphql_node_id: String,
}

/// Inputs to [`Gh::create_pr`].
#[derive(Debug, Clone)]
pub struct CreatePrRequest {
    pub owner: String,
    pub repo: String,
    pub title: String,
    pub body: String,
    pub head: String,
    pub base: String,
    pub draft: bool,
}

/// Result of a successful [`Gh::create_pr`].
#[derive(Debug, Clone)]
pub struct PrCreated {
    pub number: u64,
    pub html_url: String,
    /// GraphQL node ID for the PR. Needed to enable auto-merge.
    pub node_id: String,
}

pub trait Gh {
    /// First open PR whose `head` matches `head_spec`. Must be `owner:branch`;
    /// GitHub silently ignores the `head` filter without the owner prefix.
    ///
    /// # Errors
    ///
    /// Propagates API errors.
    async fn find_open_pr(
        &self,
        owner: &str,
        repo: &str,
        head_spec: &str,
    ) -> Result<Option<PrSummary>>;

    /// Whether `branch` exists on `owner/repo`.
    ///
    /// # Errors
    ///
    /// Propagates API errors other than 404 (which becomes `Ok(false)`).
    async fn branch_exists(&self, owner: &str, repo: &str, branch: &str) -> Result<bool>;

    /// Create a pull request.
    ///
    /// # Errors
    ///
    /// Propagates API errors.
    async fn create_pr(&self, req: CreatePrRequest) -> Result<PrCreated>;

    /// Add reviewers to a PR. Will not remove existing reviewers.
    async fn add_reviewers(
        &self,
        owner: &str,
        repo: &str,
        pr: u64,
        reviewers: Vec<String>,
    ) -> Result<()>;

    /// Add labels to a PR.
    ///
    /// # Errors
    ///
    /// Propagates API errors.
    async fn add_labels(
        &self,
        owner: &str,
        repo: &str,
        pr_num: u64,
        labels: &[String],
    ) -> Result<()>;

    /// Fetch full metadata for a PR by number.
    ///
    /// # Errors
    ///
    /// Returns a clear "not found" error on 404; propagates other API errors.
    async fn get_pr(&self, owner: &str, repo: &str, number: u64) -> Result<PrDetails>;

    /// Enable auto-merge on a PR via the GraphQL `enablePullRequestAutoMerge`
    /// mutation. The PR is identified by its GraphQL node ID (see
    /// [`PrCreated::node_id`]).
    ///
    /// # Errors
    ///
    /// Propagates API errors. Common failures: the repo does not have
    /// auto-merge enabled, required branch protections are missing, or the PR
    /// is already mergeable.
    async fn enable_auto_merge(&self, pr_node_id: &str, method: AutoMergeMethod) -> Result<()>;

    /// Fetch open PRs with head commit SHA and CI status, scoped to the given
    /// `branches` (head ref names). Used by `pr log` to build a `commit_id` →
    /// PR mapping for jj template aliases.
    ///
    /// The search is `is:pr is:open head:<b1> head:<b2> ...`. Implementations
    /// may batch large `branches` lists into multiple requests to stay under
    /// GitHub's search query length limit.
    ///
    /// Returns an empty vec when `branches` is empty (skips the API call).
    ///
    /// # Errors
    ///
    /// Propagates GraphQL/API errors.
    async fn local_pulls(
        &self,
        owner: &str,
        repo: &str,
        branches: &[String],
    ) -> Result<Vec<PrWithCiStatus>>;
}
