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
    jj::Jj,
    pr::PrLogArgs,
};
use anyhow::{Context, Result, anyhow};
use std::io::Write;
use tempfile::NamedTempFile;
use tokio::process::Command;

pub async fn log(args: &PrLogArgs, config: &Config, gh: &impl Gh, jj: &impl Jj) -> Result<()> {
    let origin_url = jj
        .remote_url("origin")
        .await?
        .ok_or_else(|| anyhow!("origin remote is not configured"))?;
    let (owner, repo) = git::url::parse_owner_repo(&origin_url)?;
    let branches = jj.pushed_bookmarks().await?;
    let prs = gh.local_pulls(&owner, &repo, &branches).await?;

    let config_toml = render_config(&prs, config);
    let mut tmp = NamedTempFile::with_suffix(".toml").context("creating temp config file")?;
    tmp.write_all(config_toml.as_bytes())
        .context("writing template-alias config")?;
    let tmp = tmp.into_temp_path();

    let mut cmd = Command::new("jj");
    cmd.arg("--config-file").arg(&tmp).arg("log");
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

/// Build the TOML config that defines our `pr_*` template aliases, default
/// colors, and the `pr_log` template.
///
/// jj template aliases lose static type info when called from another alias
/// (their return type becomes `Any`), which breaks `if(pr_x, ...)` and
/// `pr_x == ""` in nested aliases. To sidestep this we render the *entire*
/// inline PR fragment (hyperlinked number + colored CI icon) as a single
/// `pr_meta` alias whose body is a per-commit if-chain; the default `pr_log`
/// template then wraps it with `surround(" ", "", pr_meta)` so spacing only
/// appears for commits that actually have a PR. We still expose `pr_number` /
/// `pr_url` / `pr_ci_status` as raw String aliases for users who want to
/// build custom templates — they work in direct contexts even if they can't
/// be re-chained through `if()`.
fn render_config(prs: &[PrWithCiStatus], config: &Config) -> String {
    let number = if_chain_alias(prs, |pr| format!(r#""{}""#, pr.number));
    let url = if_chain_alias(prs, |pr| format!(r#""{}""#, escape_toml_dq(&pr.url)));
    let status = if_chain_alias(prs, |pr| format!(r#""{}""#, ci_status_str(pr.ci_status)));
    let meta = if_chain_alias(prs, |pr| render_pr_meta_body(pr, config));

    format!(
        r#"[template-aliases]
pr_number = '''{number}'''
pr_url = '''{url}'''
pr_ci_status = '''{status}'''
pr_meta = '''{meta}'''
pr_log = '''
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
'''

[colors]
gh-ci-success = "green"
gh-ci-failed = "red"
gh-ci-pending = "yellow"
gh-pr-merge-status = "bright black"
"#
    )
}

/// Render the body of a single `pr_meta` if-chain arm: the full template
/// fragment for one PR (hyperlinked number plus colored CI-status icon).
fn render_pr_meta_body(pr: &PrWithCiStatus, config: &Config) -> String {
    let github_icon = if config.nerdfonts { " " } else { "" };
    let url = escape_toml_dq(&pr.url);
    let mut template = format!(
        r##""{github_icon}" ++ hyperlink("{url}", "#{n}")"##,
        n = pr.number
    );

    template = match ci_status_icon_label(pr) {
        Some(icon) => format!(r#"{template} ++ " " ++ {icon}"#),
        None => template,
    };

    template = match merge_metadata(pr) {
        Some(metadata) => format!(r#"{template} ++ " " ++ {metadata}"#),
        None => template,
    };

    template
}

fn merge_metadata(pr: &PrWithCiStatus) -> Option<&'static str> {
    if pr.merged {
        Some(r#"label("gh-pr-merge-status", "( merged)")"#)
    } else if pr.is_in_merge_queue {
        Some(r#"label("gh-pr-merge-status", "( in merge queue)")"#)
    } else {
        None
    }
}

fn ci_status_icon_label(pr: &PrWithCiStatus) -> Option<&'static str> {
    Some(match pr.ci_status {
        CiStatus::Success => r#"label("gh-ci-success", "✓")"#,
        CiStatus::Failed => r#"label("gh-ci-failed", "✗")"#,
        CiStatus::Pending => r#"label("gh-ci-pending", "●")"#,
        CiStatus::None => return None,
    })
}

/// Build a nested `if(commit_id.short(40) == "<sha>", <body>, ...)` chain that
/// terminates in the empty string. Generated PR SHAs are 40-char hex (SHA-1).
fn if_chain_alias<F>(prs: &[PrWithCiStatus], render: F) -> String
where
    F: Fn(&PrWithCiStatus) -> String,
{
    let mut expr = String::from(r#""""#);
    for pr in prs.iter().rev() {
        expr = format!(
            r#"if(commit_id.short(40) == "{sha}", {body}, {expr})"#,
            sha = pr.head_sha,
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

/// Escape a value for embedding in a TOML double-quoted string. We only ever
/// embed PR URLs and SHAs (no control chars), so handling `\` and `"` is
/// sufficient.
fn escape_toml_dq(s: &str) -> String {
    s.replace('\\', r"\\").replace('"', "\\\"")
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
            head_sha: sha.into(),
            is_draft: false,
            is_in_merge_queue: false,
            ci_status: status,
            merged: false,
        }
    }

    fn pr_merge_status(number: u64, merged: bool, is_in_merge_queue: bool) -> PrWithCiStatus {
        PrWithCiStatus {
            id: format!("ID{number}"),
            number,
            url: format!("https://github.com/o/r/pull/{number}"),
            title: format!("PR {number}"),
            head_sha: number.to_string(),
            is_draft: false,
            ci_status: CiStatus::Success,
            is_in_merge_queue,
            merged,
        }
    }

    #[test]
    fn if_chain_empty_returns_empty_string_literal() {
        let expr = if_chain_alias::<fn(&PrWithCiStatus) -> String>(&[], |_| String::new());
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
        let expr = if_chain_alias(&prs, |pr| format!(r#""{}""#, pr.number));
        assert_eq!(
            expr,
            r#"if(commit_id.short(40) == "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "1", if(commit_id.short(40) == "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", "2", ""))"#
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

    #[test]
    fn config_contains_alias_for_each_pr() {
        let prs = vec![pr("a".repeat(40).as_str(), 42, CiStatus::Success)];
        let cfg = render_config(&prs, &Config::default());
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
        let prs = vec![
            pr_merge_status(1, true, false),
            pr_merge_status(2, false, true),
        ];
        let cfg = render_config(&prs, &Config::default());
        assert!(cfg.contains("( in merge queue)"));
        assert!(cfg.contains("( merged)"));
    }

    #[test]
    fn escape_toml_dq_handles_backslash_and_quote() {
        assert_eq!(escape_toml_dq(r#"a"b\c"#), r#"a\"b\\c"#);
    }
}
