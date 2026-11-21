//! External process management and synthesis. All UtauTTS specifics live here.
//! The diva runs as a child process; please do not poke her with a stick.

use std::{fs, path::{Path, PathBuf}, process::Stdio, sync::atomic::{AtomicU64, Ordering}, time::Duration};

use anyhow::{Context, Result, bail};
use reqwest::{Client, StatusCode};
use serde::{Deserialize, de::DeserializeOwned};
use tokio::{io::AsyncBufReadExt, process::{Child, Command}, sync::{Mutex, oneshot}};
use tracing::{info, warn};

use crate::{
    compat::{self, Catalog, Health, ModelInfo, Voicebank},
    config::{BackendKind, Config},
    protocol::UtauRequest,
};

#[derive(Debug, Clone)]
pub struct SynthOutput {
    pub wav: Vec<u8>,
    pub reading: Option<String>,
    pub method: Option<String>,
    pub diagnostics: usize,
}

#[derive(Debug, Clone)]
pub struct BackendError {
    pub status: StatusCode,
    pub code: String,
    pub message: String,
}

impl BackendError {
    fn new(status: StatusCode, code: impl Into<String>, message: impl Into<String>) -> Self {
        Self { status, code: code.into(), message: message.into() }
    }

    fn transport(err: reqwest::Error) -> Self {
        Self::new(StatusCode::BAD_GATEWAY, "backend_unreachable", format!("UtauTTS request failed: {err}"))
    }
}

impl std::fmt::Display for BackendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
struct UpstreamError {
    error: String,
    code: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
struct VoicebankList {
    voicebanks: Vec<Voicebank>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
struct ModelList {
    models: Option<Vec<ModelInfo>>,
}

pub enum Backend {
    Server(ServerBackend),
    Cli(CliBackend),
}

impl Backend {
    /// Launch the best available backend. `Auto` prefers the fast HTTP server
    /// and silently degrades to the CLI, which has a much smaller API surface.
    pub async fn launch(cfg: &Config, server: Option<&Path>, cli: Option<&Path>) -> Result<(Self, Catalog)> {
        let kind = match cfg.backend_kind {
            BackendKind::Auto if server.is_some() => BackendKind::Server,
            BackendKind::Auto if cli.is_some() => BackendKind::Cli,
            other => other,
        };

        match kind {
            BackendKind::Server => {
                let Some(exe) = server else {
                    bail!("utautts-server not found. Put it on PATH, pass --utautts-server, or use --backend cli (the diva cannot sing without her engine)");
                };
                let backend = ServerBackend::start(cfg, exe).await?;
                let catalog = backend.catalog().await;
                info!("backend: long-lived utautts-server at {} (the diva has entered the building)", backend.base);
                Ok((Self::Server(backend), catalog))
            }
            BackendKind::Cli => {
                let Some(exe) = cli else {
                    bail!("utautts-cli not found. Put it on PATH or pass --utautts-cli");
                };
                let backend = CliBackend::new(cfg, exe);
                let catalog = backend.catalog();
                info!("backend: utautts-cli (one process per synthesis — the CLI diva is a method actor)");
                Ok((Self::Cli(backend), catalog))
            }
            BackendKind::Auto => bail!("no UtauTTS executable found"),
        }
    }

    pub fn name(&self) -> &'static str {
        match self { Self::Server(_) => "utautts-server", Self::Cli(_) => "utautts-cli" }
    }

    pub async fn synthesize(&self, req: &UtauRequest) -> std::result::Result<SynthOutput, BackendError> {
        match self {
            Self::Server(b) => b.synthesize(req).await,
            Self::Cli(b) => b.synthesize(req).await,
        }
    }

    pub async fn shutdown(&self) {
        if let Self::Server(b) = self { b.shutdown().await; }
    }
}

pub struct ServerBackend {
    child: Mutex<Option<Child>>,
    client: Client,
    base: String,
    voice_dir: Option<PathBuf>,
    version: String,
}

impl ServerBackend {
    async fn start(cfg: &Config, exe: &Path) -> Result<Self> {
        let port = if cfg.backend_port == 0 { free_port()? } else { cfg.backend_port };
        let mut command = Command::new(exe);
        command
            .arg("--host").arg("127.0.0.1")
            .arg("--port").arg(port.to_string())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(dir) = &cfg.voice_dir {
            command.arg("--voice-dir").arg(dir);
        }
        for dir in &cfg.model_dir {
            command.arg("--model-dir").arg(dir);
        }
        for arg in &cfg.extra_args {
            command.arg(arg);
        }

        let mut child = command.spawn()
            .with_context(|| format!("starting {}", exe.display()))?;

        let stdout = child.stdout.take().context("no stdout from utautts-server")?;
        let stderr = child.stderr.take().context("no stderr from utautts-server")?;
        let (ready_tx, ready_rx) = oneshot::channel::<String>();

        tokio::spawn(async move {
            let mut ready = Some(ready_tx);
            let mut lines = tokio::io::BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if let Some(url) = line.strip_prefix("UTAUTTS_READY=") {
                    if !url.trim().is_empty() {
                        if let Some(tx) = ready.take() {
                            let _ = tx.send(url.trim().to_string());
                        }
                    }
                }
                info!("[utautts] {line}");
            }
        });
        tokio::spawn(async move {
            let mut lines = tokio::io::BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                warn!("[utautts] {line}");
            }
        });

        let url = tokio::time::timeout(cfg.startup_timeout, ready_rx)
            .await
            .context("timed out waiting for UTAUTTS_READY (the diva was warming up her haircut memory card)")?
            .context("utautts-server exited before becoming ready")?;

        if let Some(status) = child.try_wait().context("checking utautts-server")? {
            bail!("utautts-server exited immediately with {status}");
        }

        Ok(Self {
            child: Mutex::new(Some(child)),
            client: Client::builder()
                .timeout(Duration::from_secs(300))
                .build()
                .context("building HTTP client")?,
            base: url.trim_end_matches('/').to_string(),
            voice_dir: cfg.voice_dir.clone(),
            version: String::new(),
        })
    }

    async fn get_json<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        let response = self.client.get(format!("{}{}", self.base, path)).send().await?;
        if !response.status().is_success() {
            bail!("{} returned {}", path, response.status());
        }
        Ok(response.json::<T>().await?)
    }

    /// Lenient discovery: API failures degrade to a directory scan instead of
    /// taking the whole service down.
    async fn catalog(&self) -> Catalog {
        let mut out = Catalog::default();

        match self.get_json::<Health>("/api/health").await {
            Ok(health) => {
                compat::report_version(&health);
                // Stored for diagnostics; the struct is not Sync-mutable, so we
                // simply log here and keep the field for future use.
                let _ = &self.version;
                info!("upstream health: {:?}", health.status);
            }
            Err(err) => warn!("health probe failed: {err:#}"),
        }

        match self.get_json::<VoicebankList>("/api/voicebanks").await {
            Ok(list) => out.voicebanks = list.voicebanks,
            Err(err) => warn!("voicebank catalog unavailable: {err} (falling back to directory scan)"),
        }
        match self.get_json::<ModelList>("/api/models").await {
            Ok(list) => out.models = list.models.unwrap_or_default(),
            Err(err) => warn!("model catalog unavailable: {err}"),
        }

        for scanned in compat::scan_voice_dir(self.voice_dir.as_deref()) {
            if !out.voicebanks.iter().any(|v| v.id == scanned.id) {
                out.voicebanks.push(scanned);
            }
        }
        out.voicebanks.sort_by(|a, b| a.id.cmp(&b.id));
        out
    }

    async fn synthesize(&self, req: &UtauRequest) -> std::result::Result<SynthOutput, BackendError> {
        let response = self.client
            .post(format!("{}/api/synthesize/audio", self.base))
            .json(req)
            .send()
            .await
            .map_err(BackendError::transport)?;

        let status = response.status();
        let reading = header(&response, "X-UtauTTS-Reading");
        let method = header(&response, "X-UtauTTS-Method");
        let diagnostics = header(&response, "X-UtauTTS-Diagnostic-Count")
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(0);

        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            let parsed = serde_json::from_str::<UpstreamError>(&body).unwrap_or_default();
            let message = if parsed.error.is_empty() {
                format!("UtauTTS returned {status}: {body}")
            } else {
                parsed.error
            };
            let code: String = if parsed.code.is_empty() {
                upstream_code(status).to_string()
            } else {
                parsed.code
            };
            return Err(BackendError::new(map_status(status), code, message));
        }

        let bytes = response.bytes().await.map_err(BackendError::transport)?;
        Ok(SynthOutput { wav: bytes.to_vec(), reading, method, diagnostics })
    }

    async fn shutdown(&self) {
        let mut guard = self.child.lock().await;
        let Some(child) = guard.as_mut() else { return; };

        #[cfg(unix)]
        if let Some(pid) = child.id() {
            unsafe {
                libc::kill(pid as i32, libc::SIGTERM);
            }
        }
        #[cfg(not(unix))]
        let _ = child.start_kill();

        match tokio::time::timeout(Duration::from_secs(5), child.wait()).await {
            Ok(Ok(_)) => info!("utautts-server shut down cleanly (the diva has left the stage)"),
            _ => {
                warn!("utautts-server did not exit in time; forcing shutdown (the diva refused to leave)");
                let _ = child.start_kill();
            }
        }
        *guard = None;
    }
}

fn header(response: &reqwest::Response, name: &str) -> Option<String> {
    response.headers().get(name)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
}

pub struct CliBackend {
    exe: PathBuf,
    voice_dir: Option<PathBuf>,
    seq: AtomicU64,
}

impl CliBackend {
    fn new(cfg: &Config, exe: &Path) -> Self {
        Self { exe: exe.to_path_buf(), voice_dir: cfg.voice_dir.clone(), seq: AtomicU64::new(0) }
    }

    fn catalog(&self) -> Catalog {
        Catalog {
            voicebanks: compat::scan_voice_dir(self.voice_dir.as_deref()),
            models: Vec::new(),
        }
    }

    async fn synthesize(&self, req: &UtauRequest) -> std::result::Result<SynthOutput, BackendError> {
        if req.voicebank_id.is_empty() {
            return Err(BackendError::new(StatusCode::BAD_REQUEST, "invalid_request", "no voicebank selected"));
        }
        let Some(voice_dir) = &self.voice_dir else {
            return Err(BackendError::new(StatusCode::BAD_REQUEST, "invalid_request",
                "CLI backend needs --voice-dir"));
        };
        let voicebank = voice_dir.join(&req.voicebank_id);
        if !voicebank.is_dir() {
            return Err(BackendError::new(StatusCode::BAD_REQUEST, "invalid_request",
                format!("voicebank directory not found: {}", voicebank.display())));
        }

        let n = self.seq.fetch_add(1, Ordering::Relaxed);
        let request_file = std::env::temp_dir().join(format!(
            "teto-tss-{}-{}-{}.json", std::process::id(), n,
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos()
        ));
        let written = fs::write(&request_file, serde_json::to_vec(&req.for_cli()).unwrap())
            .map_err(|e| BackendError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal_error", e.to_string()));
        if let Err(e) = written { return Err(e); }

        let output = Command::new(&self.exe)
            .arg("--voicebank").arg(&voicebank)
            .arg("--request").arg(&request_file)
            .arg("--out").arg("-")
            .output()
            .await
            .map_err(|e| BackendError::new(StatusCode::BAD_GATEWAY, "backend_unreachable", e.to_string()));
        let _ = fs::remove_file(&request_file);
        let output = output?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Err(BackendError::new(StatusCode::UNPROCESSABLE_ENTITY, "synthesis_error",
                format!("utautts-cli exited with {}: {}", output.status, stderr)));
        }
        Ok(SynthOutput { wav: output.stdout, reading: None, method: Some(req.method.clone()), diagnostics: 0 })
    }
}

fn free_port() -> Result<u16> {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0))
        .context("finding a free loopback port")?;
    Ok(listener.local_addr()?.port())
}

fn upstream_code(status: StatusCode) -> &'static str {
    match status.as_u16() {
        400 => "invalid_request",
        404 => "not_found",
        413 => "limit_exceeded",
        422 => "synthesis_error",
        429 => "too_many_requests",
        503 => "unavailable",
        _ => "internal_error",
    }
}

fn map_status(status: StatusCode) -> StatusCode {
    match status.as_u16() {
        400 | 404 | 413 | 422 | 429 => status,
        503 => StatusCode::SERVICE_UNAVAILABLE,
        _ => StatusCode::BAD_GATEWAY,
    }
}
