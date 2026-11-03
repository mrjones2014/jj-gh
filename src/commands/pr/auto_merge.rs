use crate::{
    cli::GlobalOpts,
    config::AutoMergeMethod,
    gh::{Gh, PrDetails, pr_lookup::MissingPr},
    model::Model,
};
use anyhow::{Context, Result, bail};
use jj_gh_config_derive::subcommand_args;

subcommand_args! {
    pub struct AutoMergeArgs {
        /// PR numbers and/or jj revsets to look up PRs from. A numeric
        /// argument is treated as a PR number; anything else is resolved as a
        /// revset, which may match multiple commits (e.g. a whole stack via
        /// `qv::uw`).
        #[arg(value_name = "PR_NUM|REVSET", required = true)]
        pub revsets: Vec<String>,

        /// Merge method used when enabling auto-merge. Overrides config
        /// `auto_merge_method` (default `merge`).
        #[arg(long = "method", short = 'm', value_name = "METHOD", value_enum)]
        #[config]
        pub auto_merge_method: AutoMergeMethod,
    }
}

pub async fn run(model: &impl Model, args: &AutoMergeArgs) -> Result<()> {
    let gh = model.gh().await?;
    let AutoMergeArgs {
        revsets,
        auto_merge_method,
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
    let (_, prs) = model
        .resolve_pr_targets(remote, upstream_remote, revsets, MissingPr::Skip)
        .await?;
    if prs.is_empty() {
        bail!("no open PRs found for the given targets");
    }

    let multi = prs.len() > 1;
    let mut errors = Vec::<anyhow::Error>::new();
    for pr in &prs {
        let result = enable_auto_merge(gh, pr, *auto_merge_method)
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
    pr: &PrDetails,
    auto_merge_method: AutoMergeMethod,
) -> Result<()> {
    if pr.in_merge_queue {
        bail!(
            "auto-merge not supported for repos with merge queues enabled; this is a limitation of the GitHub API. See https://github.com/mrjones2014/jj-gh/issues/103"
        );
    }

    let _status = crate::ui::status::push(format!("Enabling auto-merge on #{}", pr.number));
    gh.enable_auto_merge(&pr.graphql_node_id, auto_merge_method)
        .await
        .with_context(|| format!("enabling auto-merge on #{}", pr.number))?;

    log::info!("Enabled auto-merge for PR #{}", pr.number);
    crate::ui::print_url(&pr.html_url);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        gh::{
            BaseLookup, CreatePrRequest, PrCreated, PrDetails, PrSummary, PrWithCiStatus, UpdatePr,
            WorkflowJob, WorkflowRun, pr_lookup, remote,
        },
        jj::{CommitInfo, PushedBookmark},
        model::TestModel,
    };
    use anyhow::Result;
    use std::collections::HashMap;
    use std::path::Path;
    use std::sync::Mutex;

    fn pr_details(number: u64, in_merge_queue: bool) -> PrDetails {
        PrDetails {
            is_draft: false,
            auto_merge: false,
            auto_merge_method: None,
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
            in_merge_queue,
            labels: vec![],
            reviewers: vec![],
            body: String::new(),
            stack_number: None,
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
    }

    struct FakeGh {
        prs_by_number: HashMap<u64, PrDetails>,
        /// `head_spec` -> open PR number, for `find_open_pr`.
        open_prs: HashMap<String, u64>,
        enabled: Mutex<Vec<String>>,
    }

    impl FakeGh {
        fn new(prs: Vec<PrDetails>) -> Self {
            Self {
                prs_by_number: prs.into_iter().map(|pr| (pr.number, pr)).collect(),
                open_prs: HashMap::new(),
                enabled: Mutex::new(Vec::new()),
            }
        }

        fn with_open_pr(mut self, head_spec: &str, number: u64) -> Self {
            self.open_prs.insert(head_spec.into(), number);
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
            _m: crate::config::AutoMergeMethod,
        ) -> impl Future<Output = Result<()>> {
            self.enabled.lock().unwrap().push(pr_node_id.to_string());
            std::future::ready(Ok(()))
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
        async fn unstack_prs(&self, _: &str, _: &str, _: u64, _: &[u64]) -> Result<()> {
            unimplemented!()
        }
        async fn list_stacks(&self, _: &str, _: &str) -> Result<Vec<crate::gh::PullRequestStack>> {
            unimplemented!()
        }
    }

    fn args(targets: &[&str]) -> AutoMergeArgs {
        AutoMergeArgs {
            revsets: targets.iter().map(|t| (*t).into()).collect(),
            auto_merge_method: AutoMergeMethod::Merge,
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

        assert_eq!(*gh.enabled.lock().unwrap(), vec!["node-1", "node-2"]);
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

        assert_eq!(*gh.enabled.lock().unwrap(), vec!["node-10", "node-11"]);
    }

    #[tokio::test]
    async fn merge_queue_pr_fails_without_stopping_others() {
        let gh = FakeGh::new(vec![pr_details(1, true), pr_details(2, false)]);
        let jj = FakeJj::default();

        let err = run(&TestModel::without_git(&jj, &gh), &args(&["1", "2"]))
            .await
            .expect_err("merge-queue PR should fail the command");
        let msg = format!("{err:#}");
        assert!(msg.contains("1 of 2"), "msg: {msg}");

        assert_eq!(*gh.enabled.lock().unwrap(), vec!["node-2"]);
    }

    #[tokio::test]
    async fn single_merge_queue_pr_returns_the_real_error() {
        let gh = FakeGh::new(vec![pr_details(1, true)]);
        let jj = FakeJj::default();

        let err = run(&TestModel::without_git(&jj, &gh), &args(&["1"]))
            .await
            .expect_err("merge-queue PR should fail");
        let msg = format!("{err:#}");
        assert!(msg.contains("merge queues"), "msg: {msg}");
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
}
