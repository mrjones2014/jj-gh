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

pub use get_pr_internal::{ResponseData as GetPrResponseData, Variables as GetPrVariables};
