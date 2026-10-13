//! `cc-switch server` — run the local HTTP management API.

use clap::Args;

use crate::error::AppError;
use crate::server;

#[derive(Args, Debug)]
pub struct ServerCommand {
    /// Bind address (default 127.0.0.1; anything else exposes API keys
    /// and provider control to the network).
    #[arg(long)]
    pub host: Option<String>,
    /// Bind port (default 7861).
    #[arg(long)]
    pub port: Option<u16>,
    /// Use an explicit token instead of the stored one.
    #[arg(long)]
    pub token: Option<String>,
    #[command(subcommand)]
    pub command: Option<ServerSubcommand>,
}

#[derive(clap::Subcommand, Debug)]
pub enum ServerSubcommand {
    /// Print the current access token.
    Token,
}

pub fn execute(cmd: ServerCommand) -> Result<(), AppError> {
    match cmd.command {
        Some(ServerSubcommand::Token) => {
            let token = crate::server::auth::load_or_create_token()?;
            println!("{token}");
            Ok(())
        }
        None => {
            let config = server::ServerConfig {
                host: cmd.host.unwrap_or_else(|| "127.0.0.1".to_string()),
                port: cmd.port.unwrap_or(7861),
                token: cmd.token,
            };
            if config.host != "127.0.0.1" && config.host != "localhost" {
                eprintln!(
                    "{}",
                    crate::cli::ui::warning(
                        "WARNING: binding beyond localhost exposes API keys and provider control."
                    )
                );
            }
            let token = match &config.token {
                Some(explicit) => explicit.clone(),
                None => crate::server::auth::load_or_create_token()?,
            };
            println!(
                "{}",
                crate::cli::ui::success(&format!(
                    "cc-switch server listening on http://{}:{}",
                    config.host, config.port
                ))
            );
            println!(
                "{}",
                crate::cli::ui::info(&format!("Auth: Authorization: Bearer {token}"))
            );
            println!(
                "{}",
                crate::cli::ui::info(
                    "OpenAPI: http://127.0.0.1:7861/api/openapi.json (Ctrl+C to stop)"
                )
            );
            server::run(config)
        }
    }
}
