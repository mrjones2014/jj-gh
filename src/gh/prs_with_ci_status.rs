use serde::Deserialize;

#[derive(Deserialize)]
#[allow(non_camel_case_types, clippy::upper_case_acronyms)]
enum StatusState {
    ERROR,
    EXPECTED,
    FAILURE,
    PENDING,
    SUCCESS,
}

#[derive(Deserialize)]
struct StatusCheckRollup {
    state: StatusState,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PullRequest {
    id: String,
    number: u64,
    url: String,
    title: String,
    head_ref_oid: String,
    is_draft: bool,
    is_in_merge_queue: bool,
    status_check_rollup: Option<StatusCheckRollup>,
}

#[derive(Deserialize)]
struct SearchResults {
    nodes: Vec<PullRequest>,
}

#[derive(Deserialize)]
pub struct PrsWithCiStatusInternal {
    search: SearchResults,
}

/// Status of CI checks for a pull request.
#[derive(Debug, Clone, Copy)]
pub enum CiStatus {
    /// All checks succeeded.
    Success,
    /// At least one check failed.
    Failed,
    /// Checks still running or expected.
    Pending,
    /// No checks have been configured for the PR.
    None,
}

impl From<Option<StatusCheckRollup>> for CiStatus {
    fn from(value: Option<StatusCheckRollup>) -> Self {
        match value {
            None => Self::None,
            Some(rollup) => match rollup.state {
                StatusState::ERROR | StatusState::FAILURE => Self::Failed,
                StatusState::EXPECTED | StatusState::PENDING => Self::Pending,
                StatusState::SUCCESS => Self::Success,
            },
        }
    }
}

/// Open PR with CI status and head commit SHA. Used by `jj-gh pr log` to map
/// jj revisions to their PR metadata.
#[derive(Debug)]
pub struct PrWithCiStatus {
    /// GraphQL node ID.
    pub id: String,
    /// PR number.
    pub number: u64,
    /// PR URL.
    pub url: String,
    /// PR title.
    pub title: String,
    /// SHA of the commit at the head of the PR's branch. Matches a jj
    /// `commit_id` in colocated repos.
    pub head_sha: String,
    /// Whether the PR is a draft.
    pub is_draft: bool,
    /// Whether the PR is in the merge queue.
    pub is_in_merge_queue: bool,
    /// Rolled-up CI status across all required checks.
    pub ci_status: CiStatus,
}

impl From<PrsWithCiStatusInternal> for Vec<PrWithCiStatus> {
    fn from(value: PrsWithCiStatusInternal) -> Self {
        value
            .search
            .nodes
            .into_iter()
            .map(
                |PullRequest {
                     id,
                     number,
                     url,
                     title,
                     head_ref_oid,
                     is_draft,
                     is_in_merge_queue,
                     status_check_rollup,
                 }| PrWithCiStatus {
                    id,
                    number,
                    url,
                    title,
                    head_sha: head_ref_oid,
                    is_draft,
                    is_in_merge_queue,
                    ci_status: status_check_rollup.into(),
                },
            )
            .collect()
    }
}
