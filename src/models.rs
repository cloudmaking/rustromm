//! Types mirroring the parts of the RomM API that RustRomM actually uses.
//!
//! Deliberately partial. `SimpleRomSchema` alone has 73 fields, most of which
//! are metadata-provider IDs we have no use for. Every field is `#[serde(default)]`
//! or `Option` so that a RomM release adding, removing or nulling a field can't
//! break deserialisation of the whole response.

use serde::{Deserialize, Serialize};

/// FastAPI limit/offset pagination wrapper (`CustomLimitOffsetPage_*`).
#[derive(Debug, Clone, Deserialize)]
pub struct Page<T> {
    #[serde(default = "Vec::new")]
    pub items: Vec<T>,
    #[serde(default)]
    pub total: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Platform {
    pub id: i64,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub slug: String,
    #[serde(default)]
    pub fs_slug: String,
    #[serde(default)]
    pub rom_count: i64,
    #[serde(default)]
    pub custom_name: Option<String>,
}

impl Platform {
    /// RomM lets you override a platform's name; prefer that when it's set.
    pub fn display_name(&self) -> &str {
        match self.custom_name.as_deref() {
            Some(n) if !n.trim().is_empty() => n,
            _ => &self.name,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct RomFile {
    pub id: i64,
    #[serde(default)]
    pub file_name: String,
    #[serde(default)]
    pub file_size_bytes: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Rom {
    pub id: i64,
    /// Scraped title. Absent for unidentified ROMs, hence `fallback` in `title()`.
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub fs_name: String,
    #[serde(default)]
    pub fs_extension: String,
    #[serde(default)]
    pub fs_size_bytes: i64,
    #[serde(default)]
    pub platform_id: i64,
    #[serde(default)]
    pub platform_slug: String,
    #[serde(default)]
    pub platform_display_name: String,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub path_cover_small: Option<String>,
    #[serde(default)]
    pub path_cover_large: Option<String>,
    #[serde(default)]
    pub has_multiple_files: bool,
    #[serde(default)]
    pub files: Vec<RomFile>,
    /// RomM has the row but the file is gone from disk. Downloading would 404.
    #[serde(default)]
    pub missing_from_fs: bool,
}

impl Rom {
    pub fn title(&self) -> &str {
        match self.name.as_deref() {
            Some(n) if !n.trim().is_empty() => n,
            _ => &self.fs_name,
        }
    }

    /// Cover art path relative to the server root, small variant preferred —
    /// the grid never renders these large enough to justify the bigger file.
    pub fn cover_path(&self) -> Option<&str> {
        self.path_cover_small
            .as_deref()
            .or(self.path_cover_large.as_deref())
            .filter(|p| !p.trim().is_empty())
    }

    /// What to save the download as.
    ///
    /// Multi-file ROMs (discs, m3u sets) come back from the content endpoint as
    /// a zip regardless of what `fs_name` says, so the extension has to change
    /// to match or the file is unopenable.
    pub fn download_file_name(&self) -> String {
        if self.has_multiple_files || self.files.len() > 1 {
            let stem = self
                .fs_name
                .strip_suffix(&format!(".{}", self.fs_extension))
                .unwrap_or(&self.fs_name);
            format!("{stem}.zip")
        } else {
            self.fs_name.clone()
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Collection {
    pub id: i64,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub rom_count: i64,
}

/// Human-readable byte size. RomM reports sizes in bytes and some PSP ISOs
/// are multi-gigabyte, so raw numbers are unreadable in the UI.
pub fn human_size(bytes: i64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    if bytes <= 0 {
        return "—".to_string();
    }
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{} {}", bytes, UNITS[unit])
    } else {
        format!("{size:.1} {}", UNITS[unit])
    }
}
