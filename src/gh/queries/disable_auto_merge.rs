#[derive(graphql_client::GraphQLQuery)]
#[graphql(
    schema_path = "src/gh/github.graphql",
    query_path = "src/gh/queries/disable_auto_merge.gql"
)]
pub struct DisableAutoMergeInternal;

pub use disable_auto_merge_internal::{
    ResponseData as DisableAutoMergeResponseData, Variables as DisableAutoMergeVariables,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn include_stack_is_sent_as_a_graphql_variable() {
        for include_stack in [false, true] {
            let variables = DisableAutoMergeVariables {
                pr_id: "PR_node".into(),
                include_stack,
            };
            let json = serde_json::to_value(variables).unwrap();
            assert_eq!(json["includeStack"], include_stack);
        }
    }
}
