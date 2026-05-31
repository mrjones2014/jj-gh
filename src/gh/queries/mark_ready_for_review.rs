#[derive(graphql_client::GraphQLQuery)]
#[graphql(
    schema_path = "src/gh/github.graphql",
    query_path = "src/gh/queries/mark_ready_for_review.gql"
)]
pub struct MarkReadyForReviewInternal;

pub use mark_ready_for_review_internal::{
    ResponseData as MarkReadyForReviewResponseData, Variables as MarkReadyForReviewVariables,
};
