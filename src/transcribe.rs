//! Batch transcription: one multipart POST to `/v1/audio/transcriptions`,
//! the same request shape Mistral, OpenAI and Groq accept.

use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use serde_json::Value;

use crate::config;
use crate::providers::{error_message, normalize_base_url};

/// Providers cap uploads around 25 MB; 16 kHz mono s16 is 1.92 MB a minute.
pub const MAX_UPLOAD_BYTES: usize = 24 * 1024 * 1024;

pub fn batch(provider: &config::Provider, api_key: &str, wav: Vec<u8>) -> Result<String> {
    if wav.len() > MAX_UPLOAD_BYTES {
        return Err(anyhow!("the recording is too long to upload in one piece"));
    }
    let base = normalize_base_url(&provider.base_url)?;
    let boundary = format!("abla{:x}{:x}", std::process::id(), nanos());
    let body = multipart(&boundary, &provider.model, &provider.language, &wav);

    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(180)))
        .http_status_as_error(false)
        .build()
        .into();
    let mut response = agent
        .post(format!("{base}/audio/transcriptions"))
        .header("Authorization", &format!("Bearer {}", api_key.trim()))
        .header(
            "Content-Type",
            &format!("multipart/form-data; boundary={boundary}"),
        )
        .send(&body[..])
        .context("could not reach the provider")?;
    let status = response.status().as_u16();
    let text = response
        .body_mut()
        .read_to_string()
        .context("could not read the answer")?;
    let json: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    match status {
        200..=299 => {}
        401 | 403 => return Err(anyhow!("the API key was rejected")),
        413 => return Err(anyhow!("the recording is too large for this provider")),
        _ => return Err(anyhow!("{}", error_message(&json, status))),
    }
    let transcript = json
        .get("text")
        .and_then(Value::as_str)
        .context("the provider answered without a text field")?
        .trim()
        .to_string();
    Ok(transcript)
}

pub fn multipart(boundary: &str, model: &str, language: &str, wav: &[u8]) -> Vec<u8> {
    let mut body = Vec::with_capacity(wav.len() + 512);
    let mut field = |name: &str, value: &str| {
        body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        body.extend_from_slice(
            format!("Content-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n")
                .as_bytes(),
        );
    };
    field("model", model);
    if !language.trim().is_empty() {
        field("language", language.trim());
    }
    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(
        b"Content-Disposition: form-data; name=\"file\"; filename=\"audio.wav\"\r\n\
          Content-Type: audio/wav\r\n\r\n",
    );
    body.extend_from_slice(wav);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    body
}

fn nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multipart_has_every_part_in_order() {
        let body = multipart("B", "voxtral-mini-latest", "es", b"RIFFdata");
        let text = String::from_utf8_lossy(&body);
        let model = text
            .find("name=\"model\"\r\n\r\nvoxtral-mini-latest")
            .unwrap();
        let lang = text.find("name=\"language\"\r\n\r\nes").unwrap();
        let file = text
            .find("filename=\"audio.wav\"\r\nContent-Type: audio/wav\r\n\r\nRIFFdata")
            .unwrap();
        assert!(model < lang && lang < file);
        assert!(text.ends_with("\r\n--B--\r\n"));
    }

    #[test]
    fn empty_language_is_left_out() {
        let body = multipart("B", "m", "  ", b"x");
        assert!(!String::from_utf8_lossy(&body).contains("language"));
    }
}
