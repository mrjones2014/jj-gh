//! `jj-gh pr restack`: update PR base refs on GitHub to match the current
//! `jj` graph shape. Restack does not touch the jj graph itself; the user
//! shapes the graph first, then runs this command to push the new bases up.
//!
//! The command always launches an interactive TUI unless `--dry-run` /
//! `--json` is passed, or stdin/stdout is not a TTY (in which case it falls
//! back to a dry-run print).

mod interactive;

use crate::{
    cli::GlobalOpts,
    gh::{Gh, PrWithCiStatus, UpdatePr},
    git,
    jj::{Jj, JjExt, PushedBookmark},
    ui::Spinner,
};
use anyhow::{Result, anyhow};
use futures::{StreamExt, stream::FuturesUnordered};
use jj_gh_config_derive::subcommand_args;
use serde::Serialize;
use std::collections::HashMap;
use std::io::IsTerminal;

subcommand_args! {
    pub struct RestackArgs {
        /// PR number or revision ID to position the cursor on when the TUI opens;
        /// if omitted the cursor starts on the first PR in the stack.
        #[arg(value_name = "PR_NUM|REV")]
        pub number_or_rev: Option<String>,

        /// Print the proposed plan and exit without launching the TUI. No PR is
        /// updated. Auto-enabled when stdout is not a terminal.
        #[arg(long)]
        pub dry_run: bool,

        /// Emit the proposed plan as JSON. Implies `--dry-run`.
        #[arg(long)]
        pub json: bool,

        /// Template to use in interactive mode; conflicts with `--json` and `--dry-run`.
        /// The same template aliases as `pr log` are injected here. See `jj-gh pr log --help`.
        #[arg(long, short = 'T', conflicts_with_all = ["json", "dry_run"])]
        #[config(maps_to = "pr_restack_template")]
        pub template: Option<String>,

        #[config]
        pub pr_log_template: Option<String>,

        /// Force enable nerdfont icons in the default restack/log template.
        /// Overrides config. Use `--no-nerdfonts` to disable.
        #[arg(
            long,
            num_args = 0,
            default_missing_value = "true",
            default_value_if("no_nerdfonts", "true", Some("false"))
        )]
        #[config]
        pub nerdfonts: bool,

        /// Force the default restack/log template not to use nerdfont icons.
        /// Overrides config.
        #[arg(long, conflicts_with = "nerdfonts")]
        pub no_nerdfonts: bool,
    }
}

/// One PR's proposed base-ref transition. Computed up-front so the TUI and
/// dry-run output share a single representation.
#[derive(Debug, Clone, Serialize)]
pub struct PrPlan {
    pub pr_number: u64,
    pub pr_node_id: String,
    pub title: String,
    pub bookmark: String,
    pub local_commit_id: String,
    pub current_base: String,
    pub proposed_base: String,
}

impl PrPlan {
    #[must_use]
    pub fn is_no_change(&self) -> bool {
        self.current_base == self.proposed_base
    }
}

/// User's decision for a single PR in the interactive loop.
#[derive(Debug, Clone)]
pub enum Decision {
    /// User hasn't acted on this PR yet.
    Unset,
    /// Apply the proposed base.
    Confirm,
    /// Apply this bookmark name instead of the proposed base.
    EditedTo(String),
    /// Leave this PR alone.
    Skip,
}

impl Decision {
    /// Resolve the final base ref this decision should submit, if any.
    /// `Unset` and `Skip` produce `None`; `Confirm` returns the plan's
    /// proposed base; `EditedTo` returns the override.
    #[must_use]
    pub fn final_base<'a>(&'a self, plan: &'a PrPlan) -> Option<&'a str> {
        match self {
            Self::Unset | Self::Skip => None,
            Self::Confirm => Some(&plan.proposed_base),
            Self::EditedTo(b) => Some(b.as_str()),
        }
    }
}

pub async fn run<J, G>(jj: &J, gh: &G, args: &RestackArgs) -> Result<()>
where
    J: Jj,
    G: Gh,
{
    let GlobalOpts {
        remote,
        verbose: _,
        quiet: _,
        log_level: _,
        upstream_remote: _,
        gh_askpass: _,
        askpass_timeout_secs: _,
    } = &args.globals;

    let remote = jj.resolve_default_remote(remote.as_ref()).await?;
    let ctx = gather_context(jj, gh, &remote).await?;

    let force_dry = args.dry_run
        || args.json
        || !std::io::stdout().is_terminal()
        || !std::io::stderr().is_terminal();

    if ctx.plans.is_empty() {
        if args.json {
            println!("[]");
        } else {
            println!("no PRs to restack");
        }
        return Ok(());
    }

    if force_dry {
        if args.json {
            serde_json::to_writer_pretty(std::io::stdout().lock(), &ctx.plans)?;
            println!();
        } else {
            print_dry_run_text(&ctx.plans);
        }
        return Ok(());
    }

    let decisions = interactive::run(jj, &ctx, args).await?;
    submit(gh, &ctx, &decisions).await
}

/// Bundle of everything restack needs after the initial fetch: PR metadata,
/// bookmark map, and the per-PR plan.
pub(crate) struct RestackContext {
    pub plans: Vec<PrPlan>,
    pub prs: Vec<PrWithCiStatus>,
    pub bookmarks: Vec<PushedBookmark>,
}

async fn gather_context<J: Jj, G: Gh>(
    jj: &J,
    gh: &G,
    default_remote: &str,
) -> Result<RestackContext> {
    let spinner = Spinner::start("Resolving local PRs");
    let origin_url = jj
        .remote_url(default_remote)
        .await?
        .ok_or_else(|| anyhow!("`{default_remote}` remote is not configured"))?;
    let (owner, repo) = git::url::parse_owner_repo(&origin_url)?;

    let bookmarks = jj.pushed_bookmarks(default_remote).await?;
    let branch_to_local = bookmarks
        .iter()
        .map(|b| (b.name.clone(), b.local_commit_id.clone()))
        .collect::<HashMap<String, String>>();
    let names = bookmarks
        .iter()
        .map(|b| b.name.clone())
        .collect::<Vec<String>>();
    let prs = gh.local_pulls(&owner, &repo, &names).await?;
    let trunk = jj.trunk_branch().await?;
    let plans = propose_plans(jj, &prs, &branch_to_local, trunk.as_deref()).await?;
    spinner.stop();

    Ok(RestackContext {
        plans,
        prs,
        bookmarks,
    })
}

/// Compute the proposed base for every PR. Pure aside from the per-PR
/// `stacked_ancestor_bookmark` jj call; that's the only piece that needs the
/// real graph.
async fn propose_plans<J: Jj>(
    jj: &J,
    prs: &[PrWithCiStatus],
    branch_to_local: &HashMap<String, String>,
    trunk: Option<&str>,
) -> Result<Vec<PrPlan>> {
    let mut plans = Vec::with_capacity(prs.len());
    for pr in prs {
        let Some(local_commit) = branch_to_local.get(&pr.head_ref_name) else {
            continue;
        };
        let ancestor = jj.stacked_ancestor_bookmark(local_commit).await?;
        let proposed = ancestor
            .or_else(|| trunk.map(str::to_string))
            .unwrap_or_else(|| pr.base_ref_name.clone());
        plans.push(PrPlan {
            pr_number: pr.number,
            pr_node_id: pr.id.clone(),
            title: pr.title.clone(),
            bookmark: pr.head_ref_name.clone(),
            local_commit_id: local_commit.clone(),
            current_base: pr.base_ref_name.clone(),
            proposed_base: proposed,
        });
    }
    plans.sort_by_key(|p| p.pr_number);
    Ok(plans)
}

fn print_dry_run_text(plans: &[PrPlan]) {
    const RESET: &str = "\x1b[0m";
    const CYAN: &str = "\x1b[36m";
    const MAGENTA: &str = "\x1b[35m";
    const GREEN: &str = "\x1b[32m";
    const BLUE: &str = "\x1b[34m";
    const DIM: &str = "\x1b[2m";

    let tty = std::io::stdout().is_terminal();
    let on = |code: &'static str| -> &'static str { if tty { code } else { "" } };

    let title_w = plans
        .iter()
        .map(|p| p.title.len())
        .max()
        .unwrap_or(0)
        .max(8);
    let base_w = plans
        .iter()
        .map(|p| p.proposed_base.len().max(p.current_base.len()))
        .max()
        .unwrap_or(0)
        .max(4);
    let mut changes = 0usize;
    for p in plans {
        let PrPlan {
            pr_number,
            title,
            bookmark,
            current_base,
            proposed_base,
            ..
        } = p;
        let (base, base_color) = if p.is_no_change() {
            (current_base, on(BLUE))
        } else {
            (proposed_base, on(GREEN))
        };
        let num = format!("{}#{pr_number:<5}{}", on(CYAN), on(RESET));
        let title_field = format!("{title:<title_w$}");
        let base_field = format!("{}{base:<base_w$}{}", base_color, on(RESET));
        let arrow = format!("{}\u{2190}{}", on(DIM), on(RESET));
        let bookmark_field = format!("{}{bookmark}{}", on(MAGENTA), on(RESET));
        let suffix = if p.is_no_change() {
            format!("{}(no change){}", on(DIM), on(RESET))
        } else {
            changes += 1;
            format!("{}(was: {current_base}){}", on(DIM), on(RESET))
        };
        println!("{num} {title_field}  {base_field} {arrow} {bookmark_field}  {suffix}");
    }
    let count = plans.len();
    println!();
    println!(
        "{dim}{count} PR(s), {changes} change(s) proposed{reset}",
        dim = on(DIM),
        reset = on(RESET),
    );
}

async fn submit<G: Gh>(
    gh: &G,
    ctx: &RestackContext,
    decisions: &HashMap<u64, Decision>,
) -> Result<()> {
    let updates = ctx
        .plans
        .iter()
        .filter_map(|p| {
            let decision = decisions.get(&p.pr_number).unwrap_or(&Decision::Unset);
            let base = decision.final_base(p)?;
            if base == p.current_base {
                return None;
            }
            Some((
                p.pr_number,
                UpdatePr {
                    pr_node_id: p.pr_node_id.clone(),
                    base_ref_name: Some(base.to_string()),
                    ..Default::default()
                },
            ))
        })
        .collect::<Vec<(u64, UpdatePr)>>();

    if updates.is_empty() {
        println!("no PRs updated");
        return Ok(());
    }

    let total = updates.len();
    let spinner = Spinner::start(format!("Updating PRs (0/{total})"));
    let mut futs = updates
        .into_iter()
        .map(|(num, req)| async move {
            let result = gh.update_pr(req).await;
            (num, result)
        })
        .collect::<FuturesUnordered<_>>();

    let mut results = Vec::<(u64, Result<()>)>::with_capacity(total);
    while let Some((num, res)) = futs.next().await {
        results.push((num, res));
        spinner.set_message(format!("Updating PRs ({}/{total})", results.len()));
    }
    spinner.stop();

    results.sort_by_key(|(n, _)| *n);
    let mut had_failure = false;
    for (num, res) in &results {
        match res {
            Ok(()) => println!("OK  #{num} base updated"),
            Err(e) => {
                println!("ERR #{num}: {e:#}");
                had_failure = true;
            }
        }
    }

    if had_failure {
        Err(anyhow!("one or more PR updates failed"))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(number: u64, bookmark: &str, current: &str, proposed: &str) -> PrPlan {
        PrPlan {
            pr_number: number,
            pr_node_id: format!("ID{number}"),
            title: format!("title {number}"),
            bookmark: bookmark.into(),
            local_commit_id: format!("{number:040}"),
            current_base: current.into(),
            proposed_base: proposed.into(),
        }
    }

    #[test]
    fn plan_no_change_when_current_eq_proposed() {
        assert!(plan(1, "b", "master", "master").is_no_change());
        assert!(!plan(1, "b", "master", "feature").is_no_change());
    }

    #[test]
    fn decision_final_base_resolves_each_variant() {
        let p = plan(1, "b", "master", "feature");
        assert_eq!(Decision::Unset.final_base(&p), None);
        assert_eq!(Decision::Skip.final_base(&p), None);
        assert_eq!(Decision::Confirm.final_base(&p), Some("feature"));
        assert_eq!(
            Decision::EditedTo("custom".into()).final_base(&p),
            Some("custom")
        );
    }
}
