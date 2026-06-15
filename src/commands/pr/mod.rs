//! End-to-end orchestrator for the `pr` subcommand: loads config, builds the
//! model, then dispatches to the matching handler. The model resolves auth and
//! builds the GitHub client lazily. One module per CLI subcommand; PR lookup
//! helpers live in [`crate::gh::pr_lookup`].

mod auto_merge;
mod create;
mod edit;
pub mod fetch;
mod log;
mod restack;
mod retry_failed;
mod url;

use crate::{
    cli::{GlobalOpts, GlobalOptsInput},
    commands::pr::url::{PrUrlArgs, PrUrlArgsInput},
    config,
    model::ModelImpl,
    ui::Spinner,
};
use anyhow::Result;
use clap::Subcommand;
#[cfg(test)]
use figment::providers::Serialized;
use serde::Serialize;

use self::log::{PrLogArgs, PrLogArgsInput};
use auto_merge::{AutoMergeArgs, AutoMergeArgsInput};
use edit::{EditArgs, EditArgsInput};
use fetch::{FetchArgs, FetchArgsInput};
use restack::{RestackArgs, RestackArgsInput};
use retry_failed::{RetryFailedArgs, RetryFailedArgsInput};

pub use create::{CreateArgs, CreateArgsInput};

#[derive(Serialize)]
struct ConfigOverrides<'a, T> {
    #[serde(flatten)]
    global: &'a GlobalOptsInput,
    #[serde(flatten)]
    action: &'a T,
}

#[derive(Debug, Serialize, Subcommand)]
#[serde(untagged)] // NB: `untagged` here is important for config merging
pub enum PrAction {
    /// Enable auto-merge on a PR.
    ///
    /// Accepts either a PR number or a revision; with a revision, the PR is looked up by the rev's
    /// local bookmark. Fails if the repo does not allow auto-merge.
    #[command(visible_alias = "am")]
    AutoMerge(AutoMergeArgsInput),

    /// Open your preferred editor to create a PR from a revision.
    ///
    /// Opens your editor to a markdown file where you can write the PR description,
    /// and set PR metadata like title, labels, auto-merge, etc. via the markdown frontmatter.
    /// This supports stacked PRs; by default the base branch is set to the closest ancestor bookmark
    /// if one exists, otherwise `trunk()`.
    #[command(visible_alias = "c")]
    Create(CreateArgsInput),

    /// Edit an existing PR's title, body, base, labels, reviewers, draft state,
    /// and auto-merge settings via the markdown frontmatter editor flow.
    ///
    /// Resolves the PR from a revision (via its local bookmark) or a PR number,
    /// fetches its current state, and opens your editor. By default, the editor
    /// includes a read-only preview of the PR diff. Applies only metadata you
    /// change: labels you didn't touch keep whatever others (CI bots, etc.) set.
    #[command(visible_alias = "e")]
    Edit(EditArgsInput),

    /// Fetch a pull request into a local bookmark.
    ///
    /// This command accepts either a revision ID or a PR number. If given a revision ID, the
    /// PR number will be looked up via the API. Requires a colocated git repository; the special
    /// `refs/pull/123/head` ref is fetched via `git` because `jj` cannot yet fetch arbitrary refs.
    ///
    /// See: <https://github.com/jj-vcs/jj/issues/4388>
    #[command(visible_alias = "f")]
    Fetch(FetchArgsInput),

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

    /// Re-run failed CI jobs on a PR, or on all local PRs with failed CI.
    ///
    /// Resolves the PR from a revision (via its local bookmark) or PR number,
    /// then re-runs failed workflow runs on the PR's head commit. By default
    /// the command fails if CI has not yet completed, because GitHub refuses
    /// to re-run a workflow run until it reaches the `completed` state.
    /// Pass `--all` to retry every local PR whose rolled-up CI status failed.
    ///
    /// With `--cancel`, in-progress runs are cancelled first; once they
    /// finalize, every workflow run is re-run (full pipeline restart).
    #[command(
        visible_alias = "rerun",
        group = clap::ArgGroup::new("retry_target").required(true).multiple(false)
    )]
    RetryFailed(RetryFailedArgsInput),

    /// Lookup the PR by the given number or revision ID and print its
    /// full URL. This is useful in pipes such as `jj-gh pr url <rev> | pbcopy`
    /// or `jj-gh pr url <rev> | wl-copy`.
    Url(PrUrlArgsInput),
}

/// Dispatch the `pr` subcommand to the matching handler.
///
/// # Errors
///
/// Propagates errors from config loading, lazy auth resolution, jj/GitHub API
/// calls, the editor round-trip, or any sub-handler (`create`, `fetch`,
/// `auto-merge`).
pub async fn dispatch(global: GlobalOptsInput, action: PrAction) -> Result<()> {
    let startup = Spinner::start("Resolving workspace");

    let config = config::resolve(&ConfigOverrides {
        global: &global,
        action: &action,
    })?;
    let globals = GlobalOpts::resolve(global, &config);
    let model = ModelImpl::new(&config, &globals).await?;
    startup.stop();
    match action {
        PrAction::AutoMerge(input) => {
            let args = AutoMergeArgs::resolve(input, &config, &globals);
            auto_merge::run(&model, &args).await?;
        }
        PrAction::Create(input) => {
            let args = CreateArgs::resolve(input, &config, &globals);
            create::run(&model, &args).await?;
        }
        PrAction::Edit(input) => {
            let args = EditArgs::resolve(input, &config, &globals);
            edit::run(&model, &args).await?;
        }
        PrAction::Fetch(input) => {
            let args = FetchArgs::resolve(input, &config, &globals);
            fetch::run(&model, &args).await?;
        }
        PrAction::Log(input) => {
            let args = PrLogArgs::resolve(input, &config, &globals);
            self::log::run(&model, &args).await?;
        }
        PrAction::Restack(input) => {
            let args = RestackArgs::resolve(input, &config, &globals);
            restack::run(&model, &args).await?;
        }
        PrAction::RetryFailed(input) => {
            let args = RetryFailedArgs::resolve(input, &config, &globals);
            retry_failed::run(&model, &args).await?;
        }
        PrAction::Url(input) => {
            let args = PrUrlArgs::resolve(input, &config, &globals);
            url::run(&model, &args).await?;
        }
    }
    Ok(())
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
    struct EditArgsParser {
        #[command(flatten)]
        args: EditArgsInput,
    }

    #[derive(clap::Parser, Debug)]
    #[command(no_binary_name = true)]
    struct PrLogArgsParser {
        #[command(flatten)]
        args: PrLogArgsInput,
    }

    #[derive(clap::Parser, Debug)]
    #[command(
        no_binary_name = true,
        group = clap::ArgGroup::new("retry_target").required(true).multiple(false)
    )]
    struct RetryFailedArgsParser {
        #[command(flatten)]
        args: RetryFailedArgsInput,
    }

    fn parse_create(argv: &[&str]) -> CreateArgsInput {
        CreateArgsParser::try_parse_from(argv.iter().copied())
            .expect("CreateArgsInput failed to parse")
            .args
    }

    fn parse_edit(argv: &[&str]) -> EditArgsInput {
        EditArgsParser::try_parse_from(argv.iter().copied())
            .expect("EditArgsInput failed to parse")
            .args
    }

    fn parse_pr_log(argv: &[&str]) -> PrLogArgsInput {
        PrLogArgsParser::try_parse_from(argv.iter().copied())
            .expect("PrLogArgsInput failed to parse")
            .args
    }

    #[test]
    fn action_serializes_as_flat_config_overlay() {
        let action = PrAction::Create(parse_create(&["@-", "--draft"]));
        let value = serde_json::to_value(action).unwrap();

        assert_eq!(value.get("draft"), Some(&serde_json::Value::Bool(true)));
        assert!(value.get("Create").is_none());
        assert!(value.get("create").is_none());
    }

    #[test]
    fn retry_failed_requires_pr_or_all() {
        RetryFailedArgsParser::try_parse_from::<[&str; 0], _>([])
            .expect_err("bare retry-failed should be rejected");
    }

    #[test]
    fn retry_failed_all_conflicts_with_pr() {
        RetryFailedArgsParser::try_parse_from(["--all", "42"])
            .expect_err("--all with a PR should be rejected");
    }

    #[test]
    fn retry_failed_all_allows_cancel() {
        let parsed = RetryFailedArgsParser::try_parse_from(["--all", "--cancel"])
            .expect("--all --cancel should parse")
            .args;
        assert!(parsed.all);
        assert!(parsed.cancel);
        assert!(parsed.number_or_rev.is_none());
    }

    fn merged_create(argv: &[&str], toml_config: &str) -> Config {
        let argv = parse_create(argv);
        let fig = config::defaults_figment()
            .merge(config::JjConfProvider::from_memory("test", toml_config))
            .merge(Serialized::defaults(&argv));
        config::extract(&fig).unwrap()
    }

    fn merged_edit(argv: &[&str], toml_config: &str) -> Config {
        let argv = parse_edit(argv);
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
    fn create_title_template_cli_overrides_config() {
        let c = merged_create(
            &["@-", "--title-template", "description.first_line()"],
            r#"
            [jj-gh]
            pr_create_title_template = "description"
            "#,
        );
        assert_eq!(c.pr_create_title_template, "description.first_line()");
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
    fn edit_bare_argv_lets_diff_config_win() {
        let c = merged_edit(
            &["42"],
            "\
            [jj-gh]\n\
            pr_edit_show_diffs = false\n\
            ",
        );
        assert!(!c.pr_edit_show_diffs);
    }

    #[test]
    fn edit_diff_flags_override_config() {
        let c = merged_edit(
            &["42", "--diffs"],
            "\
            [jj-gh]\n\
            pr_edit_show_diffs = false\n\
            ",
        );
        assert!(c.pr_edit_show_diffs);

        let c = merged_edit(
            &["42", "--no-diffs"],
            "\
            [jj-gh]\n\
            pr_edit_show_diffs = true\n\
            ",
        );
        assert!(!c.pr_edit_show_diffs);
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
