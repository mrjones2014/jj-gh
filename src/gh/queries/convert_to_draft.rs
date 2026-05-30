#[derive(graphql_client::GraphQLQuery)]
#[graphql(
    schema_path = "src/gh/github.graphql",
    query_path = "src/gh/queries/convert_to_draft.gql"
)]
pub struct ConvertToDraftInternal;

pub use convert_to_draft_internal::{
    ResponseData as ConvertToDraftResponseData, Variables as ConvertToDraftVariables,
};
