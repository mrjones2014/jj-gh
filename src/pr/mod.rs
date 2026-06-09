//! End-to-end orchestrator for `jj-gh pr create` / `jj-gh pr fetch` / `jj-gh pr auto-merge`.

mod auto_merge;
mod editor;
pub mod fetch;
mod frontmatter;
mod pr_log;
mod restack;
mod retry_failed;
mod template;
mod validation;

use crate::{
    auth::{self, OsEnv},
    cli::{GlobalOpts, GlobalOptsInput},
    config,
    fs::RealFs,
    gh::{self, Gh, PrDetails, PrSummary, remote},
    git::real::RealGit,
    jj::{
        self, Jj,
        inject::{TemplateAliases, escape_jj_string},
    },
    pr::{
        auto_merge::{AutoMergeArgs, AutoMergeArgsInput},
        editor::{
            TempfileEditor,
            edit::{EditArgs, EditArgsInput},
        },
        fetch::{FetchArgs, FetchArgsInput},
        pr_log::{PrLogArgs, PrLogArgsInput},
        restack::{RestackArgs, RestackArgsInput},
        retry_failed::{RetryFailedArgs, RetryFailedArgsInput},
        template::TemplateSource,
    },
};
use anyhow::{Context, Result, anyhow};
use clap::Subcommand;
use figment::providers::Serialized;

pub use editor::create::{CreateArgs, CreateArgsInput};

#[derive(Debug, Subcommand)]
pub enum PrAction {
    /// Open your preferred editor to create a PR from a revision.
    ///
    /// Opens your editor to a markdown file where you can write the PR description,
    /// and set PR metadata like title, labels, auto-merge, etc. via the markdown frontmatter.
    /// This supports stacked PRs; by default the base branch is set to the closest ancestor bookmark
    /// if one exists, otherwise `trunk()`.
    #[command(visible_alias = "c")]
    Create(CreateArgsInput),
    /// Fetch a pull request into a local bookmark.
    ///
    /// This command accepts either a revision ID or a PR number. If given a revision ID, the
    /// PR number will be looked up via the API. Requires a colocated git repository; the special
    /// `refs/pull/123/head` ref is fetched via `git` because `jj` cannot yet fetch arbitrary refs.
    ///
    /// See: <https://github.com/jj-vcs/jj/issues/4388>
    #[command(visible_alias = "f")]
    Fetch(FetchArgsInput),
    /// Enable auto-merge on a PR.
    ///
    /// Accepts either a PR number or a revision; with a revision, the PR is looked up by the rev's
    /// local bookmark. Fails if the repo does not allow auto-merge.
    #[command(visible_alias = "am")]
    AutoMerge(AutoMergeArgsInput),
    /// Edit an existing PR's title, body, base, labels, reviewers, draft state,
    /// and auto-merge settings via the markdown frontmatter editor flow.
    ///
    /// Resolves the PR from a revision (via its local bookmark) or a PR number,
    /// fetches its current state, opens your editor, and applies only the diffs:
    /// labels you didn't touch keep whatever others (CI bots, etc.) set.
    #[command(visible_alias = "e")]
    Edit(EditArgsInput),

    /// Like `jj log`, but injects PR metadata (e.g. number, CI status, URL).
    ///
    /// This works by injecting template aliases keyed by `commit_id` and renders inline PR info
    /// in a temporary config file added via `jj`'s `--config-file` argument. Any arguments after
    /// `--` are forwarded to the underlying `jj log` invocation, e.g. `jj-gh pr log -- -r 'mine()'`.
    /// A default template that mirror's `jj`'s default template is provided, but you may provide
    /// your own with the `-T|--template` argument and use the injected template aliases.
    #[command(visible_alias = "l")]
    Log(PrLogArgsInput),

    /// Push the current `jj` stack shape up to GitHub by updating each PR's
    /// base branch to match its closest stacked ancestor bookmark.
    ///
    /// Restack does not rewrite the jj graph; the user shapes the graph first
    /// (e.g. via `jj rebase`) and then runs `jj-gh pr restack` to set each
    /// PR's `baseRefName` on the remote. Launches an interactive TUI by
    /// default. Pass `--dry-run` or `--json` to print the proposed plan
    /// without making any API calls.
    #[command(visible_alias = "rs")]
    Restack(RestackArgsInput),

    /// Re-run failed CI jobs on a PR.
    ///
    /// Resolves the PR from a revision (via its local bookmark) or PR number,
    /// then re-runs failed workflow runs on the PR's head commit. By default
    /// the command fails if CI has not yet completed, because GitHub refuses
    /// to re-run a workflow run until it reaches the `completed` state.
    ///
    /// With `--cancel`, in-progress runs are cancelled first; once they
    /// finalize, every workflow run is re-run (full pipeline restart).
    #[command(visible_alias = "rerun")]
    RetryFailed(RetryFailedArgsInput),
}

/// Dispatch the `pr` subcommand to the matching handler.
///
/// # Errors
///
/// Propagates errors from config loading, auth resolution, jj/GitHub API
/// calls, the editor round-trip, or any sub-handler (`create`, `fetch`,
/// `auto-merge`).
pub async fn dispatch(global: GlobalOptsInput, action: PrAction) -> Result<()> {
    let mut fig = config::load_figment().merge(Serialized::defaults(&global));
    fig = match &action {
        PrAction::Create(a) => fig.merge(Serialized::defaults(a)),
        PrAction::Fetch(a) => fig.merge(Serialized::defaults(a)),
        PrAction::AutoMerge(a) => fig.merge(Serialized::defaults(a)),
        PrAction::Edit(a) => fig.merge(Serialized::defaults(a)),
        PrAction::Log(a) => fig.merge(Serialized::defaults(a)),
        PrAction::Restack(a) => fig.merge(Serialized::defaults(a)),
        PrAction::RetryFailed(a) => fig.merge(Serialized::defaults(a)),
    };
    let config = config::extract(&fig)?;
    let globals = GlobalOpts::resolve(global, &config);

    let token = auth::resolve_token(&config).await?;
    let jj = jj::real::JjCli::new().await?;
    let gh = gh::real::OctocrabGh::new(&token)?;
    let editor = TempfileEditor;
    match action {
        PrAction::AutoMerge(input) => {
            let args = AutoMergeArgs::resolve(input, &config, &globals);
            auto_merge::run(&jj, &gh, &args).await?;
        }
        PrAction::Create(input) => {
            let args = CreateArgs::resolve(input, &config, &globals);
            editor::create::run(&jj, &gh, &OsEnv, &editor, &args).await?;
        }
        PrAction::Edit(input) => {
            let args = EditArgs::resolve(input, &config, &globals);
            editor::edit::run(&jj, &gh, &OsEnv, &editor, &args).await?;
        }
        PrAction::Fetch(input) => {
            let git = RealGit::new(jj.repo().clone());
            let args = FetchArgs::resolve(input, &config, &globals);
            fetch::run(&jj, &gh, &git, &args).await?;
        }
        PrAction::Log(input) => {
            let args = PrLogArgs::resolve(input, &config, &globals);
            pr_log::run(&gh, &jj, &args).await?;
        }
        PrAction::Restack(input) => {
            let args = RestackArgs::resolve(input, &config, &globals);
            restack::run(&jj, &gh, &args).await?;
        }
        PrAction::RetryFailed(input) => {
            let args = RetryFailedArgs::resolve(input, &config, &globals);
            retry_failed::run(&jj, &gh, &args).await?;
        }
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
/// Returns an error if `rev` has no local bookmark, if the configured
/// `default_remote` is unset, if `trunk()` is empty, or if any underlying
/// jj/GH call fails.
pub async fn get_pr<J: Jj, G: Gh>(
    jj: &J,
    gh: &G,
    default_remote: &str,
    upstream_remote: &str,
    number_or_rev: &str,
) -> Result<PrDetails> {
    Ok(
        resolve_pr_with_target(jj, gh, default_remote, upstream_remote, number_or_rev)
            .await?
            .0,
    )
}

/// Same as [`get_pr`] but also returns the resolved [`remote::Target`] so
/// callers that need the owner/repo for further API calls don't have to
/// re-derive it from the remote URL.
///
/// # Errors
///
/// See [`get_pr`].
pub async fn resolve_pr_with_target<J: Jj, G: Gh>(
    jj: &J,
    gh: &G,
    default_remote: &str,
    upstream_remote: &str,
    number_or_rev: &str,
) -> Result<(PrDetails, remote::Target)> {
    if let Ok(num) = number_or_rev.parse::<u64>() {
        let origin_url = jj
            .remote_url(default_remote)
            .await?
            .ok_or_else(|| anyhow!("`{default_remote}` remote is not configured"))?;
        let upstream_url = jj.remote_url(upstream_remote).await?;
        let target = remote::target(&origin_url, upstream_url.as_deref())?;
        let pr = gh.get_pr(&target.owner, &target.repo, num).await?;
        Ok((pr, target))
    } else {
        let lookup =
            resolve_pr_for_rev(jj, gh, default_remote, upstream_remote, number_or_rev).await?;
        let summary = lookup.summary.ok_or_else(|| {
            anyhow!(
                "no open PR for revision `{number_or_rev}` (head `{}`)",
                lookup.head_spec,
            )
        })?;
        let pr = gh
            .get_pr(&lookup.target.owner, &lookup.target.repo, summary.number)
            .await?;
        Ok((pr, lookup.target))
    }
}

/// Resolve a revision into its PR-lookup context: bookmark, remote target,
/// head spec, trunk bookmark, and any existing open PR.
///
/// # Errors
///
/// Returns an error if `rev` has no local bookmark, if the configured
/// `default_remote` is unset, if `trunk()` is empty, or if any underlying
/// jj/GH call fails.
pub async fn resolve_pr_for_rev<J: Jj, G: Gh>(
    jj: &J,
    gh: &G,
    default_remote: &str,
    upstream_remote: &str,
    rev: &str,
) -> Result<PrLookup> {
    let info = jj.resolve_rev(rev).await?;
    let branch = info
        .bookmarks
        .first()
        .cloned()
        .ok_or_else(|| anyhow!("no local bookmark on `{rev}`; nothing to look up"))?;

    let origin_url = jj
        .remote_url(default_remote)
        .await?
        .ok_or_else(|| anyhow!("`{default_remote}` remote is not configured"))?;
    let upstream_url = jj.remote_url(upstream_remote).await?;
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

async fn load_template_for<J: Jj>(
    args: &CreateArgs,
    jj: &J,
    title_revset: &str,
    default_title: &str,
    base: &str,
    head_branch: Option<&str>,
) -> Result<Option<String>> {
    let repo_root = std::env::current_dir().context("could not read cwd")?;
    let fs = RealFs;
    let user_layer = config::user_layer_template()?;
    let repo_layer = config::repo_layer_template()?;
    match template::resolve_template_source(args, &repo_layer, &user_layer, &repo_root, &fs) {
        TemplateSource::None => Ok(None),
        TemplateSource::File(p) => template::load_template_file(&p, &fs),
        TemplateSource::JjTemplate(t) => {
            let oldest_rev_id = jj
                .eval_template(title_revset, r#"commit_id.short(40) ++ "\n""#, None, true)
                .await
                .context("resolving oldest commit id for `pr_oldest_rev_id` alias")?
                .lines()
                .next()
                .unwrap_or("")
                .to_string();
            let aliases = TemplateAliases::builder()
                .alias("pr_title", quote_jj(default_title))
                .alias("pr_base", quote_jj(base))
                .alias("pr_head_branch", quote_jj(head_branch.unwrap_or("")))
                .alias("pr_oldest_rev_id", quote_jj(&oldest_rev_id));
            let tmp = aliases.write_temp_config()?;
            let body = jj
                .eval_template(title_revset, &t, Some(tmp.path()), true)
                .await
                .context("evaluating PR body template")?;
            Ok(Some(body.trim_end_matches('\n').to_string()))
        }
    }
}

/// Wrap `s` as a jj template double-quoted string literal, escaping `\` and `"`.
fn quote_jj(s: &str) -> String {
    format!(r#""{}""#, escape_jj_string(s))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AutoMergeMethod, Config};
    use clap::Parser;

    #[derive(clap::Parser, Debug)]
    #[command(no_binary_name = true)]
    struct CreateArgsParser {
        #[command(flatten)]
        args: CreateArgsInput,
    }

    #[derive(clap::Parser, Debug)]
    #[command(no_binary_name = true)]
    struct PrLogArgsParser {
        #[command(flatten)]
        args: PrLogArgsInput,
    }

    fn parse_create(argv: &[&str]) -> CreateArgsInput {
        CreateArgsParser::try_parse_from(argv.iter().copied())
            .expect("CreateArgsInput failed to parse")
            .args
    }

    fn parse_pr_log(argv: &[&str]) -> PrLogArgsInput {
        PrLogArgsParser::try_parse_from(argv.iter().copied())
            .expect("PrLogArgsInput failed to parse")
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

    #[test]
    fn create_bare_argv_lets_config_win() {
        let c = merged_create(
            &["@-"],
            r#"
            [jj-gh]
            draft = true
            auto_merge = true
            auto_merge_method = "squash"
            pr_create_show_diffs = false
            "#,
        );
        assert!(c.draft);
        assert!(c.auto_merge);
        assert_eq!(c.auto_merge_method, AutoMergeMethod::Squash);
        assert!(!c.pr_create_show_diffs);

        let c = merged_create(
            &["@-"],
            r#"
            [jj-gh]
            draft = false
            auto_merge = false
            auto_merge_method = "rebase"
            pr_create_show_diffs = true
            "#,
        );
        assert!(!c.draft);
        assert!(!c.auto_merge);
        assert_eq!(c.auto_merge_method, AutoMergeMethod::Rebase);
        assert!(c.pr_create_show_diffs);
    }

    #[test]
    fn create_positive_flags_override_config() {
        let c = merged_create(
            &[
                "@-",
                "--draft",
                "--auto-merge",
                "--diffs",
                "--auto-merge-method",
                "rebase",
            ],
            r#"
            [jj-gh]
            draft = false
            auto_merge = false
            auto_merge_method = "merge"
            pr_create_show_diffs = false
            "#,
        );
        assert!(c.draft);
        assert!(c.auto_merge);
        assert_eq!(c.auto_merge_method, AutoMergeMethod::Rebase);
        assert!(c.pr_create_show_diffs);
    }

    #[test]
    fn create_negative_flags_override_config() {
        let c = merged_create(
            &["@-", "--no-draft", "--no-auto-merge", "--no-diffs"],
            "\
            [jj-gh]\n\
            draft = true\n\
            auto_merge = true\n\
            pr_create_show_diffs = true\n\
            ",
        );
        assert!(!c.draft);
        assert!(!c.auto_merge);
        assert!(!c.pr_create_show_diffs);
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

    #[test]
    fn fetch_cli_template_overrides_config() {
        let input = FetchArgsInput {
            pr: 1,
            template: Some("cli-{number}".into()),
            force: false,
        };
        let fig = config::defaults_figment()
            .merge(Serialized::default(
                "pr_fetch_bookmark_template",
                "cfg-{number}",
            ))
            .merge(Serialized::defaults(&input));
        let c = config::extract(&fig).unwrap();
        assert_eq!(
            c.pr_fetch_bookmark_template.as_deref(),
            Some("cli-{number}")
        );
    }

    #[test]
    fn auto_merge_args_method_overrides_config_via_rename() {
        let input = AutoMergeArgsInput {
            number_or_rev: "1".into(),
            auto_merge_method: Some(AutoMergeMethod::Squash),
        };
        let fig = config::defaults_figment()
            .merge(Serialized::default(
                "auto_merge_method",
                AutoMergeMethod::Rebase,
            ))
            .merge(Serialized::defaults(&input));
        let c = config::extract(&fig).unwrap();
        assert_eq!(c.auto_merge_method, AutoMergeMethod::Squash);
    }

    #[test]
    #[expect(deprecated)]
    fn global_remote_overrides_default_remote_config() {
        use crate::cli::GlobalOptsInput;
        use clap::Parser;

        #[derive(clap::Parser, Debug)]
        #[command(no_binary_name = true)]
        struct GlobalParser {
            #[command(flatten)]
            opts: GlobalOptsInput,
        }

        let global =
            GlobalParser::try_parse_from(["--remote", "fork", "--upstream-remote", "canonical"])
                .unwrap()
                .opts;
        let fig = config::defaults_figment()
            .merge(config::JjConfProvider::from_memory(
                "test",
                r#"
                [jj-gh]
                default_remote = "cfg-origin"
                upstream_remote = "cfg-upstream"
                "#,
            ))
            .merge(Serialized::defaults(&global));
        let c = config::extract(&fig).unwrap();
        assert_eq!(c.default_remote, Some("fork".to_string()));
        assert_eq!(c.upstream_remote, "canonical");
    }

    #[test]
    #[expect(deprecated)]
    fn config_remote_used_when_global_not_set() {
        use crate::cli::GlobalOptsInput;
        use clap::Parser;

        #[derive(clap::Parser, Debug)]
        #[command(no_binary_name = true)]
        struct GlobalParser {
            #[command(flatten)]
            opts: GlobalOptsInput,
        }

        let global = GlobalParser::try_parse_from::<[&str; 0], _>([])
            .unwrap()
            .opts;
        let fig = config::defaults_figment()
            .merge(config::JjConfProvider::from_memory(
                "test",
                r#"
                [jj-gh]
                default_remote = "cfg-origin"
                upstream_remote = "cfg-upstream"
                "#,
            ))
            .merge(Serialized::defaults(&global));
        let c = config::extract(&fig).unwrap();
        assert_eq!(c.default_remote, Some("cfg-origin".to_string()));
        assert_eq!(c.upstream_remote, "cfg-upstream");
    }
}
