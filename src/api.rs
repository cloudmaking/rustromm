//! Blocking RomM API client.
//!
//! Auth is HTTP Basic on every request. RomM's OpenAPI spec lists `HTTPBasic`
//! as an accepted scheme on the data endpoints alongside OAuth2 bearer tokens,
//! so this avoids carrying a session and refreshing it — at the cost of sending
//! credentials each time, which is fine over HTTPS or a private network.

use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use reqwest::StatusCode;
use reqwest::blocking::Client;

use crate::models::{Page, Platform, Rom};

/// Cap on a single library page. RomM will happily return more, but each
/// `SimpleRomSchema` is large and the UI paginates anyway.
pub const PAGE_SIZE: i64 = 100;

#[derive(Clone)]
pub struct Api {
    /// Short timeout — used for everything except file transfers.
    client: Client,
    /// No timeout: a PSP ISO over a slow link legitimately takes many minutes,
    /// and a stalled transfer is caught by the cancel flag instead.
    transfer: Client,
    base: String,
    user: String,
    pass: String,
}

impl Api {
    pub fn new(base_url: &str, user: &str, pass: &str) -> Result<Self> {
        let base = normalise_base_url(base_url)?;
        let ua = concat!("rustromm/", env!("CARGO_PKG_VERSION"));
        Ok(Self {
            client: Client::builder()
                .user_agent(ua)
                .timeout(Duration::from_secs(30))
                .build()?,
            transfer: Client::builder().user_agent(ua).build()?,
            base,
            user: user.to_string(),
            pass: pass.to_string(),
        })
    }

    pub fn base_url(&self) -> &str {
        &self.base
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base, path)
    }

    /// Verify the server is a RomM instance *and* the credentials work.
    ///
    /// Two calls on purpose: `/api/heartbeat` needs no auth, so if it succeeds
    /// and `/api/platforms` then 401s, we can say "wrong password" rather than
    /// the much less helpful "couldn't connect".
    pub fn check_connection(&self) -> Result<String> {
        let hb = self
            .client
            .get(self.url("/api/heartbeat"))
            .send()
            .with_context(|| format!("could not reach {}", self.base))?;

        if !hb.status().is_success() {
            bail!(
                "{} answered {} for /api/heartbeat — is this a RomM server?",
                self.base,
                hb.status()
            );
        }

        // RomM 5.x nests this under SYSTEM; the flat forms are accepted as a
        // fallback in case an older or future release moves it back.
        let version = hb
            .json::<serde_json::Value>()
            .ok()
            .and_then(|v| {
                v.pointer("/SYSTEM/VERSION")
                    .or_else(|| v.pointer("/VERSION"))
                    .or_else(|| v.pointer("/version"))
                    .and_then(|s| s.as_str())
                    .map(str::to_string)
            })
            .unwrap_or_else(|| "unknown".to_string());

        let probe = self
            .client
            .get(self.url("/api/platforms"))
            .basic_auth(&self.user, Some(&self.pass))
            .send()
            .context("credential check failed")?;

        match probe.status() {
            s if s.is_success() => Ok(version),
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => Err(anyhow!(
                "server reachable, but the username or password was rejected"
            )),
            s => Err(anyhow!("unexpected response {s} from /api/platforms")),
        }
    }

    pub fn platforms(&self) -> Result<Vec<Platform>> {
        let resp = self
            .client
            .get(self.url("/api/platforms"))
            .basic_auth(&self.user, Some(&self.pass))
            .send()
            .context("fetching platforms")?
            .error_for_status()?;
        resp.json().context("parsing platforms response")
    }

    /// One page of the library. `platform_id == None` means every platform.
    pub fn roms(&self, platform_id: Option<i64>, search: &str, offset: i64) -> Result<Page<Rom>> {
        let limit = PAGE_SIZE.to_string();
        let offset_s = offset.to_string();
        let mut query: Vec<(&str, String)> = vec![
            ("limit", limit),
            ("offset", offset_s),
            ("order_by", "name".to_string()),
            ("order_dir", "asc".to_string()),
        ];
        if let Some(pid) = platform_id {
            query.push(("platform_ids", pid.to_string()));
        }
        let search = search.trim();
        if !search.is_empty() {
            query.push(("search_term", search.to_string()));
        }

        let resp = self
            .client
            .get(self.url("/api/roms"))
            .basic_auth(&self.user, Some(&self.pass))
            .query(&query)
            .send()
            .context("fetching roms")?
            .error_for_status()?;
        resp.json().context("parsing roms response")
    }

    /// Cover art bytes. Returns `None` rather than erroring — missing art is
    /// cosmetic and shouldn't surface as a failure in the UI.
    pub fn cover(&self, path: &str) -> Option<Vec<u8>> {
        let url = format!("{}/{}", self.base, path.trim_start_matches('/'));
        let resp = self
            .client
            .get(url)
            .basic_auth(&self.user, Some(&self.pass))
            .send()
            .ok()?;
        if !resp.status().is_success() {
            return None;
        }
        resp.bytes().ok().map(|b| b.to_vec())
    }

    /// Stream a ROM to `dest`, reporting progress and honouring cancellation.
    ///
    /// Writes to `dest.part` and renames on success, so an interrupted transfer
    /// never leaves a truncated file that looks like a complete ROM.
    pub fn download_rom<F>(
        &self,
        rom: &Rom,
        dest: &Path,
        cancel: &AtomicBool,
        mut on_progress: F,
    ) -> Result<()>
    where
        F: FnMut(u64, Option<u64>),
    {
        if rom.missing_from_fs {
            bail!("RomM has this game indexed but the file is missing from the server's disk");
        }

        let url = format!(
            "{}/api/roms/{}/content/{}",
            self.base,
            rom.id,
            urlencoding::encode(&rom.fs_name)
        );

        let mut resp = self
            .transfer
            .get(url)
            .basic_auth(&self.user, Some(&self.pass))
            .send()
            .context("starting download")?
            .error_for_status()
            .context("server refused the download")?;

        let total = resp.content_length();

        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        let part = dest.with_extension("part");
        let mut file =
            std::fs::File::create(&part).with_context(|| format!("creating {}", part.display()))?;

        let mut buf = vec![0u8; 128 * 1024];
        let mut written: u64 = 0;
        loop {
            if cancel.load(Ordering::Relaxed) {
                drop(file);
                let _ = std::fs::remove_file(&part);
                bail!("cancelled");
            }
            let n = resp.read(&mut buf).context("reading from server")?;
            if n == 0 {
                break;
            }
            file.write_all(&buf[..n]).context("writing to disk")?;
            written += n as u64;
            on_progress(written, total);
        }
        file.flush().ok();
        drop(file);

        std::fs::rename(&part, dest).with_context(|| format!("finalising {}", dest.display()))?;
        Ok(())
    }
}

/// Accept what a person would actually type: `192.168.1.5:8087`,
/// `http://romm.box/`, `romm.example.com`. Default to http:// for bare
/// host:port (overwhelmingly a LAN address) but keep an explicit scheme.
fn normalise_base_url(input: &str) -> Result<String> {
    let trimmed = input.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        bail!("server address is empty");
    }
    let with_scheme = if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        trimmed.to_string()
    } else {
        format!("http://{trimmed}")
    };
    // Validate early so a typo surfaces on Connect, not on first request.
    reqwest::Url::parse(&with_scheme)
        .with_context(|| format!("'{input}' is not a valid server address"))?;
    Ok(with_scheme)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_host_port_gets_http() {
        assert_eq!(
            normalise_base_url("192.168.1.5:8087").unwrap(),
            "http://192.168.1.5:8087"
        );
    }

    #[test]
    fn explicit_https_is_preserved() {
        assert_eq!(
            normalise_base_url("https://romm.example.com/").unwrap(),
            "https://romm.example.com"
        );
    }

    #[test]
    fn trailing_slashes_go() {
        assert_eq!(
            normalise_base_url("http://romm.box///").unwrap(),
            "http://romm.box"
        );
    }

    #[test]
    fn empty_is_rejected() {
        assert!(normalise_base_url("   ").is_err());
    }
}
