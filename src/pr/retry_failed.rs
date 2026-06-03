//! `jj-gh pr retry-failed`: re-run failed CI on a PR's head commit.
//!
//! Default: refuses to act if any workflow run is still in progress, because
//! GitHub will reject `rerun-failed-jobs` until the run is `completed`.
//!
//! `--cancel`: cancels in-progress runs, waits for them to finalize, then
//! re-runs every workflow run (full pipeline restart).

use crate::{
    cli::GlobalOpts,
    gh::{Gh, WorkflowRun, WorkflowRunStatus},
    jj::Jj,
    pr,
    ui::Spinner,
};
use anyhow::{Context, Result, bail};
use jj_gh_config_derive::subcommand_args;
use std::time::{Duration, Instant};

subcommand_args! {
    pub struct RetryFailedArgs {
        /// PR number, or revision ID to look up a PR from.
        #[arg(value_name = "PR_NUM|REV")]
        pub number_or_rev: String,

        /// Cancel any in-progress runs and restart the entire pipeline.
        /// Without this flag, the command fails if CI has not yet completed.
        #[arg(long)]
        pub cancel: bool,

        /// Seconds to wait for cancelled runs to finalize before re-running.
        /// Only meaningful with --cancel.
        #[arg(long, value_name = "SECS", default_value_t = 30, requires = "cancel")]
        pub cancel_timeout: u64,
    }
}

const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(2);

pub async fn run<J, G>(jj: &J, gh: &G, args: &RetryFailedArgs) -> Result<()>
where
    J: Jj,
    G: Gh,
{
    run_with(jj, gh, args, DEFAULT_POLL_INTERVAL).await
}

async fn run_with<J, G>(
    jj: &J,
    gh: &G,
    args: &RetryFailedArgs,
    poll_interval: Duration,
) -> Result<()>
where
    J: Jj,
    G: Gh,
{
    let RetryFailedArgs {
        number_or_rev,
        cancel,
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

    let (pr, target) =
        pr::resolve_pr_with_target(jj, gh, remote, upstream_remote, number_or_rev).await?;
    let owner = &target.owner;
    let repo = &target.repo;

    let runs = gh
        .list_workflow_runs_for_sha(owner, repo, &pr.head_sha)
        .await
        .with_context(|| format!("listing workflow runs for PR #{}", pr.number))?;

    let in_progress: Vec<&WorkflowRun> = runs
        .iter()
        .filter(|r| r.status != WorkflowRunStatus::Completed)
        .collect();

    if *cancel {
        if !in_progress.is_empty() {
            let spinner = Spinner::start(format!(
                "cancelling {} in-progress workflow run(s)",
                in_progress.len()
            ));
            let cancel_result = cancel_each(gh, owner, repo, pr.number, &in_progress).await;
            if let Err(e) = cancel_result {
                spinner.stop().await;
                return Err(e);
            }
            let poll_result = poll_until_completed(
                gh,
                owner,
                repo,
                &pr.head_sha,
                Duration::from_secs(*cancel_timeout),
                poll_interval,
            )
            .await;
            spinner.stop().await;
            poll_result?;
        }

        let final_runs = gh
            .list_workflow_runs_for_sha(owner, repo, &pr.head_sha)
            .await
            .with_context(|| format!("re-listing workflow runs for PR #{}", pr.number))?;

        if final_runs.is_empty() {
            log::info!("No workflow runs found for PR #{}", pr.number);
        } else {
            let spinner =
                Spinner::start(format!("restarting {} workflow run(s)", final_runs.len()));
            let result = rerun_each(gh, owner, repo, pr.number, &final_runs).await;
            spinner.stop().await;
            result?;

            log::info!(
                "Cancelled {} run(s) and restarted {} workflow run(s) for PR #{}",
                in_progress.len(),
                final_runs.len(),
                pr.number
            );
        }
    } else {
        if !in_progress.is_empty() {
            bail!(
                "CI still in progress for PR #{} ({} run(s) not yet completed). \
                Pass --cancel to cancel and restart the pipeline. \
                This is a GitHub limitation; it does not let you retry jobs \
                while jobs are still in progress.",
                pr.number,
                in_progress.len()
            );
        }
        retry_failed_jobs(gh, owner, repo, pr.number, &runs).await?;
    }

    println!("{}", pr.html_url);
    Ok(())
}

async fn retry_failed_jobs<G: Gh>(
    gh: &G,
    owner: &str,
    repo: &str,
    pr_number: u64,
    runs: &[WorkflowRun],
) -> Result<()> {
    let failed: Vec<&WorkflowRun> = runs
        .iter()
        .filter(|r| {
            r.conclusion
                .is_some_and(super::super::gh::WorkflowRunConclusion::is_retryable_failure)
        })
        .collect();

    if failed.is_empty() {
        log::info!("No failed workflow runs to retry on PR #{pr_number}");
        return Ok(());
    }

    let spinner = Spinner::start(format!(
        "retrying failed jobs on {} workflow run(s)",
        failed.len()
    ));
    let result = retry_each(gh, owner, repo, pr_number, &failed).await;
    spinner.stop().await;
    result?;

    log::info!(
        "Retried failed jobs on {} workflow run(s) for PR #{pr_number}",
        failed.len()
    );
    Ok(())
}

async fn retry_each<G: Gh>(
    gh: &G,
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

async fn cancel_each<G: Gh>(
    gh: &G,
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

async fn rerun_each<G: Gh>(
    gh: &G,
    owner: &str,
    repo: &str,
    pr_number: u64,
    runs: &[WorkflowRun],
) -> Result<()> {
    for r in runs {
        gh.rerun_workflow_run(owner, repo, r.id)
            .await
            .with_context(|| format!("re-running workflow run {} on PR #{pr_number}", r.id))?;
    }
    Ok(())
}

async fn poll_until_completed<G: Gh>(
    gh: &G,
    owner: &str,
    repo: &str,
    head_sha: &str,
    timeout: Duration,
    interval: Duration,
) -> Result<()> {
    let start = Instant::now();
    loop {
        let runs = gh.list_workflow_runs_for_sha(owner, repo, head_sha).await?;
        if runs
            .iter()
            .all(|r| r.status == WorkflowRunStatus::Completed)
        {
            return Ok(());
        }
        if start.elapsed() >= timeout {
            bail!(
                "workflow runs did not finish cancelling within {}s; re-run \
                 `jj pr retry-failed --cancel` once they settle",
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
    };
    use std::path::{Path, PathBuf};
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
            head_sha: sha.into(),
            head_user_login: Some("o".into()),
            head_repo_name: Some("r".into()),
            graphql_node_id: "node".into(),
            in_merge_queue: false,
            labels: Vec::<Label>::new(),
            reviewers: vec![],
            body: String::new(),
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

    struct FakeJj {
        workspace_root: PathBuf,
    }

    impl FakeJj {
        fn new() -> Self {
            Self {
                workspace_root: PathBuf::from("/tmp"),
            }
        }
    }

    impl crate::jj::Jj for FakeJj {
        async fn resolve_rev(&self, _rev: &str) -> Result<CommitInfo> {
            unimplemented!("retry-failed tests pass PR numbers, not revs")
        }
        async fn stacked_ancestor_bookmark(&self, _rev: &str) -> Result<Option<String>> {
            unimplemented!()
        }
        async fn first_commit_description(&self, _revset: &str) -> Result<String> {
            unimplemented!()
        }
        async fn remote_url(&self, name: &str) -> Result<Option<String>> {
            // resolve_pr_with_target asks for default + upstream remote URLs.
            if name == "origin" {
                Ok(Some("git@github.com:o/r.git".into()))
            } else {
                Ok(None)
            }
        }
        async fn remote_bookmark_sha(&self, _: &str, _: &str) -> Result<Option<String>> {
            unimplemented!()
        }
        async fn push(&self, _rev: &str) -> Result<()> {
            unimplemented!()
        }
        async fn trunk_branch(&self) -> Result<Option<String>> {
            unimplemented!()
        }
        async fn workspace_root(&self) -> Result<&PathBuf> {
            Ok(&self.workspace_root)
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
        ) -> Result<String> {
            unimplemented!()
        }
    }

    #[derive(Default)]
    struct Calls {
        cancelled: Vec<u64>,
        rerun: Vec<u64>,
        rerun_failed: Vec<u64>,
    }

    struct FakeGh {
        pr: PrDetails,
        /// Successive responses to `list_workflow_runs_for_sha`. Last entry
        /// repeats once exhausted.
        list_responses: Mutex<Vec<Vec<WorkflowRun>>>,
        calls: Mutex<Calls>,
    }

    impl FakeGh {
        fn new(pr: PrDetails, responses: Vec<Vec<WorkflowRun>>) -> Self {
            Self {
                pr,
                list_responses: Mutex::new(responses),
                calls: Mutex::new(Calls::default()),
            }
        }
    }

    impl Gh for FakeGh {
        async fn find_open_pr(&self, _o: &str, _r: &str, _h: &str) -> Result<Option<PrSummary>> {
            unimplemented!()
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
        async fn get_pr(&self, _o: &str, _r: &str, _n: u64) -> Result<PrDetails> {
            Ok(self.pr.clone())
        }
        async fn enable_auto_merge(
            &self,
            _id: &str,
            _m: crate::config::AutoMergeMethod,
        ) -> Result<()> {
            unimplemented!()
        }
        async fn local_pulls(
            &self,
            _o: &str,
            _r: &str,
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
            let mut q = self.list_responses.lock().unwrap();
            if q.len() > 1 {
                Ok(q.remove(0))
            } else {
                Ok(q.first().cloned().unwrap_or_default())
            }
        }
        async fn cancel_workflow_run(&self, _o: &str, _r: &str, id: u64) -> Result<()> {
            self.calls.lock().unwrap().cancelled.push(id);
            Ok(())
        }
        async fn rerun_workflow_run(&self, _o: &str, _r: &str, id: u64) -> Result<()> {
            self.calls.lock().unwrap().rerun.push(id);
            Ok(())
        }
        async fn rerun_failed_jobs(&self, _o: &str, _r: &str, id: u64) -> Result<()> {
            self.calls.lock().unwrap().rerun_failed.push(id);
            Ok(())
        }
    }

    fn args(number: &str, cancel: bool, timeout: u64) -> RetryFailedArgs {
        RetryFailedArgs {
            number_or_rev: number.into(),
            cancel,
            cancel_timeout: timeout,
            globals: GlobalOpts {
                verbose: 0,
                quiet: false,
                log_level: None,
                remote: "origin".into(),
                upstream_remote: "upstream".into(),
                gh_askpass: None,
                askpass_timeout_secs: 20,
            },
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
        let jj = FakeJj::new();

        run_with(&jj, &gh, &args("42", false, 30), Duration::from_millis(5))
            .await
            .expect("default path should succeed");

        let calls = gh.calls.lock().unwrap();
        assert_eq!(calls.rerun_failed, vec![2, 3]);
        assert!(calls.cancelled.is_empty());
        assert!(calls.rerun.is_empty());
    }

    #[tokio::test]
    async fn default_errors_when_runs_in_progress() {
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
        let jj = FakeJj::new();

        let err = run_with(&jj, &gh, &args("7", false, 30), Duration::from_millis(5))
            .await
            .expect_err("should refuse while CI in progress");
        let msg = format!("{err:#}");
        assert!(msg.contains("still in progress"), "msg: {msg}");

        let calls = gh.calls.lock().unwrap();
        assert!(calls.cancelled.is_empty());
        assert!(calls.rerun.is_empty());
        assert!(calls.rerun_failed.is_empty());
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
        let jj = FakeJj::new();

        run_with(&jj, &gh, &args("9", false, 30), Duration::from_millis(5))
            .await
            .unwrap();

        let calls = gh.calls.lock().unwrap();
        assert!(calls.rerun_failed.is_empty());
    }

    #[tokio::test]
    async fn cancel_path_cancels_then_reruns_all_after_settle() {
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
        let jj = FakeJj::new();

        run_with(&jj, &gh, &args("11", true, 5), Duration::from_millis(1))
            .await
            .expect("cancel path should succeed once runs settle");

        let calls = gh.calls.lock().unwrap();
        assert_eq!(calls.cancelled, vec![2, 3]);
        assert_eq!(calls.rerun, vec![1, 2, 3]);
        assert!(calls.rerun_failed.is_empty());
    }

    #[tokio::test]
    async fn cancel_path_times_out_if_runs_never_settle() {
        let pr = pr_details(13, "sha");
        let stuck = vec![wr(1, WorkflowRunStatus::InProgress, None)];
        let gh = FakeGh::new(pr, vec![stuck]);
        let jj = FakeJj::new();

        let err = run_with(
            &jj,
            &gh,
            // 0s timeout: the very first poll iteration sees elapsed >= timeout and errors.
            &args("13", true, 0),
            Duration::from_millis(1),
        )
        .await
        .expect_err("should time out");
        let msg = format!("{err:#}");
        assert!(msg.contains("did not finish"), "msg: {msg}");

        let calls = gh.calls.lock().unwrap();
        assert_eq!(calls.cancelled, vec![1]);
        assert!(calls.rerun.is_empty());
    }

    #[tokio::test]
    async fn cancel_path_with_no_in_progress_just_reruns_all() {
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
        // Same list returned for both the initial and re-list before rerun.
        let gh = FakeGh::new(pr, vec![runs.clone(), runs]);
        let jj = FakeJj::new();

        run_with(&jj, &gh, &args("21", true, 30), Duration::from_millis(1))
            .await
            .unwrap();

        let calls = gh.calls.lock().unwrap();
        assert!(calls.cancelled.is_empty());
        assert_eq!(calls.rerun, vec![1, 2]);
    }
}
