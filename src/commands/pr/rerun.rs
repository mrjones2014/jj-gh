//! `jj-gh pr rerun`: re-run CI on one or more PRs.
//!
//! Targets are PR numbers and/or jj revsets (which may match multiple
//! commits), or `--all` for every local PR. Per PR, the flags compose:
//!
//! - default: re-run failed jobs on completed runs with a retryable failure;
//!   in-progress runs are skipped with a note.
//! - `--all-jobs`: fully re-run every completed run, even successful ones.
//! - `--cancel`: additionally cancel in-progress runs that already contain a
//!   failed job ("doomed" runs), wait for them to finalize, then retry.
//! - `--cancel --all-jobs`: cancel ALL in-progress runs and restart the
//!   entire pipeline.
//!
//! Hard errors on one PR do not stop the others; a summary error is returned
//! at the end if any PR failed.

use crate::{
    cli::GlobalOpts,
    gh::{
        CiStatus, Gh, WorkflowJob, WorkflowRun, WorkflowRunStatus, pr_lookup::MissingPr,
        remote::Target,
    },
    model::Model,
    ui::Spinner,
    util::EvalWithCfgFallback,
};
use anyhow::{Context, Result, bail};
use jj_gh_config_derive::subcommand_args;
use std::collections::HashSet;
use std::time::{Duration, Instant};

subcommand_args! {
    pub struct RerunArgs {
        /// PR numbers and/or jj revsets to look up PRs from. A numeric
        /// argument is treated as a PR number; anything else is resolved as a
        /// revset, which may match multiple commits.
        #[arg(value_name = "PR_NUM|REVSET", group = "retry_target")]
        pub revsets: Vec<String>,

        /// Rerun CI on all local PRs (the default/--cancel/--all-jobs rules
        /// still apply to each PR).
        #[arg(long, group = "retry_target")]
        pub all: bool,

        /// Cancel in-progress runs that already contain at least one failed
        /// job, wait for them to finalize, then retry. With --all-jobs,
        /// cancels ALL in-progress runs and restarts the entire pipeline.
        #[arg(long)]
        pub cancel: bool,

        /// Re-run ALL jobs in the targeted workflow runs, even successful
        /// ones, instead of only failed jobs. Not to be confused with
        /// `--all`, which targets all local PRs rather than all jobs.
        #[arg(long)]
        pub all_jobs: bool,

        /// Seconds to wait for cancelled runs to finalize before re-running.
        /// Only meaningful with --cancel.
        #[arg(long, value_name = "SECS", default_value_t = 30, requires = "cancel")]
        pub cancel_timeout: u64,
    }
}

const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(2);

/// Describes how/which jobs to rerun
#[derive(Clone, Copy)]
struct Mode {
    cancel: bool,
    all_jobs: bool,
}

/// One PR to act on (owner/repo come from the shared [`Target`]).
struct RerunTarget {
    number: u64,
    head_sha: String,
    html_url: String,
}

/// Counters summed across PRs for the exit-code decision.
#[derive(Default)]
struct Outcome {
    retried: usize,
    skipped_in_progress: usize,
}

#[inline]
pub async fn run(model: &impl Model, args: &RerunArgs) -> Result<()> {
    run_with(model, args, DEFAULT_POLL_INTERVAL).await
}

async fn run_with(model: &impl Model, args: &RerunArgs, poll_interval: Duration) -> Result<()> {
    let gh = model.gh().await?;
    let RerunArgs {
        revsets,
        all,
        cancel,
        all_jobs,
        cancel_timeout,
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
    let upstream_remote = crate::gh::remote::resolved_upstream_remote(upstream_remote);
    let mode = Mode {
        cancel: *cancel,
        all_jobs: *all_jobs,
    };

    let (target, targets) = if *all {
        targets_from_all(model, remote, upstream_remote, mode).await?
    } else {
        targets_from_args(model, remote, upstream_remote, revsets).await?
    };

    if targets.is_empty() {
        log::info!("Nothing to rerun");
        return Ok(());
    }

    let multi = targets.len() > 1;
    let mut totals = Outcome::default();
    let mut errors = Vec::<anyhow::Error>::new();
    for t in &targets {
        let result = rerun_pr(
            gh,
            &target.owner,
            &target.repo,
            t,
            mode,
            *cancel_timeout,
            poll_interval,
        )
        .await
        .with_context(|| format!("PR #{}", t.number));
        if let Ok(outcome) = result
            .inspect_err(|e| {
                // Recoverable mid-loop: the remaining PRs are still tried and
                // the summary error below makes the command exit non-zero.
                if multi {
                    log::warn!("{e:#}");
                }
            })
            .map_err(|e| errors.push(e))
        {
            totals.retried += outcome.retried;
            totals.skipped_in_progress += outcome.skipped_in_progress;
        }
    }

    if let Some(error) = errors.pop() {
        if !multi {
            return Err(error);
        }
        bail!(
            "failed to rerun CI on {} of {} PR(s)",
            errors.len() + 1,
            targets.len()
        );
    }
    if totals.retried == 0 && totals.skipped_in_progress > 0 {
        bail!(
            "no runs retried; {} run(s) still in progress. Pass --cancel to \
             cancel runs that already have failed jobs. This is a GitHub \
             limitation; it does not let you retry jobs while the run is \
             still in progress.",
            totals.skipped_in_progress
        );
    }
    if totals.retried == 0 {
        log::info!("Nothing to rerun; no failed workflow runs found");
    }
    Ok(())
}

/// Resolve positional PR numbers and revsets into a deduped target list.
/// Setup errors here (bad revset, unresolvable single rev) fail the whole
/// command.
async fn targets_from_args(
    model: &impl Model,
    remote: &EvalWithCfgFallback<String>,
    upstream_remote: &str,
    revsets: &[String],
) -> Result<(Target, Vec<RerunTarget>)> {
    let (target, prs) = model
        .resolve_pr_targets(remote, upstream_remote, revsets, MissingPr::Skip)
        .await?;
    let targets = prs
        .into_iter()
        .map(|pr| RerunTarget {
            number: pr.number,
            head_sha: pr.head_sha,
            html_url: pr.html_url,
        })
        .collect();
    Ok((target, targets))
}

/// Resolve `--all` into a target list: every local open PR, pre-filtered by
/// CI status as an optimization (the per-PR rules would skip the filtered-out
/// PRs anyway).
async fn targets_from_all(
    model: &impl Model,
    remote: &EvalWithCfgFallback<String>,
    upstream_remote: &str,
    mode: Mode,
) -> Result<(Target, Vec<RerunTarget>)> {
    let local = model.local_pulls(remote, Some(upstream_remote)).await?;
    let targets = local
        .prs
        .into_iter()
        .filter(|pr| {
            if mode.all_jobs {
                true
            } else if mode.cancel {
                matches!(pr.ci_status, CiStatus::Failed | CiStatus::Pending)
            } else {
                matches!(pr.ci_status, CiStatus::Failed)
            }
        })
        .map(|pr| RerunTarget {
            number: pr.number,
            head_sha: pr.head_sha,
            html_url: pr.url,
        })
        .collect();
    Ok((local.target, targets))
}

async fn rerun_pr(
    gh: &impl Gh,
    owner: &str,
    repo: &str,
    pr: &RerunTarget,
    mode: Mode,
    cancel_timeout: u64,
    poll_interval: Duration,
) -> Result<Outcome> {
    let number = pr.number;
    let runs = gh
        .list_workflow_runs_for_sha(owner, repo, &pr.head_sha)
        .await
        .with_context(|| format!("listing workflow runs for PR #{number}"))?;
    let (mut completed, in_progress) = split_completed(runs);
    let mut outcome = Outcome::default();

    if mode.cancel && !in_progress.is_empty() {
        let doomed =
            doomed_in_progress(gh, owner, repo, number, &in_progress, mode.all_jobs).await?;
        let doomed_ids = doomed.iter().map(|r| r.id).collect::<HashSet<u64>>();
        for run in in_progress.iter().filter(|r| !doomed_ids.contains(&r.id)) {
            log::info!(
                "PR #{number}: skipping run {}: still in progress with no failed jobs",
                run.id
            );
            outcome.skipped_in_progress += 1;
        }
        if !doomed.is_empty() {
            completed = cancel_and_settle(
                gh,
                owner,
                repo,
                pr,
                &doomed,
                Duration::from_secs(cancel_timeout),
                poll_interval,
            )
            .await?;
        }
    } else {
        for run in &in_progress {
            log::info!(
                "PR #{number}: skipping run {}: still in progress (GitHub cannot \
                 retry jobs until the run completes; pass --cancel)",
                run.id
            );
            outcome.skipped_in_progress += 1;
        }
    }

    let candidates = rerun_candidates(&completed, mode.all_jobs);
    if candidates.is_empty() {
        log::info!("No workflow runs to rerun on PR #{number}");
    } else if mode.all_jobs {
        let spinner = Spinner::start(format!("restarting {} workflow run(s)", candidates.len()));
        let result = rerun_each(gh, owner, repo, number, &candidates).await;
        spinner.stop();
        result?;
        outcome.retried += candidates.len();
        log::info!(
            "Restarted {} workflow run(s) for PR #{number}",
            candidates.len()
        );
    } else {
        let spinner = Spinner::start(format!(
            "retrying failed jobs on {} workflow run(s)",
            candidates.len()
        ));
        let result = retry_each(gh, owner, repo, number, &candidates).await;
        spinner.stop();
        result?;
        outcome.retried += candidates.len();
        log::info!(
            "Retried failed jobs on {} workflow run(s) for PR #{number}",
            candidates.len()
        );
    }

    crate::ui::print_url(&pr.html_url);
    Ok(outcome)
}

/// Split runs into `(completed, in_progress)`, where "in progress" is any
/// non-`Completed` status (queued, waiting, etc.).
fn split_completed(runs: Vec<WorkflowRun>) -> (Vec<WorkflowRun>, Vec<WorkflowRun>) {
    runs.into_iter()
        .partition(|r| r.status == WorkflowRunStatus::Completed)
}

/// The in-progress runs that `--cancel` should cancel. With `--all-jobs`
/// every in-progress run is doomed (full pipeline restart); otherwise only
/// runs that already contain a failed job (they will finish failed anyway,
/// cancelling unlocks the retry now).
async fn doomed_in_progress<'a>(
    gh: &impl Gh,
    owner: &str,
    repo: &str,
    pr_number: u64,
    in_progress: &'a [WorkflowRun],
    all_jobs: bool,
) -> Result<Vec<&'a WorkflowRun>> {
    if all_jobs {
        return Ok(in_progress.iter().collect());
    }
    let spinner = Spinner::start(format!(
        "checking {} in-progress run(s) for failed jobs",
        in_progress.len()
    ));
    let result = collect_doomed(gh, owner, repo, pr_number, in_progress).await;
    spinner.stop();
    result
}

async fn collect_doomed<'a>(
    gh: &impl Gh,
    owner: &str,
    repo: &str,
    pr_number: u64,
    in_progress: &'a [WorkflowRun],
) -> Result<Vec<&'a WorkflowRun>> {
    let mut doomed = Vec::new();
    for run in in_progress {
        let jobs = gh
            .list_jobs_for_run(owner, repo, run.id)
            .await
            .with_context(|| {
                format!(
                    "listing jobs for workflow run {} on PR #{pr_number}",
                    run.id
                )
            })?;
        if let Some(job) = first_failed_job(&jobs) {
            log::debug!(
                "run {}: job {} already failed; run will be cancelled",
                run.id,
                job.id
            );
            doomed.push(run);
        }
    }
    Ok(doomed)
}

fn first_failed_job(jobs: &[WorkflowJob]) -> Option<&WorkflowJob> {
    jobs.iter().find(|j| {
        j.conclusion
            .is_some_and(crate::gh::WorkflowRunConclusion::is_retryable_failure)
    })
}

/// The completed runs worth re-running: all of them with `--all-jobs`, else
/// only those with a retryable failure conclusion.
fn rerun_candidates(completed: &[WorkflowRun], all_jobs: bool) -> Vec<&WorkflowRun> {
    completed
        .iter()
        .filter(|r| {
            all_jobs
                || r.conclusion
                    .is_some_and(crate::gh::WorkflowRunConclusion::is_retryable_failure)
        })
        .collect()
}

/// Cancel `doomed` runs, wait until exactly those run IDs report completed,
/// then re-list and return the now-completed runs.
async fn cancel_and_settle(
    gh: &impl Gh,
    owner: &str,
    repo: &str,
    pr: &RerunTarget,
    doomed: &[&WorkflowRun],
    timeout: Duration,
    poll_interval: Duration,
) -> Result<Vec<WorkflowRun>> {
    let spinner = Spinner::start(format!(
        "cancelling {} in-progress workflow run(s)",
        doomed.len()
    ));
    let cancel_result = cancel_each(gh, owner, repo, pr.number, doomed).await;
    if let Err(e) = cancel_result {
        spinner.stop();
        return Err(e);
    }
    let ids = doomed.iter().map(|r| r.id).collect::<HashSet<u64>>();
    let poll_result =
        poll_until_settled(gh, owner, repo, &pr.head_sha, &ids, timeout, poll_interval).await;
    spinner.stop();
    poll_result?;

    let runs = gh
        .list_workflow_runs_for_sha(owner, repo, &pr.head_sha)
        .await
        .with_context(|| format!("re-listing workflow runs for PR #{}", pr.number))?;
    let (completed, _) = split_completed(runs);
    Ok(completed)
}

async fn retry_each(
    gh: &impl Gh,
    owner: &str,
    repo: &str,
    pr_number: u64,
    failed: &[&WorkflowRun],
) -> Result<()> {
    for r in failed {
        gh.rerun_failed_jobs(owner, repo, r.id)
            .await
            .with_context(|| {
                format!(
                    "re-running failed jobs on workflow run {} for PR #{pr_number}",
                    r.id
                )
            })?;
    }
    Ok(())
}

async fn cancel_each(
    gh: &impl Gh,
    owner: &str,
    repo: &str,
    pr_number: u64,
    in_progress: &[&WorkflowRun],
) -> Result<()> {
    for r in in_progress {
        gh.cancel_workflow_run(owner, repo, r.id)
            .await
            .with_context(|| format!("cancelling workflow run {} on PR #{pr_number}", r.id))?;
    }
    Ok(())
}

async fn rerun_each(
    gh: &impl Gh,
    owner: &str,
    repo: &str,
    pr_number: u64,
    runs: &[&WorkflowRun],
) -> Result<()> {
    for r in runs {
        gh.rerun_workflow_run(owner, repo, r.id)
            .await
            .with_context(|| format!("re-running workflow run {} on PR #{pr_number}", r.id))?;
    }
    Ok(())
}

/// Poll until every run in `ids` reports `Completed`. Only the cancelled run
/// IDs are watched: other runs may legitimately keep executing (plain
/// `--cancel` leaves healthy in-progress runs alone).
async fn poll_until_settled(
    gh: &impl Gh,
    owner: &str,
    repo: &str,
    head_sha: &str,
    ids: &HashSet<u64>,
    timeout: Duration,
    interval: Duration,
) -> Result<()> {
    let start = Instant::now();
    loop {
        let runs = gh.list_workflow_runs_for_sha(owner, repo, head_sha).await?;
        if runs
            .iter()
            .filter(|r| ids.contains(&r.id))
            .all(|r| r.status == WorkflowRunStatus::Completed)
        {
            return Ok(());
        }
        if start.elapsed() >= timeout {
            bail!(
                "workflow runs did not finish cancelling within {}s; re-run \
                 `jj-gh pr rerun --cancel` once they settle",
                timeout.as_secs()
            );
        }
        tokio::time::sleep(interval).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        gh::{
            BaseLookup, CreatePrRequest, Label, PrCreated, PrDetails, PrSummary, PrWithCiStatus,
            UpdatePr, WorkflowRun, WorkflowRunConclusion, WorkflowRunStatus,
        },
        jj::{CommitInfo, PushedBookmark},
        model::TestModel,
    };
    use std::collections::HashMap;
    use std::path::Path;
    use std::sync::Mutex;

    fn pr_details(number: u64, sha: &str) -> PrDetails {
        PrDetails {
            is_draft: false,
            auto_merge: false,
            auto_merge_method: None,
            number,
            title: "t".into(),
            html_url: format!("https://github.com/o/r/pull/{number}"),
            head_ref: "feat".into(),
            base_ref: "main".into(),
            head_sha: sha.into(),
            base_sha: "base".into(),
            head_user_login: Some("o".into()),
            head_repo_name: Some("r".into()),
            graphql_node_id: "node".into(),
            in_merge_queue: false,
            labels: Vec::<Label>::new(),
            reviewers: vec![],
            body: String::new(),
            stack_number: None,
        }
    }

    fn wr(
        id: u64,
        status: WorkflowRunStatus,
        conclusion: Option<WorkflowRunConclusion>,
    ) -> WorkflowRun {
        WorkflowRun {
            id,
            status,
            conclusion,
        }
    }

    fn job(id: u64, conclusion: Option<WorkflowRunConclusion>) -> WorkflowJob {
        WorkflowJob { id, conclusion }
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
            unimplemented!("rerun resolves revsets, not single revs")
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
            // resolve_target asks for default + upstream remote URLs.
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
        fn pushed_bookmarks(
            &self,
            _remote: &str,
        ) -> impl Future<Output = Result<Vec<PushedBookmark>>> {
            std::future::ready(Ok(vec![PushedBookmark {
                name: "feat".into(),
                local_commit_id: "local".into(),
            }]))
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
    }

    #[derive(Default)]
    struct Calls {
        cancelled: Vec<u64>,
        rerun: Vec<u64>,
        rerun_failed: Vec<u64>,
        list_jobs: Vec<u64>,
        listed_shas: Vec<String>,
    }

    struct FakeGh {
        pr: PrDetails,
        prs_by_number: HashMap<u64, PrDetails>,
        /// `head_spec` -> open PR number, for `find_open_pr`.
        open_prs: HashMap<String, u64>,
        local_prs: Mutex<Vec<PrWithCiStatus>>,
        /// Successive responses to `list_workflow_runs_for_sha`. Last entry
        /// repeats once exhausted.
        list_responses: Mutex<Vec<Vec<WorkflowRun>>>,
        /// `list_workflow_runs_for_sha` fails for these head SHAs.
        fail_list_for_shas: HashSet<String>,
        jobs_by_run: HashMap<u64, Vec<WorkflowJob>>,
        calls: Mutex<Calls>,
    }

    impl FakeGh {
        fn new(pr: PrDetails, responses: Vec<Vec<WorkflowRun>>) -> Self {
            Self {
                pr,
                prs_by_number: HashMap::new(),
                open_prs: HashMap::new(),
                local_prs: Mutex::new(Vec::new()),
                list_responses: Mutex::new(responses),
                fail_list_for_shas: HashSet::new(),
                jobs_by_run: HashMap::new(),
                calls: Mutex::new(Calls::default()),
            }
        }

        fn with_local_prs(self, prs: Vec<PrWithCiStatus>) -> Self {
            *self.local_prs.lock().unwrap() = prs;
            self
        }

        fn with_pr(mut self, pr: PrDetails) -> Self {
            self.prs_by_number.insert(pr.number, pr);
            self
        }

        fn with_open_pr(mut self, head_spec: &str, number: u64) -> Self {
            self.open_prs.insert(head_spec.into(), number);
            self
        }

        fn with_jobs(mut self, run_id: u64, jobs: Vec<WorkflowJob>) -> Self {
            self.jobs_by_run.insert(run_id, jobs);
            self
        }

        fn with_list_failure(mut self, sha: &str) -> Self {
            self.fail_list_for_shas.insert(sha.into());
            self
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
        async fn disable_auto_merge(&self, _id: &str) -> Result<()> {
            unimplemented!()
        }
        fn get_pr(&self, _o: &str, _r: &str, n: u64) -> impl Future<Output = Result<PrDetails>> {
            std::future::ready(Ok(self
                .prs_by_number
                .get(&n)
                .cloned()
                .unwrap_or_else(|| self.pr.clone())))
        }
        async fn get_pr_diff(&self, _: &str, _: &str, _: u64) -> Result<String> {
            unimplemented!()
        }
        async fn enable_auto_merge(
            &self,
            _id: &str,
            _m: crate::config::AutoMergeMethod,
        ) -> Result<()> {
            unimplemented!()
        }
        fn local_pulls(
            &self,
            _o: &str,
            _r: &str,
            _head_owner: &str,
            _b: &[String],
        ) -> impl Future<Output = Result<Vec<PrWithCiStatus>>> {
            std::future::ready(Ok(std::mem::take(&mut *self.local_prs.lock().unwrap())))
        }
        fn list_workflow_runs_for_sha(
            &self,
            _o: &str,
            _r: &str,
            sha: &str,
        ) -> impl Future<Output = Result<Vec<WorkflowRun>>> {
            self.calls.lock().unwrap().listed_shas.push(sha.into());
            if self.fail_list_for_shas.contains(sha) {
                return std::future::ready(Err(anyhow::anyhow!("boom listing runs for {sha}")));
            }
            let mut q = self.list_responses.lock().unwrap();
            std::future::ready(if q.len() > 1 {
                Ok(q.remove(0))
            } else {
                Ok(q.first().cloned().unwrap_or_default())
            })
        }
        fn list_jobs_for_run(
            &self,
            _o: &str,
            _r: &str,
            run_id: u64,
        ) -> impl Future<Output = Result<Vec<WorkflowJob>>> {
            self.calls.lock().unwrap().list_jobs.push(run_id);
            std::future::ready(Ok(self
                .jobs_by_run
                .get(&run_id)
                .cloned()
                .unwrap_or_default()))
        }
        fn cancel_workflow_run(
            &self,
            _o: &str,
            _r: &str,
            id: u64,
        ) -> impl Future<Output = Result<()>> {
            self.calls.lock().unwrap().cancelled.push(id);
            std::future::ready(Ok(()))
        }
        fn rerun_workflow_run(
            &self,
            _o: &str,
            _r: &str,
            id: u64,
        ) -> impl Future<Output = Result<()>> {
            self.calls.lock().unwrap().rerun.push(id);
            std::future::ready(Ok(()))
        }
        fn rerun_failed_jobs(
            &self,
            _o: &str,
            _r: &str,
            id: u64,
        ) -> impl Future<Output = Result<()>> {
            self.calls.lock().unwrap().rerun_failed.push(id);
            std::future::ready(Ok(()))
        }
        async fn create_stack(
            &self,
            _: &str,
            _: &str,
            _: &[u64],
        ) -> Result<crate::gh::PullRequestStack> {
            unimplemented!()
        }
        async fn unstack_prs(&self, _: &str, _: &str, _: u64, _: &[u64]) -> Result<()> {
            unimplemented!()
        }
        async fn list_stacks(&self, _: &str, _: &str) -> Result<Vec<crate::gh::PullRequestStack>> {
            unimplemented!()
        }
    }

    fn args(targets: &[&str], cancel: bool, timeout: u64) -> RerunArgs {
        RerunArgs {
            revsets: targets.iter().map(|t| (*t).into()).collect(),
            all: false,
            cancel,
            all_jobs: false,
            cancel_timeout: timeout,
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

    fn all_args(cancel: bool, timeout: u64) -> RerunArgs {
        RerunArgs {
            all: true,
            ..args(&[], cancel, timeout)
        }
    }

    fn local_pr(number: u64, sha: &str, status: CiStatus) -> PrWithCiStatus {
        PrWithCiStatus {
            id: format!("node-{number}"),
            number,
            url: format!("https://github.com/o/r/pull/{number}"),
            title: "t".into(),
            head_ref_name: "feat".into(),
            head_owner: Some("o".into()),
            head_sha: sha.into(),
            base_ref_name: "main".into(),
            is_draft: false,
            merged: false,
            is_in_merge_queue: false,
            ci_status: status,
            auto_merge_enabled: false,
        }
    }

    #[tokio::test]
    async fn default_retries_only_failed_completed_runs() {
        let pr = pr_details(42, "deadbeef");
        let runs = vec![
            wr(
                1,
                WorkflowRunStatus::Completed,
                Some(WorkflowRunConclusion::Success),
            ),
            wr(
                2,
                WorkflowRunStatus::Completed,
                Some(WorkflowRunConclusion::Failure),
            ),
            wr(
                3,
                WorkflowRunStatus::Completed,
                Some(WorkflowRunConclusion::TimedOut),
            ),
            wr(
                4,
                WorkflowRunStatus::Completed,
                Some(WorkflowRunConclusion::Skipped),
            ),
        ];
        let gh = FakeGh::new(pr, vec![runs]);
        let jj = FakeJj::default();

        run_with(
            &TestModel::without_git(&jj, &gh),
            &args(&["42"], false, 30),
            Duration::from_millis(5),
        )
        .await
        .expect("default path should succeed");

        let calls = gh.calls.lock().unwrap();
        assert_eq!(calls.rerun_failed, vec![2, 3]);
        assert_eq!(calls.cancelled, [] as [u64; 0]);
        assert_eq!(calls.rerun, [] as [u64; 0]);
        assert_eq!(calls.list_jobs, [] as [u64; 0]);
    }

    #[tokio::test]
    async fn default_skips_in_progress_and_retries_failed() {
        let pr = pr_details(7, "abc");
        let runs = vec![
            wr(
                1,
                WorkflowRunStatus::Completed,
                Some(WorkflowRunConclusion::Failure),
            ),
            wr(2, WorkflowRunStatus::InProgress, None),
        ];
        let gh = FakeGh::new(pr, vec![runs]);
        let jj = FakeJj::default();

        run_with(
            &TestModel::without_git(&jj, &gh),
            &args(&["7"], false, 30),
            Duration::from_millis(5),
        )
        .await
        .expect("should retry what it can and skip the in-progress run");

        let calls = gh.calls.lock().unwrap();
        assert_eq!(calls.rerun_failed, vec![1]);
        assert_eq!(calls.cancelled, [] as [u64; 0]);
        assert_eq!(calls.rerun, [] as [u64; 0]);
    }

    #[tokio::test]
    async fn default_errors_when_nothing_retried_and_runs_in_progress() {
        let pr = pr_details(7, "abc");
        let runs = vec![
            wr(1, WorkflowRunStatus::InProgress, None),
            wr(2, WorkflowRunStatus::Queued, None),
        ];
        let gh = FakeGh::new(pr, vec![runs]);
        let jj = FakeJj::default();

        let err = run_with(
            &TestModel::without_git(&jj, &gh),
            &args(&["7"], false, 30),
            Duration::from_millis(5),
        )
        .await
        .expect_err("nothing retried while CI in progress should be an error");
        let msg = format!("{err:#}");
        assert!(msg.contains("still in progress"), "msg: {msg}");

        let calls = gh.calls.lock().unwrap();
        assert_eq!(calls.cancelled, [] as [u64; 0]);
        assert_eq!(calls.rerun, [] as [u64; 0]);
        assert_eq!(calls.rerun_failed, [] as [u64; 0]);
    }

    #[tokio::test]
    async fn default_all_success_does_nothing() {
        let pr = pr_details(9, "sha");
        let runs = vec![
            wr(
                1,
                WorkflowRunStatus::Completed,
                Some(WorkflowRunConclusion::Success),
            ),
            wr(
                2,
                WorkflowRunStatus::Completed,
                Some(WorkflowRunConclusion::Success),
            ),
        ];
        let gh = FakeGh::new(pr, vec![runs]);
        let jj = FakeJj::default();

        run_with(
            &TestModel::without_git(&jj, &gh),
            &args(&["9"], false, 30),
            Duration::from_millis(5),
        )
        .await
        .unwrap();

        let calls = gh.calls.lock().unwrap();
        assert_eq!(calls.rerun_failed, [] as [u64; 0]);
    }

    #[tokio::test]
    async fn all_jobs_reruns_all_completed_and_skips_in_progress() {
        let pr = pr_details(10, "sha");
        let runs = vec![
            wr(
                1,
                WorkflowRunStatus::Completed,
                Some(WorkflowRunConclusion::Success),
            ),
            wr(
                2,
                WorkflowRunStatus::Completed,
                Some(WorkflowRunConclusion::Failure),
            ),
            wr(3, WorkflowRunStatus::InProgress, None),
        ];
        let gh = FakeGh::new(pr, vec![runs]);
        let jj = FakeJj::default();

        run_with(
            &TestModel::without_git(&jj, &gh),
            &RerunArgs {
                all_jobs: true,
                ..args(&["10"], false, 30)
            },
            Duration::from_millis(5),
        )
        .await
        .expect("all-jobs path should rerun completed runs");

        let calls = gh.calls.lock().unwrap();
        assert_eq!(calls.rerun, vec![1, 2]);
        assert_eq!(calls.rerun_failed, [] as [u64; 0]);
        assert_eq!(calls.cancelled, [] as [u64; 0]);
    }

    #[tokio::test]
    async fn cancel_cancels_only_doomed_runs_then_retries_failed() {
        let pr = pr_details(11, "sha");
        // Run 1: in progress with a failed job (doomed). Run 2: in progress,
        // healthy. Run 3: completed failure.
        let initial = vec![
            wr(1, WorkflowRunStatus::InProgress, None),
            wr(2, WorkflowRunStatus::InProgress, None),
            wr(
                3,
                WorkflowRunStatus::Completed,
                Some(WorkflowRunConclusion::Failure),
            ),
        ];
        let settled = vec![
            wr(
                1,
                WorkflowRunStatus::Completed,
                Some(WorkflowRunConclusion::Cancelled),
            ),
            wr(2, WorkflowRunStatus::InProgress, None),
            wr(
                3,
                WorkflowRunStatus::Completed,
                Some(WorkflowRunConclusion::Failure),
            ),
        ];
        let gh = FakeGh::new(pr, vec![initial, settled])
            .with_jobs(
                1,
                vec![
                    job(100, Some(WorkflowRunConclusion::Failure)),
                    job(101, None),
                ],
            )
            .with_jobs(2, vec![job(200, Some(WorkflowRunConclusion::Success))]);
        let jj = FakeJj::default();

        run_with(
            &TestModel::without_git(&jj, &gh),
            &args(&["11"], true, 5),
            Duration::from_millis(1),
        )
        .await
        .expect("cancel path should cancel doomed run and retry failures");

        let calls = gh.calls.lock().unwrap();
        assert_eq!(calls.list_jobs, vec![1, 2]);
        assert_eq!(calls.cancelled, vec![1]);
        assert_eq!(calls.rerun_failed, vec![1, 3]);
        assert_eq!(calls.rerun, [] as [u64; 0]);
    }

    #[tokio::test]
    async fn cancel_with_no_in_progress_skips_job_checks() {
        let pr = pr_details(21, "sha");
        let runs = vec![
            wr(
                1,
                WorkflowRunStatus::Completed,
                Some(WorkflowRunConclusion::Success),
            ),
            wr(
                2,
                WorkflowRunStatus::Completed,
                Some(WorkflowRunConclusion::Failure),
            ),
        ];
        let gh = FakeGh::new(pr, vec![runs]);
        let jj = FakeJj::default();

        run_with(
            &TestModel::without_git(&jj, &gh),
            &args(&["21"], true, 30),
            Duration::from_millis(1),
        )
        .await
        .unwrap();

        let calls = gh.calls.lock().unwrap();
        assert_eq!(calls.list_jobs, [] as [u64; 0]);
        assert_eq!(calls.cancelled, [] as [u64; 0]);
        assert_eq!(calls.rerun_failed, vec![2]);
    }

    #[tokio::test]
    async fn cancel_all_jobs_cancels_everything_then_reruns_all() {
        let pr = pr_details(11, "sha");
        let initial = vec![
            wr(
                1,
                WorkflowRunStatus::Completed,
                Some(WorkflowRunConclusion::Failure),
            ),
            wr(2, WorkflowRunStatus::InProgress, None),
            wr(3, WorkflowRunStatus::Queued, None),
        ];
        // Second list call (poll): still in progress.
        let still_running = vec![
            wr(
                1,
                WorkflowRunStatus::Completed,
                Some(WorkflowRunConclusion::Failure),
            ),
            wr(2, WorkflowRunStatus::InProgress, None),
            wr(
                3,
                WorkflowRunStatus::Completed,
                Some(WorkflowRunConclusion::Cancelled),
            ),
        ];
        // Third list call (poll): all completed.
        let all_done = vec![
            wr(
                1,
                WorkflowRunStatus::Completed,
                Some(WorkflowRunConclusion::Failure),
            ),
            wr(
                2,
                WorkflowRunStatus::Completed,
                Some(WorkflowRunConclusion::Cancelled),
            ),
            wr(
                3,
                WorkflowRunStatus::Completed,
                Some(WorkflowRunConclusion::Cancelled),
            ),
        ];
        // Fourth: same all-done state for the post-poll re-list before rerun.
        let post_poll = all_done.clone();
        let gh = FakeGh::new(pr, vec![initial, still_running, all_done, post_poll]);
        let jj = FakeJj::default();

        run_with(
            &TestModel::without_git(&jj, &gh),
            &RerunArgs {
                all_jobs: true,
                ..args(&["11"], true, 5)
            },
            Duration::from_millis(1),
        )
        .await
        .expect("cancel --all-jobs should restart the full pipeline");

        let calls = gh.calls.lock().unwrap();
        assert_eq!(calls.list_jobs, [] as [u64; 0]);
        assert_eq!(calls.cancelled, vec![2, 3]);
        assert_eq!(calls.rerun, vec![1, 2, 3]);
        assert_eq!(calls.rerun_failed, [] as [u64; 0]);
    }

    #[tokio::test]
    async fn cancel_path_times_out_if_runs_never_settle() {
        let pr = pr_details(13, "sha");
        let stuck = vec![wr(1, WorkflowRunStatus::InProgress, None)];
        let gh = FakeGh::new(pr, vec![stuck])
            .with_jobs(1, vec![job(100, Some(WorkflowRunConclusion::Failure))]);
        let jj = FakeJj::default();

        let err = run_with(
            &TestModel::without_git(&jj, &gh),
            // 0s timeout: the very first poll iteration sees elapsed >= timeout and errors.
            &args(&["13"], true, 0),
            Duration::from_millis(1),
        )
        .await
        .expect_err("should time out");
        let msg = format!("{err:#}");
        assert!(msg.contains("did not finish"), "msg: {msg}");

        let calls = gh.calls.lock().unwrap();
        assert_eq!(calls.cancelled, vec![1]);
        assert_eq!(calls.rerun, [] as [u64; 0]);
    }

    #[tokio::test]
    async fn revset_resolves_commits_to_prs_and_skips_prless_commits() {
        let pr = pr_details(42, "deadbeef");
        let runs = vec![wr(
            5,
            WorkflowRunStatus::Completed,
            Some(WorkflowRunConclusion::Failure),
        )];
        let gh = FakeGh::new(pr, vec![runs]).with_open_pr("o:feat", 42);
        let jj = FakeJj::with_revset_commits(vec![
            commit("aaa", &[]),
            commit("bbb", &["nopr"]),
            commit("ccc", &["feat"]),
        ]);

        run_with(
            &TestModel::without_git(&jj, &gh),
            &args(&["trunk()..@"], false, 30),
            Duration::from_millis(1),
        )
        .await
        .expect("multi-commit revset should skip PR-less commits");

        let calls = gh.calls.lock().unwrap();
        assert_eq!(calls.rerun_failed, vec![5]);
        assert_eq!(calls.listed_shas, vec!["deadbeef"]);
    }

    #[tokio::test]
    async fn single_commit_revset_without_bookmark_is_a_hard_error() {
        let gh = FakeGh::new(pr_details(0, "unused"), vec![]);
        let jj = FakeJj::with_revset_commits(vec![commit("aaa", &[])]);

        let err = run_with(
            &TestModel::without_git(&jj, &gh),
            &args(&["aaa"], false, 30),
            Duration::from_millis(1),
        )
        .await
        .expect_err("single rev without bookmark should fail");
        let msg = format!("{err:#}");
        assert!(msg.contains("no local bookmark"), "msg: {msg}");
    }

    #[tokio::test]
    async fn single_commit_revset_without_open_pr_is_a_hard_error() {
        let gh = FakeGh::new(pr_details(0, "unused"), vec![]);
        let jj = FakeJj::with_revset_commits(vec![commit("aaa", &["feat"])]);

        let err = run_with(
            &TestModel::without_git(&jj, &gh),
            &args(&["aaa"], false, 30),
            Duration::from_millis(1),
        )
        .await
        .expect_err("single rev without open PR should fail");
        let msg = format!("{err:#}");
        assert!(msg.contains("no open PR"), "msg: {msg}");
    }

    #[tokio::test]
    async fn empty_revset_is_a_hard_error() {
        let gh = FakeGh::new(pr_details(0, "unused"), vec![]);
        let jj = FakeJj::default();

        let err = run_with(
            &TestModel::without_git(&jj, &gh),
            &args(&["none()"], false, 30),
            Duration::from_millis(1),
        )
        .await
        .expect_err("empty revset should fail");
        let msg = format!("{err:#}");
        assert!(msg.contains("matched no commits"), "msg: {msg}");
    }

    #[tokio::test]
    async fn targets_are_deduped_across_args() {
        let runs = vec![wr(
            5,
            WorkflowRunStatus::Completed,
            Some(WorkflowRunConclusion::Failure),
        )];
        let gh = FakeGh::new(pr_details(42, "sha42"), vec![runs])
            .with_pr(pr_details(42, "sha42"))
            .with_pr(pr_details(43, "sha43"))
            .with_open_pr("o:feat", 42)
            .with_open_pr("o:feat2", 43);
        let jj =
            FakeJj::with_revset_commits(vec![commit("aaa", &["feat"]), commit("bbb", &["feat2"])]);

        run_with(
            &TestModel::without_git(&jj, &gh),
            &args(&["42", "somerev"], false, 30),
            Duration::from_millis(1),
        )
        .await
        .expect("dedupe path should succeed");

        let calls = gh.calls.lock().unwrap();
        // PR 42 appears both as a number and via the revset; it must only be
        // processed once, so exactly two PRs (42, 43) are retried.
        assert_eq!(calls.listed_shas, vec!["sha42", "sha43"]);
        assert_eq!(calls.rerun_failed, vec![5, 5]);
    }

    #[tokio::test]
    async fn per_pr_errors_do_not_stop_other_prs() {
        let runs = vec![wr(
            7,
            WorkflowRunStatus::Completed,
            Some(WorkflowRunConclusion::Failure),
        )];
        let gh = FakeGh::new(pr_details(0, "unused"), vec![runs])
            .with_pr(pr_details(1, "bad"))
            .with_pr(pr_details(2, "good"))
            .with_list_failure("bad");
        let jj = FakeJj::default();

        let err = run_with(
            &TestModel::without_git(&jj, &gh),
            &args(&["1", "2"], false, 30),
            Duration::from_millis(1),
        )
        .await
        .expect_err("should report the failed PR");
        let msg = format!("{err:#}");
        assert!(msg.contains("1 of 2"), "msg: {msg}");

        let calls = gh.calls.lock().unwrap();
        assert_eq!(calls.rerun_failed, vec![7]);
    }

    #[tokio::test]
    async fn all_retries_only_local_prs_with_failed_ci() {
        let runs = vec![wr(
            2,
            WorkflowRunStatus::Completed,
            Some(WorkflowRunConclusion::Failure),
        )];
        let gh = FakeGh::new(pr_details(0, "unused"), vec![runs]).with_local_prs(vec![
            local_pr(41, "success", CiStatus::Success),
            local_pr(42, "failed", CiStatus::Failed),
            local_pr(43, "pending", CiStatus::Pending),
        ]);
        let jj = FakeJj::default();

        run_with(
            &TestModel::without_git(&jj, &gh),
            &all_args(false, 30),
            Duration::from_millis(1),
        )
        .await
        .expect("all-local path should retry failed PRs");

        let calls = gh.calls.lock().unwrap();
        assert_eq!(calls.listed_shas, vec!["failed"]);
        assert_eq!(calls.rerun_failed, vec![2]);
        assert_eq!(calls.cancelled, [] as [u64; 0]);
        assert_eq!(calls.rerun, [] as [u64; 0]);
    }

    #[tokio::test]
    async fn all_with_cancel_also_checks_pending_prs() {
        let runs = vec![wr(
            2,
            WorkflowRunStatus::Completed,
            Some(WorkflowRunConclusion::Failure),
        )];
        let gh = FakeGh::new(pr_details(0, "unused"), vec![runs]).with_local_prs(vec![
            local_pr(41, "success", CiStatus::Success),
            local_pr(42, "failed", CiStatus::Failed),
            local_pr(43, "pending", CiStatus::Pending),
        ]);
        let jj = FakeJj::default();

        run_with(
            &TestModel::without_git(&jj, &gh),
            &all_args(true, 5),
            Duration::from_millis(1),
        )
        .await
        .expect("all-local cancel path should check failed and pending PRs");

        let calls = gh.calls.lock().unwrap();
        assert_eq!(calls.listed_shas, vec!["failed", "pending"]);
    }

    #[tokio::test]
    async fn all_with_all_jobs_checks_every_local_pr() {
        let runs = vec![wr(
            2,
            WorkflowRunStatus::Completed,
            Some(WorkflowRunConclusion::Success),
        )];
        let gh = FakeGh::new(pr_details(0, "unused"), vec![runs]).with_local_prs(vec![
            local_pr(41, "success", CiStatus::Success),
            local_pr(42, "failed", CiStatus::Failed),
        ]);
        let jj = FakeJj::default();

        run_with(
            &TestModel::without_git(&jj, &gh),
            &RerunArgs {
                all_jobs: true,
                ..all_args(false, 30)
            },
            Duration::from_millis(1),
        )
        .await
        .expect("all + all-jobs should rerun every local PR");

        let calls = gh.calls.lock().unwrap();
        assert_eq!(calls.listed_shas, vec!["success", "failed"]);
        assert_eq!(calls.rerun, vec![2, 2]);
    }

    #[tokio::test]
    async fn all_with_no_matching_prs_is_a_clean_noop() {
        let gh = FakeGh::new(pr_details(0, "unused"), vec![]).with_local_prs(vec![local_pr(
            41,
            "success",
            CiStatus::Success,
        )]);
        let jj = FakeJj::default();

        run_with(
            &TestModel::without_git(&jj, &gh),
            &all_args(false, 30),
            Duration::from_millis(1),
        )
        .await
        .expect("nothing to do should be a clean exit");

        let calls = gh.calls.lock().unwrap();
        assert_eq!(calls.listed_shas, [] as [String; 0]);
    }

    #[test]
    fn rerun_candidates_respects_all_jobs() {
        let completed = vec![
            wr(
                1,
                WorkflowRunStatus::Completed,
                Some(WorkflowRunConclusion::Success),
            ),
            wr(
                2,
                WorkflowRunStatus::Completed,
                Some(WorkflowRunConclusion::Failure),
            ),
        ];
        let failed_only = rerun_candidates(&completed, false);
        assert_eq!(
            failed_only.iter().map(|r| r.id).collect::<Vec<_>>(),
            vec![2]
        );
        let all = rerun_candidates(&completed, true);
        assert_eq!(all.iter().map(|r| r.id).collect::<Vec<_>>(), vec![1, 2]);
    }

    #[test]
    fn first_failed_job_ignores_running_and_successful_jobs() {
        assert!(
            first_failed_job(&[
                job(1, None),
                job(2, Some(WorkflowRunConclusion::Success)),
                job(3, Some(WorkflowRunConclusion::Skipped)),
            ])
            .is_none()
        );
        assert_eq!(
            first_failed_job(&[job(1, None), job(2, Some(WorkflowRunConclusion::Failure)),])
                .map(|j| j.id),
            Some(2)
        );
    }
}
