//! Live transcription over Mistral's realtime WebSocket.
//!
//! Protocol, as the official SDK implements it:
//!
//! ```text
//! wss://api.mistral.ai/v1/audio/transcriptions/realtime?model=…   (Bearer auth)
//! <-  {"type":"session.created", "session":{…}}
//! ->  {"type":"session.update", "session":{"audio_format":{"encoding":"pcm_s16le","sample_rate":16000}}}
//! ->  {"type":"input_audio.append", "audio":"<base64 pcm>"}   (repeated)
//! ->  {"type":"input_audio.end"}
//! <-  {"type":"transcription.text.delta", "text":"…"}          (repeated)
//! <-  {"type":"transcription.done", "text":"…"}
//! <-  {"type":"error", "error":{"code":…, "message":…}}
//! ```
//!
//! One thread owns the socket. It drains the audio channel, sends what it
//! finds, then reads with a short timeout so neither side starves the other.

use std::io::ErrorKind;
use std::net::TcpStream;
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use base64::Engine;
use serde_json::{Value, json};
use tungstenite::client::IntoClientRequest;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};

use crate::audio::SAMPLE_RATE;
use crate::config;
use crate::providers::normalize_base_url;

#[derive(Debug)]
pub enum Event {
    /// More text, to be appended to what came before.
    Delta(String),
    /// The final transcript. The session is over.
    Done(String),
    /// Something failed. The session is over.
    Error(String),
    /// The server closed the socket without a final transcript.
    Closed,
}

pub struct Session {
    pub events: Receiver<Event>,
}

/// Opens the session in a background thread. Audio arrives on `audio`; when
/// the sender side is dropped, the session tells the server the audio ended
/// and waits for the final transcript.
pub fn start(provider: &config::Provider, api_key: &str, audio: Receiver<Vec<u8>>) -> Session {
    let (tx, rx) = channel();
    let provider = provider.clone();
    let api_key = api_key.trim().to_string();
    std::thread::Builder::new()
        .name("abla-live".into())
        .spawn(move || {
            if let Err(err) = run(&provider, &api_key, audio, &tx) {
                let _ = tx.send(Event::Error(format!("{err:#}")));
            }
        })
        .expect("could not start the live transcription thread");
    Session { events: rx }
}

fn run(provider: &config::Provider, api_key: &str, audio: Receiver<Vec<u8>>, tx: &Sender<Event>) -> Result<()> {
    let base = normalize_base_url(&provider.base_url)?;
    let ws_base = base.replacen("https://", "wss://", 1).replacen("http://", "ws://", 1);
    let url = format!("{ws_base}/audio/transcriptions/realtime?model={}", provider.model);
    let mut request = url.into_client_request()?;
    request
        .headers_mut()
        .insert("Authorization", format!("Bearer {api_key}").parse()?);
    let (mut ws, _) = tungstenite::connect(request).context("could not open the live session")?;
    set_read_timeout(&mut ws, Duration::from_millis(30));

    wait_for_session(&mut ws)?;
    let update = json!({
        "type": "session.update",
        "session": {"audio_format": {"encoding": "pcm_s16le", "sample_rate": SAMPLE_RATE}}
    });
    ws.send(Message::Text(update.to_string().into()))?;

    let mut ended_at: Option<Instant> = None;
    loop {
        if ended_at.is_none() {
            loop {
                match audio.try_recv() {
                    Ok(chunk) => {
                        let payload = json!({
                            "type": "input_audio.append",
                            "audio": base64::engine::general_purpose::STANDARD.encode(&chunk),
                        });
                        ws.send(Message::Text(payload.to_string().into()))?;
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        ws.send(Message::Text(json!({"type": "input_audio.end"}).to_string().into()))?;
                        ended_at = Some(Instant::now());
                        break;
                    }
                }
            }
        }
        match ws.read() {
            Ok(Message::Text(text)) => {
                let value: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
                match value.get("type").and_then(Value::as_str).unwrap_or("") {
                    "transcription.text.delta" => {
                        if let Some(t) = value.get("text").and_then(Value::as_str) {
                            let _ = tx.send(Event::Delta(t.to_string()));
                        }
                    }
                    "transcription.done" => {
                        let t = value.get("text").and_then(Value::as_str).unwrap_or("").trim().to_string();
                        let _ = tx.send(Event::Done(t));
                        let _ = ws.close(None);
                        return Ok(());
                    }
                    "error" => {
                        return Err(anyhow!("{}", error_text(&value)));
                    }
                    _ => {}
                }
            }
            Ok(Message::Close(_)) => {
                let _ = tx.send(Event::Closed);
                return Ok(());
            }
            Ok(_) => {}
            Err(tungstenite::Error::Io(e)) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
            Err(tungstenite::Error::ConnectionClosed) | Err(tungstenite::Error::AlreadyClosed) => {
                let _ = tx.send(Event::Closed);
                return Ok(());
            }
            Err(e) => return Err(e).context("the live session dropped"),
        }
        if let Some(t) = ended_at {
            if t.elapsed() > Duration::from_secs(25) {
                let _ = tx.send(Event::Closed);
                return Ok(());
            }
        }
    }
}

fn wait_for_session(ws: &mut WebSocket<MaybeTlsStream<TcpStream>>) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        match ws.read() {
            Ok(Message::Text(text)) => {
                let value: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
                match value.get("type").and_then(Value::as_str).unwrap_or("") {
                    "session.created" => return Ok(()),
                    "error" => return Err(anyhow!("{}", error_text(&value))),
                    _ => {}
                }
            }
            Ok(Message::Close(_)) => return Err(anyhow!("the server closed the live session")),
            Ok(_) => {}
            Err(tungstenite::Error::Io(e)) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
            Err(e) => return Err(e).context("the live session failed to start"),
        }
        if Instant::now() > deadline {
            return Err(anyhow!("the live session did not start in time"));
        }
    }
}

fn set_read_timeout(ws: &mut WebSocket<MaybeTlsStream<TcpStream>>, timeout: Duration) {
    match ws.get_mut() {
        MaybeTlsStream::Plain(s) => {
            let _ = s.set_read_timeout(Some(timeout));
        }
        MaybeTlsStream::Rustls(s) => {
            let _ = s.get_mut().set_read_timeout(Some(timeout));
        }
        _ => {}
    }
}

fn error_text(value: &Value) -> String {
    match value.pointer("/error/message") {
        Some(Value::String(s)) => s.clone(),
        Some(other) if !other.is_null() => other.to_string(),
        _ => value
            .pointer("/error")
            .map(Value::to_string)
            .unwrap_or_else(|| "unknown error".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_messages_may_be_strings_or_objects() {
        assert_eq!(error_text(&json!({"error": {"message": "Unauthorized"}})), "Unauthorized");
        let nested = json!({"error": {"message": {"detail": [{"msg": "bad"}]}}});
        assert!(error_text(&nested).contains("bad"));
        assert_eq!(error_text(&json!({})), "unknown error");
    }
}
