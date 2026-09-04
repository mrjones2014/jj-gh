//! Turning the detected local shape (see [`crate::gh::stack_detect`]) into
//! real GitHub PR stacks.
//!
//! Two API rules dictate the order of every write in here, and they point in
//! opposite directions:
//!
//! - `createStack` only accepts PR numbers whose base refs *already* chain
//!   into each other, so the bases have to move first;
//! - `updatePullRequest` refuses to change the base ref of a PR that is in a
//!   stack ("Cannot change the base branch because the pull request is part of
//!   a stack"), so the stack has to go first.
//!
//! The only order satisfying both is **unstack, then retarget, then create**.
//! Getting it wrong surfaces as `422: Pull requests must form a stack` one way
//! and the "part of a stack" GraphQL error the other.
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

use crate::gh::{Gh, PrDetails, PullRequestStack, UpdatePr, remote::Target};
use anyhow::{Context, Result};
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChainOutcome {
    /// A new stack was created, carrying this stack number.
    Created(u64),
    /// GitHub already has this exact stack, so nothing was created.
    AlreadyExists,
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

/// Remove `pr_numbers` from whatever stacks they currently belong to, one call
/// per stack. A stack left with no PRs is dissolved by GitHub.
///
/// Only PRs present in `pr_details` and currently carrying a stack number are
/// touched, so a stack member with no local bookmark keeps its membership
/// rather than being silently unstacked.
///
/// Returns the PRs that were unstacked, sorted.
///
/// # Errors
///
/// Propagates the API error. This used to warn and continue, but a failed
/// unstack now guarantees the *next* call fails with GitHub's opaque "part of
/// a stack" message, so the original failure is the one worth surfacing. Note
/// that a success is not a full guarantee either: GitHub silently leaves
/// behind PRs it cannot unstack, such as ones queued for merge.
pub async fn unstack(
    gh: &impl Gh,
    target: &Target,
    pr_numbers: &[u64],
    pr_details: &[PrDetails],
) -> Result<Vec<u64>> {
    let wanted = pr_numbers.iter().copied().collect::<HashSet<u64>>();

    let mut by_stack = HashMap::<u64, Vec<u64>>::new();
    for pr in pr_details.iter().filter(|pr| wanted.contains(&pr.number)) {
        if let Some(stack_number) = pr.stack_number {
            by_stack.entry(stack_number).or_default().push(pr.number);
        }
    }

    let mut unstacked = Vec::new();
    for (stack_number, members) in by_stack {
        gh.unstack_prs(&target.owner, &target.repo, stack_number, &members)
            .await
            .with_context(|| {
                let list = members
                    .iter()
                    .map(|n| format!("#{n}"))
                    .collect::<Vec<String>>()
                    .join(", ");
                format!("unstacking {list} from stack #{stack_number}")
            })?;
        unstacked.extend(members);
    }
    unstacked.sort_unstable();
    Ok(unstacked)
}

/// Whether GitHub already has `chain` as a stack, verbatim.
#[must_use]
pub fn stack_exists(existing_stacks: &[PullRequestStack], chain: &[u64]) -> bool {
    existing_stacks.iter().any(|stack| {
        stack
            .pull_requests
            .iter()
            .map(|pr| pr.number)
            .eq(chain.iter().copied())
    })
}

/// The PR numbers in `stack`, in order.
#[must_use]
pub fn stack_members(stack: &PullRequestStack) -> Vec<u64> {
    stack.pull_requests.iter().map(|pr| pr.number).collect()
}

/// Create a GitHub stack for each chain, unstacking and retargeting base refs
/// as needed to satisfy the API's preconditions.
///
/// `existing_stacks` comes from [`Gh::list_stacks`]; callers fetch it so they
/// can show which chains are actually new before applying anything. Callers
/// that unstack ahead of this must drop the stacks they dissolved from it,
/// otherwise a chain whose stack was just torn down is skipped as
/// [`ChainOutcome::AlreadyExists`] and never rebuilt.
///
/// Returns one [`ChainResult`] per input chain, in the input order.
///
/// # Errors
///
/// Returns an error if an unstack, a base-ref update, or a create fails.
pub async fn create_stacks(
    gh: &impl Gh,
    target: &Target,
    chains: &[Vec<u64>],
    pr_details: &[PrDetails],
    existing_stacks: &[PullRequestStack],
) -> Result<Vec<ChainResult>> {
    let prs = by_number(pr_details);

    let mut results = Vec::with_capacity(chains.len());
    for chain in chains {
        let mut retargeted = Vec::new();
        let outcome = if stack_exists(existing_stacks, chain) {
            // Nothing to do, and nothing can have drifted: GitHub does not
            // allow a stacked PR's base ref to move in the first place.
            ChainOutcome::AlreadyExists
        } else {
            // Members may still belong to a stale stack (the chain grew,
            // shrank, or was reordered). That membership has to go before the
            // base refs can move, and the base refs have to chain before the
            // new stack can be asserted. See the module docs.
            unstack(gh, target, chain, pr_details).await?;
            retargeted = align_chain_bases(gh, chain, &prs).await?;
            let stack = gh.create_stack(&target.owner, &target.repo, chain).await?;
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
            BaseLookup, CreatePrRequest, PrCreated, PrSummary, PrWithCiStatus, PullRequestStack,
            StackPullRequest, WorkflowRun,
        },
    };
    use anyhow::anyhow;
    use std::sync::Mutex;

    /// One recorded write. Kept in a single ordered log because the property
    /// that broke in #249 was the *order* of these calls, not their contents.
    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Call {
        Unstacked { stack: u64, prs: Vec<u64> },
        Retargeted { node_id: String, base: String },
        Created(Vec<u64>),
    }

    struct FakeGh {
        calls: Mutex<Vec<Call>>,
        next_stack_number: Mutex<u64>,
        unstack_fails: bool,
    }

    impl FakeGh {
        fn new() -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                next_stack_number: Mutex::new(100),
                unstack_fails: false,
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

        fn unstacked(&self) -> Vec<(u64, Vec<u64>)> {
            self.calls()
                .into_iter()
                .filter_map(|c| match c {
                    Call::Unstacked { stack, prs } => Some((stack, prs)),
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
        async fn update_pr(&self, req: UpdatePr) -> Result<()> {
            let base = req
                .base_ref_name
                .expect("stack_create only updates base refs");
            self.record(Call::Retargeted {
                node_id: req.pr_node_id,
                base,
            });
            Ok(())
        }
        async fn set_draft(&self, _: &str, _: bool) -> Result<()> {
            unimplemented!()
        }
        async fn disable_auto_merge(&self, _: &str) -> Result<()> {
            unimplemented!()
        }
        async fn get_pr(&self, _: &str, _: &str, _: u64) -> Result<PrDetails> {
            unimplemented!()
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
        async fn create_stack(
            &self,
            _: &str,
            _: &str,
            pr_numbers: &[u64],
        ) -> Result<PullRequestStack> {
            self.record(Call::Created(pr_numbers.to_vec()));
            let mut next = self.next_stack_number.lock().unwrap();
            let number = *next;
            *next += 1;
            Ok(PullRequestStack {
                number,
                pull_requests: pr_numbers
                    .iter()
                    .map(|&number| StackPullRequest { number })
                    .collect(),
            })
        }
        async fn unstack_prs(
            &self,
            _: &str,
            _: &str,
            stack_number: u64,
            pr_numbers: &[u64],
        ) -> Result<()> {
            if self.unstack_fails {
                return Err(anyhow!("stack {stack_number} cannot be modified"));
            }
            self.record(Call::Unstacked {
                stack: stack_number,
                prs: pr_numbers.to_vec(),
            });
            Ok(())
        }
        async fn list_stacks(&self, _: &str, _: &str) -> Result<Vec<PullRequestStack>> {
            unimplemented!("callers pass the existing stacks in")
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
            auto_merge: false,
            auto_merge_method: None,
            base_sha: "base".into(),
            head_user_login: None,
            head_repo_name: None,
            graphql_node_id: format!("node_{number}"),
            in_merge_queue: false,
            labels: vec![],
            reviewers: vec![],
            body: String::new(),
            stack_number,
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
                .map(|&number| StackPullRequest { number })
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
        assert!(gh.created().is_empty());
        assert!(gh.unstacked().is_empty());
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
    async fn grown_chain_unstacks_the_old_stack_then_recreates() {
        // Stack #7 holds [1, 2]; the local graph now says [1, 2, 3].
        let gh = FakeGh::new();
        let prs = vec![
            pr(1, "main", Some(7)),
            pr(2, "branch-1", Some(7)),
            pr(3, "branch-2", None),
        ];
        let results = create_stacks(&gh, &target(), &[vec![1, 2, 3]], &prs, &[stack(7, &[1, 2])])
            .await
            .unwrap();

        assert_eq!(results[0].outcome, ChainOutcome::Created(100));
        assert_eq!(gh.unstacked(), vec![(7, vec![1, 2])]);
        assert_eq!(gh.created(), vec![vec![1, 2, 3]]);
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
            .map(|r| (r.chain.clone(), r.outcome))
            .collect::<Vec<_>>();
        assert_eq!(
            outcomes,
            vec![
                (vec![1, 2, 3], ChainOutcome::Created(100)),
                (vec![5, 6], ChainOutcome::Created(101)),
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
        assert!(gh.created().is_empty());
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
                Call::Unstacked {
                    stack: 7,
                    prs: vec![1, 2, 3],
                },
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

        assert!(gh.calls().is_empty());
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
        let err = create_stacks(&gh, &target(), &[vec![1, 2, 3]], &prs, &[])
            .await
            .expect_err("the unstack failed");

        let msg = format!("{err:#}");
        assert!(msg.contains("stack #7"), "unexpected error: {msg}");
        assert!(
            gh.retargeted().is_empty(),
            "must not retarget after failing"
        );
        assert!(gh.created().is_empty());
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
        let unstacked = unstack(&gh, &target(), &[1, 2, 9, 10], &prs).await.unwrap();

        assert_eq!(unstacked, vec![1, 2, 9]);
        let mut calls = gh.unstacked();
        calls.sort_by_key(|(stack, _)| *stack);
        assert_eq!(calls, vec![(7, vec![1, 2]), (11, vec![9])]);
    }

    #[tokio::test]
    async fn unstack_ignores_prs_it_has_no_details_for() {
        // A stack member with no local bookmark keeps its membership rather
        // than being silently unstacked.
        let gh = FakeGh::new();
        let prs = vec![pr(2, "branch-1", Some(11))];
        let unstacked = unstack(&gh, &target(), &[2, 99], &prs).await.unwrap();

        assert_eq!(unstacked, vec![2]);
        assert_eq!(gh.unstacked(), vec![(11, vec![2])]);
    }
}
