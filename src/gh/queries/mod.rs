//! Generated GraphQL query bindings via `graphql_client`.

mod convert_to_draft;
mod create_pr;
mod disable_auto_merge;
mod enable_auto_merge;
mod find_open_pr;
mod get_pr;
mod lookup_base;
mod mark_ready_for_review;
mod prs_with_ci_status;
mod remove_labels;
mod update_pr;

pub use convert_to_draft::{
    ConvertToDraftInternal, ConvertToDraftResponseData, ConvertToDraftVariables,
};
pub use create_pr::{CreatePrInternal, CreatePrResponseData, CreatePrVariables};
pub use disable_auto_merge::{
    DisableAutoMergeInternal, DisableAutoMergeResponseData, DisableAutoMergeVariables,
};
pub use enable_auto_merge::{
    EnableAutoMergeInternal, EnableAutoMergeResponseData, EnableAutoMergeVariables,
    PullRequestMergeMethod,
};
pub use find_open_pr::{
    FindOpenPrInternal, FindOpenPrResponseData, FindOpenPrVariables, PullRequestState,
};
pub use get_pr::{
    GetPrInternal, GetPrInternalRepositoryPullRequest, GetPrResponseData, GetPrVariables,
    RequestedReviewer,
};
pub use lookup_base::{LookupBaseInternal, LookupBaseResponseData, LookupBaseVariables};
pub use mark_ready_for_review::{
    MarkReadyForReviewInternal, MarkReadyForReviewResponseData, MarkReadyForReviewVariables,
};
pub use prs_with_ci_status::{
    CiStatus, PrWithCiStatus, PrsWithCiStatusInternal, PrsWithCiStatusResponseData,
    PrsWithCiStatusVariables,
};
pub use remove_labels::{RemoveLabelsInternal, RemoveLabelsResponseData, RemoveLabelsVariables};
pub use update_pr::{UpdatePrInternal, UpdatePrResponseData, UpdatePrVariables};
