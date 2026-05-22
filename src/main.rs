use anyhow::Result;
use clap::Parser;

mod auth;
mod cli;
mod config;
mod debug;
mod fs;
mod gh;
mod git;
mod jj;
mod logging;
mod pr;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let args = cli::Cli::parse();
    let _logger = logging::init(&args.global)?;

    match args.command {
        cli::Command::Pr { action } => match action {
            cli::PrAction::Create(create) => run_pr_create(create).await?,
        },
        cli::Command::Debug { action } => debug::dispatch(action).await?,
    }
    Ok(())
}

async fn run_pr_create(args: cli::CreateArgs) -> Result<()> {
    let config = config::load()?;
    let token = auth::resolve_token(&config).await?;
    let jj = jj::real::JjCli;
    let gh = gh::real::OctocrabGh::new(&token)?;
    let editor = pr::TempfileEditor;
    pr::create(&jj, &gh, &editor, &config, &args).await
}
