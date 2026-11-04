//! Shared test fakes for the one-shot rerun tests and the watch-mode tests.
//!
//! `FakeGh` queues successive responses per API so multi-tick watch tests can
//! script a timeline; single-response one-shot tests just queue one entry
//! (the last entry repeats once the queue is exhausted).

use crate::{
    cli::GlobalOpts,
    commands::pr::rerun::RerunArgs,
    gh::{
        BaseLookup, CiStatus, CreatePrRequest, Gh, Label, PrCreated, PrDetails, PrSummary,
        PrWithCiStatus, UpdatePr, WorkflowJob, WorkflowRun, WorkflowRunConclusion,
    },
    jj::{CommitInfo, PushedBookmark},
};
use anyhow::Result;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Mutex;

pub(crate) fn pr_details(number: u64, sha: &str) -> PrDetails {
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

pub(crate) fn wr(
    id: u64,
    status: crate::gh::WorkflowRunStatus,
    conclusion: Option<WorkflowRunConclusion>,
) -> WorkflowRun {
    WorkflowRun {
        id,
        status,
        conclusion,
    }
}

pub(crate) fn job(id: u64, conclusion: Option<WorkflowRunConclusion>) -> WorkflowJob {
    WorkflowJob { id, conclusion }
}

pub(crate) fn commit(change_id: &str, bookmarks: &[&str]) -> CommitInfo {
    CommitInfo {
        change_id: change_id.into(),
        commit_id: format!("{change_id}-commit"),
        description: "d".into(),
        bookmarks: bookmarks.iter().map(|b| (*b).into()).collect(),
    }
}

pub(crate) fn local_pr(number: u64, sha: &str, status: CiStatus) -> PrWithCiStatus {
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
        check_counts: crate::gh::CheckCounts::default(),
        auto_merge_enabled: false,
    }
}

pub(crate) fn args(targets: &[&str], cancel: bool, timeout: u64) -> RerunArgs {
    RerunArgs {
        revsets: targets.iter().map(|t| (*t).into()).collect(),
        all: false,
        cancel,
        all_jobs: false,
        cancel_timeout: timeout,
        watch: false,
        poll_interval: 15,
        max_retries: None,
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

pub(crate) fn all_args(cancel: bool, timeout: u64) -> RerunArgs {
    RerunArgs {
        all: true,
        ..args(&[], cancel, timeout)
    }
}

#[derive(Default)]
pub(crate) struct FakeJj {
    revset_commits: Mutex<Vec<CommitInfo>>,
}

impl FakeJj {
    pub(crate) fn with_revset_commits(commits: Vec<CommitInfo>) -> Self {
        Self {
            revset_commits: Mutex::new(commits),
        }
    }

    /// Replace the commits every revset resolves to, simulating the local
    /// graph changing between watch ticks.
    pub(crate) fn set_revset_commits(&self, commits: Vec<CommitInfo>) {
        *self.revset_commits.lock().unwrap() = commits;
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
        std::future::ready(Ok(self.revset_commits.lock().unwrap().clone()))
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
    fn pushed_bookmarks(&self, _remote: &str) -> impl Future<Output = Result<Vec<PushedBookmark>>> {
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
pub(crate) struct Calls {
    pub(crate) cancelled: Vec<u64>,
    pub(crate) rerun: Vec<u64>,
    pub(crate) rerun_failed: Vec<u64>,
    pub(crate) list_jobs: Vec<u64>,
    pub(crate) listed_shas: Vec<String>,
}

/// Pop the next scripted response, repeating the last entry once the queue is
/// down to one.
fn pop_or_repeat<T: Clone>(queue: &mut Vec<T>) -> Option<T> {
    if queue.len() > 1 {
        Some(queue.remove(0))
    } else {
        queue.first().cloned()
    }
}

pub(crate) struct FakeGh {
    pub(crate) pr: PrDetails,
    pub(crate) prs_by_number: HashMap<u64, PrDetails>,
    /// `head_spec` -> open PR number, for `find_open_pr`.
    pub(crate) open_prs: HashMap<String, u64>,
    /// Successive responses to `local_pulls`. Last entry repeats.
    pub(crate) local_prs: Mutex<Vec<Vec<PrWithCiStatus>>>,
    /// Successive responses to `list_workflow_runs_for_sha` for shas without
    /// a `runs_by_sha` entry. Last entry repeats once exhausted.
    pub(crate) list_responses: Mutex<Vec<Vec<WorkflowRun>>>,
    /// Per-sha response queues for `list_workflow_runs_for_sha`. Last entry
    /// repeats once exhausted.
    pub(crate) runs_by_sha: Mutex<HashMap<String, Vec<Vec<WorkflowRun>>>>,
    /// `list_workflow_runs_for_sha` fails for these head SHAs.
    pub(crate) fail_list_for_shas: HashSet<String>,
    pub(crate) jobs_by_run: HashMap<u64, Vec<WorkflowJob>>,
    pub(crate) calls: Mutex<Calls>,
}

impl FakeGh {
    pub(crate) fn new(pr: PrDetails, responses: Vec<Vec<WorkflowRun>>) -> Self {
        Self {
            pr,
            prs_by_number: HashMap::new(),
            open_prs: HashMap::new(),
            local_prs: Mutex::new(Vec::new()),
            list_responses: Mutex::new(responses),
            runs_by_sha: Mutex::new(HashMap::new()),
            fail_list_for_shas: HashSet::new(),
            jobs_by_run: HashMap::new(),
            calls: Mutex::new(Calls::default()),
        }
    }

    /// Queue one `local_pulls` response batch. Call repeatedly to script
    /// successive ticks.
    pub(crate) fn with_local_prs(self, prs: Vec<PrWithCiStatus>) -> Self {
        self.local_prs.lock().unwrap().push(prs);
        self
    }

    pub(crate) fn with_pr(mut self, pr: PrDetails) -> Self {
        self.prs_by_number.insert(pr.number, pr);
        self
    }

    pub(crate) fn with_open_pr(mut self, head_spec: &str, number: u64) -> Self {
        self.open_prs.insert(head_spec.into(), number);
        self
    }

    pub(crate) fn with_jobs(mut self, run_id: u64, jobs: Vec<WorkflowJob>) -> Self {
        self.jobs_by_run.insert(run_id, jobs);
        self
    }

    /// Queue one `list_workflow_runs_for_sha` response for `sha`. Call
    /// repeatedly to script successive ticks.
    pub(crate) fn with_runs_for_sha(self, sha: &str, runs: Vec<WorkflowRun>) -> Self {
        self.runs_by_sha
            .lock()
            .unwrap()
            .entry(sha.into())
            .or_default()
            .push(runs);
        self
    }

    pub(crate) fn with_list_failure(mut self, sha: &str) -> Self {
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
    async fn enable_auto_merge(&self, _id: &str, _m: crate::config::AutoMergeMethod) -> Result<()> {
        unimplemented!()
    }
    fn local_pulls(
        &self,
        _o: &str,
        _r: &str,
        _head_owner: &str,
        _b: &[String],
    ) -> impl Future<Output = Result<Vec<PrWithCiStatus>>> {
        let batch = pop_or_repeat(&mut self.local_prs.lock().unwrap()).unwrap_or_default();
        std::future::ready(Ok(batch))
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
        if let Some(queue) = self.runs_by_sha.lock().unwrap().get_mut(sha) {
            return std::future::ready(Ok(pop_or_repeat(queue).unwrap_or_default()));
        }
        let batch = pop_or_repeat(&mut self.list_responses.lock().unwrap()).unwrap_or_default();
        std::future::ready(Ok(batch))
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
    fn cancel_workflow_run(&self, _o: &str, _r: &str, id: u64) -> impl Future<Output = Result<()>> {
        self.calls.lock().unwrap().cancelled.push(id);
        std::future::ready(Ok(()))
    }
    fn rerun_workflow_run(&self, _o: &str, _r: &str, id: u64) -> impl Future<Output = Result<()>> {
        self.calls.lock().unwrap().rerun.push(id);
        std::future::ready(Ok(()))
    }
    fn rerun_failed_jobs(&self, _o: &str, _r: &str, id: u64) -> impl Future<Output = Result<()>> {
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
