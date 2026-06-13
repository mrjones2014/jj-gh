use clap::CommandFactory;
use jj_gh::Cli;

fn main() -> std::io::Result<()> {
    clap_mangen::Man::new(Cli::command()).render(&mut std::io::stdout().lock())
}
