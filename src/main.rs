use clap::{Parser, Subcommand};
use api_router::config::Config;
use api_router::state::AppState;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Parser)]
#[command(name = "api-router", about = "LLM API router with TUI for Claude Code")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Port to listen on (overrides config)
    #[arg(long)]
    port: Option<u16>,

    /// Path to config file
    #[arg(long)]
    config: Option<PathBuf>,
}

#[derive(Subcommand)]
enum Commands {
    /// Output environment variables for Claude Code integration
    Env,
}

fn resolve_config_path(cli_path: Option<PathBuf>) -> PathBuf {
    cli_path
        .or_else(Config::default_path)
        .unwrap_or_else(|| PathBuf::from("config.toml"))
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let config_path = resolve_config_path(cli.config);

    let mut config = Config::load_or_create(&config_path)?;

    if let Some(port) = cli.port {
        config.proxy.listen = format!("127.0.0.1:{port}");
    }

    let app_state = AppState::new(config, config_path);

    if let Some(Commands::Env) = cli.command {
        println!(
            "export ANTHROPIC_BASE_URL=http://{}",
            app_state.config.proxy.listen
        );
        println!("export ANTHROPIC_API_KEY={}", app_state.local_token);
        return Ok(());
    }

    let local_token = app_state.local_token.clone();
    let listen_addr = app_state.config.proxy.listen.clone();
    let state = Arc::new(RwLock::new(app_state));
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

    let server_state = state.clone();
    let server_handle = tokio::spawn(async move {
        if let Err(e) = api_router::proxy::server::start(server_state, shutdown_rx).await {
            eprintln!("Proxy server error: {e}");
        }
    });

    eprintln!("api-router listening on {listen_addr}");
    eprintln!("Local token: {local_token}");

    let tui_state = state.clone();
    let tui_handle = tokio::task::spawn_blocking(move || {
        api_router::tui::app::run_tui(tui_state, shutdown_tx)
    });

    // Wait for TUI to exit, then shut down
    let _ = tui_handle.await;

    // Mark shutdown and wait for server with timeout
    state.write().await.shutdown = true;
    let _ = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        server_handle,
    )
    .await;

    Ok(())
}
