// required to satisfy GraphQL interfaces for the `PullRequest` type
#[expect(clippy::upper_case_acronyms)]
type URI = String;

#[derive(graphql_client::GraphQLQuery)]
#[graphql(
    schema_path = "src/gh/github.graphql",
    query_path = "src/gh/queries/create_pr.gql"
)]
pub struct CreatePrInternal;

pub use create_pr_internal::{
    ResponseData as CreatePrResponseData, Variables as CreatePrVariables,
};
