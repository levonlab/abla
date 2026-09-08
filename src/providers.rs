//! Transcription providers that speak the OpenAI-style `/v1/audio/transcriptions`
//! API, and how to list the models a given key can use.
//!
//! Prices are deliberately absent: no provider exposes them through its API,
//! and what a key costs is between its owner and the provider.

use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Mistral,
    OpenAi,
    Groq,
    Custom,
}

pub const ALL: [Kind; 4] = [Kind::Mistral, Kind::OpenAi, Kind::Groq, Kind::Custom];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Model {
    pub id: String,
    /// Streams text while you speak (WebSocket), instead of after you stop.
    pub realtime: bool,
}

impl Kind {
    pub fn id(self) -> &'static str {
        match self {
            Kind::Mistral => "mistral",
            Kind::OpenAi => "openai",
            Kind::Groq => "groq",
            Kind::Custom => "custom",
        }
    }

    pub fn from_id(id: &str) -> Kind {
        ALL.into_iter()
            .find(|k| k.id() == id.trim().to_ascii_lowercase())
            .unwrap_or(Kind::Custom)
    }

    pub fn label(self) -> &'static str {
        match self {
            Kind::Mistral => "Mistral",
            Kind::OpenAi => "OpenAI",
            Kind::Groq => "Groq",
            Kind::Custom => "Custom (OpenAI-compatible)",
        }
    }

    pub fn default_base_url(self) -> &'static str {
        match self {
            Kind::Mistral => "https://api.mistral.ai/v1",
            Kind::OpenAi => "https://api.openai.com/v1",
            Kind::Groq => "https://api.groq.com/openai/v1",
            Kind::Custom => "",
        }
    }

    /// Where to create an API key.
    pub fn key_url(self) -> Option<&'static str> {
        match self {
            Kind::Mistral => Some("https://console.mistral.ai/api-keys"),
            Kind::OpenAi => Some("https://platform.openai.com/api-keys"),
            Kind::Groq => Some("https://console.groq.com/keys"),
            Kind::Custom => None,
        }
    }

    /// Decides whether a `/v1/models` entry is a transcription model, and of
    /// which kind. Returns `None` for chat, embedding, TTS and the rest.
    pub fn classify(self, entry: &Value) -> Option<Model> {
        let id = entry.get("id")?.as_str()?.to_string();
        match self {
            Kind::Mistral => {
                // Dated snapshots that a `-latest` alias points at are the
                // same model twice; only the alias is offered.
                let alias_is_latest =
                    entry
                        .get("aliases")
                        .and_then(Value::as_array)
                        .is_some_and(|a| {
                            a.iter()
                                .filter_map(Value::as_str)
                                .any(|s| s.ends_with("-latest"))
                        });
                if alias_is_latest && !id.ends_with("-latest") {
                    return None;
                }
                let caps = entry.get("capabilities")?;
                let flag = |name: &str| caps.get(name).and_then(Value::as_bool).unwrap_or(false);
                if flag("audio_transcription_realtime") {
                    Some(Model { id, realtime: true })
                } else if flag("audio_transcription") {
                    Some(Model {
                        id,
                        realtime: false,
                    })
                } else {
                    None
                }
            }
            Kind::OpenAi => {
                let lower = id.to_ascii_lowercase();
                let transcribes = lower.contains("transcribe") || lower.starts_with("whisper");
                let streams_only = lower.contains("realtime");
                (transcribes && !streams_only).then_some(Model {
                    id,
                    realtime: false,
                })
            }
            Kind::Groq => {
                let lower = id.to_ascii_lowercase();
                lower.contains("whisper").then_some(Model {
                    id,
                    realtime: false,
                })
            }
            Kind::Custom => Some(Model {
                id,
                realtime: false,
            }),
        }
    }

    pub fn sort(self, models: &mut Vec<Model>) {
        models.sort_by(|a, b| {
            let rank = |m: &Model| (m.realtime, !m.id.ends_with("-latest"), m.id.clone());
            rank(a).cmp(&rank(b))
        });
        models.dedup();
    }
}

/// Asks the provider which models this key can use, keeping only the ones
/// that transcribe audio.
pub fn list_models(kind: Kind, base_url: &str, api_key: &str) -> Result<Vec<Model>> {
    let base = normalize_base_url(base_url)?;
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(20)))
        .http_status_as_error(false)
        .build()
        .into();
    let mut response = agent
        .get(format!("{base}/models"))
        .header("Authorization", &format!("Bearer {}", api_key.trim()))
        .call()
        .context("could not reach the provider")?;
    let status = response.status().as_u16();
    let text = response
        .body_mut()
        .read_to_string()
        .context("could not read the answer")?;
    let body: Value =
        serde_json::from_str(&text).context("the provider did not answer with JSON")?;
    if status == 401 || status == 403 {
        return Err(anyhow!("the API key was rejected"));
    }
    if !(200..300).contains(&status) {
        return Err(anyhow!("{}", error_message(&body, status)));
    }
    let entries = body
        .get("data")
        .and_then(Value::as_array)
        .context("unexpected answer: no model list")?;
    let mut models: Vec<Model> = entries.iter().filter_map(|e| kind.classify(e)).collect();
    kind.sort(&mut models);
    if models.is_empty() {
        return Err(anyhow!("this key has no transcription models"));
    }
    Ok(models)
}

/// Accepts `https://host`, `https://host/`, `https://host/v1/` and returns
/// `https://host/v1`.
pub fn normalize_base_url(url: &str) -> Result<String> {
    let trimmed = url.trim().trim_end_matches('/');
    if !(trimmed.starts_with("https://") || trimmed.starts_with("http://")) {
        return Err(anyhow!("the base URL must start with https://"));
    }
    if trimmed.len() <= "https://".len() {
        return Err(anyhow!("the base URL is empty"));
    }
    Ok(if trimmed.ends_with("/v1") {
        trimmed.to_string()
    } else {
        format!("{trimmed}/v1")
    })
}

/// Pulls a human message out of the JSON error shapes providers use.
pub fn error_message(body: &Value, status: u16) -> String {
    let candidates = [
        body.pointer("/error/message"),
        body.pointer("/message"),
        body.pointer("/detail"),
        body.pointer("/error"),
    ];
    for c in candidates.into_iter().flatten() {
        match c {
            Value::String(s) if !s.is_empty() => return format!("HTTP {status}: {s}"),
            Value::Array(_) | Value::Object(_) => return format!("HTTP {status}: {c}"),
            _ => {}
        }
    }
    format!("HTTP {status}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn mistral_uses_capability_flags() {
        let batch =
            json!({"id": "voxtral-mini-latest", "capabilities": {"audio_transcription": true}});
        let live = json!({"id": "voxtral-mini-realtime-latest",
                          "capabilities": {"audio_transcription_realtime": true}});
        let chat = json!({"id": "mistral-small-latest", "capabilities": {"completion_chat": true}});
        assert!(!Kind::Mistral.classify(&batch).unwrap().realtime);
        assert!(Kind::Mistral.classify(&live).unwrap().realtime);
        assert!(Kind::Mistral.classify(&chat).is_none());
    }

    #[test]
    fn mistral_dated_copies_of_latest_are_hidden() {
        let dated = json!({"id": "voxtral-mini-2602", "aliases": ["voxtral-mini-latest"],
                           "capabilities": {"audio_transcription": true}});
        let latest = json!({"id": "voxtral-mini-latest", "aliases": ["voxtral-mini-2602"],
                            "capabilities": {"audio_transcription": true}});
        let lone = json!({"id": "voxtral-mini-2512", "aliases": [],
                          "capabilities": {"audio_transcription": true}});
        assert!(Kind::Mistral.classify(&dated).is_none());
        assert!(Kind::Mistral.classify(&latest).is_some());
        assert!(
            Kind::Mistral.classify(&lone).is_some(),
            "a snapshot with no alias is still offered"
        );
    }

    #[test]
    fn openai_and_groq_go_by_name() {
        assert!(
            Kind::OpenAi
                .classify(&json!({"id": "gpt-4o-transcribe"}))
                .is_some()
        );
        assert!(Kind::OpenAi.classify(&json!({"id": "whisper-1"})).is_some());
        assert!(
            Kind::OpenAi
                .classify(&json!({"id": "gpt-4o-realtime-preview"}))
                .is_none()
        );
        assert!(Kind::OpenAi.classify(&json!({"id": "gpt-4o"})).is_none());
        assert!(
            Kind::Groq
                .classify(&json!({"id": "whisper-large-v3-turbo"}))
                .is_some()
        );
        assert!(
            Kind::Groq
                .classify(&json!({"id": "llama-3.3-70b"}))
                .is_none()
        );
    }

    #[test]
    fn latest_aliases_come_first_and_live_last() {
        let mut models = vec![
            Model {
                id: "voxtral-mini-realtime-latest".into(),
                realtime: true,
            },
            Model {
                id: "voxtral-mini-2602".into(),
                realtime: false,
            },
            Model {
                id: "voxtral-mini-latest".into(),
                realtime: false,
            },
        ];
        Kind::Mistral.sort(&mut models);
        let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "voxtral-mini-latest",
                "voxtral-mini-2602",
                "voxtral-mini-realtime-latest"
            ]
        );
    }

    #[test]
    fn base_urls_are_normalised() {
        assert_eq!(
            normalize_base_url("https://api.mistral.ai").unwrap(),
            "https://api.mistral.ai/v1"
        );
        assert_eq!(
            normalize_base_url("https://api.mistral.ai/v1/").unwrap(),
            "https://api.mistral.ai/v1"
        );
        assert!(normalize_base_url("api.mistral.ai").is_err());
        assert!(normalize_base_url("https://").is_err());
    }

    #[test]
    fn error_messages_are_extracted() {
        assert_eq!(
            error_message(&json!({"error": {"message": "bad key"}}), 401),
            "HTTP 401: bad key"
        );
        assert_eq!(
            error_message(&json!({"message": "too big"}), 413),
            "HTTP 413: too big"
        );
        assert_eq!(error_message(&json!({}), 500), "HTTP 500");
    }
}
