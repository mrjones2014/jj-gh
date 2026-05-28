#[derive(graphql_client::GraphQLQuery)]
#[graphql(
    schema_path = "src/gh/github.graphql",
    query_path = "src/gh/queries/enqueue_pr.gql"
)]
pub struct EnqueuePrInternal;

pub use enqueue_pr_internal::{
    ResponseData as EnqueuePrResponseData, Variables as EnqueuePrVariables,
};
