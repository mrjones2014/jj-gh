//! `jj-gh pr log`: wraps `jj log` with PR metadata injected as template
//! aliases keyed by `commit_id`.
//!
//! We don't re-implement log rendering. We fetch open PRs, build a TOML config that
//! defines template aliases like `pr_number(commit_id)` as nested
//! `if(commit_id.short(40) == "<sha>", "<value>", ...)` chains, write it to a
//! temp file, then spawn `jj --config-file <file> log` with the user's
//! forwarded args. We also ship a default `pr_log` template that inlines a
//! hyperlinked PR number and CI-status icon, applied only when the user
//! didn't pass their own `-T` / `--template`.

use crate::{
    config::Config,
    gh::{CiStatus, Gh, PrWithCiStatus},
    git,
    jj::{
        Jj,
        inject::{TemplateAliases, escape_jj_string},
    },
};
use anyhow::{Context, Result, anyhow};
use serde::Serialize;
use std::collections::HashMap;
use tokio::process::Command;

/// jj `label(...)` keys for which we ship default colors in the injected
/// config. Templates reference them by name so users can override the colors
/// in their own jj config.
const COLOR_CI_SUCCESS: &str = "gh-ci-success";
const COLOR_CI_FAILED: &str = "gh-ci-failed";
const COLOR_CI_PENDING: &str = "gh-ci-pending";
const COLOR_PR_MERGE_STATUS: &str = "gh-pr-merge-status";

/// Default `pr_log` template applied when the user did not pass their own
/// `-T` / `--template`. References the `pr_meta` alias so spacing only appears
/// for commits that actually have a PR.
const PR_LOG_TEMPLATE: &str = r#"
if(root,
  format_root_commit(self),
  label(
    separate(" ",
      if(current_working_copy, "working_copy"),
      if(immutable, "immutable", "mutable"),
      if(conflict, "conflicted"),
    ),
    concat(
      format_short_commit_header(self)  ++ surround(" ", "", pr_meta) ++ "\n",
      separate(" ",
        if(empty, empty_commit_marker),
        if(description,
          description.first_line(),
          label(if(empty, "empty"), description_placeholder),
        ),
      ) ++ "\n",
    ),
  )
)
"#;

#[derive(Debug, clap::Args, Serialize)]
pub struct PrLogArgs {
    /// Arguments forwarded verbatim to the underlying `jj log` invocation.
    /// Pass after `--`, e.g. `jj-gh pr log -- -r 'mine()' -T builtin_log_compact`.
    /// If you pass `-T` / `--template`, the default PR-aware template is not
    /// applied; the following per-commit aliases are then available in your
    /// own template, each keyed on `commit_id`:
    ///
    /// - `pr_number`: PR number as a string, or empty for commits without a
    ///   PR.
    /// - `pr_url`: PR URL, or empty.
    /// - `pr_ci_status`: `SUCCESS`, `FAILED`, `PENDING`, or empty.
    /// - `pr_merge_status`: merged / in-merge-queue / auto-merge label, or
    ///   empty.
    /// - `pr_meta`: pre-formatted hyperlinked PR number plus colored CI icon
    ///   plus merge status (empty for commits without a PR).
    #[arg(last = true, allow_hyphen_values = true, value_name = "JJ_LOG_ARGS")]
    #[serde(skip)]
    pub jj_log_args: Vec<String>,

    /// Force enable the use of nerdfont icons in the default
    /// `pr log` template. Overrides config. Use `--no-nerdfonts` to disable.
    #[arg(
        long,
        num_args = 0,
        default_missing_value = "true",
        default_value_if("no_nerdfonts", "true", Some("false"))
    )]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nerdfonts: Option<bool>,

    /// Force the default `pr log` template not to use nerdfont icons.
    /// Overrides config.
    #[arg(long = "no-nerdfonts", conflicts_with = "nerdfonts")]
    #[serde(skip)]
    pub no_nerdfonts: bool,
}

pub async fn run(args: &PrLogArgs, config: &Config, gh: &impl Gh, jj: &impl Jj) -> Result<()> {
    let origin_url = jj
        .remote_url(&config.default_remote)
        .await?
        .ok_or_else(|| anyhow!("`{}` remote is not configured", config.default_remote))?;
    let (owner, repo) = git::url::parse_owner_repo(&origin_url)?;
    let bookmarks = jj.pushed_bookmarks(&config.default_remote).await?;
    let branch_to_local: HashMap<String, String> = bookmarks
        .iter()
        .map(|b| (b.name.clone(), b.local_commit_id.clone()))
        .collect();
    let names = bookmarks.into_iter().map(|b| b.name).collect::<Vec<_>>();
    let prs = gh.local_pulls(&owner, &repo, &names).await?;

    let aliases = build_aliases(&prs, &branch_to_local, config);
    let tmp = aliases.write_temp_config()?;

    let mut cmd = Command::new("jj");
    cmd.arg("--config-file").arg(tmp.path()).arg("log");
    if !user_set_template(&args.jj_log_args) {
        cmd.args(["-T", "pr_log"]);
    }
    cmd.args(&args.jj_log_args);

    let status = cmd.status().await.context("failed to spawn `jj log`")?;
    if !status.success() {
        return Err(anyhow!("`jj log` failed with {status}"));
    }
    Ok(())
}

/// Whether the user already passed `-T` / `--template` in the forwarded args.
fn user_set_template(args: &[String]) -> bool {
    args.iter().any(|a| {
        a == "-T"
            || a == "--template"
            || a.starts_with("-T") && a.len() > 2
            || a.starts_with("--template=")
    })
}

/// Build the [`TemplateAliases`] that define our `pr_*` aliases, the
/// `pr_log` default template, and the colors it labels.
///
/// jj template aliases lose static type info when called from another alias
/// (their return type becomes `Any`), which breaks `if(pr_x, ...)` and
/// `pr_x == ""` in nested aliases. To sidestep this we render the entire
/// inline PR fragment (hyperlinked number, colored CI icon, merge status) as
/// a single `pr_meta` alias whose body is a per-commit if-chain; the default
/// `pr_log` template then wraps it with `surround(" ", "", pr_meta)` so
/// spacing only appears for commits that actually have a PR. We still expose
/// `pr_number`, `pr_url`, and `pr_ci_status` as raw `String` aliases for
/// users who want to build custom templates; they work in direct contexts
/// even if they cannot be re-chained through `if()`.
fn build_aliases(
    prs: &[PrWithCiStatus],
    branch_to_local: &HashMap<String, String>,
    config: &Config,
) -> TemplateAliases {
    let number = if_chain_alias(prs, branch_to_local, |pr| format!(r#""{}""#, pr.number));
    let url = if_chain_alias(prs, branch_to_local, |pr| {
        format!(r#""{}""#, escape_jj_string(&pr.url))
    });
    let status = if_chain_alias(prs, branch_to_local, |pr| {
        format!(r#""{}""#, ci_status_str(pr.ci_status))
    });
    let merge_status = if_chain_alias(prs, branch_to_local, |pr| {
        format!(r#""{}""#, merge_status(pr, config).unwrap_or_default())
    });
    let meta = if_chain_alias(prs, branch_to_local, |pr| render_pr_meta_body(pr, config));

    TemplateAliases::builder()
        .alias("pr_number", number)
        .alias("pr_url", url)
        .alias("pr_ci_status", status)
        .alias("pr_meta", meta)
        .alias("pr_merge_status", merge_status)
        .alias("pr_log", PR_LOG_TEMPLATE)
        .color(COLOR_CI_SUCCESS, "green")
        .color(COLOR_CI_FAILED, "red")
        .color(COLOR_CI_PENDING, "yellow")
        .color(COLOR_PR_MERGE_STATUS, "bright black")
}

/// Render the body of a single `pr_meta` if-chain arm: the full template
/// fragment for one PR (hyperlinked number plus colored CI-status icon).
fn render_pr_meta_body(pr: &PrWithCiStatus, config: &Config) -> String {
    let github_icon = if config.nerdfonts { " " } else { "" };
    let url = escape_jj_string(&pr.url);
    let mut template = format!(
        r##""{github_icon}" ++ hyperlink("{url}", "#{n}")"##,
        n = pr.number
    );

    template = match ci_status_icon_label(pr) {
        Some(icon) => format!(r#"{template} ++ " " ++ {icon}"#),
        None => template,
    };

    template = match merge_status(pr, config) {
        Some(metadata) => {
            format!(
                r#"{template} ++ " " ++ label("{COLOR_PR_MERGE_STATUS}", "(") ++ {metadata} ++ label("{COLOR_PR_MERGE_STATUS}", ")")"#
            )
        }
        None => template,
    };

    template
}

fn merge_status(pr: &PrWithCiStatus, config: &Config) -> Option<String> {
    if pr.merged {
        let icon = if config.nerdfonts { " " } else { "" };
        Some(format!(
            r#"label("{COLOR_PR_MERGE_STATUS}", "{icon}merged")"#
        ))
    } else if pr.is_in_merge_queue {
        let icon = if config.nerdfonts { " " } else { "" };
        Some(format!(
            r#"label("{COLOR_PR_MERGE_STATUS}", "{icon}in merge queue")"#
        ))
    } else if pr.auto_merge_enabled {
        let icon = if config.nerdfonts { "󰾨 " } else { "" };
        Some(format!(
            r#"label("{COLOR_PR_MERGE_STATUS}", "{icon}auto-merge enabled")"#
        ))
    } else {
        None
    }
}

fn ci_status_icon_label(pr: &PrWithCiStatus) -> Option<String> {
    Some(match pr.ci_status {
        CiStatus::Success => format!(r#"label("{COLOR_CI_SUCCESS}", "✓")"#),
        CiStatus::Failed => format!(r#"label("{COLOR_CI_FAILED}", "✗")"#),
        CiStatus::Pending => format!(r#"label("{COLOR_CI_PENDING}", "●")"#),
        CiStatus::None => return None,
    })
}

/// Build a nested `if(commit_id.short(40) == "<sha>", <body>, ...)` chain that
/// terminates in the empty string. Generated PR SHAs are 40-char hex (SHA-1).
///
/// Each arm keys on the **local** bookmark target (from `branch_to_local`)
/// rather than `pr.head_sha`, so the badge renders on the commit the user
/// sees locally, even when they've rebased/squashed/etc. without pushing and the local
/// commit no longer matches the PR's remote head. Falls back to
/// `pr.head_sha` when no matching local bookmark is found (defensive: this
/// shouldn't happen since the PR was returned by a branch-name search
/// scoped to `branch_to_local`'s keys).
fn if_chain_alias<F>(
    prs: &[PrWithCiStatus],
    branch_to_local: &HashMap<String, String>,
    render: F,
) -> String
where
    F: Fn(&PrWithCiStatus) -> String,
{
    let mut expr = String::from(r#""""#);
    for pr in prs.iter().rev() {
        let sha = branch_to_local
            .get(&pr.head_ref_name)
            .map_or(pr.head_sha.as_str(), String::as_str);
        expr = format!(
            r#"if(commit_id.short(40) == "{sha}", {body}, {expr})"#,
            body = render(pr),
        );
    }
    expr
}

fn ci_status_str(status: CiStatus) -> &'static str {
    match status {
        CiStatus::Success => "SUCCESS",
        CiStatus::Failed => "FAILED",
        CiStatus::Pending => "PENDING",
        CiStatus::None => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pr(sha: &str, number: u64, status: CiStatus) -> PrWithCiStatus {
        PrWithCiStatus {
            id: format!("ID{number}"),
            number,
            url: format!("https://github.com/o/r/pull/{number}"),
            title: format!("PR {number}"),
            head_ref_name: format!("branch-{number}"),
            head_sha: sha.into(),
            is_draft: false,
            is_in_merge_queue: false,
            ci_status: status,
            merged: false,
            auto_merge_enabled: false,
        }
    }

    fn pr_merge_status(
        number: u64,
        merged: bool,
        is_in_merge_queue: bool,
        auto_merge_enabled: bool,
    ) -> PrWithCiStatus {
        PrWithCiStatus {
            id: format!("ID{number}"),
            number,
            url: format!("https://github.com/o/r/pull/{number}"),
            title: format!("PR {number}"),
            head_ref_name: format!("branch-{number}"),
            head_sha: number.to_string(),
            is_draft: false,
            ci_status: CiStatus::Success,
            auto_merge_enabled,
            is_in_merge_queue,
            merged,
        }
    }

    #[test]
    fn if_chain_empty_returns_empty_string_literal() {
        let map = HashMap::new();
        let expr = if_chain_alias::<fn(&PrWithCiStatus) -> String>(&[], &map, |_| String::new());
        assert_eq!(expr, r#""""#);
    }

    #[test]
    fn if_chain_nests_in_input_order() {
        let prs = vec![
            pr(
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                1,
                CiStatus::None,
            ),
            pr(
                "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                2,
                CiStatus::None,
            ),
        ];
        let map: HashMap<String, String> = prs
            .iter()
            .map(|p| (p.head_ref_name.clone(), p.head_sha.clone()))
            .collect();
        let expr = if_chain_alias(&prs, &map, |pr| format!(r#""{}""#, pr.number));
        assert_eq!(
            expr,
            r#"if(commit_id.short(40) == "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "1", if(commit_id.short(40) == "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", "2", ""))"#
        );
    }

    #[test]
    fn if_chain_uses_local_sha_when_bookmark_diverges() {
        // Regression for #74: PR's remote head_sha is stale (pre-rebase);
        // the local bookmark points at a different commit. The arm must key
        // on the local commit so the badge appears on the user's current
        // working state.
        let pr = pr(
            "remote_remote_remote_remote_remote_remote",
            42,
            CiStatus::Success,
        );
        let local_sha = "local0_local0_local0_local0_local0_local0";
        let mut map = HashMap::new();
        map.insert(pr.head_ref_name.clone(), local_sha.to_string());
        let expr = if_chain_alias(&[pr], &map, |pr| format!(r#""{}""#, pr.number));
        assert!(
            expr.contains(&format!(r#"commit_id.short(40) == "{local_sha}""#)),
            "if-chain should key on local sha, got: {expr}"
        );
        assert!(
            !expr.contains("remote_remote_remote_remote_remote_remote"),
            "if-chain should not reference stale remote sha, got: {expr}"
        );
    }

    #[test]
    fn if_chain_falls_back_to_remote_sha_when_no_local_mapping() {
        let pr = pr(
            "cccccccccccccccccccccccccccccccccccccccc",
            7,
            CiStatus::None,
        );
        let map = HashMap::new();
        let expr = if_chain_alias(&[pr], &map, |pr| format!(r#""{}""#, pr.number));
        assert!(
            expr.contains(r#"commit_id.short(40) == "cccccccccccccccccccccccccccccccccccccccc""#)
        );
    }

    #[test]
    fn user_set_template_detects_short_form() {
        assert!(user_set_template(&["-T".into(), "x".into()]));
    }

    #[test]
    fn user_set_template_detects_glued_short_form() {
        assert!(user_set_template(&["-Tx".into()]));
    }

    #[test]
    fn user_set_template_detects_long_form() {
        assert!(user_set_template(&["--template".into(), "x".into()]));
        assert!(user_set_template(&["--template=x".into()]));
    }

    #[test]
    fn user_set_template_ignores_other_args() {
        assert!(!user_set_template(&["-r".into(), "@-".into()]));
    }

    fn local_map_from(prs: &[PrWithCiStatus]) -> HashMap<String, String> {
        prs.iter()
            .map(|p| (p.head_ref_name.clone(), p.head_sha.clone()))
            .collect()
    }

    #[test]
    fn config_contains_alias_for_each_pr() {
        let prs = vec![pr("a".repeat(40).as_str(), 42, CiStatus::Success)];
        let map = local_map_from(&prs);
        let cfg = build_aliases(&prs, &map, &Config::default()).to_toml();
        assert!(
            cfg.contains(r#"commit_id.short(40) == "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa""#)
        );
        assert!(cfg.contains(r#""42""#));
        assert!(cfg.contains(r#""SUCCESS""#));
        assert!(cfg.contains(r#"label("gh-ci-success", "✓")"#));
        assert!(cfg.contains(r#"hyperlink(""#));
    }

    #[test]
    fn default_template_shows_merge_metadata() {
        let prs = vec![pr_merge_status(1, true, false, false)];
        let map = local_map_from(&prs);
        let cfg = build_aliases(&prs, &map, &Config::default()).to_toml();
        assert!(cfg.contains(" merged"));

        let prs = vec![pr_merge_status(2, false, true, false)];
        let map = local_map_from(&prs);
        let cfg = build_aliases(&prs, &map, &Config::default()).to_toml();
        assert!(cfg.contains(" in merge queue"), "{}", cfg);

        let prs = vec![pr_merge_status(3, false, false, true)];
        let map = local_map_from(&prs);
        let cfg = build_aliases(&prs, &map, &Config::default()).to_toml();
        assert!(cfg.contains("󰾨 auto-merge enabled"));
    }

    #[test]
    fn merge_status_labels_use_pr_merge_color_const() {
        let prs = vec![pr_merge_status(1, true, false, false)];
        let map = local_map_from(&prs);
        let cfg = build_aliases(&prs, &map, &Config::default()).to_toml();
        assert!(
            cfg.contains(r#"label("gh-pr-merge-status""#),
            "expected gh-pr-merge-status color label, got: {cfg}"
        );
    }
}
