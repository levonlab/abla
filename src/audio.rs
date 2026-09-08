//! Microphone capture through PipeWire's `pw-record`, as 16 kHz mono s16le.
//!
//! The recorder keeps every byte in memory (for the batch upload), reports a
//! peak level for the animation, and can forward chunks to a live session.

use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use anyhow::{Context, Result};

pub const SAMPLE_RATE: u32 = 16_000;
pub const BYTES_PER_SECOND: usize = SAMPLE_RATE as usize * 2;

pub struct Recorder {
    child: Child,
    pcm: Arc<Mutex<Vec<u8>>>,
    level: Arc<AtomicU32>,
    reader: Option<JoinHandle<()>>,
}

impl Recorder {
    /// Starts capturing. When `live` is given, every chunk read is also sent
    /// there, in arrival order.
    pub fn start(live: Option<Sender<Vec<u8>>>) -> Result<Recorder> {
        let mut child = Command::new("pw-record")
            .args(["--rate", "16000", "--channels", "1", "--format", "s16", "-"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .context("pw-record is not installed (it comes with pipewire-utils)")?;
        let mut stdout = child.stdout.take().context("pw-record has no stdout")?;
        let pcm = Arc::new(Mutex::new(Vec::with_capacity(BYTES_PER_SECOND * 60)));
        let level = Arc::new(AtomicU32::new(0));
        let reader = {
            let pcm = Arc::clone(&pcm);
            let level = Arc::clone(&level);
            std::thread::Builder::new()
                .name("abla-mic".into())
                .spawn(move || {
                    let mut buf = vec![0u8; 3200];
                    loop {
                        let n = match stdout.read(&mut buf) {
                            Ok(0) | Err(_) => break,
                            Ok(n) => n,
                        };
                        let chunk = &buf[..n];
                        level.store(peak(chunk).to_bits(), Ordering::Relaxed);
                        pcm.lock().unwrap().extend_from_slice(chunk);
                        if let Some(tx) = &live {
                            let _ = tx.send(chunk.to_vec());
                        }
                    }
                })
                .context("could not start the capture thread")?
        };
        Ok(Recorder {
            child,
            pcm,
            level,
            reader: Some(reader),
        })
    }

    /// Peak amplitude of the most recent chunk, 0..1.
    pub fn level(&self) -> f32 {
        f32::from_bits(self.level.load(Ordering::Relaxed))
    }

    /// Stops the capture and returns everything recorded.
    pub fn stop(mut self) -> Vec<u8> {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
        std::mem::take(&mut *self.pcm.lock().unwrap())
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Peak of an s16le chunk, scaled so normal speech reaches the top.
pub fn peak(chunk: &[u8]) -> f32 {
    let max = chunk
        .chunks_exact(2)
        .map(|s| i16::from_le_bytes([s[0], s[1]]).unsigned_abs())
        .max()
        .unwrap_or(0);
    (max as f32 / 32768.0 * 2.2).min(1.0)
}

/// Wraps raw s16le mono samples in a WAV container.
pub fn wav(pcm: &[u8], rate: u32) -> Vec<u8> {
    let data_len = pcm.len() as u32;
    let mut out = Vec::with_capacity(44 + pcm.len());
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    out.extend_from_slice(pcm);
    out
}

/// Strips a WAV header if there is one, so a file can be fed where raw PCM
/// is expected. Only 16 kHz mono s16le is accepted.
pub fn pcm_from_wav(bytes: &[u8]) -> Result<Vec<u8>> {
    if bytes.len() < 44 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        anyhow::bail!("not a WAV file");
    }
    let channels = u16::from_le_bytes([bytes[22], bytes[23]]);
    let rate = u32::from_le_bytes([bytes[24], bytes[25], bytes[26], bytes[27]]);
    let bits = u16::from_le_bytes([bytes[34], bytes[35]]);
    if channels != 1 || rate != SAMPLE_RATE || bits != 16 {
        anyhow::bail!("expected 16 kHz mono 16-bit, got {rate} Hz, {channels} ch, {bits} bit");
    }
    let mut pos = 12;
    while pos + 8 <= bytes.len() {
        let id = &bytes[pos..pos + 4];
        let len = u32::from_le_bytes([
            bytes[pos + 4],
            bytes[pos + 5],
            bytes[pos + 6],
            bytes[pos + 7],
        ]) as usize;
        let start = pos + 8;
        if id == b"data" {
            let end = (start + len).min(bytes.len());
            return Ok(bytes[start..end].to_vec());
        }
        pos = start + len + (len & 1);
    }
    anyhow::bail!("WAV file has no data chunk")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silence_is_zero_and_full_scale_is_one() {
        assert_eq!(peak(&[0, 0, 0, 0]), 0.0);
        assert_eq!(peak(&i16::MIN.to_le_bytes()), 1.0);
        let half = (16384i16).to_le_bytes();
        assert!(
            (peak(&half) - 1.0).abs() < 1e-6,
            "half scale is boosted to the top"
        );
    }

    #[test]
    fn wav_header_is_44_bytes_and_round_trips() {
        let pcm: Vec<u8> = (0..3200u32).map(|i| (i % 251) as u8).collect();
        let file = wav(&pcm, SAMPLE_RATE);
        assert_eq!(file.len(), 44 + pcm.len());
        assert_eq!(&file[0..4], b"RIFF");
        assert_eq!(pcm_from_wav(&file).unwrap(), pcm);
    }

    #[test]
    fn rejects_other_formats() {
        let stereo = {
            let mut f = wav(&[0u8; 8], SAMPLE_RATE);
            f[22] = 2;
            f
        };
        assert!(pcm_from_wav(&stereo).is_err());
        assert!(pcm_from_wav(b"not audio").is_err());
    }
}
