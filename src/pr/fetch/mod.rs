//! `jj-gh pr fetch`
//!
//! Download a PR's `refs/pull/123/head` into a local bookmark via git, then
//! import into jj. The bookmark name is rendered by evaluating a jj template
//! against an injected `--config-file` that defines `pr_*` aliases populated
//! from the PR's GitHub metadata.
//!
//! Requires a colocated git repository: jj cannot yet fetch arbitrary refs
//! (only `refs/heads/*`), so we shell to git for the special pull ref.

use crate::{
    cli::GlobalOpts,
    gh::{Gh, PrDetails},
    git::{real::GitOps, url::parse_owner_repo},
    jj::{
        Jj,
        inject::{TemplateAliases, escape_jj_string},
    },
};
use anyhow::{Context, Result, anyhow};
use jj_gh_config_derive::subcommand_args;
use std::path::Path;

/// Default jj template used to render the bookmark name when neither the
/// `pr_fetch_bookmark_template` config nor the `-T|--template` CLI flag is
/// set. Mirrors the legacy `pr-{number}/{branch}` format.
pub const DEFAULT_FETCH_TEMPLATE: &str = r#""pr-" ++ pr_number ++ "/" ++ pr_branch"#;

/// Cap length for the auto-generated `pr_slug` alias. Bookmark names are git
/// refs and many filesystems cap a single ref-component near 255 bytes; 50
/// characters keeps the slug short while leaving room for surrounding template
/// text.
const PR_SLUG_MAX_LEN: usize = 50;

/// Verify the workspace is a colocated git repo. Returns an explanatory error
/// otherwise.
fn ensure_colocated(workspace_root: &Path) -> Result<()> {
    if workspace_root.join(".git").exists() {
        return Ok(());
    }
    Err(anyhow!(
        "`jj pr fetch` requires a colocated git repository (a `.git` directory \
         at the workspace root, `{}`). jj cannot yet fetch arbitrary refs like \
         `refs/pull/123/head`, so we shell out to git for this step. Use a repo \
         that was initialized with `jj git init --colocate`.",
        workspace_root.display()
    ))
}

subcommand_args! {
    pub struct FetchArgs {
        /// PR number to fetch.
        #[arg(value_name = "PR_NUM")]
        pub pr: u64,

        /// Override the bookmark template. The argument is a jj template string
        /// evaluated once against `root()` (no commit context). Default:
        /// `pr_fetch_bookmark_template` in config, else
        /// `"pr-" ++ pr_number ++ "/" ++ pr_branch"`.
        ///
        /// All standard jj template builtins are available (`description`,
        /// `commit_id`, `author`, etc.). The following template aliases are also
        /// injected:
        ///
        /// - `pr_number`: PR number as a decimal string.
        ///
        /// - `pr_title`: PR title.
        ///
        /// - `pr_branch`: head ref name (the source branch on the PR's fork).
        ///
        /// - `pr_url`: PR's `html_url`.
        ///
        /// - `pr_head_sha`: 40-char hex commit SHA of the PR's head.
        ///
        /// - `pr_head_user`: PR's head fork owner login, or empty if the fork was deleted.
        ///
        /// - `pr_head_repo`: PR's head fork repository name, or empty if the fork was deleted.
        ///
        /// - `pr_slug`: sanitized lowercase ASCII slug of the title (max 50 chars), suitable for embedding in a bookmark name.
        #[arg(short = 'T', long, value_name = "TEMPLATE")]
        #[config(maps_to = "pr_fetch_bookmark_template")]
        pub template: Option<String>,

        /// Replace an existing local bookmark of the same name.
        #[arg(short = 'f', long)]
        pub force: bool,
    }
}

/// Run `pr fetch` end-to-end. Parameterized over [`GitOps`] so tests can
/// swap in a fake; production callers pass [`crate::git::real::RealGit`].
///
/// # Errors
///
/// Propagates errors from any step (auth, GH API, colocation, git fetch, jj
/// import, template eval).
pub async fn run<J: Jj, G: Gh, GO: GitOps>(
    jj: &J,
    gh: &G,
    git: &GO,
    args: &FetchArgs,
) -> Result<()> {
    let FetchArgs {
        pr: pr_num,
        template,
        force,
        globals:
            GlobalOpts {
                remote,
                verbose: _,
                quiet: _,
                log_level: _,
                upstream_remote: _,
                gh_askpass: _,
                askpass_timeout_secs: _,
            },
    } = args;

    let workspace_root = jj.workspace_root().await?;
    ensure_colocated(workspace_root)?;

    let origin_url = jj
        .remote_url(remote)
        .await?
        .ok_or_else(|| anyhow!("`{remote}` remote is not configured"))?;
    let (owner, repo) = parse_owner_repo(&origin_url)?;

    let pr = gh.get_pr(&owner, &repo, *pr_num).await?;
    if pr.head_user_login.is_none() || pr.head_repo_name.is_none() {
        log::warn!(
            "PR #{}: head fork appears deleted; `pr_head_user` / `pr_head_repo` will be empty",
            pr.number
        );
    }

    let tmpl = template.as_deref().unwrap_or(DEFAULT_FETCH_TEMPLATE);
    let aliases = build_fetch_aliases(&pr);
    let tmp = aliases.write_temp_config()?;
    let bookmark = jj
        .eval_template("root()", tmpl, Some(tmp.path()), false)
        .await
        .context("evaluating bookmark template")?
        .trim()
        .to_string();

    if bookmark.is_empty() {
        return Err(anyhow!(
            "bookmark template rendered to an empty string; check `pr_fetch_bookmark_template`"
        ));
    }

    if git.local_bookmark_exists(&bookmark).await? && !force {
        return Err(anyhow!(
            "local bookmark `{bookmark}` already exists; pass --force to overwrite"
        ));
    }

    git.fetch_pr(remote, *pr_num, &bookmark, *force).await?;
    jj.git_import().await?;

    log::info!("PR #{}: {}", pr.number, pr.title);
    log::info!("head: {} ({})", pr.head_sha, pr.html_url);
    log::info!("hint: jj new {bookmark}");
    println!("{bookmark}");
    Ok(())
}

/// Build the [`TemplateAliases`] populated from `pr`'s GitHub metadata. Pure
/// so it can be unit tested without spawning jj.
fn build_fetch_aliases(pr: &PrDetails) -> TemplateAliases {
    TemplateAliases::builder()
        .alias("pr_number", quote_jj(&pr.number.to_string()))
        .alias("pr_title", quote_jj(&pr.title))
        .alias("pr_branch", quote_jj(&pr.head_ref))
        .alias("pr_url", quote_jj(&pr.html_url))
        .alias("pr_head_sha", quote_jj(&pr.head_sha))
        .alias(
            "pr_head_user",
            quote_jj(pr.head_user_login.as_deref().unwrap_or("")),
        )
        .alias(
            "pr_head_repo",
            quote_jj(pr.head_repo_name.as_deref().unwrap_or("")),
        )
        .alias("pr_slug", quote_jj(&slugify(&pr.title)))
}

/// Wrap `s` as a jj template double-quoted string literal, escaping `\` and `"`.
fn quote_jj(s: &str) -> String {
    format!(r#""{}""#, escape_jj_string(s))
}

/// Sanitize a string into a bookmark-safe slug: lowercase, runs of
/// non-alphanumeric ASCII collapsed to a single `-`, trimmed of leading and
/// trailing `-`, capped at [`PR_SLUG_MAX_LEN`] characters. Non-ASCII input is
/// dropped (no transliteration).
fn slugify(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_dash = true;
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    out.truncate(PR_SLUG_MAX_LEN);
    out.trim_matches('-').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gh::{BaseLookup, CreatePrRequest, PrCreated, PrSummary};
    use crate::jj::CommitInfo;
    use std::cell::RefCell;
    use std::path::PathBuf;
    use std::sync::Mutex;
    use tempfile::TempDir;

    #[derive(Debug, Clone, Default)]
    struct EvalCall {
        revset: String,
        template: String,
        reversed: bool,
    }

    struct FakeJj {
        workspace_root: PathBuf,
        origin: Option<String>,
        expected_remote: String,
        import_calls: Mutex<u32>,
        eval_template_return: String,
        eval_template_calls: Mutex<Vec<EvalCall>>,
    }

    impl Jj for FakeJj {
        async fn resolve_rev(&self, _rev: &str) -> Result<CommitInfo> {
            unimplemented!("fetch does not call resolve_rev")
        }
        async fn stacked_ancestor_bookmark(&self, _rev: &str) -> Result<Option<String>> {
            unimplemented!("fetch does not call stacked_ancestor_bookmark")
        }
        async fn first_commit_description(&self, _revset: &str) -> Result<String> {
            unimplemented!("fetch does not call first_commit_description")
        }
        async fn remote_url(&self, name: &str) -> Result<Option<String>> {
            assert_eq!(name, self.expected_remote);
            Ok(self.origin.clone())
        }
        async fn remote_bookmark_sha(&self, _: &str, _: &str) -> Result<Option<String>> {
            unimplemented!("fetch does not call remote_bookmark_sha")
        }
        async fn push(&self, _rev: &str) -> Result<()> {
            unimplemented!("fetch does not call push")
        }
        async fn trunk_branch(&self) -> Result<Option<String>> {
            unimplemented!("fetch does not call trunk_branch")
        }
        async fn workspace_root(&self) -> Result<&PathBuf> {
            Ok(&self.workspace_root)
        }
        async fn git_import(&self) -> Result<()> {
            *self.import_calls.lock().unwrap() += 1;
            Ok(())
        }
        async fn pushed_bookmarks(&self, _remote: &str) -> Result<Vec<crate::jj::PushedBookmark>> {
            unimplemented!("fetch does not call pushed_bookmarks")
        }
        async fn eval_template(
            &self,
            revset: &str,
            template: &str,
            _config_file: Option<&Path>,
            reversed: bool,
        ) -> Result<String> {
            self.eval_template_calls.lock().unwrap().push(EvalCall {
                revset: revset.into(),
                template: template.into(),
                reversed,
            });
            Ok(self.eval_template_return.clone())
        }
    }

    struct FakeGh {
        pr: PrDetails,
        expected: (String, String, u64),
    }

    impl Gh for FakeGh {
        async fn find_open_pr(
            &self,
            _owner: &str,
            _repo: &str,
            _head_spec: &str,
        ) -> Result<Option<PrSummary>> {
            unimplemented!("fetch does not call find_open_pr")
        }
        async fn lookup_base(&self, _: &str, _: &str, _: &str) -> Result<BaseLookup> {
            unimplemented!("fetch does not call lookup_base")
        }
        async fn create_pr(&self, _req: CreatePrRequest) -> Result<PrCreated> {
            unimplemented!("fetch does not call create_pr")
        }
        async fn add_reviewers(
            &self,
            _owner: &str,
            _repo: &str,
            _pr: u64,
            _reviewers: Vec<crate::gh::Reviewer>,
        ) -> Result<()> {
            unimplemented!("fetch does not call add_reviewers")
        }
        async fn remove_reviewers(
            &self,
            _owner: &str,
            _repo: &str,
            _pr: u64,
            _reviewers: Vec<crate::gh::Reviewer>,
        ) -> Result<()> {
            unimplemented!("fetch does not call remove_reviewers")
        }
        async fn add_labels(&self, _: &str, _: &str, _: u64, _: &[String]) -> Result<()> {
            unimplemented!("fetch does not call add_labels")
        }
        async fn remove_labels(&self, _: &str, _: &[String]) -> Result<()> {
            unimplemented!("fetch does not call remove_labels")
        }
        async fn update_pr(&self, _req: crate::gh::UpdatePr) -> Result<()> {
            unimplemented!("fetch does not call update_pr")
        }
        async fn set_draft(&self, _pr_node_id: &str, _draft: bool) -> Result<()> {
            unimplemented!("fetch does not call set_draft")
        }
        async fn disable_auto_merge(&self, _pr_node_id: &str) -> Result<()> {
            unimplemented!("fetch does not call disable_auto_merge")
        }
        async fn get_pr(&self, owner: &str, repo: &str, number: u64) -> Result<PrDetails> {
            assert_eq!(owner, self.expected.0);
            assert_eq!(repo, self.expected.1);
            assert_eq!(number, self.expected.2);
            Ok(self.pr.clone())
        }
        async fn enable_auto_merge(
            &self,
            _node_id: &str,
            _has_merge_queue: bool,
            _method: crate::config::AutoMergeMethod,
        ) -> Result<()> {
            unimplemented!("fetch does not call enable_auto_merge")
        }
        async fn local_pulls(
            &self,
            _owner: &str,
            _repo: &str,
            _branches: &[String],
        ) -> Result<Vec<crate::gh::PrWithCiStatus>> {
            unimplemented!("fetch does not call local_pulls")
        }
        async fn list_workflow_runs_for_sha(
            &self,
            _owner: &str,
            _repo: &str,
            _sha: &str,
        ) -> Result<Vec<crate::gh::WorkflowRun>> {
            unimplemented!("fetch does not call list_workflow_runs_for_sha")
        }
        async fn cancel_workflow_run(&self, _: &str, _: &str, _: u64) -> Result<()> {
            unimplemented!("fetch does not call cancel_workflow_run")
        }
        async fn rerun_workflow_run(&self, _: &str, _: &str, _: u64) -> Result<()> {
            unimplemented!("fetch does not call rerun_workflow_run")
        }
        async fn rerun_failed_jobs(&self, _: &str, _: &str, _: u64) -> Result<()> {
            unimplemented!("fetch does not call rerun_failed_jobs")
        }
    }

    #[derive(Debug, Clone)]
    struct FetchCall {
        remote: String,
        pr: u64,
        bookmark: String,
        force: bool,
    }

    struct FakeGit {
        exists: bool,
        fetches: RefCell<Vec<FetchCall>>,
    }

    impl GitOps for FakeGit {
        async fn local_bookmark_exists(&self, _name: &str) -> Result<bool> {
            Ok(self.exists)
        }
        async fn fetch_pr(&self, remote: &str, pr: u64, bookmark: &str, force: bool) -> Result<()> {
            self.fetches.borrow_mut().push(FetchCall {
                remote: remote.to_string(),
                pr,
                bookmark: bookmark.to_string(),
                force,
            });
            Ok(())
        }
    }

    fn details() -> PrDetails {
        PrDetails {
            number: 1234,
            title: "Add the feature".into(),
            html_url: "https://github.com/o/r/pull/1234".into(),
            head_ref: "feature/foo".into(),
            head_sha: "abc123".into(),
            head_user_login: Some("octocat".into()),
            head_repo_name: Some("r".into()),
            graphql_node_id: "PR_kwDOABCDEF".into(),
            in_merge_queue: false,
            is_draft: false,
            auto_merge: false,
            auto_merge_method: None,
            labels: vec![],
            reviewers: vec![],
            body: String::new(),
        }
    }

    fn args(pr: u64, template: Option<&str>, force: bool) -> FetchArgs {
        args_with_remote(pr, template, force, "origin")
    }

    fn args_with_remote(pr: u64, template: Option<&str>, force: bool, remote: &str) -> FetchArgs {
        FetchArgs {
            pr,
            template: template.map(str::to_string),
            force,
            globals: GlobalOpts {
                verbose: 0,
                quiet: false,
                log_level: None,
                remote: remote.into(),
                upstream_remote: "upstream".into(),
                gh_askpass: None,
                askpass_timeout_secs: 20,
            },
        }
    }

    fn colocated_workspace() -> TempDir {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        dir
    }

    fn jj_for(dir: &TempDir, origin: Option<&str>, eval_return: &str) -> FakeJj {
        FakeJj {
            workspace_root: dir.path().to_path_buf(),
            origin: origin.map(str::to_string),
            expected_remote: "origin".into(),
            import_calls: Mutex::new(0),
            eval_template_return: eval_return.into(),
            eval_template_calls: Mutex::new(Vec::new()),
        }
    }

    fn gh_for(pr: PrDetails, owner: &str, repo: &str) -> FakeGh {
        let expected = (owner.to_string(), repo.to_string(), pr.number);
        FakeGh { pr, expected }
    }

    #[tokio::test]
    async fn happy_path_prints_bookmark_and_imports() {
        let dir = colocated_workspace();
        let jj = jj_for(&dir, Some("git@github.com:o/r.git"), "pr-1234/feature/foo");
        let gh = gh_for(details(), "o", "r");
        let git = FakeGit {
            exists: false,
            fetches: RefCell::new(vec![]),
        };
        run(&jj, &gh, &git, &args(1234, None, false)).await.unwrap();

        let calls = git.fetches.borrow();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].remote, "origin");
        assert_eq!(calls[0].pr, 1234);
        assert_eq!(calls[0].bookmark, "pr-1234/feature/foo");
        assert!(!calls[0].force);
        assert_eq!(*jj.import_calls.lock().unwrap(), 1);
    }

    #[tokio::test]
    async fn config_default_remote_propagates_to_jj_and_git() {
        let dir = colocated_workspace();
        let mut jj = jj_for(&dir, Some("git@github.com:o/r.git"), "pr-1234/feature/foo");
        jj.expected_remote = "fork".into();
        let gh = gh_for(details(), "o", "r");
        let git = FakeGit {
            exists: false,
            fetches: RefCell::new(vec![]),
        };
        run(&jj, &gh, &git, &args_with_remote(1234, None, false, "fork"))
            .await
            .unwrap();

        let calls = git.fetches.borrow();
        assert_eq!(calls[0].remote, "fork");
    }

    #[tokio::test]
    async fn existing_bookmark_without_force_errors() {
        let dir = colocated_workspace();
        let jj = jj_for(&dir, Some("git@github.com:o/r.git"), "pr-1234/feature/foo");
        let gh = gh_for(details(), "o", "r");
        let git = FakeGit {
            exists: true,
            fetches: RefCell::new(vec![]),
        };
        let err = run(&jj, &gh, &git, &args(1234, None, false))
            .await
            .unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("--force"), "msg: {msg}");
        assert!(git.fetches.borrow().is_empty());
    }

    #[tokio::test]
    async fn force_flag_passes_through() {
        let dir = colocated_workspace();
        let jj = jj_for(&dir, Some("git@github.com:o/r.git"), "pr-1234/feature/foo");
        let gh = gh_for(details(), "o", "r");
        let git = FakeGit {
            exists: true,
            fetches: RefCell::new(vec![]),
        };
        run(&jj, &gh, &git, &args(1234, None, true)).await.unwrap();

        let calls = git.fetches.borrow();
        assert_eq!(calls.len(), 1);
        assert!(calls[0].force);
    }

    #[tokio::test]
    async fn config_template_is_used() {
        let dir = colocated_workspace();
        let jj = jj_for(&dir, Some("git@github.com:o/r.git"), "cfg-from-template");
        let gh = gh_for(details(), "o", "r");
        let git = FakeGit {
            exists: false,
            fetches: RefCell::new(vec![]),
        };
        let cfg_template = r#""cfg-" ++ pr_number ++ "-" ++ pr_head_user"#;

        run(&jj, &gh, &git, &args(1234, Some(cfg_template), false))
            .await
            .unwrap();

        let evals = jj.eval_template_calls.lock().unwrap();
        assert_eq!(evals.len(), 1);
        assert_eq!(evals[0].revset, "root()");
        assert_eq!(evals[0].template, cfg_template);
        assert!(!evals[0].reversed);
        assert_eq!(git.fetches.borrow()[0].bookmark, "cfg-from-template");
    }

    #[tokio::test]
    async fn cli_template_overrides_config() {
        let dir = colocated_workspace();
        let jj = jj_for(&dir, Some("git@github.com:o/r.git"), "from-cli");
        let gh = gh_for(details(), "o", "r");
        let git = FakeGit {
            exists: false,
            fetches: RefCell::new(vec![]),
        };
        let cli_template = r#""cli-" ++ pr_number"#;

        run(&jj, &gh, &git, &args(1234, Some(cli_template), false))
            .await
            .unwrap();

        let evals = jj.eval_template_calls.lock().unwrap();
        assert_eq!(evals[0].template, cli_template);
    }

    #[tokio::test]
    async fn default_template_used_when_no_override() {
        let dir = colocated_workspace();
        let jj = jj_for(&dir, Some("git@github.com:o/r.git"), "pr-1234/feature/foo");
        let gh = gh_for(details(), "o", "r");
        let git = FakeGit {
            exists: false,
            fetches: RefCell::new(vec![]),
        };
        run(&jj, &gh, &git, &args(1234, None, false)).await.unwrap();

        let evals = jj.eval_template_calls.lock().unwrap();
        assert_eq!(evals[0].template, DEFAULT_FETCH_TEMPLATE);
    }

    #[tokio::test]
    async fn empty_bookmark_rendering_errors() {
        let dir = colocated_workspace();
        let jj = jj_for(&dir, Some("git@github.com:o/r.git"), "   ");
        let gh = gh_for(details(), "o", "r");
        let git = FakeGit {
            exists: false,
            fetches: RefCell::new(vec![]),
        };
        let err = run(&jj, &gh, &git, &args(1234, None, false))
            .await
            .unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("empty"), "msg: {msg}");
        assert!(git.fetches.borrow().is_empty());
    }

    #[tokio::test]
    async fn missing_origin_errors_clearly() {
        let dir = colocated_workspace();
        let jj = jj_for(&dir, None, "irrelevant");
        let gh = gh_for(details(), "o", "r");
        let git = FakeGit {
            exists: false,
            fetches: RefCell::new(vec![]),
        };
        let err = run(&jj, &gh, &git, &args(1234, None, false))
            .await
            .unwrap_err();
        assert!(
            err.to_string()
                .contains("`origin` remote is not configured"),
            "msg: {err}"
        );
    }

    #[tokio::test]
    async fn non_colocated_repo_errors_with_explanation() {
        let dir = TempDir::new().unwrap();
        let jj = jj_for(&dir, Some("git@github.com:o/r.git"), "irrelevant");
        let gh = gh_for(details(), "o", "r");
        let git = FakeGit {
            exists: false,
            fetches: RefCell::new(vec![]),
        };
        let err = run(&jj, &gh, &git, &args(1234, None, false))
            .await
            .unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("colocated"), "msg: {msg}");
        assert!(msg.contains("refs/pull/123/head"), "msg: {msg}");
    }

    #[test]
    fn slugify_handles_punct_and_case() {
        assert_eq!(
            slugify("Fix: Auth bug (issue #42)!"),
            "fix-auth-bug-issue-42"
        );
    }

    #[test]
    fn slugify_collapses_runs_of_separators() {
        assert_eq!(slugify("a___b   c"), "a-b-c");
    }

    #[test]
    fn slugify_trims_leading_and_trailing_dashes() {
        assert_eq!(slugify("---hi---"), "hi");
    }

    #[test]
    fn slugify_caps_length_and_trims_trailing_dash() {
        let s = "a".repeat(60);
        assert_eq!(slugify(&s).len(), PR_SLUG_MAX_LEN);
        let mixed = format!("{} bbb", "a".repeat(PR_SLUG_MAX_LEN - 1));
        let out = slugify(&mixed);
        assert!(!out.ends_with('-'));
    }

    #[test]
    fn slugify_drops_non_ascii() {
        assert_eq!(slugify("café 修复"), "caf");
    }

    #[test]
    fn build_fetch_aliases_contains_all_pr_fields() {
        let cfg = build_fetch_aliases(&details()).to_toml();
        let parsed: toml::Table = toml::from_str(&cfg).unwrap();
        let aliases = parsed["template-aliases"].as_table().unwrap();
        assert_eq!(aliases["pr_number"].as_str(), Some(r#""1234""#));
        assert_eq!(aliases["pr_title"].as_str(), Some(r#""Add the feature""#));
        assert_eq!(aliases["pr_branch"].as_str(), Some(r#""feature/foo""#));
        assert_eq!(
            aliases["pr_url"].as_str(),
            Some(r#""https://github.com/o/r/pull/1234""#)
        );
        assert_eq!(aliases["pr_head_sha"].as_str(), Some(r#""abc123""#));
        assert_eq!(aliases["pr_head_user"].as_str(), Some(r#""octocat""#));
        assert_eq!(aliases["pr_head_repo"].as_str(), Some(r#""r""#));
        assert_eq!(aliases["pr_slug"].as_str(), Some(r#""add-the-feature""#));
    }

    #[test]
    fn build_fetch_aliases_uses_empty_string_for_deleted_fork() {
        let mut d = details();
        d.head_user_login = None;
        d.head_repo_name = None;
        let cfg = build_fetch_aliases(&d).to_toml();
        let parsed: toml::Table = toml::from_str(&cfg).unwrap();
        let aliases = parsed["template-aliases"].as_table().unwrap();
        assert_eq!(aliases["pr_head_user"].as_str(), Some(r#""""#));
        assert_eq!(aliases["pr_head_repo"].as_str(), Some(r#""""#));
    }
}
