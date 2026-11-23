use crate::{config::AutoMergeMethod, gh::AutoMergeRequest};
use anyhow::Context;

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

fn merge_method(value: &get_pr_internal::PullRequestMergeMethod) -> Option<AutoMergeMethod> {
    Some(match value {
        get_pr_internal::PullRequestMergeMethod::MERGE => AutoMergeMethod::Merge,
        get_pr_internal::PullRequestMergeMethod::REBASE => AutoMergeMethod::Rebase,
        get_pr_internal::PullRequestMergeMethod::SQUASH => AutoMergeMethod::Squash,
        get_pr_internal::PullRequestMergeMethod::Other(_) => return None,
    })
}

// graphql_client generates separate response types for the two aliases.
macro_rules! convert_request {
    ($request:ty) => {
        impl TryFrom<$request> for AutoMergeRequest {
            type Error = anyhow::Error;

            fn try_from(value: $request) -> Result<Self, Self::Error> {
                Ok(Self {
                    pr_number: u64::try_from(value.pull_request.number)
                        .context("auto-merge request PR number out of range")?,
                    method: merge_method(&value.merge_method),
                })
            }
        }
    };
}

convert_request!(get_pr_internal::GetPrInternalRepositoryPullRequestAutoMergeRequest);
convert_request!(get_pr_internal::GetPrInternalRepositoryPullRequestStackAutoMergeRequest);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_individual_and_stack_auto_merge_requests() {
        let response: GetPrResponseData = serde_json::from_value(serde_json::json!({
            "repository": {
                "pullRequest": {
                    "id": "PR_2",
                    "isDraft": false,
                    "number": 2,
                    "title": "PR 2",
                    "url": "https://github.com/o/r/pull/2",
                    "headRefName": "pr-2",
                    "baseRefName": "pr-1",
                    "headRefOid": "head-sha",
                    "baseRefOid": "base-sha",
                    "body": "",
                    "mergeQueue": null,
                    "autoMergeRequest": {
                        "mergeMethod": "SQUASH",
                        "pullRequest": { "number": 1 }
                    },
                    "stackAutoMergeRequest": {
                        "mergeMethod": "MERGE",
                        "pullRequest": { "number": 3 }
                    },
                    "stack": { "number": 7, "baseRefName": "main" },
                    "stackEntry": { "position": 2 },
                    "reviewRequests": { "nodes": [] },
                    "headRepository": null,
                    "labels": { "nodes": [] }
                }
            }
        }))
        .expect("valid GraphQL response");

        let pr = response
            .repository
            .expect("repository")
            .pull_request
            .expect("pull request");
        let individual: AutoMergeRequest = pr
            .auto_merge_request
            .expect("individual request")
            .try_into()
            .expect("individual request conversion");
        let stack: AutoMergeRequest = pr
            .stack_auto_merge_request
            .expect("stack request")
            .try_into()
            .expect("stack request conversion");

        assert_eq!(
            individual,
            AutoMergeRequest {
                pr_number: 1,
                method: Some(AutoMergeMethod::Squash),
            }
        );
        assert_eq!(
            stack,
            AutoMergeRequest {
                pr_number: 3,
                method: Some(AutoMergeMethod::Merge),
            }
        );
        assert_eq!(pr.stack.expect("stack").base_ref_name, "main");
        assert_eq!(pr.stack_entry.expect("stack entry").position, 2);
    }
}
