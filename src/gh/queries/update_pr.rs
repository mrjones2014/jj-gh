#[derive(graphql_client::GraphQLQuery)]
#[graphql(
    schema_path = "src/gh/github.graphql",
    query_path = "src/gh/queries/update_pr.gql"
)]
pub struct UpdatePrInternal;

pub use update_pr_internal::{
    ResponseData as UpdatePrResponseData, Variables as UpdatePrVariables,
};
