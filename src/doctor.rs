//! `abla doctor`: says what works, what does not, and how to fix it.

use std::process::{Command, ExitCode};

use crate::config::Config;
use crate::inject;
use crate::providers;

struct Report {
    failed: bool,
}

impl Report {
    fn ok(&self, what: &str, detail: &str) {
        println!("  ✓ {what:<12} {detail}");
    }
    fn warn(&self, what: &str, detail: &str, hint: &str) {
        println!("  ! {what:<12} {detail}");
        println!("    {hint}");
    }
    fn fail(&mut self, what: &str, detail: &str, hint: &str) {
        self.failed = true;
        println!("  ✗ {what:<12} {detail}");
        println!("    {hint}");
    }
}

pub fn run() -> ExitCode {
    let mut r = Report { failed: false };
    println!("abla {}", env!("CARGO_PKG_VERSION"));

    let session = std::env::var("XDG_SESSION_TYPE").unwrap_or_default();
    let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_else(|_| "unknown desktop".into());
    if session == "wayland" {
        r.ok("Session", &format!("Wayland ({desktop})"));
    } else {
        r.fail(
            "Session",
            &format!("{} ({desktop})", if session.is_empty() { "not Wayland" } else { &session }),
            "abla needs a Wayland session with layer-shell: KDE Plasma, sway, Hyprland, niri or wayfire.",
        );
    }

    match Command::new("pw-record").arg("--version").output() {
        Ok(out) if out.status.success() => r.ok("Microphone", "pw-record found"),
        _ => r.fail(
            "Microphone",
            "pw-record not found",
            "Install pipewire-utils (Fedora), pipewire-bin (Debian/Ubuntu) or pipewire (Arch).",
        ),
    }

    let socket = inject::socket_path();
    if inject::available() {
        r.ok("Typing", &format!("ydotool socket at {}", socket.display()));
    } else {
        r.warn(
            "Typing",
            &format!("no ydotool socket at {}", socket.display()),
            "Without it, abla copies the text and you press Ctrl+V yourself. See README, section “Automatic pasting”.",
        );
    }

    let config = match Config::load() {
        Ok(Some(c)) => c,
        Ok(None) => {
            r.fail("Config", "not set up yet", "Run `abla` once, or `abla setup`, to choose a provider and paste your key.");
            return finish(r);
        }
        Err(err) => {
            r.fail("Config", &format!("{err:#}"), &format!("Fix or delete {}", Config::path().display()));
            return finish(r);
        }
    };
    let kind = config.kind();
    r.ok(
        "Config",
        &format!(
            "{} — {}, {}{}",
            Config::path().display(),
            kind.label(),
            if config.provider.model.is_empty() { "no model" } else { &config.provider.model },
            if config.provider.realtime { " (live)" } else { "" }
        ),
    );
    let key = config.api_key();
    if key.is_empty() {
        r.fail("Provider", "no API key", "Run `abla setup` and paste a key, or set ABLA_API_KEY.");
        return finish(r);
    }
    match providers::list_models(kind, &config.provider.base_url, &key) {
        Ok(models) => {
            let n = models.len();
            let known = models.iter().any(|m| m.id == config.provider.model);
            if known || config.provider.model.is_empty() {
                r.ok("Provider", &format!("key accepted, {n} transcription model{}", if n == 1 { "" } else { "s" }));
            } else {
                r.warn(
                    "Provider",
                    &format!("key accepted, but {} is not in the list of {n}", config.provider.model),
                    "Run `abla setup` and pick a model again.",
                );
            }
        }
        Err(err) => r.fail("Provider", &format!("{err:#}"), "Check the key and the base URL in `abla setup`."),
    }
    finish(r)
}

fn finish(r: Report) -> ExitCode {
    if r.failed {
        println!("\nSomething needs fixing before abla can work.");
        ExitCode::FAILURE
    } else {
        println!("\nAll good.");
        ExitCode::SUCCESS
    }
}
