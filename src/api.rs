//! OpenAI-compatible HTTP surface.

use std::{collections::HashSet, sync::Arc, time::Instant};

use axum::{
    body::Body,
    extract::State,
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::Response,
    routing::{get, post},
    Json, Router,
};
use serde_json::{Value, json};
use tokio::sync::RwLock;
use tracing::{info, warn};

use crate::{
    audio::{self, OutputFormat},
    backend::{Backend, SynthOutput},
    compat::Catalog,
    config::Config,
    error::{ApiError, ApiResult},
    protocol::{SpeechRequest, UtauRequest, chunk_text, speed_to_timing},
};

#[derive(Clone)]
pub struct AppState {
    pub backend: Arc<Backend>,
    pub config: Arc<Config>,
    pub catalog: Arc<RwLock<Catalog>>,
    pub started: Instant,
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(root))
        .route("/health", get(health))
        .route("/v1/models", get(models))
        .route("/v1/voicebanks", get(voicebanks))
        .route("/v1/audio/speech", post(speech))
        .with_state(state)
}

async fn root() -> Json<Value> {
    Json(json!({
        "service": "TeToS",
        "tagline": "Teto as a Service — the diva is the API",
        "endpoints": ["/health", "/v1/models", "/v1/voicebanks", "/v1/audio/speech"],
        "disclaimer": "No neural voice cloning. UTAU samples only. The LLM does the thinking; Teto does the talking."
    }))
}

async fn health(State(state): State<AppState>) -> Json<Value> {
    let catalog = state.catalog.read().await;
    Json(json!({
        "status": "ok",
        "backend": state.backend.name(),
        "voicebanks": catalog.voicebanks.len(),
        "models": catalog.models.len(),
        "uptime_secs": state.started.elapsed().as_secs(),
        "message": "Teto is fine. You are the one who needs rest."
    }))
}

async fn models(State(state): State<AppState>) -> Json<Value> {
    let catalog = state.catalog.read().await;
    let mut seen = HashSet::new();
    let mut data = Vec::new();
    for (id, note) in [
        ("tts-1", "TeToS alias of the default voice"),
        ("tts-1-hd", "TeToS alias of the default voice (no HD, only diva)"),
        ("gpt-4o-mini-tts", "TeToS alias; not actually GPT-4o"),
        ("teto", "Kasane Teto via UTAU"),
    ] {
        seen.insert(id.to_string());
        data.push(model_json(id, note));
    }
    for bank in &catalog.voicebanks {
        if seen.insert(bank.id.clone()) {
            data.push(model_json(&bank.id, "utau voicebank"));
        }
    }
    Json(json!({ "object": "list", "data": data }))
}

fn model_json(id: &str, owned_by: &str) -> Value {
    json!({
        "id": id,
        "object": "model",
        "created": 1_700_000_000,
        "owned_by": owned_by,
    })
}

async fn voicebanks(State(state): State<AppState>) -> Json<Value> {
    let catalog = state.catalog.read().await;
    Json(json!({ "voicebanks": catalog.voicebanks, "models": catalog.models }))
}

async fn speech(
    State(state): State<AppState>,
    request_headers: HeaderMap,
    Json(req): Json<SpeechRequest>,
) -> ApiResult<Response> {
    if req.input.trim().is_empty() {
        return Err(ApiError::bad_request("input must not be empty"));
    }
    if let Some(notes) = &req.instructions {
        if !notes.trim().is_empty() {
            info!("ignoring instructions (the diva does not take acting notes): {notes}");
        }
    }
    if req.stream_format.as_deref() == Some("sse") {
        warn!("stream_format=sse requested; returning plain audio instead");
    }
    if request_headers.contains_key(header::AUTHORIZATION) {
        info!("ignored an Authorization header: everything here is free tier");
    }

    let format = OutputFormat::parse(
        req.response_format.as_deref().unwrap_or(&state.config.default_format),
    )?;
    let speed = req.speed.unwrap_or(1.0);
    if !speed.is_finite() || !(0.25..=4.0).contains(&speed) {
        return Err(ApiError::bad_request("speed must be between 0.25 and 4.0"));
    }

    let extras = req.teto.clone().unwrap_or_default();
    let requested_voice = req.voice.as_ref().map(|v| v.id().to_string())
        .or_else(|| extras.voicebank.clone())
        .unwrap_or_else(|| state.config.default_voice.clone());

    let catalog = state.catalog.read().await;
    let voicebank = resolve_voicebank(&requested_voice, &state.config.voice_aliases, &catalog)?;

    let language = extras.language.as_deref()
        .or(req.language.as_deref())
        .unwrap_or(&state.config.default_language)
        .to_string();
    let method = extras.method.as_deref()
        .or(req.method.as_deref())
        .unwrap_or(&state.config.default_method)
        .to_string();

    let mut model_id = extras.model_id.as_deref()
        .or(req.model_id.as_deref())
        .or(req.model.as_deref())
        .map(str::to_string)
        .or_else(|| state.config.default_model_id.clone());
    if let Some(id) = &model_id {
        if !catalog.models.is_empty() && !catalog.models.iter().any(|m| &m.id == id) {
            warn!("prosody model '{id}' is not in the upstream catalog; ignoring it");
            model_id = None;
        }
    }
    drop(catalog);

    let (mora_ms, pause_ms) = speed_to_timing(speed);
    let base = UtauRequest {
        text: String::new(),
        reading: extras.reading.clone().or(req.reading.clone()),
        language,
        method,
        voicebank_id: voicebank.clone(),
        model_id,
        phonemizer: extras.phonemizer.clone(),
        tone: extras.tone.clone(),
        color: extras.color.clone(),
        dictionary: extras.dictionary.clone(),
        mora_duration_ms: extras.mora_duration_ms.or(Some(mora_ms)),
        pause_duration_ms: extras.pause_duration_ms.or(Some(pause_ms)),
        intonation_strength: extras.intonation_strength
            .or(req.intonation_strength)
            .or(Some(state.config.default_intonation)),
        apply_pitch: extras.apply_pitch,
        settings: extras.settings.clone(),
    };

    // UtauTTS caps text at 500 chars. Agent output does not care. We chunk.
    let chunks = if base.reading.is_some() {
        vec![req.input.clone()]
    } else {
        chunk_text(&req.input, 450)
    };

    let mut wavs = Vec::with_capacity(chunks.len());
    let mut meta: Option<SynthOutput> = None;
    for chunk in chunks {
        let mut one = base.clone();
        one.text = chunk;
        let output = state.backend.synthesize(&one).await?;
        if output.wav.is_empty() {
            return Err(ApiError::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "synthesis_error",
                "UtauTTS returned empty audio",
            ));
        }
        if meta.is_none() { meta = Some(output.clone()); }
        wavs.push(output.wav);
    }

    let merged = audio::concat_wav(wavs).await?;
    let bytes = audio::transform(merged, format).await?;

    let mut response = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, format.mime())
        .header(header::CONTENT_LENGTH, bytes.len())
        .body(Body::from(bytes))
        .map_err(|e| ApiError::internal(e.to_string()))?;

    let headers = response.headers_mut();
    set_header(headers, "X-TeToS-Voicebank", &voicebank);
    set_header(headers, "X-TeToS-Backend", state.backend.name());
    set_header(headers, "X-TeToS-Cost", "$0.00");
    if let Some(m) = &meta {
        if let Some(reading) = &m.reading { set_header(headers, "X-UtauTTS-Reading", reading); }
        if let Some(method) = &m.method { set_header(headers, "X-UtauTTS-Method", method); }
        set_header(headers, "X-UtauTTS-Diagnostic-Count", &m.diagnostics.to_string());
    }
    Ok(response)
}

fn set_header(headers: &mut HeaderMap, name: &'static str, value: &str) {
    if let Ok(value) = HeaderValue::from_str(value) {
        headers.insert(name, value);
    }
}

fn normalize(value: &str) -> String {
    value.to_lowercase().chars()
        .filter(|c| !matches!(c, '_' | '-' | ' '))
        .collect()
}

fn resolve_voicebank(
    requested: &str,
    aliases: &std::collections::HashMap<String, String>,
    catalog: &Catalog,
) -> ApiResult<String> {
    let mut candidates: Vec<String> = Vec::new();
    if let Some(alias) = aliases.get(&requested.to_lowercase()) {
        candidates.push(alias.clone());
    }
    candidates.push(requested.to_string());

    for candidate in &candidates {
        if catalog.voicebanks.iter().any(|v| &v.id == candidate) {
            return Ok(candidate.clone());
        }
    }
    // Japanese voicebanks are usually named 重音テト..., so romanized aliases
    // need a katakana hint too.
    if normalize(requested).contains("teto") {
        candidates.push("テト".to_string());
    }

    let mut matches: Vec<&crate::compat::Voicebank> = Vec::new();
    for candidate in &candidates {
        let needle = normalize(candidate);
        if needle.is_empty() { continue; }
        for bank in &catalog.voicebanks {
            if normalize(&bank.id).contains(&needle) || normalize(&bank.name).contains(&needle) {
                if !matches.iter().any(|m| m.id == bank.id) {
                    matches.push(bank);
                }
            }
        }
    }
    if !matches.is_empty() {
        // Prefer the top-level bank (absolute path in upstream's catalog), then
        // a CV bank, then whatever came first. This keeps aliases working as
        // upstream adds subbanks.
        matches.sort_by_key(|bank| {
            let mut score = 0_i32;
            if bank.path.starts_with('/') || bank.path.starts_with('\\') { score += 100; }
            let lowered = bank.id.to_lowercase();
            if lowered.contains("単独音") || lowered.contains("cv") { score += 50; }
            -score
        });
        return Ok(matches[0].id.clone());
    }
    if let Some(teto) = aliases.get("teto") {
        if catalog.voicebanks.iter().any(|v| &v.id == teto) {
            return Ok(teto.clone());
        }
    }
    if catalog.voicebanks.len() == 1 {
        warn!("no voicebank matched '{requested}', using the only one available");
        return Ok(catalog.voicebanks[0].id.clone());
    }

    let available: Vec<&str> = catalog.voicebanks.iter().map(|v| v.id.as_str()).collect();
    Err(ApiError::bad_request(format!(
        "unknown voice '{requested}'. available voicebanks: {}",
        if available.is_empty() { "none found".to_string() } else { available.join(", ") }
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compat::Voicebank;

    fn catalog(ids: &[&str]) -> Catalog {
        Catalog {
            voicebanks: ids.iter().map(|id| Voicebank {
                id: (*id).into(), name: (*id).into(), path: (*id).into(), ..Voicebank::default()
            }).collect(),
            models: Vec::new(),
        }
    }

    #[test]
    fn alias_and_fuzzy_voice_resolution() {
        let catalog = catalog(&["Kasane Teto V4", "Adachi Rei"]);
        let mut aliases = std::collections::HashMap::new();
        aliases.insert("diva".into(), "Kasane Teto V4".into());
        assert_eq!(resolve_voicebank("diva", &aliases, &catalog).unwrap(), "Kasane Teto V4");
        assert_eq!(resolve_voicebank("kasane_teto", &aliases, &catalog).unwrap(), "Kasane Teto V4");
        assert_eq!(resolve_voicebank("rei", &aliases, &catalog).unwrap(), "Adachi Rei");
    }

    #[test]
    fn single_voicebank_fallback() {
        let catalog = catalog(&["Solo Voice"]);
        let aliases = std::collections::HashMap::new();
        assert_eq!(resolve_voicebank("anyone", &aliases, &catalog).unwrap(), "Solo Voice");
    }
}
