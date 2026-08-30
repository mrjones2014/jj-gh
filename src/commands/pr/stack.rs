//! `jj-gh pr stack`: manually link PRs into a GitHub stack.
//!
//! Accepts multiple revisions or PR numbers and creates a stack on GitHub.
//! This is a manual escape hatch when auto-stacking doesn't work as expected.
//!
//! When run without arguments, automatically detects stacks from all local PRs
//! and shows a confirmation prompt before creating them.

use crate::{
    cli::GlobalOpts,
    gh::{
        Gh, PrDetails, remote,
        stack_create::{AlreadyStacked, ChainOutcome, ChainResult, create_stacks},
    },
    jj::{
        Jj,
        inject::{TemplateAliases, escape_jj_string, quote_jj},
    },
    model::Model,
    ui::Spinner,
};
use anyhow::{Result, anyhow};
use jj_gh_config_derive::subcommand_args;
use std::collections::HashMap;
use std::io::{self, IsTerminal, Write};

/// Default template for rendering each PR line in stack output.
/// Uses jj template syntax with aliases: `pr_number`, `pr_branch`, `pr_title`, `pr_sha`.
const DEFAULT_STACK_TEMPLATE: &str = r#"pr_number ++ " " ++ pr_branch ++ "  " ++ pr_title"#;

/// [`DEFAULT_STACK_TEMPLATE`] with the nerdfont glyph replaced by plain
/// spacing, used when nerdfont rendering is disabled.
const DEFAULT_STACK_TEMPLATE_PLAIN: &str = r#"pr_number ++ " " ++ pr_branch ++ "  " ++ pr_title"#;

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

        /// jj template used to render each PR line.
        /// Available aliases: `pr_number`, `pr_branch`, `pr_title`, `pr_sha`.
        /// Example: `"#" ++ pr_number ++ " " ++ pr_branch ++ "  " ++ pr_title`
        #[arg(long, short = 'T', value_name = "TEMPLATE")]
        #[config(maps_to = "pr_stack_template")]
        pub template: Option<String>,

        /// Force enable nerdfont icons in the default `pr stack` template.
        /// Overrides config. Use `--no-nerdfonts` to disable.
        #[arg(
            long,
            num_args = 0,
            default_missing_value = "true",
            default_value_if("no_nerdfonts", "true", Some("false"))
        )]
        #[config]
        pub nerdfonts: bool,

        /// Force the default `pr stack` template not to use nerdfont icons.
        /// Overrides config.
        #[arg(long, conflicts_with = "nerdfonts")]
        pub no_nerdfonts: bool,
    }
}

/// The default per-PR line template for the current nerdfont setting.
fn default_stack_template(nerdfonts: bool) -> &'static str {
    if nerdfonts {
        DEFAULT_STACK_TEMPLATE
    } else {
        DEFAULT_STACK_TEMPLATE_PLAIN
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
    let template = args
        .template
        .as_deref()
        .unwrap_or_else(|| default_stack_template(args.nerdfonts));

    // Determine whether to proceed with creation
    let should_create = if args.confirm {
        // --confirm: proceed for real (both TTY and non-TTY)
        true
    } else if !is_tty {
        // Non-TTY without --confirm: dry run
        show_stacks(jj, &chains, &pr_details, template).await?;
        println!("\nDry run: Use --confirm to apply changes");
        return Ok(());
    } else {
        // TTY without --confirm: show confirmation prompt
        show_confirmation(jj, &chains, &pr_details, template).await?
    };

    if !should_create {
        println!("Aborted");
        return Ok(());
    }

    // Create stacks (confirmation implies force)
    let mode = if force {
        AlreadyStacked::Unstack
    } else {
        AlreadyStacked::Bail
    };
    let spinner = Spinner::start("Creating stacks");
    let results = create_stacks(gh, &target, &chains, &pr_details, mode).await?;
    spinner.stop();
    print_stack_results(&results);

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
async fn show_stacks(
    jj: &impl Jj,
    chains: &[Vec<u64>],
    pr_details: &[PrDetails],
    template: &str,
) -> Result<()> {
    const RESET: &str = "\x1b[0m";
    const DIM: &str = "\x1b[2m";

    let tty = std::io::stdout().is_terminal();
    let on = |code: &'static str| -> &'static str { if tty { code } else { "" } };

    println!("\nDetected {} stack(s):\n", chains.len());

    let mut has_already_stacked = false;

    for (i, chain) in chains.iter().enumerate() {
        println!(
            "{}Stack {} ({} PRs):{}",
            on(DIM),
            i + 1,
            chain.len(),
            on(RESET)
        );
        for &num in chain {
            let pr = pr_details.iter().find(|p| p.number == num).unwrap();
            let already_stacked = pr.stack_number.is_some();
            if already_stacked {
                has_already_stacked = true;
            }
            let marker = if already_stacked { "*" } else { "" };

            // Build template aliases for this PR with colors
            let pr_number_str = format!("{num}{marker}");
            let aliases = TemplateAliases::builder()
                .alias(
                    "pr_number",
                    format!(
                        "label(\"gh-stack-pr-number\", \"#{}\")",
                        escape_jj_string(&pr_number_str)
                    ),
                )
                .alias(
                    "pr_branch",
                    format!(
                        "label(\"gh-stack-pr-branch\", \"{}\")",
                        escape_jj_string(&pr.head_ref)
                    ),
                )
                .alias("pr_title", quote_jj(&pr.title))
                .alias("pr_head_sha", quote_jj(&pr.head_sha))
                .alias(
                    "pr_head_user",
                    pr.head_user_login
                        .as_ref()
                        .map(|s| quote_jj(s))
                        .unwrap_or_default(),
                )
                .alias("pr_url", quote_jj(&pr.html_url))
                .color("gh-stack-pr-number", "cyan")
                .color("gh-stack-pr-branch", "magenta");

            // Evaluate the template using jj
            let tmp = aliases.write_temp_config()?;
            let rendered = jj
                .eval_template(&pr.head_sha, template, Some(tmp.path()), true, true)
                .await
                .unwrap_or_else(|_| format!("#{} {}  {}", num, pr.head_ref, pr.title));

            println!("  {}│{} {}", on(DIM), on(RESET), rendered.trim());
        }
        println!();
    }

    if has_already_stacked {
        println!("* PRs already in stacks will be unstacked and restacked\n");
    }

    let total_prs = chains.iter().map(Vec::len).sum::<usize>();
    println!("Total: {} PRs in {} stacks", total_prs, chains.len());
    Ok(())
}

/// Show confirmation prompt and get user input
async fn show_confirmation(
    jj: &impl Jj,
    chains: &[Vec<u64>],
    pr_details: &[PrDetails],
    template: &str,
) -> Result<bool> {
    show_stacks(jj, chains, pr_details, template).await?;

    print!("\nCreate these stacks? [y/N] ");
    io::stdout().flush()?;

    let mut input = String::new();
    io::stdin().read_line(&mut input)?;

    let response = input.trim().to_lowercase();
    Ok(response == "y" || response == "yes")
}

/// `#1 -> #2 -> #3` for a chain of PR numbers.
pub(crate) fn format_chain(chain: &[u64]) -> String {
    chain
        .iter()
        .map(|n| format!("#{n}"))
        .collect::<Vec<_>>()
        .join(" → ")
}

/// Report what [`crate::gh::stack_create::create_stacks`] did.
pub(crate) fn print_stack_results(results: &[ChainResult]) {
    let mut created = 0;
    let mut existing = 0;
    for result in results {
        let chain = format_chain(&result.chain);
        match result.outcome {
            ChainOutcome::Created(number) => {
                println!("✓ Stack #{number} created: {chain}");
                created += 1;
            }
            ChainOutcome::AlreadyExists => {
                println!("⊘ Stack already exists: {chain}");
                existing += 1;
            }
            ChainOutcome::LeftAlone => {
                log::debug!("chain {chain} is already stacked; leaving it alone");
            }
        }
    }

    if created == 0 && existing > 0 {
        println!("\nAll stacks already exist");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_template_keeps_nerdfont_glyph_when_enabled() {
        assert_eq!(default_stack_template(true), DEFAULT_STACK_TEMPLATE);
        assert!(default_stack_template(true).contains('\u{f407}'));
    }

    #[test]
    fn default_template_drops_nerdfont_glyph_when_disabled() {
        let template = default_stack_template(false);
        assert_eq!(template, DEFAULT_STACK_TEMPLATE_PLAIN);
        assert!(!template.contains('\u{f407}'));
    }
}
