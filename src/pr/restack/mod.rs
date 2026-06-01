//! `jj-gh pr restack`: update PR base refs on GitHub to match the current
//! `jj` graph shape. Restack does not touch the jj graph itself — the user
//! shapes the graph first, then runs this command to push the new bases up.
//!
//! The command always launches an interactive TUI unless `--dry-run` /
//! `--json` is passed, or stdin/stdout is not a TTY (in which case it falls
//! back to a dry-run print).

mod interactive;

use crate::{
    config::Config,
    gh::{Gh, PrWithCiStatus, UpdatePr},
    git,
    jj::{Jj, PushedBookmark},
    ui::Spinner,
};
use anyhow::{Result, anyhow};
use futures::{StreamExt, stream::FuturesUnordered};
use serde::Serialize;
use std::collections::HashMap;
use std::io::IsTerminal;

#[derive(Debug, clap::Args, Serialize)]
pub struct RestackArgs {
    /// PR number or revision ID to position the cursor on when the TUI opens;
    /// if omitted the cursor starts on the first PR in the stack.
    #[arg(value_name = "PR_NUM|REV")]
    #[serde(skip)]
    pub number_or_rev: Option<String>,

    /// Print the proposed plan and exit without launching the TUI. No PR is
    /// updated. Auto-enabled when stdout is not a terminal.
    #[arg(long)]
    #[serde(skip)]
    pub dry_run: bool,

    /// Emit the proposed plan as JSON. Implies `--dry-run`.
    #[arg(long)]
    #[serde(skip)]
    pub json: bool,
}

/// One PR's proposed base-ref transition. Computed up-front so the TUI and
/// dry-run output share a single representation.
#[derive(Debug, Clone, Serialize)]
pub struct PrPlan {
    pub pr_number: u64,
    pub pr_node_id: String,
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

pub async fn run<J, G>(jj: &J, gh: &G, config: &Config, args: &RestackArgs) -> Result<()>
where
    J: Jj,
    G: Gh,
{
    let RestackArgs {
        number_or_rev,
        dry_run,
        json,
    } = args;

    let ctx = gather_context(jj, gh, config).await?;
    let force_dry = *dry_run
        || *json
        || !std::io::stdout().is_terminal()
        || !std::io::stderr().is_terminal();

    if ctx.plans.is_empty() {
        if *json {
            println!("[]");
        } else {
            println!("no PRs to restack");
        }
        return Ok(());
    }

    if force_dry {
        if *json {
            serde_json::to_writer_pretty(std::io::stdout().lock(), &ctx.plans)?;
            println!();
        } else {
            print_dry_run_text(&ctx.plans);
        }
        return Ok(());
    }

    let decisions = interactive::run(jj, &ctx, config, number_or_rev.as_deref()).await?;
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
    config: &Config,
) -> Result<RestackContext> {
    let origin_url = jj
        .remote_url(&config.default_remote)
        .await?
        .ok_or_else(|| anyhow!("`{}` remote is not configured", config.default_remote))?;
    let (owner, repo) = git::url::parse_owner_repo(&origin_url)?;

    let spinner = Spinner::start("Resolving local PRs");
    let bookmarks = jj.pushed_bookmarks(&config.default_remote).await?;
    let branch_to_local: HashMap<String, String> = bookmarks
        .iter()
        .map(|b| (b.name.clone(), b.local_commit_id.clone()))
        .collect();
    let names: Vec<String> = bookmarks.iter().map(|b| b.name.clone()).collect();
    let prs = gh.local_pulls(&owner, &repo, &names).await?;
    let trunk = jj.trunk_branch().await?;
    let plans = propose_plans(jj, &prs, &branch_to_local, trunk.as_deref()).await?;
    spinner.stop().await;

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
    let bookmark_w = plans
        .iter()
        .map(|p| p.bookmark.len())
        .max()
        .unwrap_or(0)
        .max(8);
    let base_w = plans
        .iter()
        .map(|p| p.current_base.len())
        .max()
        .unwrap_or(0)
        .max(4);
    let mut changes = 0usize;
    for p in plans {
        let PrPlan {
            pr_number,
            bookmark,
            current_base,
            proposed_base,
            ..
        } = p;
        if p.is_no_change() {
            println!(
                "#{pr_number:<5} {bookmark:<bookmark_w$}  {current_base:<base_w$}  (no change)"
            );
        } else {
            println!(
                "#{pr_number:<5} {bookmark:<bookmark_w$}  {current_base:<base_w$} -> {proposed_base}"
            );
            changes += 1;
        }
    }
    let count = plans.len();
    println!();
    println!("{count} PR(s), {changes} change(s) proposed");
}

async fn submit<G: Gh>(
    gh: &G,
    ctx: &RestackContext,
    decisions: &HashMap<u64, Decision>,
) -> Result<()> {
    let updates: Vec<(u64, UpdatePr)> = ctx
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
        .collect();

    if updates.is_empty() {
        println!("no PRs updated");
        return Ok(());
    }

    let total = updates.len();
    let spinner = Spinner::start(format!("Updating PRs (0/{total})"));
    let mut futs: FuturesUnordered<_> = updates
        .into_iter()
        .map(|(num, req)| async move {
            let result = gh.update_pr(req).await;
            (num, result)
        })
        .collect();

    let mut results: Vec<(u64, Result<()>)> = Vec::with_capacity(total);
    while let Some((num, res)) = futs.next().await {
        results.push((num, res));
        spinner.set_message(format!("Updating PRs ({}/{total})", results.len()));
    }
    spinner.stop().await;

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
