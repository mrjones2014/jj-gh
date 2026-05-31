#[derive(graphql_client::GraphQLQuery)]
#[graphql(
    schema_path = "src/gh/github.graphql",
    query_path = "src/gh/queries/remove_labels.gql"
)]
pub struct RemoveLabelsInternal;

pub use remove_labels_internal::{
    ResponseData as RemoveLabelsResponseData, Variables as RemoveLabelsVariables,
};
