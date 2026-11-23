//! Turning the detected local shape (see [`crate::gh::stack_detect`]) into
//! real GitHub PR stacks.
//!
//! GitHub's REST API can append PRs to the top of an existing stack, so a
//! chain that merely grew at the top is extended in place. Any other change
//! still has to rebuild the stack, and two API rules dictate that order:
//!
//! - `createStack` only accepts PR numbers whose base refs *already* chain
//!   into each other, so the bases have to move first;
//! - `updatePullRequest` refuses to change the base ref of a PR that is in a
//!   stack ("Cannot change the base branch because the pull request is part of
//!   a stack"), so the stack has to go first.
//!
//! The only order satisfying both for a rebuild is **unstack, then retarget,
//! then create**. Getting it wrong surfaces as `422: Pull requests must form a
//! stack` one way and the "part of a stack" GraphQL error the other.
//!
//! A useful consequence: because GitHub will not let a stacked PR's base move,
//! a stack that exists verbatim cannot have drifted out of alignment
//! underneath us, and needs no retargeting at all.
//!
//! Nothing here touches commits: retargeting moves the `baseRefName` field on
//! the PR, it does not rebase anything in the jj or git sense.
//!
//! This module only talks to the API and reports what it did; rendering the
//! outcome is the caller's job.

use crate::gh::{Gh, PrDetails, PullRequestStack, StackPullRequest, UpdatePr, remote::Target};
use anyhow::{Context, Result, bail};
use std::collections::{HashMap, HashSet};

/// What happened to one chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainResult {
    pub chain: Vec<u64>,
    pub outcome: ChainOutcome,
    /// PRs whose base ref had to be retargeted before the stack could be
    /// created.
    pub retargeted: Vec<u64>,
}

/// The fate of one chain's stack on GitHub.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChainOutcome {
    /// A new stack was created, carrying this stack number.
    Created(u64),
    /// PRs were appended to the top of an existing stack.
    Appended { stack_number: u64, added: Vec<u64> },
    /// GitHub already has this exact stack, so nothing was created.
    AlreadyExists,
}

/// What GitHub actually did with the stacks we asked it to unstack.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnstackOutcome {
    /// PRs confirmed to be out of their stacks.
    pub unstacked: Vec<u64>,
    /// Stacks GitHub dissolved (`204`).
    pub dissolved: Vec<u64>,
    /// Stacks GitHub left in place (`200`), with their current membership.
    pub remaining: Vec<PullRequestStack>,
}

/// Index `pr_details` by PR number for the many by-number lookups below.
fn by_number(pr_details: &[PrDetails]) -> HashMap<u64, &PrDetails> {
    pr_details.iter().map(|pr| (pr.number, pr)).collect()
}

/// Point every PR in `chain`'s base ref at its predecessor's head ref, which
/// is the precondition GitHub enforces when the stack is created. The bottom
/// PR's base is left alone: nothing in the chain constrains it.
///
/// Callers must have unstacked the chain first; see the module docs.
///
/// Returns the PRs that were actually retargeted.
///
/// # Errors
///
/// Returns an error if a chain member is missing from `pr_details`, or if the
/// update call fails. A failure here is fatal for the chain, because creating
/// the stack afterwards would only fail with a less useful 422.
async fn align_chain_bases(
    gh: &impl Gh,
    chain: &[u64],
    prs: &HashMap<u64, &PrDetails>,
) -> Result<Vec<u64>> {
    let mut retargeted = Vec::new();
    for pair in chain.windows(2) {
        let [below, above] = pair else { continue };
        let below = prs
            .get(below)
            .with_context(|| format!("no details for PR #{below} in the chain"))?;
        let above = prs
            .get(above)
            .with_context(|| format!("no details for PR #{above} in the chain"))?;
        if above.base_ref == below.head_ref {
            continue;
        }
        log::debug!(
            "retargeting PR #{} base ref from `{}` to `{}` so the stack chains",
            above.number,
            above.base_ref,
            below.head_ref,
        );
        gh.update_pr(UpdatePr {
            pr_node_id: above.graphql_node_id.clone(),
            base_ref_name: Some(below.head_ref.clone()),
            ..Default::default()
        })
        .await
        .with_context(|| {
            format!(
                "retargeting PR #{} base ref to `{}`",
                above.number, below.head_ref
            )
        })?;
        retargeted.push(above.number);
    }
    Ok(retargeted)
}

fn is_open_unmerged(pr: &StackPullRequest) -> bool {
    pr.merged_at.is_none() && pr.state.eq_ignore_ascii_case("open")
}

/// Refuse to reshape a stack we do not fully understand. A member without a
/// local bookmark would be dropped by a rebuild, and an active auto-merge
/// request would either be cancelled or leave the PR locked in the old stack.
fn ensure_safe_to_reshape(stack: &PullRequestStack, prs: &HashMap<u64, &PrDetails>) -> Result<()> {
    let missing = stack
        .pull_requests
        .iter()
        .filter(|pr| is_open_unmerged(pr))
        .filter(|pr| !prs.contains_key(&pr.number))
        .map(|pr| format!("#{}", pr.number))
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        bail!(
            "cannot reshape stack #{}: PR {} {} not local; \
             unstacking them would drop nonlocal members from the stack",
            stack.number,
            missing.join(", "),
            if missing.len() == 1 { "is" } else { "are" }
        );
    }

    let auto_merging = stack
        .pull_requests
        .iter()
        .filter_map(|pr| prs.get(&pr.number))
        .filter(|pr| pr.auto_merge_request.is_some() || pr.stack_auto_merge_request.is_some())
        .map(|pr| format!("#{}", pr.number))
        .collect::<Vec<_>>();
    if !auto_merging.is_empty() {
        bail!(
            "cannot reshape stack #{}: PR {} {} auto-merge enabled; \
             disable it with `jj-gh pr edit` before reshaping",
            stack.number,
            auto_merging.join(", "),
            if auto_merging.len() == 1 {
                "has"
            } else {
                "have"
            }
        );
    }
    Ok(())
}

/// Remove `pr_numbers` from whatever stacks they currently belong to, one call
/// per stack. A stack left with no PRs is dissolved by GitHub.
///
/// Only PRs present in `pr_details` and currently carrying a stack number are
/// touched. Before each call the whole stack is checked for members we do not
/// know about and for active auto-merge requests; both make a rebuild unsafe.
/// Afterward the server's answer is inspected, because GitHub silently leaves
/// behind PRs it cannot unstack, such as ones queued for merge.
///
/// # Errors
///
/// Propagates API errors, and fails if an open PR we tried to unstack remains
/// in the stack.
pub async fn unstack(
    gh: &impl Gh,
    target: &Target,
    pr_numbers: &[u64],
    pr_details: &[PrDetails],
    existing_stacks: &[PullRequestStack],
) -> Result<UnstackOutcome> {
    let prs = by_number(pr_details);
    let wanted = pr_numbers.iter().copied().collect::<HashSet<u64>>();
    let stacks = existing_stacks
        .iter()
        .map(|stack| (stack.number, stack))
        .collect::<HashMap<u64, &PullRequestStack>>();

    let mut by_stack = HashMap::<u64, Vec<u64>>::new();
    for pr in pr_details.iter().filter(|pr| wanted.contains(&pr.number)) {
        if let Some(stack_number) = pr.stack_number {
            by_stack.entry(stack_number).or_default().push(pr.number);
        }
    }

    let mut outcome = UnstackOutcome {
        unstacked: Vec::new(),
        dissolved: Vec::new(),
        remaining: Vec::new(),
    };
    for (stack_number, members) in by_stack {
        let stack = stacks.get(&stack_number).with_context(|| {
            format!("stack #{stack_number} disappeared before it could be unstacked")
        })?;
        ensure_safe_to_reshape(stack, &prs)?;

        let remaining = gh
            .unstack_prs(&target.owner, &target.repo, stack_number)
            .await
            .with_context(|| format!("unstacking from stack #{stack_number}"))?;
        match remaining {
            None => outcome.dissolved.push(stack_number),
            Some(remaining) => {
                let left_behind = remaining
                    .pull_requests
                    .iter()
                    .filter(|pr| is_open_unmerged(pr))
                    .map(|pr| pr.number)
                    .filter(|number| members.contains(number))
                    .map(|number| format!("#{number}"))
                    .collect::<Vec<_>>();
                if !left_behind.is_empty() {
                    bail!(
                        "GitHub left PR {} stacked after unstacking stack #{}; \
                         disable auto-merge with `jj-gh pr edit` and retry",
                        left_behind.join(", "),
                        stack_number
                    );
                }
                outcome.remaining.push(remaining);
            }
        }
        outcome.unstacked.extend(members);
    }
    outcome.unstacked.sort_unstable();
    Ok(outcome)
}

/// Mirror an [`UnstackOutcome`] into the caller's working copies of the PR and
/// stack lists, so later chains in the same run neither re-unstack a PR nor
/// skip rebuilding a stack we just dissolved.
pub fn apply_unstack_result(
    outcome: &UnstackOutcome,
    pr_details: &mut [PrDetails],
    existing_stacks: &mut Vec<PullRequestStack>,
) {
    let done = outcome.unstacked.iter().copied().collect::<HashSet<u64>>();
    let dissolved = outcome.dissolved.iter().copied().collect::<HashSet<u64>>();
    for pr in pr_details.iter_mut().filter(|pr| done.contains(&pr.number)) {
        pr.stack_number = None;
    }
    existing_stacks.retain(|stack| !dissolved.contains(&stack.number));
    for remaining in &outcome.remaining {
        if let Some(current) = existing_stacks
            .iter_mut()
            .find(|stack| stack.number == remaining.number)
        {
            *current = remaining.clone();
        } else {
            existing_stacks.push(remaining.clone());
        }
    }
}

/// Whether GitHub already has `chain` as a stack, verbatim.
#[must_use]
pub fn stack_exists(existing_stacks: &[PullRequestStack], chain: &[u64]) -> bool {
    existing_stacks
        .iter()
        .any(|stack| stack_members(stack) == chain)
}

/// An existing stack that is exactly the bottom of `chain`, and the PRs that
/// would need appending to turn it into `chain`.
#[must_use]
pub fn append_candidate<'a>(
    existing_stacks: &'a [PullRequestStack],
    chain: &[u64],
) -> Option<(&'a PullRequestStack, Vec<u64>)> {
    existing_stacks.iter().find_map(|stack| {
        let members = stack_members(stack);
        (members.len() < chain.len() && chain.starts_with(&members))
            .then(|| (stack, chain[members.len()..].to_vec()))
    })
}

/// The PR numbers in `stack`, in order.
#[must_use]
pub fn stack_members(stack: &PullRequestStack) -> Vec<u64> {
    stack.pull_requests.iter().map(|pr| pr.number).collect()
}

fn replace_stack(stacks: &mut Vec<PullRequestStack>, updated: PullRequestStack) {
    if let Some(current) = stacks
        .iter_mut()
        .find(|stack| stack.number == updated.number)
    {
        *current = updated;
    } else {
        stacks.push(updated);
    }
}

fn stack_has_auto_merge(stack: &PullRequestStack, prs: &HashMap<u64, &PrDetails>) -> bool {
    stack.pull_requests.iter().any(|pr| {
        prs.get(&pr.number)
            .is_some_and(|pr| pr.stack_auto_merge_request.is_some())
    })
}

/// Once a stack with whole-stack auto-merge grows, GitHub does not necessarily
/// extend the request to the new top. Re-read the stack and say so rather than
/// implying the new PR is covered.
async fn warn_if_coverage_incomplete(
    gh: &impl Gh,
    target: &Target,
    before: &PullRequestStack,
    chain: &[u64],
    prs: &HashMap<u64, &PrDetails>,
) {
    if !stack_has_auto_merge(before, prs) {
        return;
    }
    let current = match gh
        .get_stack(&target.owner, &target.repo, before.number)
        .await
    {
        Ok(stack) => stack,
        Err(e) => {
            log::warn!(
                "could not re-read stack #{} after appending: {e:#}",
                before.number
            );
            return;
        }
    };
    if stack_members(&current) != chain {
        log::warn!(
            "stack #{} now contains {:?}; expected {:?}",
            before.number,
            stack_members(&current),
            chain
        );
    }

    let mut uncovered = Vec::new();
    for pr in current
        .pull_requests
        .iter()
        .filter(|pr| is_open_unmerged(pr))
    {
        match gh.get_pr(&target.owner, &target.repo, pr.number).await {
            Ok(details) if details.stack_auto_merge_request.is_some() => {}
            Ok(_) => uncovered.push(pr.number),
            Err(e) => log::warn!(
                "could not check auto-merge coverage for PR #{}: {e:#}",
                pr.number
            ),
        }
    }
    if !uncovered.is_empty() {
        log::warn!(
            "whole-stack auto-merge does not cover PR {}; re-request it with \
             `jj-gh pr auto-merge --stack <rev|num>`",
            uncovered
                .iter()
                .map(|n| format!("#{n}"))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
}

/// Create, extend, or rebuild a GitHub stack for each chain.
///
/// A chain that grows at the top is appended to in place; any other change
/// reshapes the stack. Reshaping is refused unless every open member of the
/// existing stack has local details and none has auto-merge enabled, so a
/// rebuild can never silently drop nonlocal PRs or cancel a merge request.
///
/// Returns one [`ChainResult`] per input chain, in the input order.
///
/// # Errors
///
/// Returns an error if an unsafe reshape is detected, or if an unstack,
/// base-ref update, append, or create fails.
pub async fn create_stacks(
    gh: &impl Gh,
    target: &Target,
    chains: &[Vec<u64>],
    pr_details: &[PrDetails],
    existing_stacks: &[PullRequestStack],
) -> Result<Vec<ChainResult>> {
    let mut pr_details = pr_details.to_vec();
    let mut existing_stacks = existing_stacks.to_vec();
    let mut results = Vec::with_capacity(chains.len());

    for chain in chains {
        let prs = by_number(&pr_details);
        let mut retargeted = Vec::new();
        let outcome = if stack_exists(&existing_stacks, chain) {
            // Nothing to do, and nothing can have drifted: GitHub does not
            // allow a stacked PR's base ref to move in the first place.
            ChainOutcome::AlreadyExists
        } else if let Some((before, added)) = append_candidate(&existing_stacks, chain) {
            let before = before.clone();
            retargeted = align_chain_bases(gh, chain, &prs).await?;
            let updated = gh
                .append_to_stack(&target.owner, &target.repo, before.number, &added)
                .await
                .with_context(|| format!("appending to stack #{}", before.number))?;
            replace_stack(&mut existing_stacks, updated);
            warn_if_coverage_incomplete(gh, target, &before, chain, &prs).await;
            ChainOutcome::Appended {
                stack_number: before.number,
                added,
            }
        } else {
            // Members may still belong to a stale stack (the chain shrank or
            // was reordered). That membership has to go before the base refs
            // can move, and the base refs have to chain before the new stack
            // can be asserted. See the module docs.
            let unstacked = unstack(gh, target, chain, &pr_details, &existing_stacks).await?;
            apply_unstack_result(&unstacked, &mut pr_details, &mut existing_stacks);
            retargeted = align_chain_bases(gh, chain, &by_number(&pr_details)).await?;
            let stack = gh.create_stack(&target.owner, &target.repo, chain).await?;
            existing_stacks.push(stack.clone());
            ChainOutcome::Created(stack.number)
        };

        results.push(ChainResult {
            chain: chain.clone(),
            outcome,
            retargeted,
        });
    }

    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::AutoMergeMethod,
        gh::{
            AutoMergeRequest, BaseLookup, CreatePrRequest, PrCreated, PrSummary, PrWithCiStatus,
            PullRequestStack, StackPullRequest, WorkflowRun,
        },
    };
    use anyhow::anyhow;
    use std::sync::Mutex;

    /// One recorded write. Kept in a single ordered log because the property
    /// that broke in #249 was the *order* of these calls, not their contents.
    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Call {
        Unstacked { stack: u64 },
        Appended { stack: u64, prs: Vec<u64> },
        Retargeted { node_id: String, base: String },
        Created(Vec<u64>),
    }

    struct FakeGh {
        calls: Mutex<Vec<Call>>,
        next_stack_number: Mutex<u64>,
        unstack_fails: bool,
        /// `Some(members)` means GitHub answered 200 with those PRs still in
        /// the stack; `None` means it dissolved the stack with 204.
        unstack_remaining: Option<Vec<u64>>,
        /// The state returned by the post-append coverage re-read.
        read_back_stack: Option<PullRequestStack>,
        read_back_prs: Vec<PrDetails>,
    }

    impl FakeGh {
        fn new() -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                next_stack_number: Mutex::new(100),
                unstack_fails: false,
                unstack_remaining: None,
                read_back_stack: None,
                read_back_prs: Vec::new(),
            }
        }

        /// A fake whose `unstack_prs` always errors, standing in for GitHub
        /// refusing to unstack (e.g. a PR queued for merge).
        fn failing_unstack() -> Self {
            Self {
                unstack_fails: true,
                ..Self::new()
            }
        }

        /// A fake where GitHub leaves the given open PRs stacked.
        fn leaving_unstacked(members: &[u64]) -> Self {
            Self {
                unstack_remaining: Some(members.to_vec()),
                ..Self::new()
            }
        }

        /// A fake whose post-append reads report the given coverage state.
        fn reading_back(stack: PullRequestStack, prs: Vec<PrDetails>) -> Self {
            Self {
                read_back_stack: Some(stack),
                read_back_prs: prs,
                ..Self::new()
            }
        }

        fn calls(&self) -> Vec<Call> {
            self.calls.lock().unwrap().clone()
        }

        fn record(&self, call: Call) {
            self.calls.lock().unwrap().push(call);
        }

        fn created(&self) -> Vec<Vec<u64>> {
            self.calls()
                .into_iter()
                .filter_map(|c| match c {
                    Call::Created(prs) => Some(prs),
                    _ => None,
                })
                .collect()
        }

        fn unstacked(&self) -> Vec<u64> {
            self.calls()
                .into_iter()
                .filter_map(|c| match c {
                    Call::Unstacked { stack } => Some(stack),
                    _ => None,
                })
                .collect()
        }

        fn appended(&self) -> Vec<(u64, Vec<u64>)> {
            self.calls()
                .into_iter()
                .filter_map(|c| match c {
                    Call::Appended { stack, prs } => Some((stack, prs)),
                    _ => None,
                })
                .collect()
        }

        fn retargeted(&self) -> Vec<(String, String)> {
            self.calls()
                .into_iter()
                .filter_map(|c| match c {
                    Call::Retargeted { node_id, base } => Some((node_id, base)),
                    _ => None,
                })
                .collect()
        }
    }

    impl Gh for FakeGh {
        async fn find_open_pr(&self, _: &str, _: &str, _: &str) -> Result<Option<PrSummary>> {
            unimplemented!()
        }
        async fn lookup_base(&self, _: &str, _: &str, _: &str) -> Result<BaseLookup> {
            unimplemented!()
        }
        async fn create_pr(&self, _: CreatePrRequest) -> Result<PrCreated> {
            unimplemented!()
        }
        async fn add_reviewers(
            &self,
            _: &str,
            _: &str,
            _: u64,
            _: Vec<crate::gh::Reviewer>,
        ) -> Result<()> {
            unimplemented!()
        }
        async fn remove_reviewers(
            &self,
            _: &str,
            _: &str,
            _: u64,
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
        fn update_pr(&self, req: UpdatePr) -> impl Future<Output = Result<()>> {
            let base = req
                .base_ref_name
                .expect("stack_create only updates base refs");
            self.record(Call::Retargeted {
                node_id: req.pr_node_id,
                base,
            });
            std::future::ready(Ok(()))
        }
        async fn set_draft(&self, _: &str, _: bool) -> Result<()> {
            unimplemented!()
        }
        async fn disable_auto_merge(&self, _: &str) -> Result<()> {
            unimplemented!()
        }
        fn get_pr(&self, _: &str, _: &str, number: u64) -> impl Future<Output = Result<PrDetails>> {
            let found = self
                .read_back_prs
                .iter()
                .find(|pr| pr.number == number)
                .cloned()
                .with_context(|| format!("no read-back details for PR #{number}"));
            std::future::ready(found)
        }
        async fn get_pr_diff(&self, _: &str, _: &str, _: u64) -> Result<String> {
            unimplemented!()
        }
        async fn enable_auto_merge(&self, _: &str, _: AutoMergeMethod) -> Result<()> {
            unimplemented!()
        }
        async fn local_pulls(
            &self,
            _: &str,
            _: &str,
            _: &str,
            _: &[String],
        ) -> Result<Vec<PrWithCiStatus>> {
            unimplemented!()
        }
        async fn list_workflow_runs_for_sha(
            &self,
            _: &str,
            _: &str,
            _: &str,
        ) -> Result<Vec<WorkflowRun>> {
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
        async fn list_jobs_for_run(
            &self,
            _: &str,
            _: &str,
            _: u64,
        ) -> Result<Vec<crate::gh::WorkflowJob>> {
            unimplemented!()
        }
        fn create_stack(
            &self,
            _: &str,
            _: &str,
            pr_numbers: &[u64],
        ) -> impl Future<Output = Result<PullRequestStack>> {
            self.record(Call::Created(pr_numbers.to_vec()));
            let mut next = self.next_stack_number.lock().unwrap();
            let number = *next;
            *next += 1;
            std::future::ready(Ok(PullRequestStack {
                number,
                pull_requests: pr_numbers
                    .iter()
                    .map(|&number| StackPullRequest {
                        number,
                        state: "open".into(),
                        merged_at: None,
                    })
                    .collect(),
            }))
        }
        fn append_to_stack(
            &self,
            _: &str,
            _: &str,
            stack_number: u64,
            pr_numbers: &[u64],
        ) -> impl Future<Output = Result<PullRequestStack>> {
            self.record(Call::Appended {
                stack: stack_number,
                prs: pr_numbers.to_vec(),
            });
            std::future::ready(Ok(PullRequestStack {
                number: stack_number,
                pull_requests: pr_numbers
                    .iter()
                    .map(|&number| StackPullRequest {
                        number,
                        state: "open".into(),
                        merged_at: None,
                    })
                    .collect(),
            }))
        }
        fn unstack_prs(
            &self,
            _: &str,
            _: &str,
            stack_number: u64,
        ) -> impl Future<Output = Result<Option<PullRequestStack>>> {
            if self.unstack_fails {
                return std::future::ready(Err(anyhow!("stack {stack_number} cannot be modified")));
            }
            self.record(Call::Unstacked {
                stack: stack_number,
            });
            let remaining = self
                .unstack_remaining
                .as_ref()
                .map(|members| PullRequestStack {
                    number: stack_number,
                    pull_requests: members
                        .iter()
                        .map(|&number| StackPullRequest {
                            number,
                            state: "open".into(),
                            merged_at: None,
                        })
                        .collect(),
                });
            std::future::ready(Ok(remaining))
        }
        async fn list_stacks(&self, _: &str, _: &str) -> Result<Vec<PullRequestStack>> {
            unimplemented!("callers pass the existing stacks in")
        }
        fn get_stack(
            &self,
            _: &str,
            _: &str,
            _: u64,
        ) -> impl Future<Output = Result<PullRequestStack>> {
            std::future::ready(
                self.read_back_stack
                    .clone()
                    .context("no read-back stack configured"),
            )
        }
    }

    fn target() -> Target {
        crate::gh::remote::target("git@github.com:o/r.git", None).unwrap()
    }

    /// A PR on `branch-<n>`, based on `base`, optionally already in a stack.
    fn pr(number: u64, base: &str, stack_number: Option<u64>) -> PrDetails {
        PrDetails {
            number,
            head_ref: format!("branch-{number}"),
            head_sha: format!("sha{number}"),
            base_ref: base.into(),
            title: format!("PR {number}"),
            html_url: format!("https://github.com/o/r/pull/{number}"),
            is_draft: false,
            auto_merge_request: None,
            stack_auto_merge_request: None,
            base_sha: "base".into(),
            head_user_login: None,
            head_repo_name: None,
            graphql_node_id: format!("node_{number}"),
            base_has_merge_queue: false,
            labels: vec![],
            reviewers: vec![],
            body: String::new(),
            stack_number,
            stack_base_ref: None,
            stack_position: None,
        }
    }

    /// A chain of PRs already based on each other: 1 on main, 2 on branch-1, ...
    fn aligned_chain(numbers: &[u64]) -> Vec<PrDetails> {
        numbers
            .iter()
            .enumerate()
            .map(|(i, &n)| match i.checked_sub(1).map(|prev| numbers[prev]) {
                Some(below) => pr(n, &format!("branch-{below}"), None),
                None => pr(n, "main", None),
            })
            .collect()
    }

    fn stack(number: u64, prs: &[u64]) -> PullRequestStack {
        PullRequestStack {
            number,
            pull_requests: prs
                .iter()
                .map(|&number| StackPullRequest {
                    number,
                    state: "open".into(),
                    merged_at: None,
                })
                .collect(),
        }
    }

    #[tokio::test]
    async fn creates_a_stack_for_a_fresh_chain() {
        let gh = FakeGh::new();
        let prs = aligned_chain(&[1, 2]);
        let results = create_stacks(&gh, &target(), &[vec![1, 2]], &prs, &[])
            .await
            .unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].outcome, ChainOutcome::Created(100));
        assert_eq!(gh.created(), vec![vec![1, 2]]);
        assert!(gh.retargeted().is_empty(), "chain was already aligned");
    }

    #[tokio::test]
    async fn identical_existing_stack_is_not_recreated() {
        let gh = FakeGh::new();
        let prs = aligned_chain(&[1, 2]);
        let results = create_stacks(&gh, &target(), &[vec![1, 2]], &prs, &[stack(7, &[1, 2])])
            .await
            .unwrap();

        assert_eq!(results[0].outcome, ChainOutcome::AlreadyExists);
        assert_eq!(gh.created(), [] as [Vec<u64>; 0]);
        assert_eq!(gh.unstacked(), [] as [u64; 0]);
    }

    #[tokio::test]
    async fn retargets_stale_base_refs_before_creating() {
        // #4 was inserted between #1 and #2 locally, so #2 still points at
        // #1's branch. Posting the chain without moving it is the 422 from
        // https://github.com/mrjones2014/jj-gh/issues/249.
        let gh = FakeGh::new();
        let prs = vec![
            pr(1, "main", None),
            pr(4, "branch-1", None),
            pr(2, "branch-1", None),
            pr(3, "branch-2", None),
        ];
        let results = create_stacks(&gh, &target(), &[vec![1, 4, 2, 3]], &prs, &[])
            .await
            .unwrap();

        assert_eq!(
            gh.retargeted(),
            vec![("node_2".to_string(), "branch-4".to_string())],
            "only #2 is misaligned"
        );
        assert_eq!(results[0].retargeted, vec![2]);
        assert_eq!(gh.created(), vec![vec![1, 4, 2, 3]]);
    }

    #[tokio::test]
    async fn bottom_pr_keeps_its_own_base() {
        let gh = FakeGh::new();
        let prs = vec![pr(1, "release-2.0", None), pr(2, "branch-1", None)];
        create_stacks(&gh, &target(), &[vec![1, 2]], &prs, &[])
            .await
            .unwrap();

        assert!(
            gh.retargeted().is_empty(),
            "nothing in the chain constrains the bottom PR's base"
        );
    }

    #[tokio::test]
    async fn grown_chain_is_appended_to_instead_of_recreated() {
        // Stack #7 holds [1, 2]; the local graph now says [1, 2, 3].
        let gh = FakeGh::new();
        let prs = aligned_chain(&[1, 2, 3])
            .into_iter()
            .enumerate()
            .map(|(i, mut pr)| {
                if i < 2 {
                    pr.stack_number = Some(7);
                }
                pr
            })
            .collect::<Vec<_>>();
        let results = create_stacks(&gh, &target(), &[vec![1, 2, 3]], &prs, &[stack(7, &[1, 2])])
            .await
            .unwrap();

        assert_eq!(
            results[0].outcome,
            ChainOutcome::Appended {
                stack_number: 7,
                added: vec![3],
            }
        );
        assert_eq!(gh.appended(), vec![(7, vec![3])]);
        assert!(gh.unstacked().is_empty(), "the stack was not reshaped");
        assert!(gh.created().is_empty(), "the stack was not recreated");
        assert!(gh.retargeted().is_empty(), "the chain was already aligned");
    }

    #[tokio::test]
    async fn untouched_chains_are_created_alongside_reshaped_ones() {
        let gh = FakeGh::new();
        let prs = vec![
            pr(1, "main", Some(7)),
            pr(2, "branch-1", Some(7)),
            pr(3, "branch-2", None),
            pr(5, "main", None),
            pr(6, "branch-5", None),
        ];
        let existing = [stack(7, &[1, 2])];
        let results = create_stacks(
            &gh,
            &target(),
            &[vec![1, 2, 3], vec![5, 6]],
            &prs,
            &existing,
        )
        .await
        .unwrap();

        let outcomes = results
            .iter()
            .map(|r| (r.chain.clone(), r.outcome.clone()))
            .collect::<Vec<_>>();
        assert_eq!(
            outcomes,
            vec![
                (
                    vec![1, 2, 3],
                    ChainOutcome::Appended {
                        stack_number: 7,
                        added: vec![3],
                    }
                ),
                (vec![5, 6], ChainOutcome::Created(100)),
            ]
        );
    }

    #[tokio::test]
    async fn missing_chain_member_is_an_error_not_a_bad_stack() {
        let gh = FakeGh::new();
        let prs = vec![pr(1, "main", None)];
        let err = create_stacks(&gh, &target(), &[vec![1, 2]], &prs, &[])
            .await
            .expect_err("PR #2 has no details");

        assert!(
            format!("{err:#}").contains("#2"),
            "unexpected error: {err:#}"
        );
        assert_eq!(gh.created(), [] as [Vec<u64>; 0]);
    }

    #[tokio::test]
    async fn unstacks_before_retargeting_and_retargets_before_creating() {
        // https://github.com/mrjones2014/jj-gh/issues/249: #4 was inserted
        // between #2 and #3, so #3's base must move. GitHub refuses that while
        // #3 is in stack #7, and refuses the new stack until the base has
        // moved, so only one order works.
        let gh = FakeGh::new();
        let prs = vec![
            pr(1, "main", Some(7)),
            pr(2, "branch-1", Some(7)),
            pr(4, "branch-2", None),
            pr(3, "branch-2", Some(7)),
        ];
        create_stacks(
            &gh,
            &target(),
            &[vec![1, 2, 4, 3]],
            &prs,
            &[stack(7, &[1, 2, 3])],
        )
        .await
        .unwrap();

        assert_eq!(
            gh.calls(),
            vec![
                Call::Unstacked { stack: 7 },
                Call::Retargeted {
                    node_id: "node_3".into(),
                    base: "branch-4".into(),
                },
                Call::Created(vec![1, 2, 4, 3]),
            ]
        );
    }

    #[tokio::test]
    async fn verbatim_stack_is_never_retargeted() {
        // GitHub will not let a stacked PR's base ref move, so a stack that
        // exists verbatim cannot have drifted; touching it would only fail.
        let gh = FakeGh::new();
        let prs = vec![pr(1, "main", Some(7)), pr(2, "branch-1", Some(7))];
        create_stacks(&gh, &target(), &[vec![1, 2]], &prs, &[stack(7, &[1, 2])])
            .await
            .unwrap();

        assert_eq!(gh.calls(), [] as [Call; 0]);
    }

    #[tokio::test]
    async fn failed_unstack_aborts_before_retargeting() {
        // Otherwise the user sees GitHub's opaque "part of a stack" error from
        // the retarget instead of the unstack failure that caused it.
        let gh = FakeGh::failing_unstack();
        let prs = vec![
            pr(1, "main", Some(7)),
            pr(2, "branch-1", Some(7)),
            pr(3, "branch-1", None),
        ];
        let err = create_stacks(&gh, &target(), &[vec![1, 3]], &prs, &[stack(7, &[1, 2, 3])])
            .await
            .expect_err("the unstack failed");

        let msg = format!("{err:#}");
        assert!(msg.contains("stack #7"), "unexpected error: {msg}");
        assert!(
            gh.retargeted().is_empty(),
            "must not retarget after failing"
        );
        assert_eq!(gh.created(), [] as [Vec<u64>; 0]);
    }

    #[tokio::test]
    async fn unstack_groups_one_call_per_stack() {
        let gh = FakeGh::new();
        let prs = vec![
            pr(1, "main", Some(7)),
            pr(2, "branch-1", Some(7)),
            pr(9, "main", Some(11)),
            pr(10, "main", None),
        ];
        let existing = [stack(7, &[1, 2]), stack(11, &[9])];
        let outcome = unstack(&gh, &target(), &[1, 2, 9, 10], &prs, &existing)
            .await
            .unwrap();

        assert_eq!(outcome.unstacked, vec![1, 2, 9]);
        let mut dissolved = outcome.dissolved;
        dissolved.sort_unstable();
        assert_eq!(dissolved, vec![7, 11]);
        assert_eq!(outcome.remaining, vec![]);
        let mut calls = gh.unstacked();
        calls.sort_unstable();
        assert_eq!(calls, vec![7, 11]);
    }

    #[tokio::test]
    async fn unstack_refuses_a_stack_with_a_nonlocal_member() {
        // Rebuilding would drop #99 from the stack, so the whole reshape has
        // to stop rather than silently cancel its membership.
        let gh = FakeGh::new();
        let prs = vec![pr(1, "main", Some(7)), pr(2, "branch-1", Some(7))];
        let err = unstack(&gh, &target(), &[1, 2], &prs, &[stack(7, &[1, 2, 99])])
            .await
            .expect_err("the stack has a nonlocal member");

        let msg = format!("{err:#}");
        assert!(msg.contains("#99"), "unexpected error: {msg}");
        assert!(msg.contains("not local"), "unexpected error: {msg}");
        assert!(
            gh.unstacked().is_empty(),
            "nothing should have been unstacked"
        );
    }

    #[tokio::test]
    async fn unstack_refuses_a_stack_with_auto_merge() {
        // Unstacking cancels GitHub's stack request; stop and let the user
        // decide how to move it instead of doing that as a side effect.
        let gh = FakeGh::new();
        let mut prs = vec![pr(1, "main", Some(7)), pr(2, "branch-1", Some(7))];
        prs[0].stack_auto_merge_request = Some(AutoMergeRequest {
            pr_number: 2,
            method: None,
        });
        let err = unstack(&gh, &target(), &[1], &prs, &[stack(7, &[1, 2])])
            .await
            .expect_err("the stack has auto-merge enabled");

        let msg = format!("{err:#}");
        assert!(msg.contains("auto-merge"), "unexpected error: {msg}");
        assert!(msg.contains("jj-gh pr edit"), "unexpected error: {msg}");
        assert!(
            gh.unstacked().is_empty(),
            "nothing should have been unstacked"
        );
    }

    #[tokio::test]
    async fn unstack_fails_when_github_leaves_an_open_pr_stacked() {
        let gh = FakeGh::leaving_unstacked(&[1, 2]);
        let prs = vec![
            pr(1, "main", Some(7)),
            pr(2, "branch-1", Some(7)),
            pr(3, "branch-2", None),
        ];
        let err = unstack(&gh, &target(), &[1, 3], &prs, &[stack(7, &[1, 2])])
            .await
            .expect_err("GitHub left #1 stacked");

        let msg = format!("{err:#}");
        assert!(msg.contains("#1"), "unexpected error: {msg}");
        assert!(msg.contains("jj-gh pr edit"), "unexpected error: {msg}");
    }

    #[tokio::test]
    async fn appending_to_an_auto_merge_stack_rechecks_coverage() {
        let mut prs = aligned_chain(&[1, 2, 3])
            .into_iter()
            .enumerate()
            .map(|(i, mut pr)| {
                if i < 2 {
                    pr.stack_number = Some(7);
                }
                pr
            })
            .collect::<Vec<_>>();
        prs[1].stack_auto_merge_request = Some(AutoMergeRequest {
            pr_number: 2,
            method: None,
        });
        let gh = FakeGh::reading_back(stack(7, &[1, 2, 3]), prs.clone());
        let results = create_stacks(&gh, &target(), &[vec![1, 2, 3]], &prs, &[stack(7, &[1, 2])])
            .await
            .unwrap();

        assert_eq!(
            results[0].outcome,
            ChainOutcome::Appended {
                stack_number: 7,
                added: vec![3],
            }
        );
        assert_eq!(gh.appended(), vec![(7, vec![3])]);
    }
}
