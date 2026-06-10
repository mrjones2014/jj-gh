#[tokio::main(flavor = "current_thread")]
async fn main() -> std::process::ExitCode {
    let bin_name = std::env::current_exe().ok();
    let bin_name = bin_name
        .as_ref()
        .and_then(|p| p.file_name())
        .and_then(|o| o.to_str())
        .unwrap_or(env!("CARGO_BIN_NAME"));
    match jj_gh::dispatch(bin_name).await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            jj_gh::logging::fatal(format_args!("{e:#}"));
            std::process::ExitCode::FAILURE
        }
    }
}
