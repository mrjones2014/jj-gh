#[derive(graphql_client::GraphQLQuery)]
#[graphql(
    schema_path = "src/gh/github.graphql",
    query_path = "src/gh/enable_auto_merge.gql"
)]
pub struct EnableAutoMergeInternal;

pub use enable_auto_merge_internal::{
    PullRequestMergeMethod, ResponseData as EnableAutoMergeResponseData,
    Variables as EnableAutoMergeVariables,
};
