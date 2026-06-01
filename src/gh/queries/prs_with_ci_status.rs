use crate::logging::ResultExt;
use anyhow::Context;
use std::ops::AddAssign;

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

/// Per-bucket check counts for a PR's status check rollup. Computed from the
/// individual `CheckRun` / `StatusContext` entries inside `statusCheckRollup`,
/// since the API does not expose them aggregated.
#[derive(Debug, Clone, Copy, Default)]
pub struct CiCounts {
    /// Checks that are queued, in progress, expected, or otherwise not yet
    /// concluded.
    pub pending: u32,
    /// Checks that finished with a non-success conclusion (failure, error,
    /// cancelled, timed out, action required, startup failure).
    pub failed: u32,
    /// Checks that finished with a success-equivalent conclusion (success,
    /// neutral, skipped).
    pub passed: u32,
}

impl CiCounts {
    /// Total number of checks in the rollup. Zero means there are no contexts
    /// (no CI configured).
    #[must_use]
    pub fn total(self) -> u32 {
        self.pending + self.failed + self.passed
    }
}

impl AddAssign for CiCounts {
    fn add_assign(&mut self, rhs: Self) {
        self.pending += rhs.pending;
        self.failed += rhs.failed;
        self.passed += rhs.passed;
    }
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

fn count_first_page(
    rollup: &prs_with_ci_status_internal::PrsWithCiStatusInternalSearchNodesOnPullRequestStatusCheckRollup,
) -> (CiCounts, Option<String>) {
    use prs_with_ci_status_internal::{
        CheckConclusionState, CheckStatusState,
        PrsWithCiStatusInternalSearchNodesOnPullRequestStatusCheckRollupContextsNodes as Node,
        StatusState,
    };
    let mut counts = CiCounts::default();
    if let Some(nodes) = rollup.contexts.nodes.as_ref() {
        for node in nodes.iter().flatten() {
            match node {
                Node::CheckRun(check) => match check.status {
                    CheckStatusState::COMPLETED => match &check.conclusion {
                        Some(
                            CheckConclusionState::SUCCESS
                            | CheckConclusionState::NEUTRAL
                            | CheckConclusionState::SKIPPED,
                        ) => counts.passed += 1,
                        Some(_) => counts.failed += 1,
                        None => counts.pending += 1,
                    },
                    _ => counts.pending += 1,
                },
                Node::StatusContext(status) => match status.state {
                    StatusState::SUCCESS => counts.passed += 1,
                    StatusState::ERROR | StatusState::FAILURE => counts.failed += 1,
                    StatusState::EXPECTED | StatusState::PENDING => counts.pending += 1,
                    StatusState::Other(_) => {}
                },
            }
        }
    }
    let next_after = rollup
        .contexts
        .page_info
        .has_next_page
        .then(|| rollup.contexts.page_info.end_cursor.clone())
        .flatten();
    (counts, next_after)
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
    /// Per-bucket counts of individual check runs and status contexts.
    /// Empty (all zero) when there are no contexts.
    pub ci_counts: CiCounts,
    /// Whether auto-merge enabled for this PR
    pub auto_merge_enabled: bool,
}

/// One PR's first page of results, plus a cursor for additional context pages
/// when the PR has more than 100 checks (GitHub's max `first:` value).
/// Callers are expected to follow the cursor and accumulate the extra counts
/// onto [`PrWithCiStatus::ci_counts`].
#[derive(Debug)]
pub struct PrPartial {
    pub pr: PrWithCiStatus,
    /// `Some(cursor)` when more context pages exist; pass it to the
    /// `pr_check_contexts_page` query as `after:`. `None` when all contexts
    /// fit in the first page.
    pub next_after: Option<String>,
}

/// Extract per-PR partial state from the search response. Use this instead of
/// converting straight into `Vec<PrWithCiStatus>` so the caller can follow
/// any context-page cursors.
#[must_use]
pub fn extract_pr_partials(data: prs_with_ci_status_internal::ResponseData) -> Vec<PrPartial> {
    let Some(nodes) = data.search.nodes else {
        return vec![];
    };
    nodes
        .into_iter()
        .filter_map(|n| match n {
            Some(prs_with_ci_status_internal::PrsWithCiStatusInternalSearchNodes::PullRequest(
                pr,
            )) => Some(pr),
            _ => None,
        })
        .map(
            |prs_with_ci_status_internal::PrsWithCiStatusInternalSearchNodesOnPullRequest {
                 id,
                 number,
                 url,
                 title: _,
                 head_ref_name,
                 head_ref_oid,
                 is_draft,
                 merged,
                 is_in_merge_queue,
                 status_check_rollup,
                 auto_merge_request,
             }| {
                let (ci_counts, next_after) = status_check_rollup
                    .as_ref()
                    .map(count_first_page)
                    .unwrap_or_default();
                let pr = PrWithCiStatus {
                    id,
                    // PR numbers will always fit in a u64, this is fine.
                    number: number
                        .try_into()
                        .context("Encountered invalid PR number")
                        .log_err()
                        .unwrap(),
                    url,
                    is_draft,
                    merged,
                    is_in_merge_queue,
                    head_ref_name,
                    head_sha: head_ref_oid,
                    ci_status: status_check_rollup.into(),
                    ci_counts,
                    auto_merge_enabled: auto_merge_request.is_some_and(|r| r.enabled_at.is_some()),
                };
                PrPartial { pr, next_after }
            },
        )
        .collect()
}
