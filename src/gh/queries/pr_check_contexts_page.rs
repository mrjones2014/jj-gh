use super::CiCounts;

#[derive(graphql_client::GraphQLQuery)]
#[graphql(
    schema_path = "src/gh/github.graphql",
    query_path = "src/gh/queries/pr_check_contexts_page.gql"
)]
pub struct PrCheckContextsPageInternal;

pub use pr_check_contexts_page_internal::{
    ResponseData as PrCheckContextsPageResponseData, Variables as PrCheckContextsPageVariables,
};

/// Count the contexts in one follow-up page and return the next cursor when
/// more pages exist. Mirrors the first-page counting in
/// [`super::prs_with_ci_status`].
#[must_use]
pub fn count_contexts_page(data: PrCheckContextsPageResponseData) -> (CiCounts, Option<String>) {
    use pr_check_contexts_page_internal::{
        CheckConclusionState, CheckStatusState, PrCheckContextsPageInternalNode as Node,
        PrCheckContextsPageInternalNodeOnPullRequestStatusCheckRollupContextsNodes as CtxNode,
        StatusState,
    };
    let mut counts = CiCounts::default();
    let Some(Node::PullRequest(pr)) = data.node else {
        return (counts, None);
    };
    let Some(rollup) = pr.status_check_rollup else {
        return (counts, None);
    };
    if let Some(nodes) = rollup.contexts.nodes.as_ref() {
        for node in nodes.iter().flatten() {
            match node {
                CtxNode::CheckRun(check) => match check.status {
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
                CtxNode::StatusContext(status) => match status.state {
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
