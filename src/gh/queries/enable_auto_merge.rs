#[derive(graphql_client::GraphQLQuery)]
#[graphql(
    schema_path = "src/gh/github.graphql",
    query_path = "src/gh/queries/enable_auto_merge.gql"
)]
pub struct EnableAutoMergeInternal;

pub use enable_auto_merge_internal::{
    PullRequestMergeMethod, ResponseData as EnableAutoMergeResponseData,
    Variables as EnableAutoMergeVariables,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn include_stack_is_sent_as_a_graphql_variable() {
        for include_stack in [false, true] {
            let variables = EnableAutoMergeVariables {
                pr_id: "PR_node".into(),
                method: PullRequestMergeMethod::MERGE,
                include_stack,
            };
            let json = serde_json::to_value(variables).unwrap();
            assert_eq!(json["includeStack"], include_stack);
        }
    }
}
