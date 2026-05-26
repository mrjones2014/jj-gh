use anyhow::Result;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let bin_name = std::env::current_exe().ok();
    let bin_name = bin_name
        .as_ref()
        .and_then(|p| p.file_name())
        .and_then(|o| o.to_str())
        .unwrap_or(env!("CARGO_BIN_NAME"));
    jj_gh::dispatch(bin_name).await
}
