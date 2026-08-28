use std::process::ExitCode;

use ffc_prototype::release_identity::current_release_identity;
use ffc_prototype::web_server::{WebServerConfig, run_web_server_until};
use tracing_subscriber::EnvFilter;

const HELP: &str = "Animal Fighter Club hosted browser authority\n\n\
Usage: afc-web-server [--check-config | --version | --release-identity | --help]\n\n\
Required production environment:\n\
  AFC_WEB_ALLOWED_ORIGINS          Comma-separated exact HTTPS itch origins\n\
  AFC_WEB_SIGNING_KEY_ID           Non-zero current signing-key ID\n\
  AFC_WEB_SIGNING_KEY              Base64url, unpadded 32-byte signing key\n\
  AFC_WEB_SIGNING_KEY_FILE         File alternative to AFC_WEB_SIGNING_KEY\n\
  AFC_WEB_PUBLIC_WEBSOCKET_URL     Public wss://.../v2/connect/ws URL\n\
  AFC_WEB_PUBLIC_WEBTRANSPORT_URL  Public https://.../v2/connect/wt URL\n\
  AFC_WEBTRANSPORT_BIND            UDP bind address, for example 0.0.0.0:4433\n\
  AFC_WEBTRANSPORT_CERT_PEM        TLS certificate-chain PEM path\n\
  AFC_WEBTRANSPORT_KEY_PEM         TLS private-key PEM path\n\n\
Optional environment:\n\
  AFC_WEB_HTTP_BIND                HTTP bind address (default 0.0.0.0:8080)\n\
  AFC_WEB_DEPLOYMENT               production (default) or development\n\
  AFC_WEB_TRUSTED_PROXY_IPS        Exact comma-separated reverse-proxy IPs\n\
  AFC_WEB_PREVIOUS_SIGNING_KEY_ID  Previous rotation key ID (paired)\n\
  AFC_WEB_PREVIOUS_SIGNING_KEY     Previous rotation key (paired)\n\
  AFC_WEB_PREVIOUS_SIGNING_KEY_FILE File alternative for previous key\n\
  RUST_LOG                         Log filter (default info)\n";

#[tokio::main]
async fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let action = args.next();
    if args.next().is_some() {
        eprintln!("afc-web-server accepts at most one option\n\n{HELP}");
        return ExitCode::from(2);
    }
    match action.as_deref() {
        Some("-h" | "--help") => {
            print!("{HELP}");
            return ExitCode::SUCCESS;
        }
        Some("--version") => {
            println!("{}", current_release_identity().version_line());
            return ExitCode::SUCCESS;
        }
        Some("--release-identity") => {
            println!("{}", current_release_identity().to_deterministic_json());
            return ExitCode::SUCCESS;
        }
        None | Some("--check-config") => {}
        Some(option) => {
            eprintln!("unknown afc-web-server option '{option}'\n\n{HELP}");
            return ExitCode::from(2);
        }
    }

    let config = match WebServerConfig::from_environment() {
        Ok(config) => config,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::from(2);
        }
    };
    if action.as_deref() == Some("--check-config") {
        println!("afc-web-server configuration is valid");
        return ExitCode::SUCCESS;
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_target(false)
        .compact()
        .init();
    match run_web_server_until(config, shutdown_signal()).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!(%error, "afc-web-server terminated with an error");
            ExitCode::FAILURE
        }
    }
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        if let Ok(mut terminate) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = terminate.recv() => {}
            }
            return;
        }
    }
    let _ = tokio::signal::ctrl_c().await;
}
