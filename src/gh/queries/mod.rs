//! Generated GraphQL query bindings via `graphql_client`.

mod create_pr;
mod enable_auto_merge;
mod enqueue_pr;
mod find_open_pr;
mod get_pr;
mod lookup_base;
mod prs_with_ci_status;

pub use create_pr::{CreatePrInternal, CreatePrResponseData, CreatePrVariables};
pub use enable_auto_merge::{
    EnableAutoMergeInternal, EnableAutoMergeResponseData, EnableAutoMergeVariables,
    PullRequestMergeMethod,
};
pub use enqueue_pr::{EnqueuePrInternal, EnqueuePrResponseData, EnqueuePrVariables};
pub use find_open_pr::{
    FindOpenPrInternal, FindOpenPrResponseData, FindOpenPrVariables, PullRequestState,
};
pub use get_pr::{
    GetPrInternal, GetPrInternalRepositoryPullRequest, GetPrResponseData, GetPrVariables,
    RequestedReviewer,
};
pub use lookup_base::{LookupBaseInternal, LookupBaseResponseData, LookupBaseVariables};
pub use prs_with_ci_status::{
    CiStatus, PrWithCiStatus, PrsWithCiStatusInternal, PrsWithCiStatusResponseData,
    PrsWithCiStatusVariables,
};
