//! GitHub API abstraction.
//!
//! All write-side calls go through [`Gh`]. The production impl wraps `octocrab`;
//! tests use a fake.

use anyhow::Result;

pub mod real;
pub mod remote;

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
}

pub trait Gh {
    /// First open PR whose `head` matches `head_spec` (`branch` or `owner:branch`).
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
}
