//! End-to-end orchestrator for `jj-gh pr create` / `jj-gh pr fetch` / `jj-gh pr auto-merge`.

mod editor;
pub mod fetch;
mod frontmatter;
mod template;
mod validation;

pub use editor::{EditorRoundTrip, TempfileEditor, resolve_editor_argv};
pub use frontmatter::Frontmatter;
pub use template::{TemplateChoice, load_template_file, resolve_template_path};

use crate::{
    auth,
    cli::AuthArgs,
    config::{self, AutoMergeMethod, Config},
    fs::RealFs,
    gh::{self, CreatePrRequest, Gh, PrSummary, remote},
    jj::{self, Jj},
};
use anyhow::{Context, Result, anyhow};
use clap::Subcommand;
use figment::providers::Serialized;
use serde::Serialize;

#[derive(Debug, Subcommand)]
pub enum PrAction {
    /// Create a pull request from a revision. This supports stacked PRs; by default the base
    /// branch is set to the closest ancestor bookmark if one exists, otherwise `trunk()`.
    #[command(visible_alias = "c")]
    Create(CreateArgs),
    /// Fetch a pull request by number into a local bookmark. Requires a colocated
    /// git repository; the special `refs/pull/123/head` ref is fetched via `git`
    /// because `jj` cannot yet fetch arbitrary refs.
    #[command(visible_alias = "f")]
    Fetch(FetchArgs),
    /// Enable auto-merge on a PR. Accepts either a PR number or a revision; with
    /// a revision, the PR is looked up by the rev's local bookmark.
    #[command(visible_alias = "am")]
    AutoMerge(AutoMergeArgs),
}

#[derive(Debug, clap::Args, Serialize)]
pub struct AutoMergeArgs {
    /// PR number, or revision ID to look up a PR from.
    #[arg(value_name = "PR_NUM|REV")]
    #[serde(skip)]
    pub number_or_rev: String,

    /// Merge method used when enabling auto-merge. Overrides config
    /// `auto_merge_method` (default `merge`).
    #[arg(long = "method", short = 'm', value_name = "METHOD", value_enum)]
    #[serde(rename = "auto_merge_method", skip_serializing_if = "Option::is_none")]
    pub method: Option<AutoMergeMethod>,

    #[command(flatten)]
    #[serde(flatten)]
    pub auth: AuthArgs,
}

#[derive(Debug, clap::Args, Serialize)]
pub struct FetchArgs {
    /// PR number to fetch.
    #[arg(value_name = "PR_NUM")]
    #[serde(skip)]
    pub pr: u64,

    /// Override the bookmark template. Default: `pr_fetch_bookmark_template`
    /// in config, else `pr-{number}/{branch}`. Placeholders: `{number}`,
    /// `{branch}` (head.ref), `{user}`
    /// (head.user.login), `{repo}` (head.repo.name). `{{` / `}}` are literal
    /// braces.
    #[arg(short = 't', long, value_name = "STR")]
    #[serde(
        rename = "pr_fetch_bookmark_template",
        skip_serializing_if = "Option::is_none"
    )]
    pub template: Option<String>,

    /// Replace an existing local bookmark of the same name.
    #[arg(short = 'f', long)]
    #[serde(skip)]
    pub force: bool,

    #[command(flatten)]
    #[serde(flatten)]
    pub auth: AuthArgs,
}

#[derive(Debug, clap::Args, Serialize)]
pub struct CreateArgs {
    /// Revision to create the PR from.
    #[arg(value_name = "REV")]
    #[serde(skip)]
    pub rev: String,

    /// Override the base bookmark. Default: closest ancestor bookmark on the
    /// stack, falling back to the remote's `main` / `master` / configured
    /// `default_base_branch`.
    #[arg(long, value_name = "BRANCH")]
    #[serde(skip)]
    pub base: Option<String>,

    /// Force the PR to be a draft. Overrides config (default: `draft = false`).
    /// Use `--draft=false` or `--no-draft` to force non-draft.
    #[arg(
        long,
        num_args = 0..=1,
        require_equals = true,
        default_missing_value = "true",
        default_value_if("no_draft", "true", Some("false")),
    )]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub draft: Option<bool>,

    /// Force the PR to be non-draft. Overrides config. Equivalent to `--draft=false`.
    #[arg(long = "no-draft", conflicts_with = "draft")]
    #[serde(skip)]
    pub no_draft: bool,

    /// Enable auto-merge on the PR after creation (merges once required checks
    /// pass). Overrides config (default: `auto_merge = false`). Use
    /// `--auto-merge=false` or `--no-auto-merge` force no auto merge.
    #[arg(
        long = "auto-merge",
        num_args = 0..=1,
        require_equals = true,
        default_missing_value = "true",
        default_value_if("no_auto_merge", "true", Some("false")),
    )]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_merge: Option<bool>,

    /// Disable auto-merge on the created PR. Overrides config. Equivalent to
    /// `--auto-merge=false`.
    #[arg(long = "no-auto-merge", conflicts_with = "auto_merge")]
    #[serde(skip)]
    pub no_auto_merge: bool,

    /// Merge method used when auto-merge is enabled. Overrides config
    /// `auto_merge_method` (default `merge`).
    #[arg(long = "auto-merge-method", value_name = "METHOD", value_enum)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_merge_method: Option<AutoMergeMethod>,

    /// Template path or name under `.github/PULL_REQUEST_TEMPLATE/`. Default:
    /// `template_path` in config, else auto-detect
    /// `.github/PULL_REQUEST_TEMPLATE.md`. CLI path resolution needs the repo
    /// root, so this stays out of figment merging and is handled by
    /// `resolve_template_path` at handler time.
    #[arg(long, value_name = "PATH_OR_NAME")]
    #[serde(skip)]
    pub template: Option<String>,

    /// Skip template selection entirely.
    #[arg(long = "no-template", conflicts_with = "template")]
    #[serde(skip)]
    pub no_template: bool,

    /// Editor command; shell-words split, e.g. `--editor "nvim +7"`. Default:
    /// `editor` in config, then `$VISUAL`, then `$EDITOR`.
    #[arg(short = 'e', long, value_name = "CMD", value_parser = shell_words::split)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub editor: Option<Vec<String>>,

    #[command(flatten)]
    #[serde(flatten)]
    pub auth: AuthArgs,
}

pub async fn dispatch(action: PrAction) -> Result<()> {
    let mut fig = config::load_figment();
    fig = match &action {
        PrAction::Create(a) => fig.merge(Serialized::defaults(a)),
        PrAction::Fetch(a) => fig.merge(Serialized::defaults(a)),
        PrAction::AutoMerge(a) => fig.merge(Serialized::defaults(a)),
    };
    let config = config::extract(&fig)?;
    config::validate(&config)?;

    let token = auth::resolve_token(&config).await?;
    let jj = jj::real::JjCli;
    let gh = gh::real::OctocrabGh::new(&token)?;
    let editor = TempfileEditor;
    match action {
        PrAction::Create(args) => create(&jj, &gh, &editor, &config, &args).await?,
        PrAction::Fetch(args) => fetch::run(&jj, &gh, &config, &args).await?,
        PrAction::AutoMerge(args) => auto_merge(&jj, &gh, &config, &args.number_or_rev).await?,
    }

    Ok(())
}

/// Resolved lookup state for a revision: the bookmark, the remote target, the
/// `owner:branch` head spec, the detected trunk bookmark, and the open PR (if
/// any) whose head matches.
///
/// Shared by `jj-gh pr auto-merge <rev>` and `jj-gh debug pr-lookup`.
#[derive(Debug)]
pub struct PrLookup {
    pub branch: String,
    pub target: remote::Target,
    pub head_spec: String,
    pub default_base: String,
    pub summary: Option<PrSummary>,
}

/// Resolve a revision into its PR-lookup context: bookmark, remote target,
/// head spec, trunk bookmark, and any existing open PR.
///
/// # Errors
///
/// Returns an error if `rev` has no local bookmark, if `origin` is unset, if
/// `trunk()` is empty, or if any underlying jj/GH call fails.
pub async fn resolve_pr<J: Jj, G: Gh>(jj: &J, gh: &G, rev: &str) -> Result<PrLookup> {
    let info = jj.resolve_rev(rev).await?;
    let branch = info
        .bookmarks
        .first()
        .cloned()
        .ok_or_else(|| anyhow!("no local bookmark on `{rev}`; nothing to look up"))?;

    let origin_url = jj
        .remote_url("origin")
        .await?
        .ok_or_else(|| anyhow!("origin remote is not configured"))?;
    let upstream_url = jj.remote_url("upstream").await?;
    let target = remote::target(&origin_url, upstream_url.as_deref())?;
    let head_spec = target.head_spec(&branch);

    let default_base = jj
        .trunk_branch()
        .await?
        .ok_or_else(|| anyhow!("could not detect trunk() bookmark"))?;

    let summary = gh
        .find_open_pr(&target.owner, &target.repo, &head_spec)
        .await?;

    Ok(PrLookup {
        branch,
        target,
        head_spec,
        default_base,
        summary,
    })
}

async fn auto_merge<J, G>(jj: &J, gh: &G, config: &Config, number_or_rev: &str) -> Result<()>
where
    J: Jj,
    G: Gh,
{
    let pr = if let Ok(num) = number_or_rev.parse::<u64>() {
        let origin_url = jj
            .remote_url("origin")
            .await?
            .ok_or_else(|| anyhow!("origin remote is not configured"))?;
        let upstream_url = jj.remote_url("upstream").await?;
        let target = remote::target(&origin_url, upstream_url.as_deref())?;
        gh.get_pr(&target.owner, &target.repo, num).await?
    } else {
        let lookup = resolve_pr(jj, gh, number_or_rev).await?;
        let summary = lookup.summary.ok_or_else(|| {
            anyhow!(
                "no open PR for revision `{number_or_rev}` (head `{}`)",
                lookup.head_spec,
            )
        })?;
        gh.get_pr(&lookup.target.owner, &lookup.target.repo, summary.number)
            .await?
    };

    gh.enable_auto_merge(&pr.graphql_node_id, config.auto_merge_method)
        .await
        .with_context(|| format!("enabling auto-merge on #{}", pr.number))?;

    log::info!("Enabled auto-merge for PR");
    println!("{}", pr.html_url);
    Ok(())
}

/// Run the full pr-create flow.
///
/// # Errors
///
/// Returns an error from any step (rev resolution, GH API, push, editor, etc.).
async fn create<J, G, E>(
    jj: &J,
    gh: &G,
    editor: &E,
    config: &Config,
    args: &CreateArgs,
) -> Result<()>
where
    J: Jj,
    G: Gh,
    E: EditorRoundTrip,
{
    let info = jj.resolve_rev(&args.rev).await?;
    let existing_branch = info.bookmarks.first().cloned();

    let origin_url = jj
        .remote_url("origin")
        .await?
        .ok_or_else(|| anyhow!("origin remote is not configured"))?;
    let upstream_url = jj.remote_url("upstream").await?;
    let target = remote::target(&origin_url, upstream_url.as_deref())?;

    // Pre-flight only when we already have a bookmark; an unpushed rev can't have
    // a matching open PR.
    if let Some(branch) = &existing_branch {
        let head_spec = target.head_spec(branch);
        if let Some(existing) = gh
            .find_open_pr(&target.owner, &target.repo, &head_spec)
            .await?
        {
            log::info!(
                "PR #{} is already {} for `{}`: {}",
                existing.number,
                existing.state,
                head_spec,
                existing.title,
            );
            println!("{}", existing.html_url);
            return Ok(());
        }
    }

    let ancestor = jj.stacked_ancestor_bookmark(&args.rev).await?;
    let detected_base = jj
        .trunk_branch()
        .await?
        .unwrap_or_else(|| config.default_base_branch.clone());
    let base = resolve_base(args, ancestor.as_deref(), &detected_base);

    if !gh.branch_exists(&target.owner, &target.repo, &base).await? {
        return Err(anyhow!(
            "base branch `{base}` does not exist on {}/{}",
            target.owner,
            target.repo,
        ));
    }

    let title_revset = jj::title_base_revset(&args.rev, ancestor.as_deref());
    let default_title = jj.first_commit_description(&title_revset).await?;

    let raw_template = load_template_for(args, config, jj)?;
    let initial_fm = Frontmatter {
        title: default_title,
        base: base.clone(),
        labels: vec![],
        draft: config.draft,
        auto_merge: config.auto_merge,
        auto_merge_method: config.auto_merge_method,
    };
    let initial_buffer = initial_fm.render(raw_template.as_deref().unwrap_or(""))?;
    let raw_template_body = raw_template.unwrap_or_default();

    let visual = std::env::var("VISUAL").ok();
    let editor_env = std::env::var("EDITOR").ok();
    let editor_argv = resolve_editor_argv(config, visual.as_deref(), editor_env.as_deref())?;
    let edited = editor.edit(&editor_argv, &initial_buffer).await?;
    let (final_fm, body) = Frontmatter::parse(&edited)?;
    validation::validate(&final_fm, &body, &raw_template_body)?;

    jj.push(&args.rev).await?;

    let branch = if let Some(b) = existing_branch {
        b
    } else {
        let refreshed = jj.resolve_rev(&args.rev).await?;
        refreshed
            .bookmarks
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("`jj git push -c {}` did not create a bookmark", args.rev))?
    };
    let head_spec = target.head_spec(&branch);
    let final_base = final_fm.base.clone();

    let created = gh
        .create_pr(CreatePrRequest {
            owner: target.owner.clone(),
            repo: target.repo.clone(),
            title: final_fm.title,
            body,
            head: head_spec,
            base: final_base,
            draft: final_fm.draft,
        })
        .await?;

    if !final_fm.labels.is_empty() {
        gh.add_labels(
            &target.owner,
            &target.repo,
            created.number,
            &final_fm.labels,
        )
        .await
        .context("PR created, but adding labels failed")?;
    }

    if final_fm.auto_merge {
        gh.enable_auto_merge(&created.node_id, final_fm.auto_merge_method)
            .await
            .context("PR created, but enabling auto-merge failed")?;
    }

    println!("{}", created.html_url);
    Ok(())
}

fn resolve_base(args: &CreateArgs, ancestor: Option<&str>, detected: &str) -> String {
    args.base
        .clone()
        .or_else(|| ancestor.map(str::to_string))
        .unwrap_or_else(|| detected.to_string())
}

fn load_template_for<J: Jj>(args: &CreateArgs, config: &Config, _jj: &J) -> Result<Option<String>> {
    let repo_root = std::env::current_dir().context("could not read cwd")?;
    let fs = RealFs;
    match resolve_template_path(args, config, &repo_root, &fs) {
        TemplateChoice::None => Ok(None),
        TemplateChoice::Path(p) => load_template_file(&p, &fs),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cli() -> CreateArgs {
        CreateArgs {
            rev: "@-".into(),
            base: None,
            draft: None,
            no_draft: false,
            auto_merge: None,
            no_auto_merge: false,
            auto_merge_method: None,
            template: None,
            no_template: false,
            editor: None,
            auth: crate::cli::AuthArgs {
                gh_askpass: None,
                askpass_timeout_secs: None,
            },
        }
    }

    fn merge_into_config(
        config_draft: Option<bool>,
        config_auto: Option<bool>,
        config_method: Option<AutoMergeMethod>,
        args: &CreateArgs,
    ) -> Config {
        let mut fig = config::defaults_figment();
        if let Some(v) = config_draft {
            fig = fig.merge(Serialized::default("draft", v));
        }
        if let Some(v) = config_auto {
            fig = fig.merge(Serialized::default("auto_merge", v));
        }
        if let Some(v) = config_method {
            fig = fig.merge(Serialized::default("auto_merge_method", v));
        }
        fig = fig.merge(Serialized::defaults(args));
        config::extract(&fig).unwrap()
    }

    fn args_with_base(base: Option<&str>) -> CreateArgs {
        let mut a = cli();
        a.base = base.map(str::to_string);
        a
    }

    #[test]
    fn base_cli_wins_over_ancestor_and_detected() {
        assert_eq!(
            resolve_base(&args_with_base(Some("release")), Some("ancestor"), "main"),
            "release"
        );
    }

    #[test]
    fn base_ancestor_wins_over_detected() {
        assert_eq!(
            resolve_base(&args_with_base(None), Some("ancestor"), "main"),
            "ancestor"
        );
    }

    #[test]
    fn base_falls_back_to_detected() {
        assert_eq!(resolve_base(&args_with_base(None), None, "main"), "main");
    }

    #[test]
    fn cli_draft_overrides_config() {
        let mut a = cli();
        a.draft = Some(true);
        let c = merge_into_config(Some(false), None, None, &a);
        assert!(c.draft);
    }

    #[test]
    fn cli_no_draft_overrides_config() {
        let mut a = cli();
        a.draft = Some(false);
        let c = merge_into_config(Some(true), None, None, &a);
        assert!(!c.draft);
    }

    #[test]
    fn draft_defaults_to_config_when_cli_unset() {
        let c1 = merge_into_config(Some(true), None, None, &cli());
        assert!(c1.draft);
        let c2 = merge_into_config(Some(false), None, None, &cli());
        assert!(!c2.draft);
    }

    #[test]
    fn cli_auto_merge_overrides_config() {
        let mut a = cli();
        a.auto_merge = Some(true);
        let c = merge_into_config(None, Some(false), None, &a);
        assert!(c.auto_merge);
    }

    #[test]
    fn cli_no_auto_merge_overrides_config() {
        let mut a = cli();
        a.auto_merge = Some(false);
        let c = merge_into_config(None, Some(true), None, &a);
        assert!(!c.auto_merge);
    }

    #[test]
    fn auto_merge_defaults_to_config_when_cli_unset() {
        let c = merge_into_config(None, Some(true), None, &cli());
        assert!(c.auto_merge);
    }

    #[test]
    fn cli_auto_merge_method_overrides_config() {
        let mut a = cli();
        a.auto_merge_method = Some(AutoMergeMethod::Squash);
        let c = merge_into_config(None, None, Some(AutoMergeMethod::Rebase), &a);
        assert_eq!(c.auto_merge_method, AutoMergeMethod::Squash);
    }

    #[test]
    fn auto_merge_method_defaults_to_config_when_cli_unset() {
        let c = merge_into_config(None, None, Some(AutoMergeMethod::Rebase), &cli());
        assert_eq!(c.auto_merge_method, AutoMergeMethod::Rebase);
    }

    fn empty_auth() -> crate::cli::AuthArgs {
        crate::cli::AuthArgs {
            gh_askpass: None,
            askpass_timeout_secs: None,
        }
    }

    #[test]
    fn fetch_cli_template_overrides_config() {
        let args = FetchArgs {
            pr: 1,
            template: Some("cli-{number}".into()),
            force: false,
            auth: empty_auth(),
        };
        let fig = config::defaults_figment()
            .merge(Serialized::default(
                "pr_fetch_bookmark_template",
                "cfg-{number}",
            ))
            .merge(Serialized::defaults(&args));
        let c = config::extract(&fig).unwrap();
        assert_eq!(
            c.pr_fetch_bookmark_template.as_deref(),
            Some("cli-{number}")
        );
    }

    #[test]
    fn auto_merge_args_method_overrides_config_via_rename() {
        let args = AutoMergeArgs {
            number_or_rev: "1".into(),
            method: Some(AutoMergeMethod::Squash),
            auth: empty_auth(),
        };
        let fig = config::defaults_figment()
            .merge(Serialized::default(
                "auto_merge_method",
                AutoMergeMethod::Rebase,
            ))
            .merge(Serialized::defaults(&args));
        let c = config::extract(&fig).unwrap();
        assert_eq!(c.auto_merge_method, AutoMergeMethod::Squash);
    }

    #[test]
    fn auth_cli_overrides_config_via_flatten() {
        let args = AutoMergeArgs {
            number_or_rev: "1".into(),
            method: None,
            auth: crate::cli::AuthArgs {
                gh_askpass: Some(vec!["cli-askpass".into()]),
                askpass_timeout_secs: Some(99),
            },
        };
        let fig = config::defaults_figment()
            .merge(Serialized::default(
                "gh_askpass",
                vec!["cfg-askpass".to_string()],
            ))
            .merge(Serialized::default("askpass_timeout_secs", 5u64))
            .merge(Serialized::defaults(&args));
        let c = config::extract(&fig).unwrap();
        assert_eq!(
            c.gh_askpass.as_deref(),
            Some(&["cli-askpass".to_string()][..])
        );
        assert_eq!(c.askpass_timeout_secs, 99);
    }
}
