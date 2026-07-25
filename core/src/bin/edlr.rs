use clap::Parser;
use edlr_core::{config, monitor, router::Router};
use std::path::PathBuf;
use std::time::Duration;

/// EliteDangerousLogRouter daemon
#[derive(Parser)]
#[command(name = "edlr", version)]
struct Args {
    /// Journal ディレクトリ(未指定時は既知パスを探索)
    #[arg(long)]
    journal_dir: Option<PathBuf>,

    /// ポーリング間隔(ミリ秒)
    #[arg(long, default_value_t = 1000)]
    poll_interval_ms: u64,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .init();
    let args = Args::parse();

    let dir = args.journal_dir.or_else(|| {
        let home = std::env::var_os("HOME").map(PathBuf::from)?;
        config::default_journal_dir(&home)
    });
    let Some(dir) = dir else {
        eprintln!("error: journal directory not found; specify one with --journal-dir <PATH>");
        std::process::exit(1);
    };

    if !dir.is_dir() {
        eprintln!("error: journal directory does not exist: {}", dir.display());
        std::process::exit(1);
    }

    tracing::info!("watching {}", dir.display());
    let router = Router::new(256);
    let mut rx = router.subscribe();
    tokio::spawn(monitor::run(
        dir,
        router.clone(),
        Duration::from_millis(args.poll_interval_ms),
    ));

    loop {
        match rx.recv().await {
            Ok(event) => {
                let json = match &*event {
                    edlr_core::event::Event::Journal { raw, .. } => raw.to_string(),
                    edlr_core::event::Event::Status { raw } => raw.to_string(),
                };
                println!("{json}");
            }
            Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                tracing::warn!("stdout consumer lagged, dropped {n} events");
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
        }
    }
}
