mod clock;
mod config;
mod error;
mod git;
mod github;
mod pr;
mod server;
mod state;

use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use clap::Parser;
use serde::Deserialize;

use config::Paths;
use pr::target::{PrRef, parse_target};

/// Review GitHub pull requests with a full-file, IDE-style diff in the browser.
#[derive(Parser)]
#[command(name = "gh-tideline", version)]
struct Cli {
    /// PR URL, owner/repo#N, or N (inferred from the `origin` remote). Omit for the home page.
    target: Option<String>,
    /// Port to listen on (default: `port` in config.toml, else 7457).
    #[arg(long)]
    port: Option<u16>,
    /// Do not open a browser; print the authorization URL instead.
    #[arg(long)]
    no_open: bool,
}

#[tokio::main]
async fn main() -> ExitCode {
    match run(Cli::parse()).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("gh-tideline: {e}");
            ExitCode::FAILURE
        }
    }
}

async fn run(cli: Cli) -> Result<(), String> {
    let paths = Paths::resolve()?;
    let config = config::load_config(&paths)?;
    let port = cli.port.unwrap_or(config.port);
    let allowed_hosts = config.allowed_hosts();

    let next = match &cli.target {
        None => "/".to_string(),
        Some(t) => {
            let pr = parse_target(t, origin_url)?;
            if !allowed_hosts.contains(&pr.host) {
                return Err(format!(
                    "{} is not github.com; add it to ghe_hosts in {}",
                    pr.host,
                    paths.config_dir.join("config.toml").display()
                ));
            }
            PrRef::web_path(&pr)
        }
    };

    let secret = config::load_or_create_secret(&paths)?;
    let local = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|e| e.to_string())?;

    if server_running(&local, port).await {
        // Whatever answers on the port only ever sees a one-time token bound to this port;
        // the secret never leaves this process.
        return open_browser(port, &secret, &next, cli.no_open).await;
    }

    git::check_git_version().await?;
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
        .await
        .map_err(|e| format!("cannot listen on 127.0.0.1:{port}: {e}"))?;
    let started_at = clock::unix_now();
    for dir in [&paths.cache_dir, &paths.config_dir] {
        git::ensure_private_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let repos = git::Repos::new(paths.repos_dir());
    repos
        .secure_existing_dirs()
        .map_err(|e| format!("{}: {e}", paths.repos_dir().display()))?;
    let server_json = paths.server_json(port);
    let pid = std::process::id();
    let info = serde_json::json!({
        "pid": pid,
        "port": port,
        "version": env!("CARGO_PKG_VERSION"),
    });
    std::fs::write(&server_json, info.to_string())
        .map_err(|e| format!("{}: {e}", server_json.display()))?;

    // Without it only the local baseline of "since your last review" is lost; keep serving.
    let state_db = match state::StateDb::open(&paths.cache_dir.join("state.sqlite")) {
        Ok(db) => Some(db),
        Err(e) => {
            eprintln!("gh-tideline: local state is disabled: {e}");
            None
        }
    };
    let pr_service = Arc::new(pr::PrService::new(repos, state_db));
    let state = server::state(
        port,
        secret.clone(),
        started_at,
        allowed_hosts,
        pr_service.clone(),
    );
    let app = server::router(state);
    eprintln!("tideline listening on http://localhost:{port}/ (Ctrl+C to stop)");

    let opener = tokio::spawn(async move {
        if let Err(e) = open_browser(port, &secret, &next, cli.no_open).await {
            eprintln!("gh-tideline: {e}");
        }
    });
    let served = axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await;
    opener.abort();
    // Let detached git work finish so no lock file is left behind by an orphaned git.
    pr_service.drain(Duration::from_secs(30)).await;
    remove_server_json(&server_json, pid);
    served.map_err(|e| format!("server error: {e}"))
}

/// Deletes `server-<port>.json` only if it still describes this process.
fn remove_server_json(path: &std::path::Path, pid: u32) {
    let ours = std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .is_some_and(|v| v.get("pid").and_then(|p| p.as_u64()) == Some(u64::from(pid)));
    if ours {
        let _ = std::fs::remove_file(path);
    }
}

fn origin_url() -> Result<String, String> {
    // Reads the user's own clone, so the user's git config (e.g. insteadOf) applies here.
    let out = std::process::Command::new("git")
        .args(["remote", "get-url", "origin"])
        .output()
        .map_err(|e| format!("cannot run git: {e}"))?;
    if !out.status.success() {
        return Err("a bare PR number needs a git clone with an `origin` remote".into());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

async fn server_running(client: &reqwest::Client, port: u16) -> bool {
    #[derive(Deserialize)]
    struct Health {
        app: String,
    }
    let Ok(resp) = client
        .get(format!("http://127.0.0.1:{port}/api/health"))
        .send()
        .await
    else {
        return false;
    };
    resp.json::<Health>()
        .await
        .is_ok_and(|h| h.app == "tideline")
}

/// Mints a one-time token `ts.rand.HMAC(secret, "dr-auth|port|ts|rand")` locally and opens
/// `/__auth?t=<token>&next=<next>`; the server sets a session cookie and redirects.
async fn open_browser(port: u16, secret: &str, next: &str, no_open: bool) -> Result<(), String> {
    let rand = hex::encode(rand::random::<[u8; 16]>());
    let token = server::security::mint_auth_token(secret, port, clock::unix_now(), &rand);
    let mut url = reqwest::Url::parse(&format!("http://localhost:{port}/__auth"))
        .map_err(|e| e.to_string())?;
    url.query_pairs_mut()
        .append_pair("t", &token)
        .append_pair("next", next);
    let url = url.to_string();
    if no_open {
        println!("{url}");
        return Ok(());
    }
    let to_open = url.clone();
    let opened = tokio::task::spawn_blocking(move || webbrowser::open(&to_open))
        .await
        .map_err(|e| e.to_string())?;
    if opened.is_err() {
        println!("Open this URL in your browser (valid once, for 60 s): {url}");
    }
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = tokio::signal::ctrl_c();
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .expect("installing SIGTERM handler");
    tokio::select! {
        _ = ctrl_c => {}
        _ = term.recv() => {}
    }
}
