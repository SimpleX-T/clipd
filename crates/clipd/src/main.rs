//! clipd — clipboard history daemon for Wayland/GNOME.
//!
//! Two modes:
//!   `clipd daemon`               long-running: spawns wl-paste --watch
//!                                children, owns the SQLite store, serves
//!                                a Unix socket for the UI.
//!   `clipd ingest --mime <m>`    one-shot: reads stdin (clipboard payload
//!                                piped by wl-paste --watch), forwards it
//!                                to the running daemon over IPC.

use std::env;
use std::process::ExitCode;

mod classify;
mod ingest_cmd;
mod ipc;
mod store;
mod watcher;

#[tokio::main]
async fn main() -> ExitCode {
    init_logging();

    let mut args = env::args().skip(1);
    let cmd = args.next().unwrap_or_else(|| "daemon".to_string());

    let result = match cmd.as_str() {
        "daemon" => run_daemon().await,
        "ingest" => ingest_cmd::run(args.collect()).await,
        "version" | "--version" | "-V" => {
            println!("clipd {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        "help" | "--help" | "-h" => {
            print_help();
            return ExitCode::SUCCESS;
        }
        other => {
            eprintln!("clipd: unknown command {other:?}");
            print_help();
            return ExitCode::from(2);
        }
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!("{:#}", e);
            ExitCode::FAILURE
        }
    }
}

fn init_logging() {
    use tracing_subscriber::{fmt, EnvFilter};
    let filter = EnvFilter::try_from_env("CLIPD_LOG")
        .unwrap_or_else(|_| EnvFilter::new("clipd=info,warn"));
    fmt().with_env_filter(filter).with_target(false).init();
}

fn print_help() {
    eprintln!(
        "clipd {} — clipboard history daemon

USAGE:
  clipd daemon                       run the daemon (default)
  clipd ingest --mime <m>            push stdin into the running daemon
  clipd version
  clipd help

ENV:
  CLIPD_LOG=clipd=debug               adjust log level
  XDG_RUNTIME_DIR                     where the socket lives (clipd.sock)
  XDG_DATA_HOME                       where history.db lives (defaults to ~/.local/share)
",
        env!("CARGO_PKG_VERSION")
    );
}

async fn run_daemon() -> anyhow::Result<()> {
    let store = store::Store::open()?;
    match store.cleanup_expired() {
        Ok(0) => {}
        Ok(n) => tracing::info!("TTL: removed {n} unpinned clips older than 24h"),
        Err(e) => tracing::warn!("TTL cleanup failed: {e:#}"),
    }
    let store = std::sync::Arc::new(store);

    let watcher = watcher::Watcher::spawn(store.clone())?;
    let server = ipc::serve(store.clone()).await?;

    tracing::info!(
        "clipd ready — socket {}, history at {}",
        server.socket_path().display(),
        store.db_path().display()
    );

    // Graceful shutdown on Ctrl-C / SIGTERM.
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            tracing::info!("shutdown requested");
        }
        r = server.serve() => {
            if let Err(e) = r {
                tracing::error!("ipc server stopped: {:#}", e);
            }
        }
    }

    drop(watcher);
    Ok(())
}
