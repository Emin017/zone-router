use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{error, info};
use zone_router::config::Config;
use zone_router::state::AppState;

#[derive(Parser)]
#[command(
    name = "zone-router",
    about = "LLM API router with TUI for Claude Code"
)]
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

    let config = Config::load_or_create(&config_path)?;

    let mut app_state = AppState::new(config, config_path)?;

    if let Some(port) = cli.port {
        app_state.config.proxy.listen = format!("127.0.0.1:{port}");
    }

    if let Some(Commands::Env) = cli.command {
        println!(
            "export ANTHROPIC_BASE_URL=http://{}",
            app_state.config.proxy.listen
        );
        println!("export ANTHROPIC_API_KEY={}", app_state.local_token);
        println!("export ANTHROPIC_AUTH_TOKEN={}", app_state.local_token);
        return Ok(());
    }

    let listen_addr = app_state.config.proxy.listen.clone();

    // Pre-bind the listener so port-in-use fails before TUI launch
    let listener = tokio::net::TcpListener::bind(&listen_addr)
        .await
        .map_err(|e| format!("Failed to bind to {listen_addr}: {e}"))?;

    // Initialise structured logging before spawning any tasks.
    let (log_rx, _log_guard, log_dir) = zone_router::logging::init_tracing();
    if let Some(ref dir) = log_dir {
        info!(path = %dir.display(), "logging to file");
    } else {
        info!("file logging disabled (log directory not writable)");
    }

    let state = Arc::new(RwLock::new(app_state));
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

    let server_state = state.clone();
    let mut server_handle = tokio::spawn(async move {
        if let Err(e) =
            zone_router::proxy::server::start_with_listener(server_state, listener, shutdown_rx)
                .await
        {
            error!("proxy server fatal error: {}", e);
        }
    });

    zone_router::logging::log_server_started(&listen_addr);
    info!("local token generated");

    let tui_state = state.clone();
    let tui_handle = tokio::task::spawn_blocking(move || {
        zone_router::tui::app::run_tui(tui_state, shutdown_tx, log_rx)
    });

    let _ = tui_handle.await;

    zone_router::proxy::server::force_shutdown(state, &mut server_handle).await;

    Ok(())
}
