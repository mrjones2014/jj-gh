use crate::config::AutoMergeMethod;

// required to satisfy GraphQL interfaces for the `PullRequest` type
type GitObjectID = String;
#[expect(clippy::upper_case_acronyms)]
type URI = String;

#[derive(graphql_client::GraphQLQuery)]
#[graphql(
    schema_path = "src/gh/github.graphql",
    query_path = "src/gh/queries/get_pr.gql"
)]
pub struct GetPrInternal;

pub use get_pr_internal::{
    GetPrInternalRepositoryPullRequest,
    GetPrInternalRepositoryPullRequestReviewRequestsNodesRequestedReviewer as RequestedReviewer,
    ResponseData as GetPrResponseData, Variables as GetPrVariables,
};

impl From<get_pr_internal::GetPrInternalRepositoryPullRequestAutoMergeRequest>
    for Option<AutoMergeMethod>
{
    fn from(value: get_pr_internal::GetPrInternalRepositoryPullRequestAutoMergeRequest) -> Self {
        Some(match value.merge_method {
            get_pr_internal::PullRequestMergeMethod::MERGE => AutoMergeMethod::Merge,
            get_pr_internal::PullRequestMergeMethod::REBASE => AutoMergeMethod::Rebase,
            get_pr_internal::PullRequestMergeMethod::SQUASH => AutoMergeMethod::Squash,
            get_pr_internal::PullRequestMergeMethod::Other(_) => return None,
        })
    }
}
