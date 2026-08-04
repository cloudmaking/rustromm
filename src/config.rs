//! On-disk settings.
//!
//! Kept as plain JSON in the OS config directory so it can be inspected and
//! hand-edited — this is a hobbyist tool and being able to fix a bad emulator
//! path in a text editor is worth more than an opaque format.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
// Missing keys fall back to Default, so a config written by an older version
// keeps loading after new settings are added.
#[serde(default)]
pub struct Config {
    pub server_url: String,
    pub username: String,
    /// Only populated when `remember_password` is set. See `save()` for the
    /// caveat that goes with storing this.
    pub password: String,
    pub remember_password: bool,
    /// Where ROMs land. Empty means "ask the OS for Downloads/RustRomM".
    pub download_dir: String,
    /// platform slug -> emulator command. `{rom}` is substituted with the
    /// file path; if absent, the path is appended as the last argument.
    pub emulators: BTreeMap<String, String>,
    /// Used when no platform-specific entry matches.
    pub default_emulator: String,
}

impl Config {
    /// Environment variable overriding where settings live.
    ///
    /// Set it to run a portable install off a USB stick — and the test suite
    /// uses it so tests never touch a real user's configuration.
    pub const CONFIG_DIR_ENV: &'static str = "RUSTROMM_CONFIG_DIR";

    pub fn path() -> Result<PathBuf> {
        if let Some(dir) = std::env::var_os(Self::CONFIG_DIR_ENV) {
            if !dir.is_empty() {
                return Ok(PathBuf::from(dir).join("config.json"));
            }
        }
        let dirs = directories::ProjectDirs::from("uk", "cloudmaking", "rustromm")
            .context("could not determine a config directory for this platform")?;
        Ok(dirs.config_dir().join("config.json"))
    }

    pub fn load() -> Self {
        let Ok(path) = Self::path() else {
            return Self::default();
        };
        let Ok(text) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        // A corrupt config shouldn't stop the app booting — fall back to
        // defaults and let the user re-enter their details.
        serde_json::from_str(&text).unwrap_or_default()
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::path()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }

        let mut to_write = self.clone();
        if !to_write.remember_password {
            to_write.password.clear();
        }

        let json = serde_json::to_string_pretty(&to_write)?;
        std::fs::write(&path, json).with_context(|| format!("writing {}", path.display()))?;
        restrict_permissions(&path);
        Ok(())
    }

    /// Resolved download directory, creating nothing — callers create on demand.
    pub fn resolved_download_dir(&self) -> PathBuf {
        if !self.download_dir.trim().is_empty() {
            return PathBuf::from(self.download_dir.trim());
        }
        directories::UserDirs::new()
            .and_then(|d| d.download_dir().map(Path::to_path_buf))
            .unwrap_or_else(std::env::temp_dir)
            .join("RustRomM")
    }

    /// Emulator command for a platform, falling back to the default.
    pub fn emulator_for(&self, platform_slug: &str) -> Option<&str> {
        self.emulators
            .get(platform_slug)
            .map(String::as_str)
            .filter(|s| !s.trim().is_empty())
            .or_else(|| Some(self.default_emulator.as_str()).filter(|s| !s.trim().is_empty()))
    }
}

/// The config may hold a password, so keep it owner-readable on Unix.
/// No-op on Windows, where the per-user AppData ACL already covers this.
#[cfg(unix)]
fn restrict_permissions(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}

#[cfg(not(unix))]
fn restrict_permissions(_path: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_default_emulator() -> Config {
        Config {
            default_emulator: "retroarch".into(),
            ..Config::default()
        }
    }

    #[test]
    fn platform_emulator_beats_default() {
        let mut c = with_default_emulator();
        c.emulators.insert("psp".into(), "ppsspp".into());
        assert_eq!(c.emulator_for("psp"), Some("ppsspp"));
        assert_eq!(c.emulator_for("snes"), Some("retroarch"));
    }

    #[test]
    fn blank_entries_fall_through_to_default() {
        let mut c = with_default_emulator();
        c.emulators.insert("snes".into(), "   ".into());
        assert_eq!(c.emulator_for("snes"), Some("retroarch"));
    }

    #[test]
    fn no_emulator_configured_at_all() {
        assert_eq!(Config::default().emulator_for("snes"), None);
    }
}
