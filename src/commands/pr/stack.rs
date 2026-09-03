//! `jj-gh pr stack`: make GitHub match the shape of the local `jj` graph.
//!
//! The local graph is the source of truth; this command never rewrites it.
//! Reconciling means three writes, in order:
//!
//! 1. push bookmarks whose remote target has fallen behind the local one, so
//!    GitHub's head commits match;
//! 2. move each PR's base ref onto its closest stacked ancestor bookmark;
//! 3. create, reshape, or dissolve GitHub stacks so they match the local
//!    chains.
//!
//! Skipping (1) or (2) is what makes GitHub answer `422: Pull requests must
//! form a stack` — the stacks API validates a shape rather than imposing one.
//!
//! Run with no arguments it reconciles every local PR, showing the `jj log`
//! it is working from plus the proposed plan before touching anything. Run
//! with revisions or PR numbers it asserts exactly that stack instead, which
//! is the escape hatch for when detection gets it wrong.

use crate::{
    cli::GlobalOpts,
    gh::{
        Gh, PrDetails, PrWithCiStatus, PullRequestStack, UpdatePr, remote,
        stack_create::{
            ChainOutcome, ChainResult, create_stacks, stack_exists, stack_members, unstack,
        },
        stack_detect::{BasePlan, LocalShape, detect},
    },
    jj::{Jj, PushedBookmark},
    model::Model,
    ui::Spinner,
};
use anyhow::{Context, Result, anyhow, bail};
use jj_gh_config_derive::subcommand_args;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::io::{self, IsTerminal, Write};

const RESET: &str = "\x1b[0m";
const DIM: &str = "\x1b[2m";
const CYAN: &str = "\x1b[36m";
const GREEN: &str = "\x1b[32m";
const MAGENTA: &str = "\x1b[35m";

subcommand_args! {
    pub struct StackArgs {
        /// Revisions or PR numbers to stack, in order from bottom to top.
        /// Each argument can be a revision ID (like `jj-gh pr create`) or a PR number.
        /// If omitted, every local PR is reconciled against the local `jj` graph.
        #[arg(value_name = "REV_OR_PR_NUM", num_args = 0..)]
        pub targets: Vec<String>,

        /// Apply the plan without prompting. Required to apply anything when
        /// stdin or stdout is not a terminal, where the default is a dry run.
        /// Also unstacks PRs that are already in a different stack; answering
        /// the interactive prompt does the same.
        #[arg(long)]
        pub force: bool,

        /// Print the proposed plan and exit without touching GitHub.
        #[arg(long)]
        pub dry_run: bool,

        /// Emit the proposed plan as JSON. Implies `--dry-run`.
        #[arg(long)]
        pub json: bool,

        /// Push bookmarks whose remote target has fallen behind before
        /// stacking. Default: true. Use `--no-push` to disable.
        #[arg(
            long = "push",
            num_args = 0,
            default_missing_value = "true",
            default_value_if("no_push", "true", Some("false"))
        )]
        #[config]
        pub auto_push: bool,

        /// Do not push bookmarks, even if their remote target is stale.
        #[arg(long = "no-push", conflicts_with = "auto_push")]
        pub no_push: bool,

        /// Force enable nerdfont icons in the `jj log` view shown above the plan.
        /// Overrides config. Use `--no-nerdfonts` to disable.
        #[arg(
            long,
            num_args = 0,
            default_missing_value = "true",
            default_value_if("no_nerdfonts", "true", Some("false"))
        )]
        #[config]
        pub nerdfonts: bool,

        /// Force the `jj log` view not to use nerdfont icons. Overrides config.
        #[arg(long, conflicts_with = "nerdfonts")]
        pub no_nerdfonts: bool,

        /// Template for the `jj log` view shown above the plan. The same
        /// aliases as `pr log` are injected here. See `jj-gh pr log --help`.
        #[config]
        pub pr_log_template: Option<String>,
    }
}

/// A bookmark whose remote target has fallen behind its local one, so GitHub
/// is looking at commits the user has already replaced.
#[derive(Debug, Clone, Serialize)]
struct PushPlan {
    bookmark: String,
    local_commit_id: String,
    remote_commit_id: Option<String>,
}

/// Everything the command intends to do, computed before anything is written
/// so the preview, the `--json` dump, and the apply step cannot disagree.
#[derive(Debug, Default, Serialize)]
struct Plan {
    /// Bookmarks to push, ordered bottom to top of their chains.
    pushes: Vec<PushPlan>,
    /// PRs whose base ref moves. Only entries that actually change.
    bases: Vec<BasePlan>,
    /// Chains that GitHub does not already have, or whose stack has to be
    /// torn down and rebuilt.
    chains: Vec<Vec<u64>>,
    /// PRs that have to leave their stack first. See [`prs_to_unstack`].
    unstack: Vec<u64>,
}

impl Plan {
    fn is_empty(&self) -> bool {
        self.pushes.is_empty()
            && self.bases.is_empty()
            && self.chains.is_empty()
            && self.unstack.is_empty()
    }
}

pub async fn run(model: &impl Model, args: &StackArgs) -> Result<()> {
    if args.targets.is_empty() {
        reconcile(model, args).await
    } else {
        explicit(model, args).await
    }
}

/// Everything the reconcile needs after the initial fetch. Gathered in one
/// place so planning stays a pure function of it.
struct Gathered {
    target: remote::Target,
    remote_name: String,
    pr_details: Vec<PrDetails>,
    prs: Vec<PrWithCiStatus>,
    bookmarks: Vec<PushedBookmark>,
    branch_to_local: HashMap<String, String>,
    existing_stacks: Vec<PullRequestStack>,
    shape: LocalShape,
}

/// Reconcile every local PR against the local graph.
async fn reconcile(model: &impl Model, args: &StackArgs) -> Result<()> {
    let Some(ctx) = gather(model, args).await? else {
        return Ok(());
    };
    let plan = build_plan(model.jj(), args, &ctx).await?;

    if args.json {
        serde_json::to_writer_pretty(io::stdout().lock(), &plan)?;
        println!();
        return Ok(());
    }

    if plan.is_empty() {
        println!("Everything already matches the local graph");
        return Ok(());
    }

    show_graph(args, &ctx).await;
    print_plan(&plan, &ctx.pr_details);

    if args.dry_run {
        println!("\n{}Dry run: nothing was changed{}", on(DIM), on(RESET));
        return Ok(());
    }
    // Outside a terminal there is nobody to answer the prompt, so the safe
    // default is to show the plan and stop.
    let interactive = io::stdin().is_terminal() && io::stdout().is_terminal();
    if !args.force && !interactive {
        println!(
            "\n{}Not a terminal, so nothing was changed. Pass --force to apply.{}",
            on(DIM),
            on(RESET),
        );
        return Ok(());
    }
    // Reaching the apply step means either `--force` or an interactive `y`;
    // both mean "yes, including unstacking whatever is in the way".
    if !args.force && !confirm()? {
        println!("Aborted");
        return Ok(());
    }

    apply(model, ctx, plan).await
}

/// Fetch the PRs and the local graph. `Ok(None)` means there was nothing to
/// work with and the reason has already been printed.
async fn gather(model: &impl Model, args: &StackArgs) -> Result<Option<Gathered>> {
    let GlobalOpts {
        remote,
        verbose: _,
        quiet: _,
        log_level: _,
        upstream_remote,
        gh_askpass: _,
        askpass_timeout_secs: _,
    } = &args.globals;
    let upstream_remote = remote::resolved_upstream_remote(upstream_remote);
    let jj = model.jj();
    let gh = model.gh().await?;

    let spinner = Spinner::start("Resolving local PRs");
    let (remote_name, target) = model.resolve_target(remote, Some(upstream_remote)).await?;
    let bookmarks = jj.pushed_bookmarks(&remote_name).await?;
    let branch_names = bookmarks
        .iter()
        .map(|b| b.name.clone())
        .collect::<Vec<String>>();
    if branch_names.is_empty() {
        spinner.stop();
        println!("No bookmarks are tracked on `{remote_name}`");
        return Ok(None);
    }

    let prs = gh
        .local_pulls(
            &target.owner,
            &target.repo,
            target.origin_owner(),
            &branch_names,
        )
        .await?;
    if prs.is_empty() {
        spinner.stop();
        println!("No local PRs found");
        return Ok(None);
    }

    // Full details, one PR at a time: `local_pulls` comes from search and
    // carries no stack membership. A failure here is fatal rather than
    // skipped, because a missing PR reads as "not in a chain" and would have
    // us dissolve its stack.
    spinner.set_message("Fetching PR details".into());
    let mut pr_details = Vec::with_capacity(prs.len());
    for pr in &prs {
        pr_details.push(
            gh.get_pr(&target.owner, &target.repo, pr.number)
                .await
                .with_context(|| format!("fetching PR #{}", pr.number))?,
        );
    }

    spinner.set_message("Reading the local graph".into());
    let branch_to_local = bookmarks
        .iter()
        .map(|b| (b.name.clone(), b.local_commit_id.clone()))
        .collect::<HashMap<String, String>>();
    let trunk = jj.trunk_branch().await?;
    let shape = detect(&pr_details, jj, &branch_to_local, trunk.as_deref()).await?;
    let existing_stacks = gh.list_stacks(&target.owner, &target.repo).await?;
    spinner.stop();

    Ok(Some(Gathered {
        target,
        remote_name,
        pr_details,
        prs,
        bookmarks,
        branch_to_local,
        existing_stacks,
        shape,
    }))
}

async fn build_plan(jj: &impl Jj, args: &StackArgs, ctx: &Gathered) -> Result<Plan> {
    let bases = ctx
        .shape
        .bases
        .iter()
        .filter(|b| !b.is_no_change())
        .cloned()
        .collect::<Vec<BasePlan>>();

    let retargeting = bases.iter().map(|b| b.pr_number).collect::<HashSet<u64>>();
    let unstack = prs_to_unstack(
        &ctx.shape.chains,
        &ctx.pr_details,
        &ctx.existing_stacks,
        &retargeting,
    );

    // A chain needs (re)creating when GitHub does not have it verbatim, or
    // when we are about to tear its stack down so a base ref can move. Missing
    // the second case would dissolve a working stack and never rebuild it.
    let unstacked = unstack.iter().copied().collect::<HashSet<u64>>();
    let chains = ctx
        .shape
        .chains
        .iter()
        .filter(|chain| {
            !stack_exists(&ctx.existing_stacks, chain)
                || chain.iter().any(|number| unstacked.contains(number))
        })
        .cloned()
        .collect::<Vec<Vec<u64>>>();

    // Only chain members need pushing: a stale head is what makes GitHub
    // reject the stack. A lone PR's base can be retargeted without it.
    let pushes = if args.auto_push {
        stale_pushes(jj, ctx, &ctx.shape.chains).await?
    } else {
        Vec::new()
    };

    Ok(Plan {
        pushes,
        bases,
        chains,
        unstack,
    })
}

/// PRs that have to be removed from their GitHub stack before the rest of the
/// plan can be applied.
///
/// Dooming is per *stack*, not per PR: a stack cannot be edited in place, so
/// one bad member means the whole thing comes down and is rebuilt. A stack is
/// doomed when any of its local members:
///
/// - **a.** is no longer in a chain at all (the usual cause is a two-PR stack
///   whose bottom merged);
/// - **b.** needs its base ref moved, which GitHub refuses while it is stacked;
/// - **c.** sits in a stack that no longer has the shape of its chain.
///
/// Only PRs in `pr_details` are considered and only they are returned, so a
/// stack member with no local bookmark keeps its membership rather than being
/// silently unstacked.
fn prs_to_unstack(
    chains: &[Vec<u64>],
    pr_details: &[PrDetails],
    existing_stacks: &[PullRequestStack],
    retargeting: &HashSet<u64>,
) -> Vec<u64> {
    let members_of = existing_stacks
        .iter()
        .map(|stack| (stack.number, stack_members(stack)))
        .collect::<HashMap<u64, Vec<u64>>>();

    let doomed = pr_details
        .iter()
        .filter_map(|pr| {
            let stack_number = pr.stack_number?;
            let bad = match chains.iter().find(|chain| chain.contains(&pr.number)) {
                None => true,
                Some(_) if retargeting.contains(&pr.number) => true,
                Some(chain) => members_of.get(&stack_number).is_none_or(|m| m != chain),
            };
            bad.then_some(stack_number)
        })
        .collect::<HashSet<u64>>();

    pr_details
        .iter()
        .filter(|pr| {
            pr.stack_number
                .is_some_and(|number| doomed.contains(&number))
        })
        .map(|pr| pr.number)
        .collect()
}

/// Chain-member bookmarks whose remote target is not the local one, in
/// bottom-to-top order.
async fn stale_pushes(jj: &impl Jj, ctx: &Gathered, chains: &[Vec<u64>]) -> Result<Vec<PushPlan>> {
    let head_ref_of = ctx
        .pr_details
        .iter()
        .map(|pr| (pr.number, pr.head_ref.as_str()))
        .collect::<HashMap<u64, &str>>();
    let local_target = ctx
        .bookmarks
        .iter()
        .map(|b| (b.name.as_str(), b.local_commit_id.as_str()))
        .collect::<HashMap<&str, &str>>();

    let mut seen = HashSet::<&str>::new();
    let mut pushes = Vec::new();
    for bookmark in chains
        .iter()
        .flatten()
        .filter_map(|number| head_ref_of.get(number).copied())
    {
        let Some(local_commit_id) = local_target.get(bookmark).copied() else {
            continue;
        };
        if !seen.insert(bookmark) {
            continue;
        }
        let remote_commit_id = jj.remote_bookmark_sha(bookmark, &ctx.remote_name).await?;
        if remote_commit_id.as_deref() == Some(local_commit_id) {
            continue;
        }
        pushes.push(PushPlan {
            bookmark: bookmark.to_string(),
            local_commit_id: local_commit_id.to_string(),
            remote_commit_id,
        });
    }
    Ok(pushes)
}

/// Render the commits the plan is derived from, using `pr log`'s PR-annotated
/// template. Best-effort: this is context for the reader, so a jj failure
/// (an empty `trunk()`, say) just means no graph.
async fn show_graph(args: &StackArgs, ctx: &Gathered) {
    let ids = ctx
        .shape
        .bases
        .iter()
        .map(|b| b.local_commit_id.as_str())
        .collect::<Vec<&str>>();
    if ids.is_empty() {
        return;
    }

    let aliases = crate::commands::pr::log::build_aliases(
        &ctx.prs,
        &ctx.branch_to_local,
        args.nerdfonts,
        args.pr_log_template.as_deref(),
    );
    let Ok(tmp) = aliases
        .write_temp_config()
        .inspect_err(|e| log::debug!("could not write the jj log config: {e:#}"))
    else {
        return;
    };

    let cfg = tmp.path().to_string_lossy().into_owned();
    let revset = format!("trunk() | trunk()..({})", ids.join("|"));
    let cmd = [
        "jj",
        "--ignore-working-copy",
        "--config-file",
        &cfg,
        "log",
        "-r",
        &revset,
        "-T",
        "pr_log",
    ];
    if let Err(e) = crate::proc::stream(&cmd).await {
        log::debug!("could not render the jj log view: {e:#}");
    }
}

fn print_plan(plan: &Plan, pr_details: &[PrDetails]) {
    let title_of = pr_details
        .iter()
        .map(|pr| (pr.number, pr.title.as_str()))
        .collect::<HashMap<u64, &str>>();

    if !plan.pushes.is_empty() {
        println!("\n{}Push{} ({}):", on(DIM), on(RESET), plan.pushes.len());
        for push in &plan.pushes {
            let from = push
                .remote_commit_id
                .as_deref()
                .map_or("(not on remote)".to_string(), short_sha);
            println!(
                "  {}{}{}  {} {}->{} {}",
                on(MAGENTA),
                push.bookmark,
                on(RESET),
                from,
                on(DIM),
                on(RESET),
                short_sha(&push.local_commit_id),
            );
        }
    }

    if !plan.bases.is_empty() {
        println!(
            "\n{}Retarget base{} ({}):",
            on(DIM),
            on(RESET),
            plan.bases.len()
        );
        for base in &plan.bases {
            println!(
                "  {}#{}{}  {}  base {}{}{} {}<-{} {}{}{}",
                on(CYAN),
                base.pr_number,
                on(RESET),
                base.title,
                on(DIM),
                base.current_base,
                on(RESET),
                on(DIM),
                on(RESET),
                on(GREEN),
                base.proposed_base,
                on(RESET),
            );
        }
    }

    if !plan.chains.is_empty() {
        println!("\n{}Stack{} ({}):", on(DIM), on(RESET), plan.chains.len());
        for chain in &plan.chains {
            println!("  {}", format_chain(chain));
        }
    }

    if !plan.unstack.is_empty() {
        println!(
            "\n{}Unstack{} ({}):",
            on(DIM),
            on(RESET),
            plan.unstack.len()
        );
        for number in &plan.unstack {
            let title = title_of.get(number).copied().unwrap_or_default();
            println!("  {}#{number}{}  {title}", on(CYAN), on(RESET));
        }
        // Worth spelling out: a user watching a healthy stack get torn down
        // mid-run should know it is a precondition, not a mistake.
        println!(
            "  {}GitHub will not move a stacked PR's base ref, so these leave{}",
            on(DIM),
            on(RESET),
        );
        println!(
            "  {}their stack first and are stacked again afterwards.{}",
            on(DIM),
            on(RESET),
        );
    }
}

/// Apply the plan. Push first so GitHub sees the right commits, then move the
/// base refs, then reshape the stacks over them.
async fn apply(model: &impl Model, ctx: Gathered, plan: Plan) -> Result<()> {
    let jj = model.jj();
    let gh = model.gh().await?;
    let Gathered {
        target,
        remote_name,
        mut pr_details,
        mut existing_stacks,
        ..
    } = ctx;

    // No spinner around the pushes: `jj git push` streams its own progress to
    // the terminal, and a live spinner would fight it for the line.
    for push in &plan.pushes {
        jj.push(
            &push.local_commit_id,
            Some(&push.bookmark),
            remote_name.clone(),
        )
        .await
        .with_context(|| format!("pushing `{}` to `{remote_name}`", push.bookmark))?;
    }

    // Unstack before anything else touches a base ref: GitHub rejects a
    // base-ref change on a PR that is in a stack. The stacks are rebuilt below.
    if !plan.unstack.is_empty() {
        let spinner = Spinner::start("Unstacking PRs");
        let unstacked = unstack(gh, &target, &plan.unstack, &pr_details).await?;
        spinner.stop();

        // Mirror the unstack locally so the create step neither re-issues an
        // unstack for a PR already removed, nor skips a chain as
        // "already exists" when we just dissolved its stack.
        let done = unstacked.iter().copied().collect::<HashSet<u64>>();
        let dissolved = pr_details
            .iter()
            .filter(|pr| done.contains(&pr.number))
            .filter_map(|pr| pr.stack_number)
            .collect::<HashSet<u64>>();
        for pr in pr_details.iter_mut().filter(|pr| done.contains(&pr.number)) {
            pr.stack_number = None;
        }
        existing_stacks.retain(|stack| !dissolved.contains(&stack.number));

        for number in &unstacked {
            println!("OK  #{number} unstacked");
        }
    }

    if !plan.bases.is_empty() {
        let total = plan.bases.len();
        let spinner = Spinner::start(format!("Updating base refs (0/{total})"));
        for (index, base) in plan.bases.iter().enumerate() {
            gh.update_pr(UpdatePr {
                pr_node_id: base.pr_node_id.clone(),
                base_ref_name: Some(base.proposed_base.clone()),
                ..Default::default()
            })
            .await
            .with_context(|| {
                format!(
                    "retargeting PR #{} base ref to `{}`",
                    base.pr_number, base.proposed_base
                )
            })?;
            // Keep the in-memory view current so the stack step does not
            // re-issue the same update.
            if let Some(pr) = pr_details.iter_mut().find(|p| p.number == base.pr_number) {
                pr.base_ref.clone_from(&base.proposed_base);
            }
            spinner.set_message(format!("Updating base refs ({}/{total})", index + 1));
        }
        spinner.stop();
        for base in &plan.bases {
            println!("OK  #{} base -> {}", base.pr_number, base.proposed_base);
        }
    }

    if !plan.chains.is_empty() {
        let spinner = Spinner::start("Creating stacks");
        let results = create_stacks(gh, &target, &plan.chains, &pr_details, &existing_stacks).await;
        spinner.stop();
        print_stack_results(&results?);
    }

    Ok(())
}

/// Assert exactly the stack the user named, in the order they named it.
async fn explicit(model: &impl Model, args: &StackArgs) -> Result<()> {
    let GlobalOpts {
        remote,
        verbose: _,
        quiet: _,
        log_level: _,
        upstream_remote,
        gh_askpass: _,
        askpass_timeout_secs: _,
    } = &args.globals;
    let upstream_remote = remote::resolved_upstream_remote(upstream_remote);
    let jj = model.jj();
    let gh = model.gh().await?;

    if args.targets.len() < 2 {
        bail!("a stack needs at least two PRs; pass them bottom to top");
    }

    let spinner = Spinner::start("Resolving PRs");
    let (remote_name, target) = model.resolve_target(remote, Some(upstream_remote)).await?;
    let mut pr_details = Vec::with_capacity(args.targets.len());
    for target_str in &args.targets {
        let (pr, _) = model
            .resolve_pr_with_target(remote, upstream_remote, target_str)
            .await?;
        pr_details.push(pr);
    }

    let stacks = pr_details
        .iter()
        .filter_map(|pr| pr.stack_number)
        .collect::<HashSet<u64>>();
    if stacks.len() > 1 && !args.force {
        let mut numbers = stacks.into_iter().collect::<Vec<u64>>();
        numbers.sort_unstable();
        spinner.stop();
        return Err(anyhow!(
            "PRs are already in different stacks: {numbers:?}. Use --force to unstack and recreate."
        ));
    }

    let chain = pr_details.iter().map(|pr| pr.number).collect::<Vec<u64>>();
    spinner.stop();

    // Naming the PRs is the confirmation here, so there is no prompt. The
    // preview flags still stop before any write.
    if args.json {
        let plan = Plan {
            chains: vec![chain],
            ..Default::default()
        };
        serde_json::to_writer_pretty(io::stdout().lock(), &plan)?;
        println!();
        return Ok(());
    }
    if args.dry_run {
        println!("{}Stack{}: {}", on(DIM), on(RESET), format_chain(&chain));
        println!("\n{}Dry run: nothing was changed{}", on(DIM), on(RESET));
        return Ok(());
    }

    // Pushed outside any spinner: `jj git push` writes its own progress.
    if args.auto_push {
        push_stale(jj, &pr_details, &remote_name).await?;
    }

    let spinner = Spinner::start("Creating stack");
    let existing_stacks = gh.list_stacks(&target.owner, &target.repo).await?;
    let results = create_stacks(gh, &target, &[chain], &pr_details, &existing_stacks).await;
    spinner.stop();
    print_stack_results(&results?);

    Ok(())
}

/// Push any of `pr_details`' bookmarks whose remote target has fallen behind.
async fn push_stale(jj: &impl Jj, pr_details: &[PrDetails], remote_name: &str) -> Result<()> {
    let local_target = jj
        .pushed_bookmarks(remote_name)
        .await?
        .into_iter()
        .map(|b| (b.name, b.local_commit_id))
        .collect::<HashMap<String, String>>();

    for pr in pr_details {
        let Some(local_commit_id) = local_target.get(&pr.head_ref) else {
            continue;
        };
        let remote_commit_id = jj.remote_bookmark_sha(&pr.head_ref, remote_name).await?;
        if remote_commit_id.as_deref() == Some(local_commit_id.as_str()) {
            continue;
        }
        jj.push(local_commit_id, Some(&pr.head_ref), remote_name.to_string())
            .await
            .with_context(|| format!("pushing `{}` to `{remote_name}`", pr.head_ref))?;
    }
    Ok(())
}

fn confirm() -> Result<bool> {
    print!("\nApply this plan? [y/N] ");
    io::stdout().flush()?;
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    let response = input.trim().to_lowercase();
    Ok(response == "y" || response == "yes")
}

/// Emit the ANSI code only when stdout is a terminal.
fn on(code: &'static str) -> &'static str {
    if io::stdout().is_terminal() { code } else { "" }
}

fn short_sha(sha: &str) -> String {
    sha.chars().take(8).collect()
}

/// `#1 -> #2 -> #3` for a chain of PR numbers.
pub(crate) fn format_chain(chain: &[u64]) -> String {
    chain
        .iter()
        .map(|n| format!("#{n}"))
        .collect::<Vec<_>>()
        .join(" \u{2192} ")
}

/// Report what [`crate::gh::stack_create::create_stacks`] did.
pub(crate) fn print_stack_results(results: &[ChainResult]) {
    for result in results {
        let chain = format_chain(&result.chain);
        for number in &result.retargeted {
            log::debug!("retargeted #{number}'s base ref to make {chain} chain");
        }
        match result.outcome {
            ChainOutcome::Created(number) => println!("OK  stack #{number} created: {chain}"),
            ChainOutcome::AlreadyExists => println!("OK  stack already exists: {chain}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gh::StackPullRequest;

    fn stack(number: u64, prs: &[u64]) -> PullRequestStack {
        PullRequestStack {
            number,
            pull_requests: prs
                .iter()
                .map(|&number| StackPullRequest { number })
                .collect(),
        }
    }

    fn pr(number: u64, stack_number: Option<u64>) -> PrDetails {
        PrDetails {
            number,
            head_ref: format!("branch-{number}"),
            head_sha: format!("sha{number}"),
            base_ref: "main".into(),
            title: format!("PR {number}"),
            html_url: format!("https://github.com/o/r/pull/{number}"),
            is_draft: false,
            auto_merge: false,
            auto_merge_method: None,
            base_sha: "base".into(),
            head_user_login: None,
            head_repo_name: None,
            graphql_node_id: format!("node_{number}"),
            in_merge_queue: false,
            labels: vec![],
            reviewers: vec![],
            body: String::new(),
            stack_number,
        }
    }

    fn retargeting(numbers: &[u64]) -> HashSet<u64> {
        numbers.iter().copied().collect()
    }

    #[test]
    fn pr_that_lost_its_chain_is_unstacked() {
        // #1 merged out of stack #11, leaving #2 alone in it.
        let prs = [pr(2, Some(11)), pr(3, None)];
        assert_eq!(
            prs_to_unstack(&[], &prs, &[stack(11, &[2])], &retargeting(&[])),
            vec![2]
        );
    }

    #[test]
    fn retargeted_pr_is_unstacked_even_when_its_stack_is_intact() {
        // GitHub refuses to move a stacked PR's base ref, so a stack that
        // matches its chain verbatim still has to come down for the retarget.
        let prs = [pr(1, Some(7)), pr(2, Some(7))];
        let existing = [stack(7, &[1, 2])];
        assert_eq!(
            prs_to_unstack(&[vec![1, 2]], &prs, &existing, &retargeting(&[2])),
            vec![1, 2]
        );
    }

    #[test]
    fn intact_stack_with_no_retargets_is_left_alone() {
        let prs = [pr(1, Some(7)), pr(2, Some(7))];
        let existing = [stack(7, &[1, 2])];
        assert!(prs_to_unstack(&[vec![1, 2]], &prs, &existing, &retargeting(&[])).is_empty());
    }

    #[test]
    fn stack_whose_shape_drifted_is_unstacked() {
        // The chain grew a third PR, so stack #7 no longer describes it.
        let prs = [pr(1, Some(7)), pr(2, Some(7)), pr(3, None)];
        let existing = [stack(7, &[1, 2])];
        assert_eq!(
            prs_to_unstack(&[vec![1, 2, 3]], &prs, &existing, &retargeting(&[])),
            vec![1, 2]
        );
    }

    #[test]
    fn stack_shared_with_a_non_local_pr_only_gives_up_its_local_members() {
        // #99 has no local bookmark, so it is absent from pr_details and keeps
        // its membership; deleting local state should not mutate GitHub.
        let prs = [pr(1, Some(7))];
        let existing = [stack(7, &[1, 99])];
        assert_eq!(
            prs_to_unstack(&[], &prs, &existing, &retargeting(&[])),
            vec![1]
        );
    }

    #[test]
    fn chain_formats_bottom_to_top() {
        assert_eq!(format_chain(&[1, 2, 3]), "#1 → #2 → #3");
    }

    #[test]
    fn existing_stack_matches_only_the_same_order() {
        let existing = [stack(7, &[1, 2, 3])];
        assert!(stack_exists(&existing, &[1, 2, 3]));
        assert!(!stack_exists(&existing, &[1, 3, 2]));
        assert!(!stack_exists(&existing, &[1, 2]));
        assert!(!stack_exists(&existing, &[1, 2, 3, 4]));
    }

    #[test]
    fn empty_plan_is_recognized() {
        assert!(Plan::default().is_empty());
        assert!(
            !Plan {
                unstack: vec![9],
                ..Default::default()
            }
            .is_empty()
        );
    }

    #[test]
    fn short_sha_truncates_without_panicking_on_short_input() {
        assert_eq!(short_sha("0123456789abcdef"), "01234567");
        assert_eq!(short_sha("abc"), "abc");
    }
}
