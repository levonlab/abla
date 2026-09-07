mod audio;
mod config;
mod desktop;
mod doctor;
mod inject;
mod providers;
mod realtime;
mod transcribe;
mod ui;

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::mpsc::channel;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

use crate::config::Config;

/// Voice dictation for Linux. Click the button, talk, click again: the text
/// lands where your cursor is.
#[derive(Parser)]
#[command(name = "abla", version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Open the settings window
    Setup,
    /// Start or stop dictation in the running abla (bind this to a key)
    Toggle,
    /// Check the microphone, automatic pasting and the provider
    Doctor,
    /// Add Abla to the application menu
    Desktop,
    /// Start abla when you log in
    Autostart {
        #[arg(value_parser = ["on", "off"])]
        state: String,
    },
    /// Transcribe a 16 kHz mono WAV file with the configured provider
    Transcribe {
        file: PathBuf,
        /// Stream the file as if it were spoken live (needs a live model)
        #[arg(long)]
        live: bool,
    },
}

fn main() -> ExitCode {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    let cli = Cli::parse();
    let result = match cli.command {
        None => return exit_code(ui::run(ui::Start::Button)),
        Some(Command::Setup) => return exit_code(ui::run(ui::Start::Settings)),
        Some(Command::Toggle) => ui::send_toggle(),
        Some(Command::Doctor) => return doctor::run(),
        Some(Command::Desktop) => desktop::install().map(|paths| {
            for p in paths {
                println!("wrote {}", p.display());
            }
            println!("Abla is now in your application menu.");
        }),
        Some(Command::Autostart { state }) => {
            let on = state == "on";
            desktop::set_autostart(on).map(|_| {
                println!("Abla will {}start when you log in.", if on { "" } else { "no longer " });
            })
        }
        Some(Command::Transcribe { file, live }) => transcribe_file(&file, live),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("abla: {err:#}");
            ExitCode::FAILURE
        }
    }
}

fn exit_code(result: Result<i32>) -> ExitCode {
    match result {
        Ok(code) => ExitCode::from(code.clamp(0, 255) as u8),
        Err(err) => {
            eprintln!("abla: {err:#}");
            ExitCode::FAILURE
        }
    }
}

fn transcribe_file(file: &PathBuf, live: bool) -> Result<()> {
    let config = Config::load()?.context("abla is not set up yet: run `abla setup`")?;
    let key = config.api_key();
    anyhow::ensure!(!key.is_empty(), "no API key: run `abla setup` or set ABLA_API_KEY");
    let bytes = std::fs::read(file).with_context(|| format!("could not read {}", file.display()))?;
    let pcm = audio::pcm_from_wav(&bytes).unwrap_or(bytes);
    let seconds = pcm.len() as f32 / audio::BYTES_PER_SECOND as f32;
    eprintln!("{:.1} s of audio, {} ({})", seconds, config.provider.model, config.kind().label());

    if live {
        let (tx, rx) = channel();
        let session = realtime::start(&config.provider, &key, rx);
        std::thread::spawn(move || {
            // 100 ms per chunk, paced like a microphone.
            for chunk in pcm.chunks(audio::BYTES_PER_SECOND / 10) {
                if tx.send(chunk.to_vec()).is_err() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        });
        let started = std::time::Instant::now();
        loop {
            match session.events.recv() {
                Ok(realtime::Event::Delta(t)) => {
                    print!("{t}");
                    use std::io::Write;
                    let _ = std::io::stdout().flush();
                }
                Ok(realtime::Event::Done(t)) => {
                    println!("\n--- final ({:.1} s) ---\n{t}", started.elapsed().as_secs_f32());
                    return Ok(());
                }
                Ok(realtime::Event::Error(e)) => anyhow::bail!("{e}"),
                Ok(realtime::Event::Closed) | Err(_) => anyhow::bail!("the live session closed without a transcript"),
            }
        }
    } else {
        let started = std::time::Instant::now();
        let text = transcribe::batch(&config.provider, &key, audio::wav(&pcm, audio::SAMPLE_RATE))?;
        eprintln!("answered in {:.1} s", started.elapsed().as_secs_f32());
        println!("{text}");
        Ok(())
    }
}
