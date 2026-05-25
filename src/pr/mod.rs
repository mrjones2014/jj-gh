//! End-to-end orchestrator for `jj-gh pr create` / `jj-gh pr fetch` / `jj-gh pr auto-merge`.

mod auto_merge;
mod create;
mod editor;
pub mod fetch;
mod frontmatter;
mod pr_log;
mod template;
mod validation;

use crate::{
    auth,
    config::{self, Config},
    fs::RealFs,
    gh::{self, Gh, PrDetails, PrSummary, remote},
    jj::{self, Jj},
    pr::{
        auto_merge::AutoMergeArgs, create::CreateArgs, editor::TempfileEditor, fetch::FetchArgs,
        pr_log::PrLogArgs, template::TemplateChoice,
    },
};
use anyhow::{Context, Result, anyhow};
use clap::Subcommand;
use figment::providers::Serialized;

#[derive(Debug, Subcommand)]
pub enum PrAction {
    /// Open your preferred editor to create a PR from a revision. Opens your editor
    /// to a markdown file where you can write the PR description, and set PR metadata
    /// like title, labels, auto-merge, etc. via the markdown frontmatter. This supports
    /// stacked PRs; by default the base branch is set to the closest ancestor bookmark
    /// if one exists, otherwise `trunk()`.
    #[command(visible_alias = "c")]
    Create(CreateArgs),
    /// Fetch a pull request into a local bookmark. This command accepts either a revision
    /// ID or a PR number. If given a revision ID, the PR number will be looked up via the API.
    /// Requires a colocated git repository; the special `refs/pull/123/head` ref is fetched via
    /// `git` because `jj` cannot yet fetch arbitrary refs.
    ///
    /// See: <https://github.com/jj-vcs/jj/issues/4388>
    #[command(visible_alias = "f")]
    Fetch(FetchArgs),
    /// Enable auto-merge on a PR. Accepts either a PR number or a revision; with
    /// a revision, the PR is looked up by the rev's local bookmark. Fails if the
    /// repo does not allow auto-merge.
    #[command(visible_alias = "am")]
    AutoMerge(AutoMergeArgs),

    /// Like `jj log`, but injects PR metadata (number, CI status, URL) as
    /// template aliases keyed by `commit_id` and renders inline PR info in the
    /// default template. Any arguments after `--` are forwarded to the
    /// underlying `jj log` invocation, e.g. `jj-gh pr log -- -r 'mine()'`.
    #[command(visible_alias = "l")]
    Log(PrLogArgs),
}

/// Dispatch the `pr` subcommand to the matching handler.
///
/// # Errors
///
/// Propagates errors from config loading, auth resolution, jj/GitHub API
/// calls, the editor round-trip, or any sub-handler (`create`, `fetch`,
/// `auto-merge`).
pub async fn dispatch(action: PrAction) -> Result<()> {
    let mut fig = config::load_figment();
    fig = match &action {
        PrAction::Create(a) => fig.merge(Serialized::defaults(a)),
        PrAction::Fetch(a) => fig.merge(Serialized::defaults(a)),
        PrAction::AutoMerge(a) => fig.merge(Serialized::defaults(a)),
        PrAction::Log(a) => fig.merge(Serialized::defaults(a)),
    };
    let config = config::extract(&fig)?;
    config::validate(&config)?;

    let token = auth::resolve_token(&config).await?;
    let jj = jj::real::JjCli;
    let gh = gh::real::OctocrabGh::new(&token)?;
    let editor = TempfileEditor;
    match action {
        PrAction::Create(args) => create::run(&jj, &gh, &editor, &config, &args).await?,
        PrAction::Fetch(args) => fetch::run(&jj, &gh, &config, &args).await?,
        PrAction::AutoMerge(args) => auto_merge::run(&jj, &gh, &config, &args).await?,
        PrAction::Log(args) => pr_log::log(&args, &config, &gh, &jj).await?,
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

/// Lookup a PR by either a revision ID or PR number
///
/// # Errors
///
/// Returns an error if `rev` has no local bookmark, if `origin` is unset, if
/// `trunk()` is empty, or if any underlying jj/GH call fails.
pub async fn get_pr<J: Jj, G: Gh>(jj: &J, gh: &G, number_or_rev: &str) -> Result<PrDetails> {
    if let Ok(num) = number_or_rev.parse::<u64>() {
        let origin_url = jj
            .remote_url("origin")
            .await?
            .ok_or_else(|| anyhow!("origin remote is not configured"))?;
        let upstream_url = jj.remote_url("upstream").await?;
        let target = remote::target(&origin_url, upstream_url.as_deref())?;
        gh.get_pr(&target.owner, &target.repo, num).await
    } else {
        let lookup = resolve_pr_for_rev(jj, gh, number_or_rev).await?;
        let summary = lookup.summary.ok_or_else(|| {
            anyhow!(
                "no open PR for revision `{number_or_rev}` (head `{}`)",
                lookup.head_spec,
            )
        })?;
        gh.get_pr(&lookup.target.owner, &lookup.target.repo, summary.number)
            .await
    }
}

/// Resolve a revision into its PR-lookup context: bookmark, remote target,
/// head spec, trunk bookmark, and any existing open PR.
///
/// # Errors
///
/// Returns an error if `rev` has no local bookmark, if `origin` is unset, if
/// `trunk()` is empty, or if any underlying jj/GH call fails.
pub async fn resolve_pr_for_rev<J: Jj, G: Gh>(jj: &J, gh: &G, rev: &str) -> Result<PrLookup> {
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

fn resolve_base(args: &CreateArgs, ancestor: Option<&str>, detected: &str) -> String {
    args.base
        .clone()
        .or_else(|| ancestor.map(str::to_string))
        .unwrap_or_else(|| detected.to_string())
}

fn load_template_for<J: Jj>(args: &CreateArgs, config: &Config, _jj: &J) -> Result<Option<String>> {
    let repo_root = std::env::current_dir().context("could not read cwd")?;
    let fs = RealFs;
    match template::resolve_template_path(args, config, &repo_root, &fs) {
        TemplateChoice::None => Ok(None),
        TemplateChoice::Path(p) => template::load_template_file(&p, &fs),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::AutoMergeMethod,
        pr::{auto_merge::AutoMergeArgs, fetch::FetchArgs},
    };
    use clap::Parser;

    #[derive(clap::Parser, Debug)]
    #[command(no_binary_name = true)]
    struct CreateArgsParser {
        #[command(flatten)]
        args: CreateArgs,
    }

    #[derive(clap::Parser, Debug)]
    #[command(no_binary_name = true)]
    struct PrLogArgsParser {
        #[command(flatten)]
        args: PrLogArgs,
    }

    fn parse_create(argv: &[&str]) -> CreateArgs {
        CreateArgsParser::try_parse_from(argv.iter().copied())
            .expect("CreateArgs failed to parse")
            .args
    }

    fn parse_pr_log(argv: &[&str]) -> PrLogArgs {
        PrLogArgsParser::try_parse_from(argv.iter().copied())
            .expect("PrLogArgs failed to parse")
            .args
    }

    fn merged_create(argv: &[&str], toml_config: &str) -> Config {
        let argv = parse_create(argv);
        let fig = config::defaults_figment()
            .merge(config::JjConfProvider::from_memory("test", toml_config))
            .merge(Serialized::defaults(&argv));
        config::extract(&fig).unwrap()
    }

    fn merged_pr_log(argv: &[&str], toml_config: &str) -> Config {
        let argv = parse_pr_log(argv);
        let fig = config::defaults_figment()
            .merge(config::JjConfProvider::from_memory("test", toml_config))
            .merge(Serialized::defaults(&argv));
        config::extract(&fig).unwrap()
    }

    fn args_with_base(base: Option<&str>) -> CreateArgs {
        let mut a = parse_create(&["@-"]);
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
    fn create_bare_argv_lets_config_win() {
        let c = merged_create(
            &["@-"],
            r#"
            [jj-gh]
            draft = true
            auto_merge = true
            auto_merge_method = "squash"
            "#,
        );
        assert!(c.draft);
        assert!(c.auto_merge);
        assert_eq!(c.auto_merge_method, AutoMergeMethod::Squash);

        let c = merged_create(
            &["@-"],
            r#"
            [jj-gh]
            draft = false
            auto_merge = false
            auto_merge_method = "rebase"
            "#,
        );
        assert!(!c.draft);
        assert!(!c.auto_merge);
        assert_eq!(c.auto_merge_method, AutoMergeMethod::Rebase);
    }

    #[test]
    fn create_positive_flags_override_config() {
        let c = merged_create(
            &[
                "@-",
                "--draft",
                "--auto-merge",
                "--auto-merge-method",
                "rebase",
            ],
            r#"
            [jj-gh]
            draft = false
            auto_merge = false
            auto_merge_method = "merge"
            "#,
        );
        assert!(c.draft);
        assert!(c.auto_merge);
        assert_eq!(c.auto_merge_method, AutoMergeMethod::Rebase);
    }

    #[test]
    fn create_negative_flags_override_config() {
        let c = merged_create(
            &["@-", "--no-draft", "--no-auto-merge"],
            "\
            [jj-gh]\n\
            draft = true\n\
            auto_merge = true\n\
            ",
        );
        assert!(!c.draft);
        assert!(!c.auto_merge);
    }

    #[test]
    fn create_equals_value_syntax_is_rejected() {
        let err =
            CreateArgsParser::try_parse_from(["@-", "--draft=true"]).expect_err("should reject");
        assert!(
            err.to_string().contains("unexpected value"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn pr_log_bare_argv_lets_config_nerdfonts_win() {
        let c = merged_pr_log(
            &[],
            "\n\
            [jj-gh]\n\
            nerdfonts = true\n\
            ",
        );
        assert!(c.nerdfonts);

        let c = merged_pr_log(
            &[],
            "\n\
            [jj-gh]\n\
            nerdfonts = false\n\
            ",
        );
        assert!(!c.nerdfonts);
    }

    #[test]
    fn pr_log_nerdfonts_flags_override_config() {
        let c = merged_pr_log(
            &["--nerdfonts"],
            "\n\
            [jj-gh]\n\
            nerdfonts = false\n\
            ",
        );
        assert!(c.nerdfonts);

        let c = merged_pr_log(
            &["--no-nerdfonts"],
            "\n\
            [jj-gh]\n\
            nerdfonts = true\n\
            ",
        );
        assert!(!c.nerdfonts);
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
