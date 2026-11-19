//! `ai-store` — 5-Layer OpenDAL AI Storage Runtime CLI & MCP Producer.

use ai_store::mcp::{
    build_router, daemon_state_file, run_http_server, run_stdio_proxy, AppState, DaemonRecord,
};
use ai_store::profile::ProjectProfile;
use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::sync::Arc;
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(
    name = "ai-store",
    version = "0.1.0",
    about = "OpenDAL AI Storage Runtime: 8 Semantic Archetypes, ProjectProfile Policy Guard, Streamable HTTP & Singleton Daemon MCP Producer"
)]
struct Cli {
    /// Path to ProjectProfile YAML (defaults to AI_STORE_PROFILE, ./.ai-store.yaml, or /etc/ai-store/project-profile.yaml)
    #[arg(long, global = true)]
    profile: Option<PathBuf>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Run the persistent Streamable HTTP MCP Server (/mcp, /healthz, /readyz, /metrics)
    Serve {
        #[arg(long)]
        host: Option<String>,
        #[arg(long)]
        port: Option<u16>,
        #[arg(long)]
        path: Option<String>,
    },
    /// Thin Stdio MCP Proxy that auto-wakes and multiplexes onto the local Singleton Daemon
    Mcp {
        #[arg(long)]
        daemon_dir: Option<PathBuf>,
        /// Run in-process without spawning/connecting to a background singleton daemon
        #[arg(long)]
        no_daemon: bool,
    },
    /// Validate or inspect the active ProjectProfile & semantic mount tree
    Profile {
        #[command(subcommand)]
        action: ProfileAction,
    },
    /// Inspect or stop the local Singleton Daemon
    Daemon {
        #[command(subcommand)]
        action: DaemonAction,
    },
}

#[derive(Subcommand, Debug)]
enum ProfileAction {
    Validate,
    Show,
    Init {
        #[arg(long, default_value = ".ai-store.yaml")]
        output: PathBuf,
    },
}

#[derive(Subcommand, Debug)]
enum DaemonAction {
    Status {
        #[arg(long)]
        daemon_dir: Option<PathBuf>,
    },
    Stop {
        #[arg(long)]
        daemon_dir: Option<PathBuf>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();
    match cli.command {
        Commands::Serve { host, port, path } => {
            let state = Arc::new(AppState::initialize(cli.profile.as_deref()).await?);
            let router = state.current_router().await;
            let bind_host = host.unwrap_or_else(|| router.profile.spec.exposure.host.clone());
            let bind_port = port.unwrap_or(router.profile.spec.exposure.port);
            let mcp_path = path.unwrap_or_else(|| router.profile.spec.exposure.path.clone());
            run_http_server(state, &bind_host, bind_port, &mcp_path).await?;
        }
        Commands::Mcp {
            daemon_dir,
            no_daemon,
        } => {
            run_stdio_proxy(cli.profile.as_deref(), daemon_dir.as_deref(), no_daemon).await?;
        }
        Commands::Profile { action } => match action {
            ProfileAction::Validate => {
                let (profile, resolved, digest) =
                    ProjectProfile::load_or_default(cli.profile.as_deref())?;
                let router = build_router(&profile, &resolved).await?;
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "valid": true,
                        "path": resolved,
                        "digestSha256": digest,
                        "mounts": router.mounts_tree()
                    }))?
                );
            }
            ProfileAction::Show => {
                let (profile, resolved, digest) =
                    ProjectProfile::load_or_default(cli.profile.as_deref())?;
                println!("# Path: {} (sha256: {})", resolved.display(), digest);
                println!("{}", serde_yaml::to_string(&profile.redacted())?);
            }
            ProfileAction::Init { output } => {
                let base = output
                    .parent()
                    .filter(|p| !p.as_os_str().is_empty())
                    .map(|p| p.to_path_buf())
                    .unwrap_or_else(|| PathBuf::from("."));
                let profile = ProjectProfile::standalone_default(&base);
                std::fs::write(&output, serde_yaml::to_string(&profile)?)?;
                println!("Wrote standalone ProjectProfile to {}", output.display());
            }
        },
        Commands::Daemon { action } => match action {
            DaemonAction::Status { daemon_dir } => {
                let sf = daemon_state_file(daemon_dir.as_deref());
                if !sf.exists() {
                    println!("{{\"running\": false}}");
                } else {
                    let raw = std::fs::read_to_string(&sf)?;
                    let rec: DaemonRecord = serde_json::from_str(&raw)?;
                    let healthy = tokio::net::TcpStream::connect((rec.host.as_str(), rec.port))
                        .await
                        .is_ok();
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&serde_json::json!({
                            "running": healthy,
                            "daemon": rec
                        }))?
                    );
                }
            }
            DaemonAction::Stop { daemon_dir } => {
                let sf = daemon_state_file(daemon_dir.as_deref());
                if sf.exists() {
                    let raw = std::fs::read_to_string(&sf)?;
                    if let Ok(rec) = serde_json::from_str::<DaemonRecord>(&raw) {
                        if rec.pid > 0 {
                            #[cfg(windows)]
                            {
                                let _ = std::process::Command::new("taskkill")
                                    .args(["/PID", &rec.pid.to_string(), "/F"])
                                    .status();
                            }
                            #[cfg(unix)]
                            {
                                let _ = std::process::Command::new("kill")
                                    .args(["-TERM", &rec.pid.to_string()])
                                    .status();
                            }
                        }
                    }
                    let _ = std::fs::remove_file(&sf);
                }
                println!("Stopped ai-store singleton daemon.");
            }
        },
    }
    Ok(())
}
