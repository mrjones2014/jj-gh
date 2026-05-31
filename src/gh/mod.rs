//! GitHub API abstraction.
//!
//! All write-side calls go through [`Gh`]. The production impl wraps `octocrab`;
//! tests use a fake.

use crate::config::AutoMergeMethod;
use anyhow::Result;

mod queries;
mod reviewer;

pub mod real;
pub mod remote;
pub use queries::{CiStatus, PrWithCiStatus};
pub use reviewer::Reviewer;

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
    pub is_draft: bool,
    pub auto_merge: bool,
    pub auto_merge_method: Option<AutoMergeMethod>,
    pub number: u64,
    pub title: String,
    pub html_url: String,
    pub head_ref: String,
    /// In GraphQL this is called `headRefOid`
    pub head_sha: String,
    pub head_user_login: Option<String>,
    pub head_repo_name: Option<String>,
    pub graphql_node_id: String,
    /// True when the PR's base branch has a merge queue. Determines whether
    /// "merge when ready" routes through `enqueuePullRequest` instead of
    /// `enablePullRequestAutoMerge`.
    pub in_merge_queue: bool,
    pub labels: Vec<Label>,
    pub reviewers: Vec<Reviewer>,
    /// Body, if you requested it
    pub body: Option<String>,
}

/// Result of [`Gh::lookup_base`]: the base repo's GraphQL node ID plus whether
/// the requested branch exists. Combines the repo-id resolution needed by
/// [`Gh::create_pr`] with the base-branch precondition check.
#[derive(Debug, Clone)]
pub struct BaseLookup {
    pub repo_node_id: String,
    pub branch_exists: bool,
}

/// Inputs to [`Gh::create_pr`]. The base repository is identified by its
/// GraphQL node ID (resolved via [`Gh::lookup_base`]).
#[derive(Debug, Clone)]
pub struct CreatePrRequest {
    pub repo_node_id: String,
    pub title: String,
    pub body: String,
    /// Head spec. For same-repo PRs this is the branch name; for cross-repo
    /// (fork) PRs use `owner:branch`.
    pub head: String,
    pub base: String,
    pub draft: bool,
}

/// Mutable text fields on an existing PR. `None` fields are left untouched.
#[derive(Debug, Clone, Default)]
pub struct UpdatePr {
    pub pr_node_id: String,
    pub title: Option<String>,
    pub body: Option<String>,
    pub base_ref_name: Option<String>,
}

impl UpdatePr {
    /// True when no text field would actually change. Callers can skip the
    /// API round-trip for this case.
    #[must_use]
    pub fn is_noop(&self) -> bool {
        self.title.is_none() && self.body.is_none() && self.base_ref_name.is_none()
    }
}

/// One label on a PR, with both the human-readable name and the GraphQL
/// node ID required to mutate it via `removeLabelsFromLabelable`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Label {
    pub name: String,
    pub node_id: String,
}

/// Result of a successful [`Gh::create_pr`].
#[derive(Debug, Clone)]
pub struct PrCreated {
    pub number: u64,
    pub html_url: String,
    /// GraphQL node ID for the PR. Needed to enable auto-merge.
    pub node_id: String,
    /// True when the PR's base branch has a merge queue. See
    /// [`PrDetails::has_merge_queue`].
    pub has_merge_queue: bool,
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

    /// Resolve the base repo's GraphQL node ID and check that `branch` exists.
    /// Combines two preconditions for [`Gh::create_pr`] into a single round
    /// trip so the mutation has the repo ID it needs without a separate probe.
    ///
    /// # Errors
    ///
    /// Returns an error if the repo does not exist; propagates other API errors.
    async fn lookup_base(&self, owner: &str, repo: &str, branch: &str) -> Result<BaseLookup>;

    /// Create a pull request.
    ///
    /// # Errors
    ///
    /// Propagates API errors.
    async fn create_pr(&self, req: CreatePrRequest) -> Result<PrCreated>;

    /// Request reviews from users and/or teams on a PR; additive, does not
    /// remove existing review requests.
    ///
    /// # Errors
    ///
    /// Propagates API errors.
    async fn add_reviewers(
        &self,
        owner: &str,
        repo: &str,
        pr: u64,
        reviewers: Vec<Reviewer>,
    ) -> Result<()>;

    /// Remove review requests for the listed users and/or teams.
    ///
    /// # Errors
    ///
    /// Propagates API errors.
    async fn remove_reviewers(
        &self,
        owner: &str,
        repo: &str,
        pr: u64,
        reviewers: Vec<Reviewer>,
    ) -> Result<()>;

    /// Add labels to a PR; additive, does not remove any labels.
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

    /// Remove the given labels from a PR by their GraphQL node IDs.
    ///
    /// # Errors
    ///
    /// Propagates API errors.
    async fn remove_labels(&self, pr_node_id: &str, label_node_ids: &[String]) -> Result<()>;

    /// Update mutable text fields on a PR.
    ///
    /// # Errors
    ///
    /// Propagates API errors.
    async fn update_pr(&self, req: UpdatePr) -> Result<()>;

    /// Toggle a PR's draft state. `draft = true` converts to draft;
    /// `draft = false` marks ready for review.
    ///
    /// # Errors
    ///
    /// Propagates API errors.
    async fn set_draft(&self, pr_node_id: &str, draft: bool) -> Result<()>;

    /// Disable "merge when ready" on a PR. No-op on the server if auto-merge
    /// was not enabled.
    ///
    /// # Errors
    ///
    /// Propagates API errors.
    async fn disable_auto_merge(&self, pr_node_id: &str) -> Result<()>;

    /// Fetch full metadata for a PR by number.
    ///
    /// # Errors
    ///
    /// Returns a clear "not found" error on 404; propagates other API errors.
    async fn get_pr(&self, owner: &str, repo: &str, number: u64, body: bool) -> Result<PrDetails>;

    /// Enable "merge when ready" on a PR. Dispatches to either
    /// `enablePullRequestAutoMerge` or `enqueuePullRequest` based on
    /// `has_merge_queue` (callers get this from [`PrDetails::has_merge_queue`]
    /// or [`PrCreated::has_merge_queue`]). `method` is ignored when the queue
    /// path is taken — the queue's own config decides the merge method.
    ///
    /// # Errors
    ///
    /// Propagates API errors. Common failures: the repo does not have
    /// auto-merge enabled, required branch protections are missing, or the PR
    /// is already mergeable.
    async fn enable_auto_merge(
        &self,
        pr_node_id: &str,
        has_merge_queue: bool,
        method: AutoMergeMethod,
    ) -> Result<()>;

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
