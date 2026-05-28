use crate::logging::ResultExt;
use anyhow::Context;

// required to satisfy GraphQL interfaces for the `PullRequest` type
type GitObjectID = String;
type DateTime = String;
#[expect(clippy::upper_case_acronyms)]
type URI = String;

#[derive(graphql_client::GraphQLQuery)]
#[graphql(
    schema_path = "src/gh/github.graphql",
    query_path = "src/gh/queries/prs_with_ci_status.gql"
)]
pub struct PrsWithCiStatusInternal;

pub use prs_with_ci_status_internal::{
    ResponseData as PrsWithCiStatusResponseData, Variables as PrsWithCiStatusVariables,
};

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

impl From<Option<prs_with_ci_status_internal::PrsWithCiStatusInternalSearchNodesOnPullRequestStatusCheckRollup>>
    for CiStatus
{
    fn from(
        value: Option<prs_with_ci_status_internal::PrsWithCiStatusInternalSearchNodesOnPullRequestStatusCheckRollup>,
    ) -> Self {
        use prs_with_ci_status_internal::StatusState;
        match value {
            None => Self::None,
            Some(rollup) => match rollup.state {
                StatusState::ERROR | StatusState::FAILURE => Self::Failed,
                StatusState::EXPECTED | StatusState::PENDING => Self::Pending,
                StatusState::SUCCESS => Self::Success,
                StatusState::Other(_) => Self::None,
            },
        }
    }
}

/// Open PR with CI status and head commit SHA. Used by `jj-gh pr log` to map
/// jj revisions to their PR metadata.
#[derive(Debug)]
#[expect(clippy::struct_excessive_bools)]
pub struct PrWithCiStatus {
    /// GraphQL node ID.
    pub id: String,
    /// PR number.
    pub number: u64,
    /// PR URL.
    pub url: String,
    /// PR title.
    pub title: String,
    /// Name of the branch the PR is opened from (the head ref). Used to map
    /// the PR onto the local bookmark with the same name, so the badge
    /// renders on the local commit even when it has diverged from the remote
    /// head SHA.
    pub head_ref_name: String,
    /// SHA of the commit at the head of the PR's branch on the remote. May
    /// not match the local bookmark target if the local bookmark has diverged
    /// from the remote PR head (e.g. if you rebase the PR on `trunk()` but have
    /// not pushed it to the remote yet).
    pub head_sha: String,
    /// Whether the PR is a draft.
    pub is_draft: bool,
    /// Whether the PR is merged
    pub merged: bool,
    /// Whether the PR is in the merge queue.
    pub is_in_merge_queue: bool,
    /// Rolled-up CI status across all required checks.
    pub ci_status: CiStatus,
    /// Whether auto-merge enabled for this PR
    pub auto_merge_enabled: bool,
}

impl From<prs_with_ci_status_internal::ResponseData> for Vec<PrWithCiStatus> {
    fn from(value: prs_with_ci_status_internal::ResponseData) -> Self {
        let Some(nodes) = value.search.nodes else {
            return vec![];
        };
        nodes
            .into_iter()
            .filter_map(|n| match n {
                Some(
                    prs_with_ci_status_internal::PrsWithCiStatusInternalSearchNodes::PullRequest(
                        pr,
                    ),
                ) => Some(pr),
                _ => None,
            })
            .map(
                |prs_with_ci_status_internal::PrsWithCiStatusInternalSearchNodesOnPullRequest {
                     id,
                     number,
                     url,
                     title,
                     head_ref_name,
                     head_ref_oid,
                     is_draft,
                     merged,
                     is_in_merge_queue,
                     status_check_rollup,
                     auto_merge_request,
                 }| PrWithCiStatus {
                    id,
                    // PR numbers will always fit in a u64, this is fine.
                    number: number
                        .try_into()
                        .context("Encountered invalid PR number")
                        .log_err()
                        .unwrap(),
                    url,
                    title,
                    merged,
                    is_draft,
                    is_in_merge_queue,
                    head_ref_name,
                    head_sha: head_ref_oid,
                    ci_status: status_check_rollup.into(),
                    auto_merge_enabled: auto_merge_request.is_some_and(|r| r.enabled_at.is_some()),
                },
            )
            .collect()
    }
}
