#[derive(graphql_client::GraphQLQuery)]
#[graphql(
    schema_path = "src/gh/github.graphql",
    query_path = "src/gh/queries/disable_auto_merge.gql"
)]
pub struct DisableAutoMergeInternal;

pub use disable_auto_merge_internal::{
    ResponseData as DisableAutoMergeResponseData, Variables as DisableAutoMergeVariables,
};
