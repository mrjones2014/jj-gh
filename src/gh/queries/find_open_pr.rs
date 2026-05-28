#[expect(clippy::upper_case_acronyms)]
type URI = String;

#[derive(graphql_client::GraphQLQuery)]
#[graphql(
    schema_path = "src/gh/github.graphql",
    query_path = "src/gh/queries/find_open_pr.gql"
)]
pub struct FindOpenPrInternal;

pub use find_open_pr_internal::{
    PullRequestState, ResponseData as FindOpenPrResponseData, Variables as FindOpenPrVariables,
};
