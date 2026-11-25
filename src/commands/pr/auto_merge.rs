use crate::{
    cli::GlobalOpts,
    config::{AutoMergeMethod, AutoMergeMode},
    gh::{
        AutoMergeRequest, Gh, PrDetails, pr_lookup::MissingPr, remote::Target,
        stack_create::is_open_unmerged,
    },
    model::Model,
    util::EvalWithCfgFallback,
};
use anyhow::{Context, Result, bail};
use jj_gh_config_derive::subcommand_args;
use std::collections::{HashMap, HashSet};

/// The subset of [`PrDetails`] needed to move an auto-merge request. Owned so
/// the editor flow can build it for a freshly created PR as well as for a PR
/// fetched from GitHub.
#[derive(Debug, Clone)]
pub(crate) struct AutoMergeTarget {
    pub number: u64,
    pub graphql_node_id: String,
    pub html_url: String,
    pub base_has_merge_queue: bool,
    pub stack_number: Option<u64>,
    pub auto_merge_request: Option<AutoMergeRequest>,
    pub stack_auto_merge_request: Option<AutoMergeRequest>,
}

impl From<&PrDetails> for AutoMergeTarget {
    fn from(pr: &PrDetails) -> Self {
        Self {
            number: pr.number,
            graphql_node_id: pr.graphql_node_id.clone(),
            html_url: pr.html_url.clone(),
            base_has_merge_queue: pr.base_has_merge_queue,
            stack_number: pr.stack_number,
            auto_merge_request: pr.auto_merge_request.clone(),
            stack_auto_merge_request: pr.stack_auto_merge_request.clone(),
        }
    }
}

impl AutoMergeTarget {
    /// A newly created PR has no requests and no stack links yet.
    pub(crate) fn created(
        number: u64,
        graphql_node_id: &str,
        html_url: &str,
        base_has_merge_queue: bool,
    ) -> Self {
        Self {
            number,
            graphql_node_id: graphql_node_id.to_string(),
            html_url: html_url.to_string(),
            base_has_merge_queue,
            stack_number: None,
            auto_merge_request: None,
            stack_auto_merge_request: None,
        }
    }
}

/// How a PR's current auto-merge state is represented in the editor.
pub(crate) struct AutoMergeView {
    pub value: Option<AutoMergeMode>,
    pub comment: Option<String>,
    pub method: AutoMergeMethod,
}

/// Auto-merge intent captured from a created PR's editor frontmatter. Held
/// until stack linking finishes so one whole-stack request can be made through
/// the right endpoint instead of one request per created PR.
#[derive(Debug, Clone)]
pub(crate) struct CreatedAutoMerge {
    pub target: AutoMergeTarget,
    pub mode: AutoMergeMode,
    pub method: AutoMergeMethod,
}

impl CreatedAutoMerge {
    pub(crate) fn new(
        number: u64,
        node_id: &str,
        html_url: &str,
        base_has_merge_queue: bool,
        mode: AutoMergeMode,
        method: AutoMergeMethod,
    ) -> Self {
        Self {
            target: AutoMergeTarget::created(number, node_id, html_url, base_has_merge_queue),
            mode,
            method,
        }
    }
}

subcommand_args! {
    pub struct AutoMergeArgs {
        /// PR numbers and/or jj revsets to look up PRs from. A numeric
        /// argument is treated as a PR number; anything else is resolved as a
        /// revset, which may match multiple commits (e.g. a whole stack via
        /// `qv::uw`).
        #[arg(value_name = "PR_NUM|REVSET", required = true)]
        pub revsets: Vec<String>,

        /// Move the covering auto-merge request to the top of each target
        /// PR's GitHub stack.
        #[arg(long, conflicts_with = "cancel")]
        pub stack: bool,

        /// Cancel the auto-merge request covering each target PR. Does nothing
        /// for a PR with no covering request.
        #[arg(long, conflicts_with = "stack")]
        pub cancel: bool,

        /// Merge method used when enabling auto-merge. Overrides config
        /// `auto_merge_method` (default `merge`). When moving a request with
        /// `--stack`, the existing method is kept unless this flag is passed.
        #[arg(long = "method", short = 'm', value_name = "METHOD", value_enum)]
        pub auto_merge_method: Option<AutoMergeMethod>,

        /// Configured merge method, used when `--method` is absent.
        #[config(maps_to = "auto_merge_method")]
        pub configured_auto_merge_method: AutoMergeMethod,
    }
}

pub async fn run(model: &impl Model, args: &AutoMergeArgs) -> Result<()> {
    let gh = model.gh().await?;
    let AutoMergeArgs {
        revsets,
        stack,
        cancel,
        auto_merge_method,
        configured_auto_merge_method,
        globals:
            GlobalOpts {
                remote,
                upstream_remote,
                verbose: _,
                quiet: _,
                log_level: _,
                gh_askpass: _,
                askpass_timeout_secs: _,
            },
    } = args;

    let requested_method =
        EvalWithCfgFallback::new(*auto_merge_method, Some(*configured_auto_merge_method));
    let upstream_remote = crate::gh::remote::resolved_upstream_remote(upstream_remote);
    let (target, prs) = model
        .resolve_pr_targets(remote, upstream_remote, revsets, MissingPr::Skip)
        .await?;
    if prs.is_empty() {
        bail!("no open PRs found for the given targets");
    }

    if *stack || *cancel {
        let action = if *cancel {
            "cancel auto-merge on"
        } else {
            "move auto-merge to the stack tip on"
        };
        let multi = prs.len() > 1;
        let mut errors = Vec::<anyhow::Error>::new();
        for pr in &prs {
            let view = AutoMergeTarget::from(pr);
            let result = if *cancel {
                cancel_covering(gh, &target, &view).await
            } else {
                enable_stack(gh, &target, &view, &requested_method).await
            }
            .with_context(|| format!("PR #{}", pr.number));
            let _ = result
                .inspect_err(|e| {
                    if multi {
                        log::warn!("{e:#}");
                    }
                })
                .map_err(|e| errors.push(e));
        }
        if let Some(error) = errors.pop() {
            if !multi {
                return Err(error);
            }
            bail!(
                "failed to {action} {} of {} PR(s)",
                errors.len() + 1,
                prs.len()
            );
        }
        return Ok(());
    }

    let method = requested_method.or_fallback().copied().unwrap_or_default();
    let multi = prs.len() > 1;
    let mut errors = Vec::<anyhow::Error>::new();
    for pr in &prs {
        let view = AutoMergeTarget::from(pr);
        let result = enable_auto_merge(gh, &view, method)
            .await
            .with_context(|| format!("PR #{}", pr.number));
        // Recoverable mid-loop: the remaining PRs are still tried and the
        // summary error below makes the command exit non-zero.
        let _ = result
            .inspect_err(|e| {
                if multi {
                    log::warn!("{e:#}");
                }
            })
            .map_err(|e| errors.push(e));
    }

    if let Some(error) = errors.pop() {
        if !multi {
            return Err(error);
        }
        bail!(
            "failed to enable auto-merge on {} of {} PR(s)",
            errors.len() + 1,
            prs.len()
        );
    }
    Ok(())
}

async fn enable_auto_merge(
    gh: &impl Gh,
    pr: &AutoMergeTarget,
    auto_merge_method: AutoMergeMethod,
) -> Result<()> {
    let _status = crate::ui::status::push(format!("Enabling auto-merge on #{}", pr.number));
    gh.enable_auto_merge(&pr.graphql_node_id, auto_merge_method, false)
        .await
        .inspect_err(|_| warn_if_merge_queue(pr.base_has_merge_queue))
        .with_context(|| format!("enabling auto-merge on #{}", pr.number))?;

    log::info!("Enabled auto-merge for PR #{}", pr.number);
    crate::ui::print_url(&pr.html_url);
    Ok(())
}

/// A merge queue on the base branch is a possible cause of a GitHub rejection;
/// the public API has no equivalent of the web UI's queue auto-merge. Keep the
/// original error as the failure and add this only as a hint.
fn warn_if_merge_queue(base_has_merge_queue: bool) {
    if base_has_merge_queue {
        log::warn!(
            "the base branch has a merge queue; GitHub's API may reject auto-merge in this configuration (https://github.com/mrjones2014/jj-gh/issues/103)"
        );
    }
}

/// The request currently covering `pr`: a stack request takes precedence over
/// an individual one. The bool is GitHub's `includeStack` flag for that request.
pub(crate) fn covering_request(pr: &AutoMergeTarget) -> Option<(AutoMergeRequest, bool)> {
    pr.stack_auto_merge_request
        .clone()
        .map(|request| (request, true))
        .or_else(|| {
            pr.auto_merge_request
                .clone()
                .map(|request| (request, false))
        })
}

/// Prefer an explicit `--method`, then the existing request's method, then the
/// configured default. This keeps a stack move from silently changing methods.
fn method_for(
    existing: Option<AutoMergeMethod>,
    requested: &EvalWithCfgFallback<AutoMergeMethod>,
) -> AutoMergeMethod {
    requested
        .cli()
        .copied()
        .or(existing)
        .or_else(|| requested.fallback().copied())
        .unwrap_or_default()
}

async fn endpoint_details(
    gh: &impl Gh,
    target: &Target,
    pr: &AutoMergeTarget,
    request: &AutoMergeRequest,
) -> Result<AutoMergeTarget> {
    if request.pr_number == pr.number {
        return Ok(pr.clone());
    }
    let details = gh
        .get_pr(&target.owner, &target.repo, request.pr_number)
        .await
        .with_context(|| {
            format!(
                "fetching PR #{} that owns the auto-merge request",
                request.pr_number
            )
        })?;
    Ok(AutoMergeTarget::from(&details))
}

async fn cancel_covering(gh: &impl Gh, target: &Target, pr: &AutoMergeTarget) -> Result<()> {
    let Some((request, include_stack)) = covering_request(pr) else {
        log::info!("PR #{} has no covering auto-merge request", pr.number);
        return Ok(());
    };
    let endpoint = endpoint_details(gh, target, pr, &request).await?;

    let _status =
        crate::ui::status::push(format!("Cancelling auto-merge covering PR #{}", pr.number));
    gh.disable_auto_merge(&endpoint.graphql_node_id, include_stack)
        .await
        .with_context(|| format!("cancelling auto-merge through PR #{}", request.pr_number))?;

    log::info!(
        "Cancelled auto-merge covering PR #{} (request through PR #{})",
        pr.number,
        request.pr_number
    );
    Ok(())
}

async fn enable_stack(
    gh: &impl Gh,
    target: &Target,
    pr: &AutoMergeTarget,
    method: &EvalWithCfgFallback<AutoMergeMethod>,
) -> Result<()> {
    let Some(stack_number) = pr.stack_number else {
        bail!("PR #{} is not part of a GitHub stack", pr.number);
    };
    let stack = gh
        .get_stack(&target.owner, &target.repo, stack_number)
        .await
        .with_context(|| format!("fetching stack #{stack_number}"))?;
    let top = stack
        .pull_requests
        .iter()
        .rev()
        .find(|member| is_open_unmerged(member))
        .with_context(|| format!("stack #{stack_number} has no open PR to merge through"))?;

    let covering = covering_request(pr);
    let existing_method = covering.as_ref().and_then(|(request, _)| request.method);
    let explicit_method = method.cli().copied();
    if covering.as_ref().is_some_and(|(request, include_stack)| {
        *include_stack
            && request.pr_number == top.number
            && explicit_method.is_none_or(|m| Some(m) == existing_method)
    }) {
        log::info!(
            "PR #{} is already covered by the whole-stack request through PR #{}",
            pr.number,
            top.number
        );
        return Ok(());
    }

    let top_details = if top.number == pr.number {
        pr.clone()
    } else {
        let details = gh
            .get_pr(&target.owner, &target.repo, top.number)
            .await
            .with_context(|| format!("fetching stack tip PR #{}", top.number))?;
        AutoMergeTarget::from(&details)
    };
    if let Some((request, include_stack)) = covering.as_ref() {
        let endpoint = endpoint_details(gh, target, pr, request).await?;
        gh.disable_auto_merge(&endpoint.graphql_node_id, *include_stack)
            .await
            .with_context(|| format!("cancelling auto-merge through PR #{}", request.pr_number))?;
    }

    let method = method_for(existing_method, method);
    let _status = crate::ui::status::push(format!(
        "Enabling whole-stack auto-merge through PR #{}",
        top.number
    ));
    gh.enable_auto_merge(&top_details.graphql_node_id, method, true)
        .await
        .inspect_err(|_| warn_if_merge_queue(top_details.base_has_merge_queue))
        .with_context(|| format!("enabling whole-stack auto-merge through PR #{}", top.number))?;

    log::info!(
        "Enabled whole-stack auto-merge for stack #{stack_number} through PR #{}",
        top.number
    );
    crate::ui::print_url(&top_details.html_url);
    Ok(())
}

/// The topmost open, unmerged member of the PR's GitHub stack, if any.
pub(crate) async fn stack_tip_number(
    gh: &impl Gh,
    target: &Target,
    pr: &PrDetails,
) -> Result<Option<u64>> {
    let Some(stack_number) = pr.stack_number else {
        return Ok(None);
    };
    let stack = gh
        .get_stack(&target.owner, &target.repo, stack_number)
        .await
        .with_context(|| format!("fetching stack #{stack_number}"))?;
    Ok(stack
        .pull_requests
        .iter()
        .rev()
        .find(|member| is_open_unmerged(member))
        .map(|member| member.number))
}

/// Render a PR's current request as one of the editor's four values. A request
/// through another PR that is not the stack tip cannot be expressed here; the
/// returned comment names its endpoint instead.
pub(crate) fn current_view(pr: &PrDetails, tip_number: Option<u64>) -> AutoMergeView {
    let Some((request, include_stack)) = covering_request(&AutoMergeTarget::from(pr)) else {
        return AutoMergeView {
            value: Some(AutoMergeMode::False),
            comment: None,
            method: AutoMergeMethod::default(),
        };
    };
    let method = request.method.unwrap_or_default();

    let whole_stack = include_stack && tip_number.is_some_and(|tip| tip == request.pr_number);
    let value = if whole_stack {
        AutoMergeMode::All
    } else if request.pr_number == pr.number {
        AutoMergeMode::Below
    } else {
        return AutoMergeView {
            value: None,
            comment: Some(format!(
                "auto-merge is requested through PR #{}; edit that PR to change it",
                request.pr_number
            )),
            method,
        };
    };

    AutoMergeView {
        value: Some(value),
        comment: None,
        method,
    }
}

/// Inputs to [`apply_editor_change`] supplied by the caller.
pub(crate) struct AutoMergeEditorChange<'a> {
    pub target: &'a Target,
    pub pr: AutoMergeTarget,
    pub before: Option<AutoMergeMode>,
    pub after: Option<AutoMergeMode>,
    pub method: AutoMergeMethod,
    pub method_explicit: bool,
}

/// Apply the auto-merge portion of an editor frontmatter diff. An unchanged
/// value (and unchanged method) never touches GitHub, which matters for
/// whole-stack requests: re-enabling them needlessly would cancel and recreate
/// the covering request.
pub(crate) async fn apply_editor_change(
    gh: &impl Gh,
    change: AutoMergeEditorChange<'_>,
) -> Result<()> {
    let AutoMergeEditorChange {
        target,
        pr,
        before,
        after,
        method,
        method_explicit,
    } = change;

    if before == after && !method_explicit {
        return Ok(());
    }
    let Some(desired) = after else {
        log::info!(
            "PR #{} has an auto-merge request this editor cannot represent; leaving it unchanged",
            pr.number
        );
        return Ok(());
    };

    let requested = EvalWithCfgFallback::new(method_explicit.then_some(method), Some(method));
    match desired {
        AutoMergeMode::False => cancel_covering(gh, target, &pr).await,
        AutoMergeMode::Below => enable_below(gh, target, &pr, &requested).await,
        AutoMergeMode::Infer | AutoMergeMode::All => {
            if pr.stack_number.is_some() {
                enable_stack(gh, target, &pr, &requested).await
            } else {
                log::info!(
                    "PR #{} is not part of a GitHub stack; enabling a single-PR request",
                    pr.number
                );
                enable_below(gh, target, &pr, &requested).await
            }
        }
    }
}

/// Enable auto-merge for created PRs after stack linking.
///
/// `linked` carries post-link PR details when the created PRs were linked into
/// GitHub stacks; `None` means they are standalone. Only one request is made
/// per stack, through the highest requesting endpoint. PRs that requested
/// `false` do not initiate a request and are never used as the endpoint of one,
/// but they do not veto requests from other PRs. Conflicting merge methods
/// abort before any request is made.
pub(crate) async fn apply_created_auto_merge(
    gh: &impl Gh,
    target: &Target,
    requests: &[CreatedAutoMerge],
    linked: Option<&[PrDetails]>,
) -> Result<()> {
    let opted_out = requests
        .iter()
        .filter(|request| request.mode == AutoMergeMode::False)
        .map(|request| request.target.number)
        .collect::<HashSet<u64>>();
    let mut active = requests
        .iter()
        .filter(|request| request.mode != AutoMergeMode::False)
        .cloned()
        .collect::<Vec<_>>();
    if active.is_empty() {
        return Ok(());
    }

    let method = active[0].method;
    if let Some(other) = active.iter().find(|request| request.method != method) {
        bail!(
            "created PRs request conflicting auto-merge methods ({method:?} and {:?}); use `jj-gh pr auto-merge` to set one method before enabling auto-merge",
            other.method
        );
    }
    let requested = EvalWithCfgFallback::new(Some(method), Some(method));

    if let Some(details) = linked {
        let stacks = details
            .iter()
            .map(|pr| (pr.number, pr.stack_number))
            .collect::<HashMap<u64, Option<u64>>>();
        for request in &mut active {
            if let Some(stack_number) = stacks.get(&request.target.number) {
                request.target.stack_number = *stack_number;
            }
        }
    }

    // One request per stack, through the highest requesting endpoint. Created
    // PRs have no covering request to cancel, so this is additive.
    let mut groups = Vec::<(Option<u64>, Vec<CreatedAutoMerge>)>::new();
    for request in active {
        match groups
            .iter_mut()
            .find(|(stack, _)| *stack == request.target.stack_number)
        {
            Some((_, members)) => members.push(request),
            None => groups.push((request.target.stack_number, vec![request])),
        }
    }

    for (stack_number, members) in groups {
        let Some(stack_number) = stack_number else {
            // Standalone PRs each need their own request.
            for request in &members {
                enable_below(gh, target, &request.target, &requested).await?;
            }
            continue;
        };

        let whole_stack = members
            .iter()
            .filter(|request| matches!(request.mode, AutoMergeMode::All | AutoMergeMode::Infer))
            .max_by_key(|request| request.target.number);
        let highest = members
            .iter()
            .max_by_key(|r| r.target.number)
            .expect("group is non-empty");

        // Only a whole-stack request needs the tip: `enable_stack` always goes
        // through it. If that tip explicitly requested `false`, honoring the
        // whole-stack request would override its opt-out, so fall back to
        // `enable_below` through the highest PR that did ask for it.
        let through_tip = if whole_stack.is_some() {
            let tip = gh
                .get_stack(&target.owner, &target.repo, stack_number)
                .await?
                .pull_requests
                .iter()
                .rev()
                .find(|member| is_open_unmerged(member))
                .map(|member| member.number);
            if tip.is_some_and(|tip| opted_out.contains(&tip)) {
                log::info!(
                    "stack tip PR #{} requested `false`; enabling auto-merge through PR #{} instead",
                    tip.expect("checked above"),
                    highest.target.number
                );
                false
            } else {
                true
            }
        } else {
            false
        };

        let endpoint = if through_tip {
            &whole_stack.expect("checked above").target
        } else {
            &highest.target
        };
        if through_tip {
            enable_stack(gh, target, endpoint, &requested).await?;
        } else {
            enable_below(gh, target, endpoint, &requested).await?;
        }
    }

    Ok(())
}

async fn enable_below(
    gh: &impl Gh,
    target: &Target,
    pr: &AutoMergeTarget,
    method: &EvalWithCfgFallback<AutoMergeMethod>,
) -> Result<()> {
    let covering = covering_request(pr);
    let existing_method = covering.as_ref().and_then(|(request, _)| request.method);
    let explicit_method = method.cli().copied();
    if covering.as_ref().is_some_and(|(request, include_stack)| {
        !include_stack
            && request.pr_number == pr.number
            && explicit_method.is_none_or(|m| Some(m) == existing_method)
    }) {
        log::info!(
            "PR #{} is already covered by an individual auto-merge request",
            pr.number
        );
        return Ok(());
    }

    if let Some((request, include_stack)) = covering.as_ref() {
        let endpoint = endpoint_details(gh, target, pr, request).await?;
        gh.disable_auto_merge(&endpoint.graphql_node_id, *include_stack)
            .await
            .with_context(|| format!("cancelling auto-merge through PR #{}", request.pr_number))?;
    }

    enable_auto_merge(gh, pr, method_for(existing_method, method)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        gh::{
            AutoMergeRequest, BaseLookup, CreatePrRequest, PrCreated, PrDetails, PrSummary,
            PrWithCiStatus, PullRequestStack, StackPullRequest, UpdatePr, WorkflowJob, WorkflowRun,
            pr_lookup, remote,
        },
        jj::{CommitInfo, PushedBookmark},
        model::TestModel,
    };
    use anyhow::Result;
    use std::collections::{HashMap, HashSet};
    use std::path::Path;
    use std::sync::Mutex;

    fn pr_details(number: u64, base_has_merge_queue: bool) -> PrDetails {
        PrDetails {
            is_draft: false,
            auto_merge_request: None,
            stack_auto_merge_request: None,
            number,
            title: "t".into(),
            html_url: format!("https://github.com/o/r/pull/{number}"),
            head_ref: "feat".into(),
            base_ref: "main".into(),
            head_sha: format!("sha{number}"),
            base_sha: "base".into(),
            head_user_login: Some("o".into()),
            head_repo_name: Some("r".into()),
            graphql_node_id: format!("node-{number}"),
            base_has_merge_queue,
            labels: vec![],
            reviewers: vec![],
            body: String::new(),
            stack_number: None,
            stack_base_ref: None,
            stack_position: None,
        }
    }

    fn stacked_pr(number: u64, stack_number: u64) -> PrDetails {
        PrDetails {
            stack_number: Some(stack_number),
            ..pr_details(number, false)
        }
    }

    fn with_requests(
        mut pr: PrDetails,
        individual: Option<AutoMergeRequest>,
        stack: Option<AutoMergeRequest>,
    ) -> PrDetails {
        pr.auto_merge_request = individual;
        pr.stack_auto_merge_request = stack;
        pr
    }

    fn request(number: u64, method: AutoMergeMethod) -> AutoMergeRequest {
        AutoMergeRequest {
            pr_number: number,
            method: Some(method),
        }
    }

    fn stack(number: u64, members: &[u64]) -> PullRequestStack {
        PullRequestStack {
            number,
            pull_requests: members
                .iter()
                .map(|member| StackPullRequest {
                    number: *member,
                    state: "OPEN".into(),
                    merged_at: None,
                })
                .collect(),
        }
    }

    fn commit(change_id: &str, bookmarks: &[&str]) -> CommitInfo {
        CommitInfo {
            change_id: change_id.into(),
            commit_id: format!("{change_id}-commit"),
            description: "d".into(),
            bookmarks: bookmarks.iter().map(|b| (*b).into()).collect(),
        }
    }

    #[derive(Default)]
    struct FakeJj {
        revset_commits: Vec<CommitInfo>,
    }

    impl FakeJj {
        fn with_revset_commits(commits: Vec<CommitInfo>) -> Self {
            Self {
                revset_commits: commits,
            }
        }
    }

    impl crate::jj::Jj for FakeJj {
        fn default_remote(&self) -> Result<Option<String>> {
            Ok(Some("origin".into()))
        }
        fn remote_names(&self) -> Result<Vec<String>> {
            Ok(vec!["origin".into()])
        }
        async fn resolve_rev(&self, _rev: &str) -> Result<CommitInfo> {
            unimplemented!()
        }
        fn resolve_revset(&self, _revset: &str) -> impl Future<Output = Result<Vec<CommitInfo>>> {
            std::future::ready(Ok(self.revset_commits.clone()))
        }
        async fn stacked_ancestor_bookmark(&self, _rev: &str) -> Result<Option<String>> {
            unimplemented!()
        }
        async fn first_commit_description(&self, _revset: &str) -> Result<String> {
            unimplemented!()
        }
        fn remote_url(&self, name: &str) -> Result<Option<String>> {
            if name == "origin" {
                Ok(Some("git@github.com:o/r.git".into()))
            } else {
                Ok(None)
            }
        }
        async fn remote_bookmark_sha(&self, _: &str, _: &str) -> Result<Option<String>> {
            unimplemented!()
        }
        async fn push(
            &self,
            _rev: &str,
            _bookmark: Option<&str>,
            _remote: String,
            _allow_private: bool,
        ) -> Result<()> {
            unimplemented!()
        }
        async fn private_commits_revset(&self) -> Result<Option<String>> {
            unimplemented!()
        }
        async fn trunk_branch(&self) -> Result<Option<String>> {
            unimplemented!()
        }
        async fn git_import(&self) -> Result<()> {
            unimplemented!()
        }
        async fn pushed_bookmarks(&self, _remote: &str) -> Result<Vec<PushedBookmark>> {
            unimplemented!()
        }
        async fn eval_template(
            &self,
            _revset: &str,
            _template: &str,
            _config_file: Option<&Path>,
            _reversed: bool,
            _color: bool,
        ) -> Result<String> {
            unimplemented!()
        }
        async fn diff(&self, _revset: &str) -> Result<String> {
            unimplemented!()
        }
        async fn pr_diff(&self, _: &str, _: &str) -> Result<String> {
            unimplemented!()
        }
        async fn push_bookmarks(&self, _: &[String], _: String, _: bool) -> Result<()> {
            unimplemented!()
        }
    }

    struct FakeGh {
        prs_by_number: HashMap<u64, PrDetails>,
        /// `head_spec` -> open PR number, for `find_open_pr`.
        open_prs: HashMap<String, u64>,
        stacks: HashMap<u64, crate::gh::PullRequestStack>,
        failing_enable: HashSet<String>,
        enabled: Mutex<Vec<(String, AutoMergeMethod, bool)>>,
        disabled: Mutex<Vec<(String, bool)>>,
    }

    impl FakeGh {
        fn new(prs: Vec<PrDetails>) -> Self {
            Self {
                prs_by_number: prs.into_iter().map(|pr| (pr.number, pr)).collect(),
                open_prs: HashMap::new(),
                stacks: HashMap::new(),
                failing_enable: HashSet::new(),
                enabled: Mutex::new(Vec::new()),
                disabled: Mutex::new(Vec::new()),
            }
        }

        fn with_open_pr(self, head_spec: &str, number: u64) -> Self {
            let mut open_prs = self.open_prs.clone();
            open_prs.insert(head_spec.into(), number);
            Self { open_prs, ..self }
        }

        fn with_stack(self, stack: crate::gh::PullRequestStack) -> Self {
            let mut stacks = self.stacks.clone();
            stacks.insert(stack.number, stack);
            Self { stacks, ..self }
        }

        fn with_failing_enable(self, node_ids: &[&str]) -> Self {
            Self {
                failing_enable: node_ids.iter().map(|id| (*id).into()).collect(),
                ..self
            }
        }

        fn enabled_nodes(&self) -> Vec<String> {
            self.enabled
                .lock()
                .unwrap()
                .iter()
                .map(|(node, _, _)| node.clone())
                .collect()
        }
    }

    impl Gh for FakeGh {
        fn find_open_pr(
            &self,
            _o: &str,
            _r: &str,
            head_spec: &str,
        ) -> impl Future<Output = Result<Option<PrSummary>>> {
            std::future::ready(Ok(self.open_prs.get(head_spec).map(|number| PrSummary {
                number: *number,
                html_url: format!("https://github.com/o/r/pull/{number}"),
                title: "t".into(),
                state: "open".into(),
            })))
        }
        async fn lookup_base(&self, _: &str, _: &str, _: &str) -> Result<BaseLookup> {
            unimplemented!()
        }
        async fn create_pr(&self, _req: CreatePrRequest) -> Result<PrCreated> {
            unimplemented!()
        }
        async fn add_reviewers(
            &self,
            _o: &str,
            _r: &str,
            _p: u64,
            _: Vec<crate::gh::Reviewer>,
        ) -> Result<()> {
            unimplemented!()
        }
        async fn remove_reviewers(
            &self,
            _o: &str,
            _r: &str,
            _p: u64,
            _: Vec<crate::gh::Reviewer>,
        ) -> Result<()> {
            unimplemented!()
        }
        async fn add_labels(&self, _: &str, _: &str, _: u64, _: &[String]) -> Result<()> {
            unimplemented!()
        }
        async fn remove_labels(&self, _: &str, _: &[String]) -> Result<()> {
            unimplemented!()
        }
        async fn update_pr(&self, _req: UpdatePr) -> Result<()> {
            unimplemented!()
        }
        async fn set_draft(&self, _id: &str, _d: bool) -> Result<()> {
            unimplemented!()
        }
        fn disable_auto_merge(
            &self,
            id: &str,
            include_stack: bool,
        ) -> impl Future<Output = Result<()>> {
            self.disabled
                .lock()
                .unwrap()
                .push((id.to_string(), include_stack));
            std::future::ready(Ok(()))
        }
        fn get_pr(&self, _o: &str, _r: &str, n: u64) -> impl Future<Output = Result<PrDetails>> {
            std::future::ready(
                self.prs_by_number
                    .get(&n)
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("no such PR #{n}")),
            )
        }
        async fn get_pr_diff(&self, _: &str, _: &str, _: u64) -> Result<String> {
            unimplemented!()
        }
        fn enable_auto_merge(
            &self,
            pr_node_id: &str,
            method: AutoMergeMethod,
            include_stack: bool,
        ) -> impl Future<Output = Result<()>> {
            self.enabled
                .lock()
                .unwrap()
                .push((pr_node_id.to_string(), method, include_stack));
            std::future::ready(if self.failing_enable.contains(pr_node_id) {
                Err(anyhow::anyhow!("GitHub rejected auto-merge"))
            } else {
                Ok(())
            })
        }
        async fn local_pulls(
            &self,
            _o: &str,
            _r: &str,
            _head_owner: &str,
            _b: &[String],
        ) -> Result<Vec<PrWithCiStatus>> {
            unimplemented!()
        }
        async fn list_workflow_runs_for_sha(
            &self,
            _o: &str,
            _r: &str,
            _sha: &str,
        ) -> Result<Vec<WorkflowRun>> {
            unimplemented!()
        }
        async fn list_jobs_for_run(&self, _: &str, _: &str, _: u64) -> Result<Vec<WorkflowJob>> {
            unimplemented!()
        }
        async fn cancel_workflow_run(&self, _: &str, _: &str, _: u64) -> Result<()> {
            unimplemented!()
        }
        async fn rerun_workflow_run(&self, _: &str, _: &str, _: u64) -> Result<()> {
            unimplemented!()
        }
        async fn rerun_failed_jobs(&self, _: &str, _: &str, _: u64) -> Result<()> {
            unimplemented!()
        }
        async fn create_stack(
            &self,
            _: &str,
            _: &str,
            _: &[u64],
        ) -> Result<crate::gh::PullRequestStack> {
            unimplemented!()
        }
        async fn append_to_stack(
            &self,
            _: &str,
            _: &str,
            _: u64,
            _: &[u64],
        ) -> Result<crate::gh::PullRequestStack> {
            unimplemented!()
        }
        async fn unstack_prs(
            &self,
            _: &str,
            _: &str,
            _: u64,
        ) -> Result<Option<crate::gh::PullRequestStack>> {
            unimplemented!()
        }
        async fn list_stacks(&self, _: &str, _: &str) -> Result<Vec<crate::gh::PullRequestStack>> {
            unimplemented!()
        }
        fn get_stack(
            &self,
            _: &str,
            _: &str,
            number: u64,
        ) -> impl Future<Output = Result<crate::gh::PullRequestStack>> {
            std::future::ready(
                self.stacks
                    .get(&number)
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("no such stack #{number}")),
            )
        }
    }

    fn args(targets: &[&str]) -> AutoMergeArgs {
        AutoMergeArgs {
            revsets: targets.iter().map(|t| (*t).into()).collect(),
            stack: false,
            cancel: false,
            auto_merge_method: None,
            configured_auto_merge_method: AutoMergeMethod::Merge,
            globals: GlobalOpts {
                verbose: 0,
                quiet: false,
                log_level: None,
                remote: crate::util::EvalWithCfgFallback::new(Some("origin".into()), None),
                upstream_remote: crate::util::EvalWithCfgFallback::new(None, None),
                gh_askpass: None,
                askpass_timeout_secs: 20,
            },
        }
    }

    fn stack_args(target: &str) -> AutoMergeArgs {
        AutoMergeArgs {
            stack: true,
            ..args(&[target])
        }
    }

    fn cancel_args(target: &str) -> AutoMergeArgs {
        AutoMergeArgs {
            cancel: true,
            ..args(&[target])
        }
    }

    fn method_args(target: &str, method: AutoMergeMethod) -> AutoMergeArgs {
        AutoMergeArgs {
            auto_merge_method: Some(method),
            ..args(&[target])
        }
    }

    fn test_target() -> remote::Target {
        remote::target("git@github.com:o/r.git", None).unwrap()
    }

    #[tokio::test]
    async fn enables_auto_merge_on_each_pr_number() {
        let gh = FakeGh::new(vec![pr_details(1, false), pr_details(2, false)]);
        let jj = FakeJj::default();

        run(&TestModel::without_git(&jj, &gh), &args(&["1", "2"]))
            .await
            .expect("should enable auto-merge on both PRs");

        assert_eq!(gh.enabled_nodes(), vec!["node-1", "node-2"]);
    }

    #[tokio::test]
    async fn revset_stack_enables_bottom_to_top() {
        // resolve_revset returns jj log order (newest first); expansion must
        // flip it so the bottom of the stack is enabled first.
        let gh = FakeGh::new(vec![pr_details(10, false), pr_details(11, false)])
            .with_open_pr("o:bottom", 10)
            .with_open_pr("o:top", 11);
        let jj = FakeJj::with_revset_commits(vec![
            commit("top", &["top"]),
            commit("mid", &[]),
            commit("bottom", &["bottom"]),
        ]);

        run(&TestModel::without_git(&jj, &gh), &args(&["qv::uw"]))
            .await
            .expect("revset stack should enable auto-merge bottom to top");

        assert_eq!(gh.enabled_nodes(), vec!["node-10", "node-11"]);
    }

    #[tokio::test]
    async fn merge_queue_pr_is_still_attempted() {
        let gh = FakeGh::new(vec![pr_details(1, true)]);

        run(
            &TestModel::without_git(&FakeJj::default(), &gh),
            &args(&["1"]),
        )
        .await
        .expect("a merge queue should not block the API attempt");

        assert_eq!(gh.enabled_nodes(), vec!["node-1"]);
    }

    #[tokio::test]
    async fn enable_failure_does_not_stop_others() {
        let gh = FakeGh::new(vec![pr_details(1, true), pr_details(2, false)])
            .with_failing_enable(&["node-1"]);

        let err = run(
            &TestModel::without_git(&FakeJj::default(), &gh),
            &args(&["1", "2"]),
        )
        .await
        .expect_err("failed enable should fail the command");
        let msg = format!("{err:#}");
        assert!(msg.contains("1 of 2"), "msg: {msg}");

        assert_eq!(gh.enabled_nodes(), vec!["node-1", "node-2"]);
    }

    #[tokio::test]
    async fn single_enable_failure_returns_the_real_error() {
        let gh = FakeGh::new(vec![pr_details(1, true)]).with_failing_enable(&["node-1"]);

        let err = run(
            &TestModel::without_git(&FakeJj::default(), &gh),
            &args(&["1"]),
        )
        .await
        .expect_err("GitHub rejection should be reported");
        let msg = format!("{err:#}");
        assert!(msg.contains("GitHub rejected auto-merge"), "msg: {msg}");
    }

    #[tokio::test]
    async fn all_skipped_targets_is_an_error() {
        let gh = FakeGh::new(vec![]);
        let jj = FakeJj::with_revset_commits(vec![commit("aaa", &[]), commit("bbb", &[])]);

        let err = run(&TestModel::without_git(&jj, &gh), &args(&["qv::uw"]))
            .await
            .expect_err("nothing resolved should be an error");
        let msg = format!("{err:#}");
        assert!(msg.contains("no open PRs"), "msg: {msg}");
    }

    #[tokio::test]
    async fn strict_matching_errors_on_bookmark_without_pr() {
        // `pr stack` uses MissingPr::Error: a bookmarked commit with no open
        // PR must fail the exact-stack assertion rather than be skipped.
        let gh = FakeGh::new(vec![pr_details(10, false)]).with_open_pr("o:bottom", 10);
        let jj = FakeJj::with_revset_commits(vec![
            commit("top", &["no-pr-bookmark"]),
            commit("bottom", &["bottom"]),
        ]);

        let err = pr_lookup::resolve_pr_targets(
            &jj,
            &gh,
            &test_target(),
            &["qv::uw".into()],
            pr_lookup::MissingPr::Error,
        )
        .await
        .expect_err("strict matching should error on PR-less bookmark");
        let msg = format!("{err:#}");
        assert!(msg.contains("no open PR"), "msg: {msg}");

        let lenient = pr_lookup::resolve_pr_targets(
            &jj,
            &gh,
            &test_target(),
            &["qv::uw".into()],
            pr_lookup::MissingPr::Skip,
        )
        .await
        .expect("lenient matching should skip the PR-less bookmark");
        assert_eq!(
            lenient.iter().map(|pr| pr.number).collect::<Vec<_>>(),
            vec![10]
        );
    }

    #[tokio::test]
    async fn cancel_cancels_individual_request() {
        let pr = with_requests(
            pr_details(1, false),
            Some(request(1, AutoMergeMethod::Squash)),
            None,
        );
        let gh = FakeGh::new(vec![pr]);

        run(
            &TestModel::without_git(&FakeJj::default(), &gh),
            &cancel_args("1"),
        )
        .await
        .expect("cancel should succeed");

        assert_eq!(*gh.disabled.lock().unwrap(), vec![("node-1".into(), false)]);
    }

    #[tokio::test]
    async fn cancel_cancels_stack_request_through_another_pr() {
        let pr = with_requests(
            pr_details(1, false),
            None,
            Some(request(3, AutoMergeMethod::Merge)),
        );
        let gh = FakeGh::new(vec![pr, pr_details(3, false)]);

        run(
            &TestModel::without_git(&FakeJj::default(), &gh),
            &cancel_args("1"),
        )
        .await
        .expect("cancel should succeed");

        assert_eq!(*gh.disabled.lock().unwrap(), vec![("node-3".into(), true)]);
    }

    #[tokio::test]
    async fn cancel_without_request_is_a_noop() {
        let gh = FakeGh::new(vec![pr_details(1, false)]);

        run(
            &TestModel::without_git(&FakeJj::default(), &gh),
            &cancel_args("1"),
        )
        .await
        .expect("cancel with no request should succeed");

        assert!(gh.disabled.lock().unwrap().is_empty());
        assert!(gh.enabled.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn stack_moves_partial_request_to_tip_and_keeps_method() {
        let pr = with_requests(
            stacked_pr(1, 7),
            None,
            Some(request(2, AutoMergeMethod::Squash)),
        );
        let gh = FakeGh::new(vec![pr, pr_details(2, false), pr_details(3, false)])
            .with_stack(stack(7, &[1, 2, 3]));

        run(
            &TestModel::without_git(&FakeJj::default(), &gh),
            &stack_args("1"),
        )
        .await
        .expect("stack move should succeed");

        assert_eq!(*gh.disabled.lock().unwrap(), vec![("node-2".into(), true)]);
        assert_eq!(
            *gh.enabled.lock().unwrap(),
            vec![("node-3".into(), AutoMergeMethod::Squash, true)]
        );
    }

    #[tokio::test]
    async fn stack_noops_when_already_covering_tip() {
        let pr = with_requests(
            stacked_pr(1, 7),
            None,
            Some(request(3, AutoMergeMethod::Merge)),
        );
        let gh = FakeGh::new(vec![pr, pr_details(3, false)]).with_stack(stack(7, &[1, 2, 3]));

        run(
            &TestModel::without_git(&FakeJj::default(), &gh),
            &stack_args("1"),
        )
        .await
        .expect("already-covered stack should succeed");

        assert!(gh.disabled.lock().unwrap().is_empty());
        assert!(gh.enabled.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn explicit_method_replaces_whole_stack_request() {
        let pr = with_requests(
            stacked_pr(1, 7),
            None,
            Some(request(3, AutoMergeMethod::Squash)),
        );
        let gh = FakeGh::new(vec![pr, pr_details(3, false)]).with_stack(stack(7, &[1, 2, 3]));
        let args = AutoMergeArgs {
            stack: true,
            ..method_args("1", AutoMergeMethod::Rebase)
        };

        run(&TestModel::without_git(&FakeJj::default(), &gh), &args)
            .await
            .expect("method change should succeed");

        assert_eq!(*gh.disabled.lock().unwrap(), vec![("node-3".into(), true)]);
        assert_eq!(
            *gh.enabled.lock().unwrap(),
            vec![("node-3".into(), AutoMergeMethod::Rebase, true)]
        );
    }

    #[tokio::test]
    async fn stack_requires_stack_membership() {
        let gh = FakeGh::new(vec![pr_details(1, false)]);

        let err = run(
            &TestModel::without_git(&FakeJj::default(), &gh),
            &stack_args("1"),
        )
        .await
        .expect_err("standalone PR should not be stackable");
        assert!(
            format!("{err:#}").contains("not part of a GitHub stack"),
            "msg: {err:#}"
        );
    }

    #[tokio::test]
    async fn stack_mode_applies_to_each_target() {
        let gh = FakeGh::new(vec![
            stacked_pr(10, 7),
            stacked_pr(11, 8),
            pr_details(20, false),
            pr_details(21, false),
        ])
        .with_stack(stack(7, &[10, 12, 20]))
        .with_stack(stack(8, &[11, 21]));
        let args = AutoMergeArgs {
            stack: true,
            ..args(&["10", "11"])
        };

        run(&TestModel::without_git(&FakeJj::default(), &gh), &args)
            .await
            .expect("stack mode should handle every target");

        assert!(gh.disabled.lock().unwrap().is_empty());
        assert_eq!(
            *gh.enabled.lock().unwrap(),
            vec![
                ("node-20".into(), AutoMergeMethod::Merge, true),
                ("node-21".into(), AutoMergeMethod::Merge, true),
            ]
        );
    }

    #[tokio::test]
    async fn stack_with_merge_queue_tip_is_still_attempted() {
        let pr = with_requests(
            stacked_pr(1, 7),
            None,
            Some(request(2, AutoMergeMethod::Squash)),
        );
        let tip = PrDetails {
            base_has_merge_queue: true,
            ..pr_details(3, false)
        };
        let gh = FakeGh::new(vec![pr, pr_details(2, false), tip]).with_stack(stack(7, &[1, 2, 3]));

        run(
            &TestModel::without_git(&FakeJj::default(), &gh),
            &stack_args("1"),
        )
        .await
        .expect("a merge queue should not block the API attempt");

        assert_eq!(*gh.disabled.lock().unwrap(), vec![("node-2".into(), true)]);
        assert_eq!(
            *gh.enabled.lock().unwrap(),
            vec![("node-3".into(), AutoMergeMethod::Squash, true)]
        );
    }

    fn view(pr: &PrDetails, tip: Option<u64>) -> AutoMergeView {
        current_view(pr, tip)
    }

    async fn editor_change(
        gh: &FakeGh,
        pr: &PrDetails,
        before: Option<AutoMergeMode>,
        after: Option<AutoMergeMode>,
        method: AutoMergeMethod,
        method_explicit: bool,
    ) -> Result<()> {
        apply_editor_change(
            gh,
            AutoMergeEditorChange {
                target: &test_target(),
                pr: AutoMergeTarget::from(pr),
                before,
                after,
                method,
                method_explicit,
            },
        )
        .await
    }

    #[test]
    fn editor_view_represents_each_request_shape() {
        assert_eq!(
            view(&pr_details(1, false), None).value,
            Some(AutoMergeMode::False)
        );

        let individual = with_requests(
            pr_details(1, false),
            Some(request(1, AutoMergeMethod::Rebase)),
            None,
        );
        let individual_view = view(&individual, None);
        assert_eq!(individual_view.value, Some(AutoMergeMode::Below));
        assert_eq!(individual_view.method, AutoMergeMethod::Rebase);

        let whole = with_requests(
            stacked_pr(1, 7),
            None,
            Some(request(3, AutoMergeMethod::Squash)),
        );
        assert_eq!(view(&whole, Some(3)).value, Some(AutoMergeMode::All));

        let partial = with_requests(
            stacked_pr(1, 7),
            None,
            Some(request(2, AutoMergeMethod::Squash)),
        );
        let partial_view = view(&partial, Some(3));
        assert_eq!(partial_view.value, None);
        assert!(
            partial_view
                .comment
                .as_deref()
                .is_some_and(|c| c.contains("PR #2")),
            "comment: {:?}",
            partial_view.comment
        );
    }

    #[tokio::test]
    async fn editor_unchanged_value_does_not_mutate() {
        let pr = with_requests(
            stacked_pr(1, 7),
            None,
            Some(request(3, AutoMergeMethod::Squash)),
        );
        let gh =
            FakeGh::new(vec![pr.clone(), pr_details(3, false)]).with_stack(stack(7, &[1, 2, 3]));

        editor_change(
            &gh,
            &pr,
            Some(AutoMergeMode::All),
            Some(AutoMergeMode::All),
            AutoMergeMethod::Squash,
            false,
        )
        .await
        .expect("unchanged value should succeed");

        assert!(gh.enabled.lock().unwrap().is_empty());
        assert!(gh.disabled.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn editor_false_cancels_whole_stack_request_through_owner() {
        let pr = with_requests(
            stacked_pr(1, 7),
            None,
            Some(request(3, AutoMergeMethod::Squash)),
        );
        let gh = FakeGh::new(vec![pr.clone(), pr_details(3, false)]);

        editor_change(
            &gh,
            &pr,
            Some(AutoMergeMode::All),
            Some(AutoMergeMode::False),
            AutoMergeMethod::Squash,
            false,
        )
        .await
        .expect("cancel should succeed");

        assert_eq!(*gh.disabled.lock().unwrap(), vec![("node-3".into(), true)]);
        assert!(gh.enabled.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn editor_below_replaces_stack_request_and_keeps_method() {
        let pr = with_requests(
            stacked_pr(1, 7),
            None,
            Some(request(3, AutoMergeMethod::Squash)),
        );
        let gh = FakeGh::new(vec![pr.clone(), pr_details(3, false)]);

        editor_change(
            &gh,
            &pr,
            Some(AutoMergeMode::All),
            Some(AutoMergeMode::Below),
            AutoMergeMethod::Squash,
            false,
        )
        .await
        .expect("switch to below should succeed");

        assert_eq!(*gh.disabled.lock().unwrap(), vec![("node-3".into(), true)]);
        assert_eq!(
            *gh.enabled.lock().unwrap(),
            vec![("node-1".into(), AutoMergeMethod::Squash, false)]
        );
    }

    #[tokio::test]
    async fn editor_all_moves_partial_request_to_tip() {
        let pr = with_requests(
            stacked_pr(1, 7),
            None,
            Some(request(2, AutoMergeMethod::Squash)),
        );
        let gh = FakeGh::new(vec![pr.clone(), pr_details(2, false), pr_details(3, false)])
            .with_stack(stack(7, &[1, 2, 3]));

        editor_change(
            &gh,
            &pr,
            Some(AutoMergeMode::Below),
            Some(AutoMergeMode::All),
            AutoMergeMethod::Squash,
            false,
        )
        .await
        .expect("stack move should succeed");

        assert_eq!(*gh.disabled.lock().unwrap(), vec![("node-2".into(), true)]);
        assert_eq!(
            *gh.enabled.lock().unwrap(),
            vec![("node-3".into(), AutoMergeMethod::Squash, true)]
        );
    }

    #[tokio::test]
    async fn editor_explicit_method_change_is_applied() {
        let pr = with_requests(
            stacked_pr(1, 7),
            None,
            Some(request(3, AutoMergeMethod::Squash)),
        );
        let gh =
            FakeGh::new(vec![pr.clone(), pr_details(3, false)]).with_stack(stack(7, &[1, 2, 3]));

        editor_change(
            &gh,
            &pr,
            Some(AutoMergeMode::All),
            Some(AutoMergeMode::All),
            AutoMergeMethod::Rebase,
            true,
        )
        .await
        .expect("method change should succeed");

        assert_eq!(*gh.disabled.lock().unwrap(), vec![("node-3".into(), true)]);
        assert_eq!(
            *gh.enabled.lock().unwrap(),
            vec![("node-3".into(), AutoMergeMethod::Rebase, true)]
        );
    }

    #[tokio::test]
    async fn editor_unrepresentable_value_is_left_unchanged() {
        let pr = with_requests(
            stacked_pr(1, 7),
            None,
            Some(request(2, AutoMergeMethod::Squash)),
        );
        let gh = FakeGh::new(vec![pr.clone(), pr_details(2, false)]);

        editor_change(&gh, &pr, None, None, AutoMergeMethod::Squash, false)
            .await
            .expect("omitted field should leave the request alone");

        assert!(gh.enabled.lock().unwrap().is_empty());
        assert!(gh.disabled.lock().unwrap().is_empty());
    }

    fn created_request(
        number: u64,
        mode: AutoMergeMode,
        method: AutoMergeMethod,
        stack: Option<u64>,
    ) -> CreatedAutoMerge {
        let mut request = CreatedAutoMerge::new(
            number,
            &format!("node-{number}"),
            &format!("https://github.com/o/r/pull/{number}"),
            false,
            mode,
            method,
        );
        request.target.stack_number = stack;
        request
    }

    async fn apply_created(
        gh: &FakeGh,
        requests: Vec<CreatedAutoMerge>,
        linked: Option<Vec<PrDetails>>,
    ) -> Result<()> {
        apply_created_auto_merge(gh, &test_target(), &requests, linked.as_deref()).await
    }

    #[tokio::test]
    async fn created_false_requests_do_not_enable_anything() {
        let gh = FakeGh::new(vec![]);
        apply_created(
            &gh,
            vec![created_request(
                1,
                AutoMergeMode::False,
                AutoMergeMethod::Merge,
                None,
            )],
            None,
        )
        .await
        .unwrap();

        assert!(gh.enabled.lock().unwrap().is_empty());
        assert!(gh.disabled.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn created_conflicting_methods_error_without_mutating() {
        let gh = FakeGh::new(vec![]);
        let err = apply_created(
            &gh,
            vec![
                created_request(1, AutoMergeMode::All, AutoMergeMethod::Merge, Some(7)),
                created_request(2, AutoMergeMode::All, AutoMergeMethod::Squash, Some(7)),
            ],
            None,
        )
        .await
        .unwrap_err();

        assert!(err.to_string().contains("jj-gh pr auto-merge"), "{err}");
        assert!(gh.enabled.lock().unwrap().is_empty());
        assert!(gh.disabled.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn created_stack_makes_one_whole_stack_request() {
        let gh = FakeGh::new(vec![pr_details(3, false)]).with_stack(stack(7, &[1, 2, 3]));
        apply_created(
            &gh,
            vec![
                created_request(1, AutoMergeMode::Infer, AutoMergeMethod::Merge, Some(7)),
                created_request(2, AutoMergeMode::Below, AutoMergeMethod::Merge, Some(7)),
                created_request(3, AutoMergeMode::All, AutoMergeMethod::Merge, Some(7)),
            ],
            None,
        )
        .await
        .unwrap();

        assert_eq!(
            *gh.enabled.lock().unwrap(),
            vec![("node-3".into(), AutoMergeMethod::Merge, true)]
        );
        assert!(gh.disabled.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn created_below_requests_use_highest_endpoint() {
        let gh = FakeGh::new(vec![]);
        apply_created(
            &gh,
            vec![
                created_request(1, AutoMergeMode::Below, AutoMergeMethod::Squash, Some(7)),
                created_request(2, AutoMergeMode::Below, AutoMergeMethod::Squash, Some(7)),
            ],
            None,
        )
        .await
        .unwrap();

        assert_eq!(
            *gh.enabled.lock().unwrap(),
            vec![("node-2".into(), AutoMergeMethod::Squash, false)]
        );
    }

    #[tokio::test]
    async fn created_standalone_prs_each_get_a_request() {
        let gh = FakeGh::new(vec![]);
        apply_created(
            &gh,
            vec![
                created_request(1, AutoMergeMode::Infer, AutoMergeMethod::Merge, None),
                created_request(2, AutoMergeMode::All, AutoMergeMethod::Merge, None),
            ],
            None,
        )
        .await
        .unwrap();

        assert_eq!(
            *gh.enabled.lock().unwrap(),
            vec![
                ("node-1".into(), AutoMergeMethod::Merge, false),
                ("node-2".into(), AutoMergeMethod::Merge, false),
            ]
        );
    }

    #[tokio::test]
    async fn created_linked_details_supply_stack_membership() {
        let gh = FakeGh::new(vec![pr_details(3, false)]).with_stack(stack(7, &[1, 2, 3]));
        apply_created(
            &gh,
            vec![created_request(
                1,
                AutoMergeMode::All,
                AutoMergeMethod::Merge,
                None,
            )],
            Some(vec![
                stacked_pr(1, 7),
                pr_details(2, false),
                pr_details(3, false),
            ]),
        )
        .await
        .unwrap();

        assert_eq!(
            *gh.enabled.lock().unwrap(),
            vec![("node-3".into(), AutoMergeMethod::Merge, true)]
        );
    }

    #[tokio::test]
    async fn created_false_tip_is_not_used_as_endpoint_but_does_not_veto() {
        let gh = FakeGh::new(vec![pr_details(1, false), pr_details(3, false)])
            .with_stack(stack(7, &[1, 2, 3]));
        apply_created(
            &gh,
            vec![
                created_request(1, AutoMergeMode::All, AutoMergeMethod::Merge, Some(7)),
                created_request(3, AutoMergeMode::False, AutoMergeMethod::Merge, Some(7)),
            ],
            None,
        )
        .await
        .unwrap();

        // The tip opted out, so the whole-stack request falls back to the
        // highest PR that did ask. A request is still made: `false` is not a
        // veto, it only keeps that PR out of the endpoint role.
        assert_eq!(
            *gh.enabled.lock().unwrap(),
            vec![("node-1".into(), AutoMergeMethod::Merge, false)]
        );
    }
}
