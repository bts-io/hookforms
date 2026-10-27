use std::net::SocketAddr;
use std::process::ExitCode;

use hookforms::config::Config;
use hookforms::{AppState, cleanup, db, import, security};
use tracing_subscriber::EnvFilter;

const LISTEN: &str = "0.0.0.0:8000";

#[tokio::main]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("import-postgres") {
        return import_postgres(&args[1..]).await;
    }

    let config = match Config::from_env() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };

    let default_level = if config.debug { "debug" } else { "info" };
    tracing_subscriber::fmt()
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stdout()))
        .with_env_filter(
            EnvFilter::try_from_env("RUST_LOG").unwrap_or_else(|_| EnvFilter::new(default_level)),
        )
        .init();

    for note in &config.ignored {
        tracing::info!("{note}");
    }

    let pool = match db::init(&config.database_url).await {
        Ok(p) => p,
        Err(e) => {
            tracing::error!(error = %e, url = %config.database_url, "database setup failed");
            return ExitCode::FAILURE;
        }
    };

    let cleanup_task = cleanup::spawn(pool.clone(), config.event_retention_days);
    let state = AppState::new(pool.clone(), config, security::safe_http_client());
    let app = hookforms::build_app(state);

    let listener = match tokio::net::TcpListener::bind(LISTEN).await {
        Ok(l) => l,
        Err(e) => {
            tracing::error!(error = %e, "cannot listen on {LISTEN}");
            return ExitCode::FAILURE;
        }
    };
    tracing::info!("listening on {LISTEN}");

    let served = axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown())
    .await;
    cleanup_task.abort();
    pool.close().await;
    match served {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!(error = %e, "server error");
            ExitCode::FAILURE
        }
    }
}

/// `hookforms import-postgres --from ... [--to ...] [--force]`: copy a Python
/// install's Postgres data into SQLite, then print the counts.
async fn import_postgres(args: &[String]) -> ExitCode {
    let opts = match import::parse_args(args) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(2);
        }
    };
    println!("Importing into {} ...", opts.to);
    match import::run(&opts).await {
        Ok(report) => {
            println!("{report}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("import failed: {e}");
            ExitCode::FAILURE
        }
    }
}

async fn shutdown() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let term = async {
        if let Ok(mut s) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            s.recv().await;
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! { _ = ctrl_c => {}, _ = term => {} }
}
