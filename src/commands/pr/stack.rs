//! `jj-gh pr stack`: manually link PRs into a GitHub stack.
//!
//! Accepts multiple revisions or PR numbers and creates a stack on GitHub.
//! This is a manual escape hatch when auto-stacking doesn't work as expected.

use crate::{cli::GlobalOpts, gh::Gh, model::Model, ui::Spinner};
use anyhow::{Result, anyhow};
use jj_gh_config_derive::subcommand_args;

subcommand_args! {
    pub struct StackArgs {
        /// Revisions or PR numbers to stack, in order from bottom to top.
        /// Each argument can be a revision ID (like `jj-gh pr create`) or a PR number.
        #[arg(value_name = "REV_OR_PR_NUM", required = true, num_args = 2..)]
        pub targets: Vec<String>,

        /// Force stack creation even if PRs are already in different stacks.
        /// This will unstack them first, then create the new stack.
        #[arg(long)]
        pub force: bool,
    }
}

pub async fn run(model: &impl Model, args: &StackArgs) -> Result<()> {
    let GlobalOpts {
        remote,
        verbose: _,
        quiet: _,
        log_level: _,
        upstream_remote,
        gh_askpass: _,
        askpass_timeout_secs: _,
    } = &args.globals;

    let upstream_remote = crate::gh::remote::resolved_upstream_remote(upstream_remote);
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
    spinner.stop();

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
        let spinner = Spinner::start("Unstacking PRs");
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
    }

    // Create the stack
    let pr_numbers = pr_details.iter().map(|pr| pr.number).collect::<Vec<u64>>();
    let spinner = Spinner::start("Creating stack");
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
