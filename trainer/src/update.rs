//! Built-in updater: checks GitHub for the latest stable release, downloads its zip, verifies it
//! against the SHA-256 file published with it, swaps this exe and restarts into the new version.
//! The helper (dinput8.dll) is embedded in the exe, so any trainer can refresh an outdated helper.
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use sha2::{Digest, Sha256};

use crate::net;

pub const REPO: &str = "2extrapickles/wbu-trainer";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
const DAY: u64 = 24 * 60 * 60;

/// The helper built together with this trainer (empty if it was not built first).
pub static HELPER: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/dinput8.dll"));

#[derive(Clone)]
pub struct Release {
    pub version: String,
    pub notes: String,
    pub page: String,
    zip_url: String,
    zip_name: String,
    sha_url: String,
}

#[derive(Clone, Default)]
pub enum Status {
    #[default]
    Idle,
    Checking,
    UpToDate,
    Available(Release),
    Downloading(Release, u64, u64),
    Installing,
    Restarting,
    Error(String),
}

#[derive(Default)]
pub struct Updater {
    pub status: Status,
    pub show_notes: bool,
    pub dismissed: Option<String>, // "Later" for this version
}

pub type Shared = Arc<Mutex<Updater>>;

pub fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

pub fn due(last_check: u64) -> bool {
    now().saturating_sub(last_check) >= DAY
}

fn parse_version(v: &str) -> Option<(u32, u32, u32)> {
    let v = v.trim().trim_start_matches(['v', 'V']);
    let mut it = v.split(['.', '-', '+']).map(|p| p.parse::<u32>());
    Some((it.next()?.ok()?, it.next().unwrap_or(Ok(0)).ok()?, it.next().unwrap_or(Ok(0)).ok()?))
}

pub fn is_newer(candidate: &str) -> bool {
    matches!((parse_version(candidate), parse_version(VERSION)), (Some(a), Some(b)) if a > b)
}

/// Ask GitHub for the latest stable release (the endpoint never returns drafts or pre-releases).
fn fetch_latest() -> Result<Option<Release>, String> {
    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let body = net::get(&url, "application/vnd.github+json", |_, _| {})?;
    let json: serde_json::Value = serde_json::from_slice(&body).map_err(|e| format!("bad reply from GitHub: {e}"))?;
    let tag = json["tag_name"].as_str().ok_or("release has no version tag")?;
    if json["prerelease"].as_bool() == Some(true) || json["draft"].as_bool() == Some(true) || !is_newer(tag) {
        return Ok(None);
    }
    let assets = json["assets"].as_array().cloned().unwrap_or_default();
    let find = |pred: &dyn Fn(&str) -> bool| {
        assets.iter().find_map(|a| {
            let name = a["name"].as_str()?;
            let url = a["browser_download_url"].as_str()?;
            pred(name).then(|| (name.to_string(), url.to_string()))
        })
    };
    let (zip_name, zip_url) =
        find(&|n| n.starts_with("wbu-trainer-") && n.ends_with(".zip")).ok_or("the release has no wbu-trainer zip")?;
    let (_, sha_url) = find(&|n| n == format!("{zip_name}.sha256")).ok_or("the release has no checksum file, so it is not installed")?;
    Ok(Some(Release {
        version: tag.trim_start_matches(['v', 'V']).to_string(),
        notes: json["body"].as_str().unwrap_or("").trim().to_string(),
        page: json["html_url"].as_str().unwrap_or("").to_string(),
        zip_url,
        zip_name,
        sha_url,
    }))
}

pub fn check(shared: &Shared, ctx: &eframe::egui::Context, manual: bool) {
    {
        let mut u = shared.lock().unwrap();
        if matches!(u.status, Status::Checking | Status::Downloading(..) | Status::Installing | Status::Restarting) {
            return;
        }
        u.status = Status::Checking;
        if manual {
            u.dismissed = None;
        }
    }
    let (shared, ctx) = (shared.clone(), ctx.clone());
    std::thread::spawn(move || {
        let result = fetch_latest();
        let mut u = shared.lock().unwrap();
        u.status = match result {
            Ok(Some(r)) => Status::Available(r),
            Ok(None) => Status::UpToDate,
            Err(e) if manual => Status::Error(format!("Update check failed: {e}")),
            Err(_) => Status::Idle, // quiet when the automatic check fails (offline etc.)
        };
        ctx.request_repaint();
    });
}

/// Download, verify, swap the exe and restart. Runs on a background thread.
pub fn install(shared: &Shared, ctx: &eframe::egui::Context, release: Release) {
    let (shared, ctx) = (shared.clone(), ctx.clone());
    std::thread::spawn(move || {
        let set = |s: Status| {
            shared.lock().unwrap().status = s;
            ctx.request_repaint();
        };
        set(Status::Downloading(release.clone(), 0, 0));
        let result = (|| -> Result<(), String> {
            let sha_text = net::get(&release.sha_url, "application/octet-stream", |_, _| {})?;
            let expected = String::from_utf8_lossy(&sha_text)
                .split_whitespace()
                .next()
                .map(str::to_lowercase)
                .filter(|h| h.len() == 64 && h.chars().all(|c| c.is_ascii_hexdigit()))
                .ok_or("the checksum file is not a SHA-256 hash")?;
            let mut last = 0;
            let zip = net::get(&release.zip_url, "application/octet-stream", |done, total| {
                if done - last > 64 * 1024 || done == total {
                    last = done;
                    set(Status::Downloading(release.clone(), done, total));
                }
            })?;
            let actual: String = Sha256::digest(&zip).iter().map(|b| format!("{b:02x}")).collect();
            if actual != expected {
                return Err(format!("the download does not match its checksum ({}); nothing was changed", release.zip_name));
            }
            set(Status::Installing);
            let exe = extract_exe(&zip)?;
            swap_and_restart(&exe, &release.version)
        })();
        match result {
            Ok(()) => set(Status::Restarting),
            Err(e) => set(Status::Error(format!("Update failed: {e}"))),
        }
    });
}

fn extract_exe(zip: &[u8]) -> Result<Vec<u8>, String> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(zip)).map_err(|e| format!("bad zip: {e}"))?;
    let mut file = archive.by_name("wbu-trainer.exe").map_err(|_| "the zip has no wbu-trainer.exe".to_string())?;
    let mut exe = Vec::new();
    file.read_to_end(&mut exe).map_err(|e| format!("bad zip: {e}"))?;
    if exe.len() < 1024 || &exe[..2] != b"MZ" {
        return Err("the zip's wbu-trainer.exe is not a program".into());
    }
    Ok(exe)
}

pub fn old_exe_path(exe: &Path) -> PathBuf {
    let mut p = exe.as_os_str().to_owned();
    p.push(".old");
    PathBuf::from(p)
}

/// A running exe cannot be overwritten but can be renamed: move it aside, write the new one in
/// its place, start it and close this one. The `.old` file is removed by the next start.
fn swap_and_restart(new_exe: &[u8], version: &str) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let old = old_exe_path(&exe);
    let _ = std::fs::remove_file(&old);
    std::fs::rename(&exe, &old).map_err(|e| {
        format!("could not replace {} ({e}); if it is in a protected folder, run the trainer as administrator", exe.display())
    })?;
    if let Err(e) = std::fs::write(&exe, new_exe) {
        let _ = std::fs::rename(&old, &exe); // put the working version back
        return Err(format!("could not write the new version ({e})"));
    }
    std::process::Command::new(&exe)
        .arg("--updated-to")
        .arg(version)
        .spawn()
        .map_err(|e| format!("installed, but could not restart ({e}); start the trainer again"))?;
    Ok(())
}

/// After an update: remove the previous exe (it may still be closing for a moment).
pub fn cleanup_old() {
    let Ok(exe) = std::env::current_exe() else { return };
    let old = old_exe_path(&exe);
    if old.exists() {
        std::thread::spawn(move || {
            for _ in 0..20 {
                if std::fs::remove_file(&old).is_ok() || !old.exists() {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(250));
            }
        });
    }
}

// ------------------------------------------------------------------ helper (dinput8.dll)

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum HelperState {
    Missing,   // not installed (or another dinput8.dll that is not ours)
    Current,   // same as the embedded one
    Outdated,  // ours, but a different build
    Unknown,   // this trainer has no embedded helper (development build)
}

pub fn is_ours(dll: &[u8]) -> bool {
    dll.windows(13).any(|w| w == b"wbu-trainer.i")
}

pub fn helper_state(game_dir: &Path) -> HelperState {
    match std::fs::read(game_dir.join("dinput8.dll")) {
        Ok(d) if is_ours(&d) => {
            if HELPER.is_empty() {
                HelperState::Unknown
            } else if d == HELPER {
                HelperState::Current
            } else {
                HelperState::Outdated
            }
        }
        _ => HelperState::Missing,
    }
}

pub fn install_helper(game_dir: &Path) -> Result<(), String> {
    if HELPER.is_empty() {
        return Err("this build has no helper inside it".into());
    }
    std::fs::write(game_dir.join("dinput8.dll"), HELPER).map_err(|e| {
        format!("could not write dinput8.dll ({e}); the game must be closed, and Program Files may need administrator rights")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_numerically() {
        assert_eq!(parse_version("v1.2.10"), Some((1, 2, 10)));
        assert_eq!(parse_version("1.3"), Some((1, 3, 0)));
        assert!(parse_version("v1.10.0") > parse_version("1.9.9"));
        assert!(parse_version("x1") .is_none());
    }

    /// Live check against GitHub (network): `cargo test -- --ignored live_latest_release`.
    #[test]
    #[ignore]
    fn live_latest_release() {
        let body = net::get(&format!("https://api.github.com/repos/{REPO}/releases/latest"), "application/vnd.github+json", |_, _| {});
        match body {
            Ok(b) => {
                let json: serde_json::Value = serde_json::from_slice(&b).unwrap();
                println!("latest tag: {}", json["tag_name"]);
                for a in json["assets"].as_array().unwrap() {
                    println!("  asset: {} {}", a["name"], a["size"]);
                }
                println!("fetch_latest: {:?}", fetch_latest().map(|r| r.map(|r| (r.version, r.zip_name))));
            }
            Err(e) => println!("request: {e}"),
        }
    }
}
