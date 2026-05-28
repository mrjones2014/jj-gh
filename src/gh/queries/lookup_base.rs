#[derive(graphql_client::GraphQLQuery)]
#[graphql(
    schema_path = "src/gh/github.graphql",
    query_path = "src/gh/queries/lookup_base.gql"
)]
pub struct LookupBaseInternal;

pub use lookup_base_internal::{
    ResponseData as LookupBaseResponseData, Variables as LookupBaseVariables,
};
