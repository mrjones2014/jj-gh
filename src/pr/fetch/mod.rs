//! `jj-gh pr fetch`
//!
//! Download a PR's `refs/pull/123/head` into a local
//! bookmark via git, then import into jj.
//!
//! Requires a colocated git repository: jj cannot yet fetch arbitrary refs
//! (only `refs/heads/*`), so we shell to git for the special pull ref.

use crate::{cli::FetchArgs, config::Config, gh::Gh, git::url::parse_owner_repo, jj::Jj};
use anyhow::{Result, anyhow};
use std::path::{Path, PathBuf};

pub mod bookmark_template;

pub use bookmark_template::{DEFAULT_FETCH_TEMPLATE, Fields};

/// Operations shelled out to `git`. Abstracted so tests can supply a fake.
pub trait GitOps {
    /// Whether `refs/heads/<name>` resolves in the workspace's git store.
    ///
    /// # Errors
    ///
    /// Propagates spawn failures.
    async fn local_bookmark_exists(&self, workdir: &Path, name: &str) -> Result<bool>;

    /// Fetch `refs/pull/<pr>/head` from `origin` into `refs/heads/<bookmark>`.
    ///
    /// # Errors
    ///
    /// Propagates non-zero exit or spawn failures.
    async fn fetch_pr(&self, workdir: &Path, pr: u64, bookmark: &str, force: bool) -> Result<()>;
}

/// Production [`GitOps`] backed by the system `git` binary.
pub struct RealGit;

impl GitOps for RealGit {
    async fn local_bookmark_exists(&self, workdir: &Path, name: &str) -> Result<bool> {
        let status = tokio::process::Command::new("git")
            .current_dir(workdir)
            .args([
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("refs/heads/{name}"),
            ])
            .status()
            .await
            .map_err(|e| anyhow!("failed to spawn `git`: {e}"))?;
        Ok(status.success())
    }

    async fn fetch_pr(&self, workdir: &Path, pr: u64, bookmark: &str, force: bool) -> Result<()> {
        let refspec = format!("refs/pull/{pr}/head:refs/heads/{bookmark}");
        let mut cmd = tokio::process::Command::new("git");
        cmd.current_dir(workdir).args(["fetch", "origin", &refspec]);
        if force {
            cmd.arg("--force");
        }
        let out = cmd
            .output()
            .await
            .map_err(|e| anyhow!("failed to spawn `git`: {e}"))?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            return Err(anyhow!(
                "git fetch origin {refspec} failed: {}",
                stderr.trim()
            ));
        }
        Ok(())
    }
}

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

fn resolve_template<'a>(args: &'a FetchArgs, config: &'a Config) -> &'a str {
    args.template
        .as_deref()
        .or(config.pr_fetch_bookmark_template.as_deref())
        .unwrap_or(DEFAULT_FETCH_TEMPLATE)
}

/// Run `pr fetch` end-to-end using the production [`RealGit`].
///
/// # Errors
///
/// Propagates errors from any step (auth, GH API, colocation, git fetch, jj
/// import, template render).
pub async fn run<J: Jj, G: Gh>(jj: &J, gh: &G, config: &Config, args: &FetchArgs) -> Result<()> {
    run_with(jj, gh, &RealGit, config, args).await
}

/// Inner runner parameterized over [`GitOps`] for tests.
///
/// # Errors
///
/// See [`run`].
pub async fn run_with<J: Jj, G: Gh, GO: GitOps>(
    jj: &J,
    gh: &G,
    git: &GO,
    config: &Config,
    args: &FetchArgs,
) -> Result<()> {
    let workspace_root: PathBuf = jj.workspace_root().await?;
    ensure_colocated(&workspace_root)?;

    let origin_url = jj
        .remote_url("origin")
        .await?
        .ok_or_else(|| anyhow!("origin remote is not configured"))?;
    let (owner, repo) = parse_owner_repo(&origin_url)?;

    let pr = gh.get_pr(&owner, &repo, args.pr).await?;

    let template = resolve_template(args, config);
    let bookmark = bookmark_template::render(
        template,
        &Fields {
            number: pr.number,
            branch: &pr.head_ref,
            user: pr.head_user_login.as_deref(),
            repo: pr.head_repo_name.as_deref(),
        },
    )?;

    if git
        .local_bookmark_exists(&workspace_root, &bookmark)
        .await?
        && !args.force
    {
        return Err(anyhow!(
            "local bookmark `{bookmark}` already exists; pass --force to overwrite"
        ));
    }

    git.fetch_pr(&workspace_root, args.pr, &bookmark, args.force)
        .await?;
    jj.git_import().await?;

    log::info!("PR #{}: {}", pr.number, pr.title);
    log::info!("head: {} ({})", pr.head_sha, pr.html_url);
    log::info!("hint: jj new {bookmark}");
    println!("{bookmark}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::AuthArgs;
    use crate::gh::{CreatePrRequest, PrCreated, PrDetails, PrSummary};
    use crate::jj::CommitInfo;
    use std::cell::RefCell;
    use std::sync::Mutex;
    use tempfile::TempDir;

    struct FakeJj {
        workspace_root: PathBuf,
        origin: Option<String>,
        import_calls: Mutex<u32>,
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
            assert_eq!(name, "origin");
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
        async fn workspace_root(&self) -> Result<PathBuf> {
            Ok(self.workspace_root.clone())
        }
        async fn git_import(&self) -> Result<()> {
            *self.import_calls.lock().unwrap() += 1;
            Ok(())
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
        async fn branch_exists(&self, _: &str, _: &str, _: &str) -> Result<bool> {
            unimplemented!("fetch does not call branch_exists")
        }
        async fn create_pr(&self, _req: CreatePrRequest) -> Result<PrCreated> {
            unimplemented!("fetch does not call create_pr")
        }
        async fn add_labels(&self, _: &str, _: &str, _: u64, _: &[String]) -> Result<()> {
            unimplemented!("fetch does not call add_labels")
        }
        async fn get_pr(&self, owner: &str, repo: &str, number: u64) -> Result<PrDetails> {
            assert_eq!(owner, self.expected.0);
            assert_eq!(repo, self.expected.1);
            assert_eq!(number, self.expected.2);
            Ok(self.pr.clone())
        }
    }

    #[derive(Debug, Clone)]
    struct FetchCall {
        pr: u64,
        bookmark: String,
        force: bool,
    }

    struct FakeGit {
        exists: bool,
        fetches: RefCell<Vec<FetchCall>>,
    }

    impl GitOps for FakeGit {
        async fn local_bookmark_exists(&self, _workdir: &Path, _name: &str) -> Result<bool> {
            Ok(self.exists)
        }
        async fn fetch_pr(
            &self,
            _workdir: &Path,
            pr: u64,
            bookmark: &str,
            force: bool,
        ) -> Result<()> {
            self.fetches.borrow_mut().push(FetchCall {
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
        }
    }

    fn args(pr: u64, template: Option<&str>, force: bool) -> FetchArgs {
        FetchArgs {
            pr,
            template: template.map(str::to_string),
            force,
            auth: AuthArgs {
                gh_askpass: None,
                askpass_timeout_secs: None,
            },
        }
    }

    fn colocated_workspace() -> TempDir {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        dir
    }

    fn jj_for(dir: &TempDir, origin: Option<&str>) -> FakeJj {
        FakeJj {
            workspace_root: dir.path().to_path_buf(),
            origin: origin.map(str::to_string),
            import_calls: Mutex::new(0),
        }
    }

    fn gh_for(pr: PrDetails, owner: &str, repo: &str) -> FakeGh {
        let expected = (owner.to_string(), repo.to_string(), pr.number);
        FakeGh { pr, expected }
    }

    #[tokio::test]
    async fn happy_path_prints_bookmark_and_imports() {
        let dir = colocated_workspace();
        let jj = jj_for(&dir, Some("git@github.com:o/r.git"));
        let gh = gh_for(details(), "o", "r");
        let git = FakeGit {
            exists: false,
            fetches: RefCell::new(vec![]),
        };
        let config = Config::default();

        run_with(&jj, &gh, &git, &config, &args(1234, None, false))
            .await
            .unwrap();

        let calls = git.fetches.borrow();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].pr, 1234);
        assert_eq!(calls[0].bookmark, "pr-1234/feature/foo");
        assert!(!calls[0].force);
        assert_eq!(*jj.import_calls.lock().unwrap(), 1);
    }

    #[tokio::test]
    async fn existing_bookmark_without_force_errors() {
        let dir = colocated_workspace();
        let jj = jj_for(&dir, Some("git@github.com:o/r.git"));
        let gh = gh_for(details(), "o", "r");
        let git = FakeGit {
            exists: true,
            fetches: RefCell::new(vec![]),
        };
        let config = Config::default();

        let err = run_with(&jj, &gh, &git, &config, &args(1234, None, false))
            .await
            .unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("--force"), "msg: {msg}");
        assert!(git.fetches.borrow().is_empty());
    }

    #[tokio::test]
    async fn force_flag_passes_through() {
        let dir = colocated_workspace();
        let jj = jj_for(&dir, Some("git@github.com:o/r.git"));
        let gh = gh_for(details(), "o", "r");
        let git = FakeGit {
            exists: true,
            fetches: RefCell::new(vec![]),
        };
        let config = Config::default();

        run_with(&jj, &gh, &git, &config, &args(1234, None, true))
            .await
            .unwrap();

        let calls = git.fetches.borrow();
        assert_eq!(calls.len(), 1);
        assert!(calls[0].force);
    }

    #[tokio::test]
    async fn cli_template_overrides_config() {
        let dir = colocated_workspace();
        let jj = jj_for(&dir, Some("git@github.com:o/r.git"));
        let gh = gh_for(details(), "o", "r");
        let git = FakeGit {
            exists: false,
            fetches: RefCell::new(vec![]),
        };
        let config = Config {
            pr_fetch_bookmark_template: Some("from-config-{number}".into()),
            ..Config::default()
        };

        run_with(
            &jj,
            &gh,
            &git,
            &config,
            &args(1234, Some("cli-{number}-{user}"), false),
        )
        .await
        .unwrap();

        assert_eq!(git.fetches.borrow()[0].bookmark, "cli-1234-octocat");
    }

    #[tokio::test]
    async fn config_template_used_when_cli_absent() {
        let dir = colocated_workspace();
        let jj = jj_for(&dir, Some("git@github.com:o/r.git"));
        let gh = gh_for(details(), "o", "r");
        let git = FakeGit {
            exists: false,
            fetches: RefCell::new(vec![]),
        };
        let config = Config {
            pr_fetch_bookmark_template: Some("cfg-{number}-{repo}".into()),
            ..Config::default()
        };

        run_with(&jj, &gh, &git, &config, &args(1234, None, false))
            .await
            .unwrap();
        assert_eq!(git.fetches.borrow()[0].bookmark, "cfg-1234-r");
    }

    #[tokio::test]
    async fn unknown_placeholder_errors() {
        let dir = colocated_workspace();
        let jj = jj_for(&dir, Some("git@github.com:o/r.git"));
        let gh = gh_for(details(), "o", "r");
        let git = FakeGit {
            exists: false,
            fetches: RefCell::new(vec![]),
        };
        let config = Config::default();

        let err = run_with(
            &jj,
            &gh,
            &git,
            &config,
            &args(1234, Some("pr-{nope}"), false),
        )
        .await
        .unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("unknown placeholder"), "msg: {msg}");
        assert!(msg.contains("{nope}"), "msg: {msg}");
    }

    #[tokio::test]
    async fn missing_origin_errors_clearly() {
        let dir = colocated_workspace();
        let jj = jj_for(&dir, None);
        let gh = gh_for(details(), "o", "r");
        let git = FakeGit {
            exists: false,
            fetches: RefCell::new(vec![]),
        };
        let config = Config::default();

        let err = run_with(&jj, &gh, &git, &config, &args(1234, None, false))
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("origin remote is not configured"),
            "msg: {err}"
        );
    }

    #[tokio::test]
    async fn non_colocated_repo_errors_with_explanation() {
        let dir = TempDir::new().unwrap(); // no .git dir
        let jj = jj_for(&dir, Some("git@github.com:o/r.git"));
        let gh = gh_for(details(), "o", "r");
        let git = FakeGit {
            exists: false,
            fetches: RefCell::new(vec![]),
        };
        let config = Config::default();

        let err = run_with(&jj, &gh, &git, &config, &args(1234, None, false))
            .await
            .unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("colocated"), "msg: {msg}");
        assert!(msg.contains("refs/pull/123/head"), "msg: {msg}");
    }

    #[tokio::test]
    async fn deleted_fork_works_when_template_omits_user_and_repo() {
        let dir = colocated_workspace();
        let jj = jj_for(&dir, Some("git@github.com:o/r.git"));
        let mut d = details();
        d.head_user_login = None;
        d.head_repo_name = None;
        let gh = gh_for(d, "o", "r");
        let git = FakeGit {
            exists: false,
            fetches: RefCell::new(vec![]),
        };
        let config = Config::default();

        run_with(&jj, &gh, &git, &config, &args(1234, None, false))
            .await
            .unwrap();
        assert_eq!(git.fetches.borrow()[0].bookmark, "pr-1234/feature/foo");
    }

    #[tokio::test]
    async fn deleted_fork_errors_when_template_references_user() {
        let dir = colocated_workspace();
        let jj = jj_for(&dir, Some("git@github.com:o/r.git"));
        let mut d = details();
        d.head_user_login = None;
        let gh = gh_for(d, "o", "r");
        let git = FakeGit {
            exists: false,
            fetches: RefCell::new(vec![]),
        };
        let config = Config::default();

        let err = run_with(
            &jj,
            &gh,
            &git,
            &config,
            &args(1234, Some("pr-{number}-{user}"), false),
        )
        .await
        .unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("{user}"), "msg: {msg}");
        assert!(
            msg.contains("unavailable") || msg.contains("null"),
            "msg: {msg}"
        );
    }
}
