use std::{collections::HashMap, env, path::PathBuf, time::Duration};

use anyhow::{Context, Result, bail};
use clap::{Parser, ValueEnum};
use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Default)]
pub enum BackendKind {
    /// Prefer the long-lived HTTP server, fall back to the CLI.
    #[default]
    Auto,
    /// Spawn and keep `utautts-server` alive.
    Server,
    /// Invoke `utautts-cli` once per request. Slower, but immune to HTTP API drift.
    Cli,
}

#[derive(Debug, Clone, Parser)]
#[command(
    name = "teto-tss",
    version,
    about = "Teto as a Service: an OpenAI-compatible /v1/audio/speech endpoint backed by UTAU voicebanks. Runs on any poTETO PC.",
    long_about = None,
    after_help = "The LLM does the thinking. Teto does the talking. Both are your problem now.\nRuns on any poTETO PC. Haircut memory card sold separately."
)]
pub struct Cli {
    /// Config file path (default: ./TeToS.toml if it exists)
    #[arg(long, value_name = "PATH", env = "TETOS_CONFIG")]
    pub config: Option<PathBuf>,

    /// Listen host for the OpenAI-compatible API
    #[arg(long, env = "TETOS_HOST")]
    pub host: Option<String>,

    /// Listen port for the OpenAI-compatible API
    #[arg(long, env = "TETOS_PORT")]
    pub port: Option<u16>,

    /// Path to `utautts-server`; implies `--backend server` unless another backend is chosen explicitly
    #[arg(long, value_name = "PATH", env = "TETOS_SERVER")]
    pub utautts_server: Option<PathBuf>,

    /// Path to `utautts-cli`; implies `--backend cli` unless another backend is chosen explicitly
    #[arg(long, value_name = "PATH", env = "TETOS_CLI")]
    pub utautts_cli: Option<PathBuf>,

    /// Directory containing UTAU voicebanks
    #[arg(long, value_name = "PATH", env = "TETOS_VOICE_DIR")]
    pub voice_dir: Option<PathBuf>,

    /// Prosody model directory (repeatable)
    #[arg(long, value_name = "PATH", env = "TETOS_MODEL_DIR")]
    pub model_dir: Vec<PathBuf>,

    /// Default voice alias (diva, queen, and friends all lead to Teto)
    #[arg(long, env = "TETOS_VOICE")]
    pub voice: Option<String>,

    /// Backend implementation (wins over the path flags; default: server when a server path is known, else cli)
    #[arg(long, value_enum, env = "TETOS_BACKEND")]
    pub backend: Option<BackendKind>,

    /// Check dependencies and voicebanks, then exit
    #[arg(long)]
    pub doctor: bool,

    /// Increase log verbosity (repeatable)
    #[arg(short = 'v', long, action = clap::ArgAction::Count)]
    pub verbose: u8,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct FileConfig {
    pub server: ServerCfg,
    pub utautts: UtAutTsCfg,
    pub defaults: DefaultsCfg,
    pub voices: HashMap<String, String>,
}
impl Default for FileConfig {
    fn default() -> Self {
        Self { server: ServerCfg::default(), utautts: UtAutTsCfg::default(), defaults: DefaultsCfg::default(), voices: HashMap::new() }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ServerCfg {
    pub host: String,
    pub port: u16,
}
impl Default for ServerCfg {
    fn default() -> Self { Self { host: "127.0.0.1".into(), port: 8000 } }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct UtAutTsCfg {
    pub executable: Option<PathBuf>,
    pub cli_executable: Option<PathBuf>,
    pub voice_dir: Option<PathBuf>,
    pub model_dir: Vec<PathBuf>,
    /// Extra flags passed verbatim to `utautts-server`. Escape hatch for upstream updates.
    pub extra_args: Vec<String>,
    /// 0 = pick a free loopback port automatically
    pub port: u16,
    pub startup_timeout_secs: u64,
}
impl Default for UtAutTsCfg {
    fn default() -> Self {
        Self {
            executable: None,
            cli_executable: None,
            voice_dir: None,
            model_dir: Vec::new(),
            extra_args: Vec::new(),
            port: 0,
            startup_timeout_secs: 90,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct DefaultsCfg {
    pub voice: String,
    pub language: String,
    pub method: String,
    pub model_id: Option<String>,
    pub intonation_strength: f32,
    pub response_format: String,
}
impl Default for DefaultsCfg {
    fn default() -> Self {
        Self {
            voice: "teto".into(),
            language: "ja".into(),
            method: "waveform-synthesis".into(),
            model_id: None,
            intonation_strength: 4.0,
            response_format: "mp3".into(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    pub host: String,
    pub port: u16,
    pub executable: Option<PathBuf>,
    pub cli_executable: Option<PathBuf>,
    pub voice_dir: Option<PathBuf>,
    pub model_dir: Vec<PathBuf>,
    pub extra_args: Vec<String>,
    pub backend_port: u16,
    pub startup_timeout: Duration,
    pub backend_kind: BackendKind,
    pub default_voice: String,
    pub default_language: String,
    pub default_method: String,
    pub default_model_id: Option<String>,
    pub default_intonation: f32,
    pub default_format: String,
    pub voice_aliases: HashMap<String, String>,
}

impl Config {
    pub fn load(cli: &Cli) -> Result<Self> {
        let path = cli.config.clone().or_else(|| {
            let candidate = PathBuf::from("TeToS.toml");
            candidate.exists().then_some(candidate)
        });

        let file = match path {
            Some(p) => {
                let text = std::fs::read_to_string(&p)
                    .with_context(|| format!("reading config {}", p.display()))?;
                toml::from_str::<FileConfig>(&text)
                    .with_context(|| format!("parsing config {}", p.display()))?
            }
            None => FileConfig::default(),
        };

        let mut voice_aliases = file.voices;
        for (alias, target) in [
            ("teto", "teto"),
            ("kasane-teto", "teto"),
            ("kasane_teto", "teto"),
            ("kasane teto", "teto"),
            ("diva", "teto"),
            ("queen", "teto"),
            ("poteto", "teto"),
            ("potato", "teto"),
            ("haircut", "teto"),
            ("memorycard", "teto"),
            ("alloy", "teto"),
            ("nova", "teto"),
            ("shimmer", "teto"),
            ("echo", "teto"),
            ("onyx", "teto"),
            ("fable", "teto"),
            ("coral", "teto"),
            ("sage", "teto"),
            ("ash", "teto"),
        ] {
            voice_aliases.entry(alias.into()).or_insert_with(|| target.into());
        }

        // Infer voice/ model directories from wherever the server binary was
        // found (explicit, configured, or auto-detected) so zero-config works.
        let detected = crate::compat::find_executables(None, None).server;
        let inference_exe = cli.utautts_server.as_ref()
            .or(file.utautts.executable.as_ref())
            .or(detected.as_ref());
        let inferred_voice_dir = infer_sibling(inference_exe.map(|p| p.as_path()), "voice");
        let inferred_model_dir = infer_sibling(inference_exe.map(|p| p.as_path()), "models");

        let mut model_dir = file.utautts.model_dir;
        model_dir.extend(cli.model_dir.iter().cloned());
        if model_dir.is_empty() { model_dir.extend(inferred_model_dir); }

        // An explicit --backend wins. Otherwise both paths given means guessing,
        // so prefer the server and say so — silently ignoring one flag is worse.
        let backend_kind = match cli.backend {
            Some(kind) => kind,
            None if cli.utautts_cli.is_some() && cli.utautts_server.is_some() => {
                tracing::warn!(
                    "both --utautts-server and --utautts-cli were given without --backend; using the server backend (pass --backend cli to change this)"
                );
                BackendKind::Server
            }
            None if cli.utautts_cli.is_some() => BackendKind::Cli,
            None if cli.utautts_server.is_some() || file.utautts.executable.is_some() => BackendKind::Server,
            None => BackendKind::default(),
        };

        let default_voice = cli.voice.clone().unwrap_or(file.defaults.voice);
        if default_voice.trim().is_empty() {
            bail!("default voice must not be empty (the diva refuses to be nameless)");
        }

        Ok(Self {
            host: cli.host.clone().unwrap_or(file.server.host),
            port: cli.port.unwrap_or(file.server.port),
            executable: cli.utautts_server.clone().or(file.utautts.executable),
            cli_executable: cli.utautts_cli.clone().or(file.utautts.cli_executable),
            voice_dir: cli.voice_dir.clone().or(file.utautts.voice_dir).or(inferred_voice_dir),
            model_dir,
            extra_args: file.utautts.extra_args,
            backend_port: file.utautts.port,
            startup_timeout: Duration::from_secs(file.utautts.startup_timeout_secs.max(1)),
            backend_kind,
            default_voice,
            default_language: file.defaults.language,
            default_method: file.defaults.method,
            default_model_id: file.defaults.model_id,
            default_intonation: file.defaults.intonation_strength,
            default_format: file.defaults.response_format,
            voice_aliases,
        })
    }
}

/// If the server binary lives in a checkout like `../UtauTTS/utautts-server`,
/// infer `../UtauTTS/voice` and `../UtauTTS/models` so zero-config works.
fn infer_sibling(exe: Option<&std::path::Path>, child: &str) -> Option<PathBuf> {
    let exe = exe?;
    let parent = exe.parent()?;
    let candidate = parent.join(child);
    candidate.is_dir().then_some(candidate)
}

pub fn env_path_exists(name: &str) -> bool {
    // Windows binaries are `name.exe`; PATH lookup here must match.
    let name = if cfg!(windows) {
        std::borrow::Cow::Owned(format!("{name}.exe"))
    } else {
        std::borrow::Cow::Borrowed(name)
    };
    env::var_os("PATH")
        .map(|paths| env::split_paths(&paths).any(|p| p.join(name.as_ref()).is_file()))
        .unwrap_or(false)
}

#[cfg(test)]
mod config_tests {
    use super::*;
    use clap::Parser;

    fn load_from(args: &[&str]) -> Config {
        // Point at a nonexistent config file so FileConfig::default() is used.
        let cli = Cli::parse_from(
            std::iter::once("teto-tss").chain(args.iter().copied())
        );
        Config::load(&cli).expect("config load should succeed")
    }

    #[test]
    fn explicit_backend_wins() {
        // Explicit --backend cli wins even when --utautts-server is also given.
        let cfg = load_from(&["--backend", "cli", "--utautts-server", "/bin/true"]);
        assert_eq!(cfg.backend_kind, BackendKind::Cli);

        // Explicit --backend server wins even when --utautts-cli is also given.
        let cfg = load_from(&["--backend", "server", "--utautts-cli", "/bin/true"]);
        assert_eq!(cfg.backend_kind, BackendKind::Server);
    }

    #[test]
    fn both_paths_warn_prefers_server() {
        // No explicit --backend; both --utautts-server and --utautts-cli given.
        // Must pick Server (and emit a warning, which we don't assert on but
        // the code path must not panic).
        let cfg = load_from(&[
            "--utautts-server", "/bin/true",
            "--utautts-cli", "/bin/true",
        ]);
        assert_eq!(cfg.backend_kind, BackendKind::Server);
    }

    #[test]
    fn server_only_picks_server() {
        let cfg = load_from(&["--utautts-server", "/bin/true"]);
        assert_eq!(cfg.backend_kind, BackendKind::Server);
    }

    #[test]
    fn cli_only_picks_cli() {
        let cfg = load_from(&["--utautts-cli", "/bin/true"]);
        assert_eq!(cfg.backend_kind, BackendKind::Cli);
    }
}
