//! Minimal, dependency-light WAV handling and OpenAI-compatible output formats.
//! The diva's voice is stitched together one sample at a time — that is a feature.

use std::io::Cursor;

use axum::http::StatusCode;
use hound::{SampleFormat, WavReader, WavSpec, WavWriter};
use tokio::{io::AsyncWriteExt, process::Command, task::spawn_blocking};

use crate::error::{ApiError, ApiResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    Mp3,
    Wav,
    Pcm,
    Flac,
    Opus,
    Aac,
}

impl OutputFormat {
    pub fn parse(raw: &str) -> ApiResult<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "mp3" => Ok(Self::Mp3),
            "wav" => Ok(Self::Wav),
            "pcm" => Ok(Self::Pcm),
            "flac" => Ok(Self::Flac),
            "opus" => Ok(Self::Opus),
            "aac" => Ok(Self::Aac),
            other => Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                format!("unsupported response_format '{other}' (supported: mp3, wav, pcm, flac, opus, aac)"),
            )),
        }
    }

    pub fn mime(&self) -> &'static str {
        match self {
            Self::Mp3 => "audio/mpeg",
            Self::Wav => "audio/wav",
            Self::Pcm => "audio/pcm",
            Self::Flac => "audio/flac",
            Self::Opus => "audio/ogg",
            Self::Aac => "audio/aac",
        }
    }
}

pub async fn concat_wav(chunks: Vec<Vec<u8>>) -> ApiResult<Vec<u8>> {
    if chunks.len() == 1 {
        return Ok(chunks.into_iter().next().unwrap());
    }
    spawn_blocking(move || concat_wav_blocking(&chunks))
        .await
        .map_err(|_| ApiError::internal("audio worker panicked"))?
}

fn concat_wav_blocking(chunks: &[Vec<u8>]) -> ApiResult<Vec<u8>> {
    if chunks.is_empty() {
        return Err(ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "synthesis_error", "no audio produced (the diva is mysteriously silent)"));
    }
    let first = WavReader::new(Cursor::new(&chunks[0]))
        .map_err(|e| ApiError::internal(format!("invalid WAV from UtauTTS: {e}")))?;
    let src = first.spec();
    let spec = WavSpec {
        channels: src.channels,
        sample_rate: src.sample_rate,
        bits_per_sample: 16,
        sample_format: SampleFormat::Int,
    };

    let mut cursor = Cursor::new(Vec::new());
    let mut writer = WavWriter::new(&mut cursor, spec)
        .map_err(|e| ApiError::internal(format!("cannot create WAV buffer: {e}")))?;

    for chunk in chunks {
        let mut reader = WavReader::new(Cursor::new(chunk))
            .map_err(|e| ApiError::internal(format!("invalid WAV chunk: {e}")))?;
        let format = reader.spec().sample_format;
        for sample in samples_i16(&mut reader, format)
            .map_err(|e| ApiError::internal(format!("cannot read WAV samples: {e}")))?
        {
            writer.write_sample(sample)
                .map_err(|e| ApiError::internal(format!("cannot write WAV samples: {e}")))?;
        }
    }
    writer.finalize()
        .map_err(|e| ApiError::internal(format!("cannot finalize WAV: {e}")))?;
    Ok(cursor.into_inner())
}

pub async fn transform(wav: Vec<u8>, format: OutputFormat) -> ApiResult<Vec<u8>> {
    match format {
        OutputFormat::Wav => Ok(wav),
        OutputFormat::Pcm => spawn_blocking(move || raw_pcm(&wav))
            .await
            .map_err(|_| ApiError::internal("audio worker panicked"))?,
        OutputFormat::Mp3 => spawn_blocking(move || encode_mp3(&wav))
            .await
            .map_err(|_| ApiError::internal("audio worker panicked"))?,
        OutputFormat::Flac | OutputFormat::Opus | OutputFormat::Aac => ffmpeg(&wav, format).await,
    }
}

fn samples_i16<R: std::io::Read>(
    reader: &mut WavReader<R>,
    format: SampleFormat,
) -> std::result::Result<Vec<i16>, hound::Error> {
    match format {
        SampleFormat::Int => reader.samples::<i16>().collect(),
        SampleFormat::Float => reader
            .samples::<f32>()
            .map(|sample| sample.map(|v| (v.clamp(-1.0, 1.0) * 32767.0) as i16))
            .collect(),
    }
}

fn raw_pcm(wav: &[u8]) -> ApiResult<Vec<u8>> {
    let mut reader = WavReader::new(Cursor::new(wav))
        .map_err(|e| ApiError::internal(format!("invalid WAV: {e}")))?;
    let format = reader.spec().sample_format;
    let samples = samples_i16(&mut reader, format)
        .map_err(|e| ApiError::internal(format!("cannot read WAV samples: {e}")))?;
    let mut out = Vec::with_capacity(samples.len() * 2);
    for sample in samples {
        out.extend_from_slice(&sample.to_le_bytes());
    }
    Ok(out)
}

fn encode_mp3(wav: &[u8]) -> ApiResult<Vec<u8>> {
    use mp3lame_encoder::{Bitrate, Builder, FlushNoGap, InterleavedPcm, Quality};

    let mut reader = WavReader::new(Cursor::new(wav))
        .map_err(|e| ApiError::internal(format!("invalid WAV: {e}")))?;
    let spec = reader.spec();
    let format = spec.sample_format;
    let samples = samples_i16(&mut reader, format)
        .map_err(|e| ApiError::internal(format!("cannot read WAV samples: {e}")))?;

    let mut builder = Builder::new()
        .ok_or_else(|| ApiError::internal("LAME encoder could not be initialized"))?;
    builder.set_num_channels(spec.channels as u8)
        .map_err(|e| ApiError::internal(format!("LAME channel error: {e}")))?;
    builder.set_sample_rate(spec.sample_rate)
        .map_err(|e| ApiError::internal(format!("LAME sample-rate error: {e}. Use response_format=wav for unusual rates")))?;
    builder.set_brate(Bitrate::Kbps192)
        .map_err(|e| ApiError::internal(format!("LAME bitrate error: {e}")))?;
    builder.set_quality(Quality::Best)
        .map_err(|e| ApiError::internal(format!("LAME quality error: {e}")))?;

    let mut encoder = builder.build()
        .map_err(|e| ApiError::internal(format!("LAME build error: {e}")))?;
    let mut out = Vec::with_capacity(mp3lame_encoder::max_required_buffer_size(samples.len()));

    if spec.channels == 1 {
        encoder.encode_to_vec(mp3lame_encoder::MonoPcm(&samples), &mut out)
            .map_err(|e| ApiError::internal(format!("MP3 encoding failed: {e}")))?;
    } else {
        encoder.encode_to_vec(InterleavedPcm(&samples), &mut out)
            .map_err(|e| ApiError::internal(format!("MP3 encoding failed: {e}")))?;
    }
    encoder.flush_to_vec::<FlushNoGap>(&mut out)
        .map_err(|e| ApiError::internal(format!("MP3 flush failed: {e}")))?;
    Ok(out)
}

async fn ffmpeg(wav: &[u8], format: OutputFormat) -> ApiResult<Vec<u8>> {
    let (codec, container) = match format {
        OutputFormat::Flac => (vec!["-c:a", "flac"], "flac"),
        OutputFormat::Opus => (vec!["-c:a", "libopus"], "ogg"),
        OutputFormat::Aac => (vec!["-c:a", "aac"], "adts"),
        _ => unreachable!(),
    };

    let mut child = Command::new("ffmpeg")
        .arg("-hide_banner")
        .arg("-loglevel").arg("error")
        .arg("-f").arg("wav")
        .arg("-i").arg("pipe:0")
        .args(codec)
        .arg("-f").arg(container)
        .arg("pipe:1")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| ApiError::new(
            StatusCode::NOT_IMPLEMENTED,
            "missing_dependency",
            format!("response_format={format:?} needs ffmpeg on PATH ({e}). Use mp3, wav, or pcm instead."),
        ))?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(wav)
            .await
            .map_err(|e| ApiError::internal(format!("failed to pipe audio into ffmpeg: {e}")))?;
    }
    let output = child.wait_with_output()
        .await
        .map_err(|e| ApiError::internal(format!("ffmpeg failed: {e}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "transcode_error",
            format!("ffmpeg exited with {}: {stderr}", output.status),
        ));
    }
    Ok(output.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mp3_from_fixture() {
        let Ok(wav) = std::fs::read("/tmp/f.wav") else { return };
        let out = encode_mp3(&wav).expect("mp3 encoding");
        assert!(!out.is_empty());
    }
}
