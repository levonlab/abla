//! Application menu entry, icon and login autostart, all under the user's
//! home. Nothing here needs root.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, Result};

const DESKTOP_TEMPLATE: &str = include_str!("../data/abla.desktop");
pub const ICON_SVG: &str = include_str!("../data/abla.svg");

fn data_home() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(dirs::data_local_dir)
        .unwrap_or_else(|| PathBuf::from(".local/share"))
}

fn config_home() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(dirs::config_dir)
        .unwrap_or_else(|| PathBuf::from(".config"))
}

pub fn applications_entry() -> PathBuf {
    data_home().join("applications").join("abla.desktop")
}

pub fn icon_path() -> PathBuf {
    data_home()
        .join("icons")
        .join("hicolor")
        .join("scalable")
        .join("apps")
        .join("abla.svg")
}

pub fn autostart_entry() -> PathBuf {
    config_home().join("autostart").join("abla.desktop")
}

/// The command the launchers should run: this very binary, by absolute path,
/// because `~/.local/bin` is not on the PATH of every desktop session.
pub fn exec_command() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.canonicalize().ok())
        .map(|p| p.to_string_lossy().into_owned())
        .filter(|p| !p.contains(char::is_whitespace))
        .unwrap_or_else(|| "abla".to_string())
}

fn desktop_file() -> String {
    DESKTOP_TEMPLATE.replace("{exec}", &exec_command())
}

/// Adds Abla to the application menu, with its icon.
pub fn install() -> Result<Vec<PathBuf>> {
    let entry = applications_entry();
    let icon = icon_path();
    for path in [&entry, &icon] {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).with_context(|| format!("could not create {}", dir.display()))?;
        }
    }
    fs::write(&entry, desktop_file()).with_context(|| format!("could not write {}", entry.display()))?;
    fs::write(&icon, ICON_SVG).with_context(|| format!("could not write {}", icon.display()))?;
    if let Some(dir) = entry.parent() {
        let _ = Command::new("update-desktop-database").arg(dir).output();
    }
    if autostart_enabled() {
        set_autostart(true)?;
    }
    Ok(vec![entry, icon])
}

pub fn autostart_enabled() -> bool {
    autostart_entry().exists()
}

pub fn set_autostart(on: bool) -> Result<()> {
    let path = autostart_entry();
    if on {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        fs::write(&path, desktop_file()).with_context(|| format!("could not write {}", path.display()))?;
    } else if path.exists() {
        fs::remove_file(&path).with_context(|| format!("could not remove {}", path.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_file_points_at_this_binary() {
        let text = desktop_file();
        assert!(text.contains("[Desktop Entry]"));
        assert!(text.contains(&format!("Exec={}", exec_command())));
        assert!(!text.contains("{exec}"));
    }

    #[test]
    fn icon_is_an_svg() {
        assert!(ICON_SVG.trim_start().starts_with("<svg"));
    }
}
