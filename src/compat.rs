//! Compatibility layer for the external UtauTTS project.
//!
//! Nothing in here patches or imports UtauTTS source. We only talk to its
//! published binary/HTTP surface and parse responses *leniently*: unknown
//! fields are ignored, missing fields get defaults. When upstream adds fields
//! or bumps its version, TeToS keeps working. We never patch upstream — the
//! diva's haircut is upstream's business.

use std::{env, fs, path::{Path, PathBuf}};

use serde::{Deserialize, Serialize};
use tracing::{info, warn};

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Health {
    pub status: String,
    pub version: String,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct Voicebank {
    pub id: String,
    pub name: String,
    pub path: String,
    #[serde(rename = "diagnostic_count")]
    pub diagnostics: usize,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct ModelInfo {
    pub id: String,
    pub display_name: String,
    pub language: String,
}

#[derive(Debug, Clone, Default)]
pub struct Catalog {
    pub voicebanks: Vec<Voicebank>,
    pub models: Vec<ModelInfo>,
}

pub struct FindResult {
    pub server: Option<PathBuf>,
    pub cli: Option<PathBuf>,
}

/// Look for the UtauTTS executables without assuming an install layout.
/// The diva hides well; check PATH, the usual checkout, and then give up gracefully.
pub fn find_executables(explicit_server: Option<&Path>, explicit_cli: Option<&Path>) -> FindResult {
    let server = explicit_server.and_then(executable_or_none)
        .or_else(|| search(&["utautts-server", "utautts-server.exe"]))
        .or_else(|| relative(&["../UtauTTS/utautts-server", "../UtauTTS/tools/utautts-server",
                               "../UtauTTS/utautts-server.exe", "../UtauTTS/tools/utautts-server.exe"]));
    let cli = explicit_cli.and_then(executable_or_none)
        .or_else(|| search(&["utautts-cli", "utautts-cli.exe"]))
        .or_else(|| relative(&["../UtauTTS/tools/utautts-cli", "../UtauTTS/utautts-cli",
                               "../UtauTTS/tools/utautts-cli.exe", "../UtauTTS/utautts-cli.exe"]));
    FindResult { server, cli }
}

fn executable_or_none(p: &Path) -> Option<PathBuf> {
    if p.is_file() { Some(p.to_path_buf()) } else { None }
}

fn search(names: &[&str]) -> Option<PathBuf> {
    let paths = env::var_os("PATH")?;
    for dir in env::split_paths(&paths) {
        for name in names {
            let candidate = dir.join(name);
            if candidate.is_file() { return Some(candidate); }
        }
    }
    None
}

/// Resolve relative candidates against the current working directory first, then
/// the executable's own directory, so auto-detection also works when TeToS is
/// launched from outside the repo root.
fn relative(candidates: &[&str]) -> Option<PathBuf> {
    if let Some(hit) = candidates.iter().map(PathBuf::from).find(|p| p.is_file()) {
        return Some(hit);
    }
    let exe_dir = env::current_exe().ok()?.parent()?.to_path_buf();
    candidates.iter().map(PathBuf::from).find_map(|p| {
        let resolved = exe_dir.join(p);
        resolved.is_file().then_some(resolved)
    })
}

/// Report the upstream version but never reject a future one: TeToS only
/// depends on a small, stable slice of the API.
pub fn report_version(health: &Health) {
    if health.version.is_empty() {
        info!("UtauTTS did not report a version; assuming a compatible build");
        return;
    }
    match parse_version(&health.version) {
        Some((major, minor, patch)) => {
            info!("UtauTTS version {major}.{minor}.{patch} (status: {})", health.status);
            if major == 0 {
                warn!("UtauTTS is pre-1.0 and may change its API; TeToS isolates this in src/compat.rs (haircut subject to change without notice)");
            }
        }
        None => warn!("unrecognised UtauTTS version string: {}", health.version),
    }
}

fn parse_version(raw: &str) -> Option<(u32, u32, u32)> {
    let trimmed = raw.trim().trim_start_matches('v');
    let core = trimmed.split(['-', '+']).next()?;
    let mut parts = core.split('.');
    Some((parts.next()?.parse().ok()?, parts.next()?.parse().ok()?, parts.next().and_then(|p| p.parse().ok()).unwrap_or(0)))
}

/// Directory scan fallback: any subdirectory with an `oto.ini` is a voicebank.
/// This works regardless of upstream's catalog JSON shape.
pub fn scan_voice_dir(dir: Option<&Path>) -> Vec<Voicebank> {
    let Some(dir) = dir.filter(|d| d.is_dir()) else { return Vec::new(); };
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else { return out; };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() { continue; }
        // A bank may keep its samples in subbanks (CV / VCV / extra). In that
        // case the *parent* folder is still the bank UtauTTS recognizes.
        let has_oto = path.join("oto.ini").is_file()
            || fs::read_dir(&path).ok().and_then(|mut inner| {
                inner.find(|e| e.as_ref().ok().map(|e| e.file_name() == "oto.ini").unwrap_or(false))
            }).is_some()
            || fs::read_dir(&path).ok().and_then(|mut inner| {
                inner.find(|e| e.as_ref().ok()
                    .map(|e| e.path().join("oto.ini").is_file())
                    .unwrap_or(false))
            }).is_some();
        if !has_oto { continue; }
        let id = path.file_name().and_then(|n| n.to_str()).unwrap_or("unknown").to_string();
        out.push(Voicebank { id: id.clone(), name: id, path: path.to_string_lossy().into_owned(), ..Voicebank::default() });
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_strings() {
        assert_eq!(parse_version("v1.7.3"), Some((1, 7, 3)));
        assert_eq!(parse_version("1.7.3+build"), Some((1, 7, 3)));
        assert_eq!(parse_version("v2.0"), Some((2, 0, 0)));
        assert_eq!(parse_version("nonsense"), None);
    }

    #[test]
    fn relative_resolves_against_current_dir() {
        // Create a file in the current working directory and verify `relative`
        // finds it via the first (cwd-relative) branch.
        let unique = format!(".teto_rel_test_{}", std::process::id());
        let tmp = PathBuf::from(&unique);
        std::fs::write(&tmp, b"hit").unwrap();
        let result = relative(&[&unique]);
        let _ = std::fs::remove_file(&tmp);
        assert_eq!(result.as_deref(), Some(tmp.as_path()));
    }

    #[test]
    fn relative_resolves_against_exe_dir() {
        // Place a file next to the test binary (exe dir), then reference it by a
        // name that does NOT exist relative to cwd. This exercises the second
        // branch: exe_dir.join(candidate).
        let exe_dir = env::current_exe().unwrap().parent().unwrap().to_path_buf();
        let unique = format!("teto_rel_exe_{}", std::process::id());
        let file_in_exe_dir = exe_dir.join(&unique);
        std::fs::write(&file_in_exe_dir, b"hit").unwrap();

        // Use a name that won't resolve from cwd (the file only exists in
        // exe_dir). Prefix with "./" so it's treated as relative.
        let candidate = format!("./{}", unique);
        let result = relative(&[&candidate]);
        let _ = std::fs::remove_file(&file_in_exe_dir);

        // The function returns exe_dir joined with the candidate path.
        // We assert the file was found (result is Some).
        assert!(result.is_some(), "expected relative() to resolve via exe dir");
    }

    #[test]
    fn relative_returns_none_when_missing() {
        assert!(relative(&["nonexistent_teto_file_xyz"]).is_none());
    }
}

