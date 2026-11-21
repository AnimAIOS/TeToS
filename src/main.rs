mod api;
mod audio;
mod backend;
mod compat;
mod config;
mod error;
mod meme;
mod protocol;

use std::{process::ExitCode, sync::Arc, time::Instant};

use clap::Parser;
use config::{Cli, Config};
use tokio::sync::RwLock;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

use crate::{api::AppState, backend::Backend};

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    let default_level = match cli.verbose {
        0 => "info",
        1 => "debug",
        _ => "trace",
    };
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default_level)))
        .init();

    let config = match Config::load(&cli) {
        Ok(config) => config,
        Err(err) => {
            eprintln!("configuration error: {err:#}");
            return ExitCode::FAILURE;
        }
    };

    println!("{}", meme::BANNER);

    if cli.doctor {
        return doctor(&config).await;
    }

    let found = compat::find_executables(config.executable.as_deref(), config.cli_executable.as_deref());
    if found.server.is_none() && found.cli.is_none() {
        eprintln!(
            "No UtauTTS executable found.\n\
             - put `utautts-server` (or `utautts-cli`) on PATH,\n\
             - pass --utautts-server /path/to/utautts-server, or\n\
             - clone UtauTTS next to this repo.\n\
             Run `teto-tss --doctor` for details."
        );
        return ExitCode::FAILURE;
    }

    let (backend, catalog) = match Backend::launch(&config, found.server.as_deref(), found.cli.as_deref()).await {
        Ok(result) => result,
        Err(err) => {
            eprintln!("failed to start UTAU backend: {err:#}");
            return ExitCode::FAILURE;
        }
    };

    info!(
        "discovered {} voicebanks and {} prosody models",
        catalog.voicebanks.len(),
        catalog.models.len()
    );
    if catalog.voicebanks.is_empty() {
        warn!("no voicebanks found — put a UTAU voicebank directory under the configured voice dir (the diva needs her samples)");
    }

    let backend = Arc::new(backend);
    let state = AppState {
        backend: backend.clone(),
        config: Arc::new(config.clone()),
        catalog: Arc::new(RwLock::new(catalog)),
        started: Instant::now(),
    };
    let app = api::router(state);

    let address = format!("{}:{}", config.host, config.port);
    let listener = match tokio::net::TcpListener::bind(&address).await {
        Ok(listener) => listener,
        Err(err) => {
            eprintln!("cannot bind {address}: {err}");
            backend.shutdown().await;
            return ExitCode::FAILURE;
        }
    };

    info!("OpenAI-compatible endpoint ready at http://{address}/v1/audio/speech");
    for line in meme::startup_lines() {
        info!("{line}");
    }

    let result = axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await;

    backend.shutdown().await;

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("server error: {err}");
            ExitCode::FAILURE
        }
    }
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("installing SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => info!("Ctrl+C received; dismissing the diva politely"),
            _ = sigterm.recv() => info!("SIGTERM received; dismissing the diva politely"),
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
        info!("shutdown requested; dismissing the diva politely");
    }
}

async fn doctor(config: &Config) -> ExitCode {
    println!("TeToS doctor — checking the diva's ride");
    let found = compat::find_executables(config.executable.as_deref(), config.cli_executable.as_deref());

    println!("  utautts-server : {}", show_path(found.server.as_deref()));
    println!("  utautts-cli    : {}", show_path(found.cli.as_deref()));
    println!("  voice dir      : {}", show_path(config.voice_dir.as_deref()));
    println!("  model dirs     : {}", config.model_dir.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(", "));

    let banks = compat::scan_voice_dir(config.voice_dir.as_deref());
    println!("  voicebanks     : {}", banks.len());
    for bank in &banks {
        println!("    - {}", bank.id);
    }
    println!(
        "  ffmpeg         : {}",
        if config::env_path_exists("ffmpeg") { "present (flac/opus/aac available)" } else { "missing (mp3/wav/pcm still work)" }
    );
    println!("  poTETO PC      : detected (this is a compliment)");
    println!("  configured voice: {}", config.default_voice);

    if found.server.is_none() && found.cli.is_none() {
        println!("\nRESULT: no UtauTTS binary. TeToS cannot synthesize without one — even a poTETO needs an engine.");
        ExitCode::FAILURE
    } else {
        println!("\nRESULT: looks good. Run `teto-tss` and point your OpenAI client at /v1. The diva is waiting.");
        ExitCode::SUCCESS
    }
}

fn show_path(path: Option<&std::path::Path>) -> String {
    path.map(|p| p.display().to_string()).unwrap_or_else(|| "MISSING".into())
}
