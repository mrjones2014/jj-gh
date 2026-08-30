//! Turning detected stack chains (see [`crate::gh::stack_detect`]) into real
//! GitHub PR stacks.
//!
//! This module only talks to the API and reports what it did; rendering the
//! outcome is the caller's job.

use crate::gh::{Gh, PrDetails, remote::Target};
use anyhow::{Result, bail};
use std::collections::HashSet;

/// What [`create_stacks`] does about PRs that already belong to a stack.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlreadyStacked {
    /// Refuse to do anything and let the caller point the user at `--force`.
    /// Used by a bare `jj pr stack`, where surprising the user is worse than
    /// doing nothing.
    Bail,
    /// Unstack them first, then create the new stack. Used by
    /// `jj pr stack --force`.
    Unstack,
    /// Leave them alone, and drop any chain that contains one. Used by the
    /// automatic linking in `pr create` and `pr restack`, which run on every
    /// invocation and must not fail or nag when there is nothing to do.
    Skip,
}

/// What happened to one chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChainOutcome {
    /// A new stack was created, carrying this stack number.
    Created(u64),
    /// The exact same stack already exists on GitHub, so nothing was done.
    AlreadyExists,
    /// [`AlreadyStacked::Skip`]: a PR in the chain belongs to some other
    /// stack, so the chain was left untouched.
    LeftAlone,
}

/// One chain and what happened to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainResult {
    pub chain: Vec<u64>,
    pub outcome: ChainOutcome,
}

/// The PRs in `pr_details` that already belong to a stack.
fn already_stacked_prs(pr_details: &[PrDetails]) -> Vec<u64> {
    pr_details
        .iter()
        .filter(|pr| pr.stack_number.is_some())
        .map(|pr| pr.number)
        .collect()
}

/// Whether `chain` contains a PR that is already part of some stack.
fn chain_is_already_stacked(chain: &[u64], already_stacked: &[u64]) -> bool {
    chain.iter().any(|num| already_stacked.contains(num))
}

/// Remove every PR in `pr_details` from the stack it currently belongs to.
///
/// Failures are logged rather than propagated: an unstack that does not land
/// surfaces as a create failure right after, which is the more useful error.
async fn unstack_all(gh: &impl Gh, target: &Target, pr_details: &[PrDetails]) {
    let unique_stacks = pr_details
        .iter()
        .filter_map(|pr| pr.stack_number)
        .collect::<HashSet<u64>>();

    for stack_num in unique_stacks {
        let pr_numbers = pr_details
            .iter()
            .filter(|pr| pr.stack_number == Some(stack_num))
            .map(|pr| pr.number)
            .collect::<Vec<u64>>();
        if !pr_numbers.is_empty()
            && let Err(e) = gh
                .unstack_prs(&target.owner, &target.repo, stack_num, &pr_numbers)
                .await
        {
            log::warn!("Failed to unstack PRs from stack #{stack_num}: {e:#}");
        }
    }
}

/// Create a GitHub stack for each chain, honoring `mode` for PRs that are
/// already stacked, and skipping chains that GitHub already has verbatim.
///
/// Returns one [`ChainResult`] per input chain, in the input order.
///
/// # Errors
///
/// Returns an error if `mode` is [`AlreadyStacked::Bail`] and any PR is
/// already stacked, or if a GitHub API call fails.
pub async fn create_stacks(
    gh: &impl Gh,
    target: &Target,
    chains: &[Vec<u64>],
    pr_details: &[PrDetails],
    mode: AlreadyStacked,
) -> Result<Vec<ChainResult>> {
    let already_stacked = already_stacked_prs(pr_details);

    if !already_stacked.is_empty() {
        match mode {
            AlreadyStacked::Unstack => unstack_all(gh, target, pr_details).await,
            AlreadyStacked::Bail => {
                bail!("Some PRs are already in stacks. Use --force to unstack and restack them.");
            }
            AlreadyStacked::Skip => {}
        }
    }

    // `Skip` mode: a chain whose PRs already live in a stack is left as-is.
    // Re-creating it is an API error, and `jj pr stack --force` is the escape
    // hatch for reshaping one.
    let mut results = Vec::with_capacity(chains.len());
    let pending = chains
        .iter()
        .filter(|chain| {
            let leave_alone =
                mode == AlreadyStacked::Skip && chain_is_already_stacked(chain, &already_stacked);
            if leave_alone {
                results.push(ChainResult {
                    chain: (*chain).clone(),
                    outcome: ChainOutcome::LeftAlone,
                });
            }
            !leave_alone
        })
        .collect::<Vec<&Vec<u64>>>();

    if pending.is_empty() {
        return Ok(results);
    }

    let existing_stacks = gh.list_stacks(&target.owner, &target.repo).await?;

    for chain in pending {
        let stack_exists = existing_stacks.iter().any(|stack| {
            let existing_prs = stack
                .pull_requests
                .iter()
                .map(|pr| pr.number)
                .collect::<Vec<u64>>();
            existing_prs == *chain
        });

        let outcome = if stack_exists {
            ChainOutcome::AlreadyExists
        } else {
            let stack = gh.create_stack(&target.owner, &target.repo, chain).await?;
            ChainOutcome::Created(stack.number)
        };
        results.push(ChainResult {
            chain: chain.clone(),
            outcome,
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
            StackPullRequest, UpdatePr, WorkflowRun,
        },
    };
    use std::sync::Mutex;

    struct FakeGh {
        existing_stacks: Vec<PullRequestStack>,
        created: Mutex<Vec<Vec<u64>>>,
        unstacked: Mutex<Vec<(u64, Vec<u64>)>>,
        next_stack_number: Mutex<u64>,
    }

    impl FakeGh {
        fn new(existing_stacks: Vec<PullRequestStack>) -> Self {
            Self {
                existing_stacks,
                created: Mutex::new(Vec::new()),
                unstacked: Mutex::new(Vec::new()),
                next_stack_number: Mutex::new(100),
            }
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
        async fn update_pr(&self, _: UpdatePr) -> Result<()> {
            unimplemented!()
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
            self.created.lock().unwrap().push(pr_numbers.to_vec());
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
            self.unstacked
                .lock()
                .unwrap()
                .push((stack_number, pr_numbers.to_vec()));
            Ok(())
        }
        async fn list_stacks(&self, _: &str, _: &str) -> Result<Vec<PullRequestStack>> {
            Ok(self.existing_stacks.clone())
        }
    }

    fn target() -> Target {
        crate::gh::remote::target("git@github.com:o/r.git", None).unwrap()
    }

    fn pr(number: u64, stack_number: Option<u64>) -> PrDetails {
        PrDetails {
            number,
            head_ref: format!("branch-{number}"),
            head_sha: format!("sha{number}"),
            base_ref: "main".into(),
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
        let gh = FakeGh::new(vec![]);
        let prs = [pr(1, None), pr(2, None)];
        let results = create_stacks(&gh, &target(), &[vec![1, 2]], &prs, AlreadyStacked::Skip)
            .await
            .unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].outcome, ChainOutcome::Created(100));
        assert_eq!(*gh.created.lock().unwrap(), vec![vec![1, 2]]);
    }

    #[tokio::test]
    async fn identical_existing_stack_is_not_recreated() {
        let gh = FakeGh::new(vec![stack(7, &[1, 2])]);
        let prs = [pr(1, None), pr(2, None)];
        let results = create_stacks(&gh, &target(), &[vec![1, 2]], &prs, AlreadyStacked::Skip)
            .await
            .unwrap();

        assert_eq!(results[0].outcome, ChainOutcome::AlreadyExists);
        assert!(gh.created.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn skip_mode_leaves_already_stacked_chains_alone() {
        let gh = FakeGh::new(vec![stack(7, &[1, 2])]);
        // The chain grew a third PR, so it no longer matches stack #7.
        let prs = [pr(1, Some(7)), pr(2, Some(7)), pr(3, None)];
        let results = create_stacks(&gh, &target(), &[vec![1, 2, 3]], &prs, AlreadyStacked::Skip)
            .await
            .unwrap();

        assert_eq!(results[0].outcome, ChainOutcome::LeftAlone);
        assert!(gh.created.lock().unwrap().is_empty());
        assert!(gh.unstacked.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn skip_mode_still_creates_untouched_chains() {
        let gh = FakeGh::new(vec![stack(7, &[1, 2])]);
        let prs = [pr(1, Some(7)), pr(2, Some(7)), pr(3, None), pr(4, None)];
        let results = create_stacks(
            &gh,
            &target(),
            &[vec![1, 2], vec![3, 4]],
            &prs,
            AlreadyStacked::Skip,
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
                (vec![1, 2], ChainOutcome::LeftAlone),
                (vec![3, 4], ChainOutcome::Created(100)),
            ]
        );
        assert_eq!(*gh.created.lock().unwrap(), vec![vec![3, 4]]);
    }

    #[tokio::test]
    async fn bail_mode_refuses_when_a_pr_is_already_stacked() {
        let gh = FakeGh::new(vec![stack(7, &[1, 2])]);
        let prs = [pr(1, Some(7)), pr(2, Some(7))];
        let err = create_stacks(&gh, &target(), &[vec![1, 2]], &prs, AlreadyStacked::Bail)
            .await
            .expect_err("already-stacked PRs should be rejected");

        assert!(err.to_string().contains("--force"));
        assert!(gh.created.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn unstack_mode_unstacks_then_creates() {
        let gh = FakeGh::new(vec![]);
        let prs = [pr(1, Some(7)), pr(2, Some(7)), pr(3, None)];
        let results = create_stacks(
            &gh,
            &target(),
            &[vec![1, 2, 3]],
            &prs,
            AlreadyStacked::Unstack,
        )
        .await
        .unwrap();

        assert_eq!(results[0].outcome, ChainOutcome::Created(100));
        assert_eq!(*gh.unstacked.lock().unwrap(), vec![(7, vec![1, 2])]);
        assert_eq!(*gh.created.lock().unwrap(), vec![vec![1, 2, 3]]);
    }
}
