//! `jj-gh pr stack`: manually link PRs into a GitHub stack.
//!
//! Accepts multiple revisions or PR numbers and creates a stack on GitHub.
//! This is a manual escape hatch when auto-stacking doesn't work as expected.
//!
//! When run without arguments, automatically detects stacks from all local PRs
//! and shows a confirmation prompt before creating them.

use crate::{
    cli::GlobalOpts,
    gh::{Gh, PrDetails, remote},
    jj::Jj,
    model::Model,
    ui::Spinner,
};
use anyhow::{Result, anyhow, bail};
use jj_gh_config_derive::subcommand_args;
use std::collections::HashMap;
use std::io::{self, IsTerminal, Write};

subcommand_args! {
    pub struct StackArgs {
        /// Revisions or PR numbers to stack, in order from bottom to top.
        /// Each argument can be a revision ID (like `jj-gh pr create`) or a PR number.
        /// If omitted, automatically detects stacks from all local PRs.
        #[arg(value_name = "REV_OR_PR_NUM", num_args = 0..)]
        pub targets: Vec<String>,

        /// Force stack creation even if PRs are already in different stacks.
        /// This will unstack them first, then create the new stack.
        #[arg(long, default_value_if("confirm", "true", Some("true")))]
        pub force: bool,

        /// Apply changes without prompting.
        /// In interactive terminals, skips the confirmation prompt.
        /// In non-interactive environments, performs the operation instead of a dry run.
        /// Implies `--force`.
        #[arg(long)]
        pub confirm: bool,
    }
}

pub async fn run(model: &impl Model, args: &StackArgs) -> Result<()> {
    if args.targets.is_empty() {
        auto_detect_and_stack(model, args).await
    } else {
        explicit_stack(model, args).await
    }
}

/// Auto-detect mode: fetch all local PRs and detect stacks
async fn auto_detect_and_stack(model: &impl Model, args: &StackArgs) -> Result<()> {
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

    // Resolve target and remote name
    let (remote_name, target) = model.resolve_target(remote, Some(upstream_remote)).await?;

    // Fetch all local PRs
    let spinner = Spinner::start("Fetching local PRs");
    let bookmarks = jj.pushed_bookmarks(&remote_name).await?;
    let branch_names = bookmarks
        .iter()
        .map(|b| b.name.clone())
        .collect::<Vec<String>>();

    if branch_names.is_empty() {
        spinner.stop();
        println!("No local PRs found");
        return Ok(());
    }

    let all_prs = gh
        .local_pulls(
            &target.owner,
            &target.repo,
            target.origin_owner(),
            &branch_names,
        )
        .await?;

    // Build head_to_local mapping
    let head_to_local = bookmarks
        .iter()
        .map(|b| (b.name.clone(), b.local_commit_id.clone()))
        .collect::<HashMap<String, String>>();

    // Fetch full PR details
    spinner.set_message("Fetching PR details".into());
    let mut pr_details = Vec::with_capacity(all_prs.len());
    for pr in &all_prs {
        match gh.get_pr(&target.owner, &target.repo, pr.number).await {
            Ok(details) => pr_details.push(details),
            Err(e) => {
                log::warn!("Failed to fetch PR #{}: {e:#}", pr.number);
            }
        }
    }

    // Detect stack chains
    spinner.set_message("Detecting stack chains".into());
    let chains =
        crate::gh::stack_detect::detect_stack_chains(&pr_details, jj, &head_to_local).await?;
    spinner.stop();

    if chains.is_empty() {
        println!("No stacks detected");
        return Ok(());
    }

    // Check if TTY
    let is_tty = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();

    // --confirm implies --force
    let force = args.force || args.confirm;

    // Determine whether to proceed with creation
    let should_create = if args.confirm {
        // --confirm: proceed for real (both TTY and non-TTY)
        true
    } else if !is_tty {
        // Non-TTY without --confirm: dry run
        show_stacks(&chains, &pr_details);
        println!("\nDry run: Use --confirm to apply changes");
        return Ok(());
    } else {
        // TTY without --confirm: show confirmation prompt
        show_confirmation(&chains, &pr_details)?
    };

    if !should_create {
        println!("Aborted");
        return Ok(());
    }

    // Create stacks (confirmation implies force)
    create_stacks(gh, &target, &chains, &pr_details, force).await?;

    Ok(())
}

/// Explicit mode: user specified targets, no confirmation needed
async fn explicit_stack(model: &impl Model, args: &StackArgs) -> Result<()> {
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
    let gh = model.gh().await?;

    // Resolve target from the first argument to get owner/repo
    let first_target = &args.targets[0];
    let (_, target) = model
        .resolve_pr_with_target(remote, upstream_remote, first_target)
        .await?;

    // Resolve all targets to PRs
    let spinner = Spinner::start("Resolving PRs");
    let mut pr_details = Vec::with_capacity(args.targets.len());
    for target_str in &args.targets {
        let (pr, _) = model
            .resolve_pr_with_target(remote, upstream_remote, target_str)
            .await?;
        pr_details.push(pr);
    }

    // Check if PRs are already in stacks
    let stack_numbers = pr_details
        .iter()
        .map(|pr| pr.stack_number)
        .collect::<Vec<Option<u64>>>();

    let unique_stacks = stack_numbers
        .iter()
        .filter_map(|n| *n)
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect::<Vec<u64>>();

    if unique_stacks.len() > 1 && !args.force {
        return Err(anyhow!(
            "PRs are already in different stacks: {unique_stacks:?}. Use --force to unstack and recreate."
        ));
    }

    // If force is set and PRs are in stacks, unstack them first
    if args.force && !unique_stacks.is_empty() {
        spinner.set_message("Unstacking PRs".into());
        for stack_num in unique_stacks {
            let pr_numbers = pr_details
                .iter()
                .filter(|pr| pr.stack_number == Some(stack_num))
                .map(|pr| pr.number)
                .collect::<Vec<u64>>();
            if !pr_numbers.is_empty() {
                let result = gh
                    .unstack_prs(&target.owner, &target.repo, stack_num, &pr_numbers)
                    .await;
                if let Err(e) = &result {
                    log::warn!("Failed to unstack PRs from stack #{stack_num}: {e:#}");
                }
            }
        }
    }

    // Create the stack
    let pr_numbers = pr_details.iter().map(|pr| pr.number).collect::<Vec<u64>>();
    spinner.set_message("Creating stack".into());
    let stack = gh
        .create_stack(&target.owner, &target.repo, &pr_numbers)
        .await?;
    spinner.stop();

    let pr_list = pr_numbers
        .iter()
        .map(|n| format!("#{n}"))
        .collect::<Vec<_>>()
        .join(" → ");
    println!("Stack created: {pr_list} (Stack #{})", stack.number);

    Ok(())
}

/// Display detected stacks with formatting
fn show_stacks(chains: &[Vec<u64>], pr_details: &[PrDetails]) {
    const RESET: &str = "\x1b[0m";
    const CYAN: &str = "\x1b[36m";
    const MAGENTA: &str = "\x1b[35m";

    let tty = std::io::stdout().is_terminal();
    let on = |code: &'static str| -> &'static str { if tty { code } else { "" } };

    println!("\nDetected {} stack(s):\n", chains.len());

    let mut has_already_stacked = false;

    for (i, chain) in chains.iter().enumerate() {
        println!("Stack {}:", i + 1);
        let pr_list = chain
            .iter()
            .map(|&num| {
                let pr = pr_details.iter().find(|p| p.number == num).unwrap();
                let already_stacked = pr.stack_number.is_some();
                if already_stacked {
                    has_already_stacked = true;
                }
                let marker = if already_stacked { "*" } else { "" };
                format!(
                    "{}#{}{}{} {}{}{}",
                    on(CYAN),
                    num,
                    marker,
                    on(RESET),
                    on(MAGENTA),
                    pr.head_ref,
                    on(RESET)
                )
            })
            .collect::<Vec<_>>()
            .join(" → ");
        println!("  {pr_list}\n");
    }

    if has_already_stacked {
        println!("* PRs already in stacks will be unstacked and restacked\n");
    }

    let total_prs = chains.iter().map(Vec::len).sum::<usize>();
    println!("Total: {} PRs in {} stacks", total_prs, chains.len());
}

/// Show confirmation prompt and get user input
fn show_confirmation(chains: &[Vec<u64>], pr_details: &[PrDetails]) -> Result<bool> {
    show_stacks(chains, pr_details);

    print!("\nCreate these stacks? [y/N] ");
    io::stdout().flush()?;

    let mut input = String::new();
    io::stdin().read_line(&mut input)?;

    let response = input.trim().to_lowercase();
    Ok(response == "y" || response == "yes")
}

/// Create stacks from detected chains
async fn create_stacks(
    gh: &impl Gh,
    target: &remote::Target,
    chains: &[Vec<u64>],
    pr_details: &[PrDetails],
    force: bool,
) -> Result<()> {
    // Collect all PRs that are already in stacks
    let already_stacked = pr_details
        .iter()
        .filter(|pr| pr.stack_number.is_some())
        .map(|pr| pr.number)
        .collect::<Vec<u64>>();

    // If force is set and there are already-stacked PRs, unstack them first
    if force && !already_stacked.is_empty() {
        let spinner = Spinner::start("Unstacking existing PRs");
        let unique_stacks = pr_details
            .iter()
            .filter_map(|pr| pr.stack_number)
            .collect::<std::collections::HashSet<u64>>();

        for stack_num in unique_stacks {
            let pr_numbers = pr_details
                .iter()
                .filter(|pr| pr.stack_number == Some(stack_num))
                .map(|pr| pr.number)
                .collect::<Vec<u64>>();
            if !pr_numbers.is_empty() {
                let result = gh
                    .unstack_prs(&target.owner, &target.repo, stack_num, &pr_numbers)
                    .await;
                if let Err(e) = &result {
                    log::warn!("Failed to unstack PRs from stack #{stack_num}: {e:#}");
                }
            }
        }
        spinner.stop();
    } else if !already_stacked.is_empty() && !force {
        bail!("Some PRs are already in stacks. Use --force to unstack and restack them.");
    }

    // Fetch existing stacks to check for duplicates
    let spinner = Spinner::start("Checking existing stacks");
    let existing_stacks = gh.list_stacks(&target.owner, &target.repo).await?;

    // Create each stack
    let mut created_count = 0;
    let mut skipped_count = 0;
    for (i, chain) in chains.iter().enumerate() {
        // Check if this exact stack already exists
        let stack_exists = existing_stacks.iter().any(|stack| {
            let existing_prs = stack
                .pull_requests
                .iter()
                .map(|pr| pr.number)
                .collect::<Vec<u64>>();
            existing_prs == *chain
        });

        if stack_exists {
            skipped_count += 1;
            let pr_list = chain
                .iter()
                .map(|n| format!("#{n}"))
                .collect::<Vec<_>>()
                .join(" → ");
            println!("⊘ Stack already exists: {pr_list}");
            continue;
        }

        spinner.set_message(format!("Creating stack {}/{}", i + 1, chains.len()));
        let stack = gh.create_stack(&target.owner, &target.repo, chain).await?;

        let pr_list = chain
            .iter()
            .map(|n| format!("#{n}"))
            .collect::<Vec<_>>()
            .join(" → ");
        println!("✓ Stack #{} created: {}", stack.number, pr_list);
        created_count += 1;
    }
    spinner.stop();

    if created_count == 0 && skipped_count > 0 {
        println!("\nAll stacks already exist");
    }

    Ok(())
}
