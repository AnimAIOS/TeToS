//! OpenAI speech request -> UTAU synthesis request translation.
//! The diva does the talking; this module does the paperwork.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum VoiceRef {
    Id(String),
    Object { id: String },
}
impl VoiceRef {
    pub fn id(&self) -> &str {
        match self { Self::Id(s) => s, Self::Object { id } => id }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct DictionaryEntry {
    pub surface: String,
    pub reading: String,
}

/// Deep tuning, namespaced so strict OpenAI clients never learn the diva is an UTAU.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct TetoExtras {
    pub voicebank: Option<String>,
    pub language: Option<String>,
    pub method: Option<String>,
    pub model_id: Option<String>,
    pub reading: Option<String>,
    pub intonation_strength: Option<f32>,
    pub mora_duration_ms: Option<f32>,
    pub pause_duration_ms: Option<f32>,
    pub apply_pitch: Option<bool>,
    pub tone: Option<String>,
    pub color: Option<String>,
    pub phonemizer: Option<String>,
    pub dictionary: Option<Vec<DictionaryEntry>>,
    pub settings: Option<serde_json::Value>,
}

/// OpenAI `POST /v1/audio/speech`, plus optional TeToS extensions.
#[derive(Debug, Clone, Deserialize)]
pub struct SpeechRequest {
    /// Accepted for compatibility; used to select a prosody model when it matches one.
    pub model: Option<String>,
    pub input: String,
    pub voice: Option<VoiceRef>,
    pub response_format: Option<String>,
    pub speed: Option<f32>,
    /// Accepted and politely ignored. The diva does not take acting notes.
    pub instructions: Option<String>,
    pub stream_format: Option<String>,
    pub teto: Option<TetoExtras>,

    // Convenience top-level aliases for callers that are not strictly OpenAI clients.
    pub language: Option<String>,
    pub reading: Option<String>,
    pub method: Option<String>,
    pub model_id: Option<String>,
    pub intonation_strength: Option<f32>,
}

/// Fields here use UTAUTTS's exact JSON names. `None` fields are never sent,
/// which keeps us compatible with its `additionalProperties: false` validation.
#[derive(Debug, Clone, Serialize)]
pub struct UtauRequest {
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reading: Option<String>,
    pub language: String,
    pub method: String,
    pub voicebank_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phonemizer: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tone: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dictionary: Option<Vec<DictionaryEntry>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mora_duration_ms: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pause_duration_ms: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub intonation_strength: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub apply_pitch: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub settings: Option<serde_json::Value>,
}

impl UtauRequest {
    /// The CLI takes the voicebank from `--voicebank`, not from JSON.
    pub fn for_cli(&self) -> Self {
        let mut copy = self.clone();
        copy.voicebank_id.clear();
        copy
    }
}

/// OpenAI speeds are 0.25..=4.0. UTAU wants milliseconds per mora.
/// Fast diva, small numbers.
pub fn speed_to_timing(speed: f32) -> (f32, f32) {
    let speed = speed.clamp(0.25, 4.0);
    ((120.0 / speed).clamp(20.0, 1000.0), (180.0 / speed).clamp(0.0, 3000.0))
}

/// Split long agent output into UTAUTTS's 500-character chunks at sentence boundaries.
/// The LLM never stops for breath; the diva does.
pub fn chunk_text(text: &str, max_chars: usize) -> Vec<String> {
    let max = max_chars.max(1);
    let mut out = Vec::new();
    let mut current = String::new();
    for ch in text.chars() {
        current.push(ch);
        let boundary = matches!(ch, '。' | '！' | '？' | '!' | '?' | '；' | ';' | '\n');
        if boundary || current.chars().count() >= max {
            if !current.trim().is_empty() {
                out.push(current.trim().to_string());
            }
            current.clear();
        }
    }
    if !current.trim().is_empty() {
        out.push(current.trim().to_string());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timing_mapping() {
        assert_eq!(speed_to_timing(1.0), (120.0, 180.0));
        assert_eq!(speed_to_timing(2.0), (60.0, 90.0));
        let (slow, _) = speed_to_timing(0.25);
        assert_eq!(slow, 480.0);
    }

    #[test]
    fn chunking_respects_limit() {
        let text = "a".repeat(1200);
        let chunks = chunk_text(&text, 500);
        assert_eq!(chunks.len(), 3);
        assert!(chunks.iter().all(|c| c.chars().count() <= 500));
        let sentences = "一つ。二つ！三つ？".to_string();
        assert_eq!(chunk_text(&sentences, 500), vec!["一つ。", "二つ！", "三つ？"]);
    }
}
