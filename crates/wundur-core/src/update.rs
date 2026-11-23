//! Checking for a newer Wundur Studio, getting it ready, and putting it in place on restart.
//!
//! The newest release is described by `latest.json` on the latest GitHub release
//! ([`MANIFEST_URL`]; `WUNDUR_UPDATE_URL` points elsewhere, for tests):
//!
//! ```json
//! { "version": "0.5.0", "notes": "https://…/releases/tag/v0.5.0",
//!   "platforms": { "macos-universal": { "url": "https://…/Wundur-0.5.0-universal.zip", "sha256": "…", "size": 43000000 } } }
//! ```
//!
//! Nothing in the manifest is trusted. A download is refused unless all of these hold:
//! its sha256 is the manifest's; the app inside is signed by Wundur's Apple Developer ID team
//! (`codesign` against [`requirement`], which also catches any changed file); Gatekeeper
//! accepts it (`spctl`: Developer ID and notarized); and its own version is the manifest's and
//! newer than this one (never a downgrade). The signing key is Apple's certificate, kept in CI's
//! secrets, never in the repo.
//!
//! [`install_on_exit`] starts a small detached helper that waits for this process to end, swaps
//! the bundle (the old one is put back if that fails), opens the new one and removes the old.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde::Deserialize;
use sha2::Digest as _;

/// The newest release's manifest.
pub const MANIFEST_URL: &str = "https://github.com/wundur-dev/wundur/releases/latest/download/latest.json";
/// The Apple Developer ID team that signs Wundur Studio.
pub const TEAM_ID: &str = "28P42HA77C";
/// The app's bundle identifier.
pub const BUNDLE_ID: &str = "ai.wundur.app";
/// The bundle's name inside the release zip.
pub const APP_NAME: &str = "Wundur Studio.app";
/// The largest download taken (the app is about 45 MB).
const MAX_SIZE: u64 = 1024 * 1024 * 1024;

/// What the release offers for one platform.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct Asset {
    pub url: String,
    pub sha256: String,
    pub size: u64,
}

/// `latest.json`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct Manifest {
    pub version: String,
    /// Where the release notes are (a page to open).
    pub notes: String,
    pub platforms: std::collections::BTreeMap<String, Asset>,
}

/// An update ready to install: checked, unpacked, waiting for a restart.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ready {
    pub version: String,
    pub notes: String,
    /// The checked app bundle, staged.
    pub app: PathBuf,
}

/// This build's platform name in the manifest (`None`: updates aren't offered here yet).
pub fn platform() -> Option<&'static str> {
    cfg!(target_os = "macos").then_some("macos-universal")
}

/// A version as numbers, and whether it is a pre-release (`0.5.0-dev` < `0.5.0`).
fn parse(v: &str) -> Option<(Vec<u64>, bool)> {
    let v = v.trim().trim_start_matches('v');
    let (core, pre) = match v.split_once(['-', '+']) {
        Some((c, rest)) => (c, v[c.len()..].starts_with('-') && !rest.is_empty()),
        None => (v, false),
    };
    let nums: Option<Vec<u64>> = core.split('.').map(|n| n.parse().ok()).collect();
    nums.filter(|n| !n.is_empty() && n.len() <= 4).map(|n| (n, pre))
}

/// Whether `offered` is newer than `current` (unreadable versions are not).
pub fn is_newer(offered: &str, current: &str) -> bool {
    let (Some((mut a, a_pre)), Some((mut b, b_pre))) = (parse(offered), parse(current)) else { return false };
    let n = a.len().max(b.len());
    a.resize(n, 0);
    b.resize(n, 0);
    match a.cmp(&b) {
        std::cmp::Ordering::Equal => b_pre && !a_pre,
        o => o == std::cmp::Ordering::Greater,
    }
}

/// The app bundle this process runs from, when it is one that can be replaced: an installed
/// `.app` (not a development build), not run from a disk image or translocated by Gatekeeper,
/// in a folder this user can write to. `Err` says why not.
pub fn installed_bundle() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let app = exe.ancestors().nth(3).filter(|p| p.extension().is_some_and(|e| e == "app")).ok_or("not running from an app bundle (a development build)")?.to_path_buf();
    let text = app.to_string_lossy();
    if text.contains("/AppTranslocation/") {
        return Err("macOS is running it from a temporary place: move Wundur Studio to Applications".into());
    }
    let parent = app.parent().ok_or("no folder")?;
    let probe = parent.join(format!(".wundur-update-probe-{}", std::process::id()));
    match std::fs::File::create(&probe) {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            Ok(app)
        }
        Err(_) => Err(format!("can't write to {}: move Wundur Studio to Applications", parent.display())),
    }
}

fn agent(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder().timeout_global(Some(timeout)).http_status_as_error(true).build().into()
}

fn manifest_url() -> String {
    crate::env_var("UPDATE_URL").ok().filter(|u| !u.is_empty()).unwrap_or_else(|| MANIFEST_URL.into())
}

/// Ask for the newest release; `Some` when it is newer than `current` and has this platform.
pub fn check(current: &str) -> Result<Option<(Manifest, Asset)>, String> {
    let Some(platform) = platform() else { return Ok(None) };
    let body = agent(Duration::from_secs(30))
        .get(&manifest_url())
        .header("User-Agent", "Wundur-Studio-updater")
        .call()
        .map_err(|e| format!("can't check for updates: {e}"))?
        .body_mut()
        .with_config()
        .limit(1024 * 1024)
        .read_to_string()
        .map_err(|e| format!("can't read the update manifest: {e}"))?;
    let manifest: Manifest = serde_json::from_str(&body).map_err(|e| format!("can't read the update manifest: {e}"))?;
    if !is_newer(&manifest.version, current) {
        return Ok(None);
    }
    let Some(asset) = manifest.platforms.get(platform).cloned() else { return Ok(None) };
    if !asset.url.starts_with("https://") || asset.sha256.len() != 64 {
        return Err("the update manifest is incomplete".into());
    }
    Ok(Some((manifest, asset)))
}

/// Where updates are downloaded and unpacked.
pub fn work_dir() -> PathBuf {
    dirs::cache_dir().unwrap_or_else(std::env::temp_dir).join("wundur-studio").join("updates")
}

/// Download `asset` to `dir`, checking its size and sha256 as it comes.
pub fn download(asset: &Asset, dir: &Path) -> Result<PathBuf, String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let file = dir.join("update.zip");
    let mut resp = agent(Duration::from_secs(30 * 60))
        .get(&asset.url)
        .header("User-Agent", "Wundur-Studio-updater")
        .call()
        .map_err(|e| format!("can't download the update: {e}"))?;
    let mut reader = resp.body_mut().with_config().limit(MAX_SIZE).reader();
    let mut out = std::fs::File::create(&file).map_err(|e| e.to_string())?;
    let mut hash = sha2::Sha256::new();
    let mut buf = vec![0u8; 256 * 1024];
    let mut total = 0u64;
    loop {
        let n = reader.read(&mut buf).map_err(|e| format!("the download broke off: {e}"))?;
        if n == 0 {
            break;
        }
        total += n as u64;
        hash.update(&buf[..n]);
        out.write_all(&buf[..n]).map_err(|e| e.to_string())?;
    }
    out.flush().map_err(|e| e.to_string())?;
    let got = hex(&hash.finalize());
    if asset.size > 0 && total != asset.size {
        let _ = std::fs::remove_file(&file);
        return Err(format!("the download is {total} bytes, not {}: refused", asset.size));
    }
    if !got.eq_ignore_ascii_case(asset.sha256.trim()) {
        let _ = std::fs::remove_file(&file);
        return Err("the download's checksum doesn't match: refused".into());
    }
    Ok(file)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The code requirement an update must meet: Apple's Developer ID chain, Wundur's team, the
/// app's identifier.
pub fn requirement() -> String {
    format!("anchor apple generic and certificate leaf[subject.OU] = \"{TEAM_ID}\" and identifier \"{BUNDLE_ID}\"")
}

fn run(program: &str, args: &[&std::ffi::OsStr]) -> Result<String, String> {
    let out = crate::platform::command(program).args(args).stdin(Stdio::null()).output().map_err(|e| format!("{program}: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        Err(format!("{program}: {}", err.lines().last().unwrap_or("failed").trim()))
    }
}

/// Check a bundle: Wundur's signature with nothing changed, Gatekeeper's yes, and `version`.
pub fn verify_app(app: &Path, version: &str) -> Result<(), String> {
    let req = format!("--test-requirement=={}", requirement());
    run("codesign", &["--verify".as_ref(), "--deep".as_ref(), "--strict".as_ref(), req.as_ref(), app.as_os_str()]).map_err(|e| format!("the update isn't signed by Wundur: refused ({e})"))?;
    run("spctl", &["--assess".as_ref(), "--type".as_ref(), "execute".as_ref(), app.as_os_str()]).map_err(|e| format!("macOS doesn't accept the update: refused ({e})"))?;
    let plist = app.join("Contents").join("Info.plist");
    let got = run("plutil", &["-extract".as_ref(), "CFBundleShortVersionString".as_ref(), "raw".as_ref(), plist.as_os_str()])?;
    if got != version {
        return Err(format!("the update says it is {version} but is {got}: refused"));
    }
    Ok(())
}

/// Unpack the downloaded `zip` into `dir` and check the app in it ([`verify_app`]): the update,
/// ready. Run off the UI thread.
pub fn stage(zip: &Path, dir: &Path, manifest: &Manifest, current: &str) -> Result<Ready, String> {
    let staged = dir.join(format!("staged-{}", manifest.version));
    let _ = std::fs::remove_dir_all(&staged);
    std::fs::create_dir_all(&staged).map_err(|e| e.to_string())?;
    run("ditto", &["-x".as_ref(), "-k".as_ref(), zip.as_os_str(), staged.as_os_str()]).map_err(|e| format!("can't unpack the update: {e}"))?;
    let _ = std::fs::remove_file(zip);
    let app = staged.join(APP_NAME);
    let check = || -> Result<(), String> {
        if !app.is_dir() {
            return Err("the update has no app in it: refused".into());
        }
        // Checked again here, against the bundle's own version: never older or the same.
        if !is_newer(&manifest.version, current) {
            return Err("the update isn't newer: refused".into());
        }
        verify_app(&app, &manifest.version)
    };
    if let Err(e) = check() {
        let _ = std::fs::remove_dir_all(&staged);
        return Err(e);
    }
    Ok(Ready { version: manifest.version.clone(), notes: manifest.notes.clone(), app })
}

/// Check for, download and stage an update in one go (blocking). `Ok(None)`: up to date.
pub fn fetch(current: &str) -> Result<Option<Ready>, String> {
    let Some((manifest, asset)) = check(current)? else { return Ok(None) };
    let dir = work_dir();
    // One already staged and still good: keep it.
    let staged = dir.join(format!("staged-{}", manifest.version)).join(APP_NAME);
    if staged.is_dir() && verify_app(&staged, &manifest.version).is_ok() {
        return Ok(Some(Ready { version: manifest.version, notes: manifest.notes, app: staged }));
    }
    let zip = download(&asset, &dir)?;
    stage(&zip, &dir, &manifest, current).map(Some)
}

/// The helper that swaps the bundle once this process (`$1`) has ended: `$2` the new bundle,
/// `$3` the installed one. The old one goes back if the swap fails.
const SWAP: &str = r#"pid=$1; new=$2; app=$3
open() { ${WUNDUR_UPDATE_OPEN:-/usr/bin/open} "$@"; }
i=0
while kill -0 "$pid" 2>/dev/null; do
  i=$((i + 1)); [ "$i" -gt 1500 ] && exit 1
  sleep 0.2
done
old="$app.old-$$"
mv "$app" "$old" || { open "$app"; exit 1; }
if mv "$new" "$app"; then
  open "$app"
  rm -rf "$old"
  rm -rf "$(dirname "$new")"
else
  rm -rf "$app"
  mv "$old" "$app"
  open "$app"
fi
"#;

/// Start the helper that installs `ready` over `app` once this process ends, then opens it.
/// Call just before quitting. The staged bundle is checked once more first.
pub fn install_on_exit(ready: &Ready, app: &Path) -> Result<(), String> {
    verify_app(&ready.app, &ready.version)?;
    crate::platform::command("/bin/sh")
        .arg("-c")
        .arg(SWAP)
        .arg("wundur-update")
        .arg(std::process::id().to_string())
        .arg(&ready.app)
        .arg(app)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("can't start the installer: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_and_never_go_back() {
        assert!(is_newer("0.5.0", "0.4.0"));
        assert!(is_newer("v0.10.0", "0.9.9"));
        assert!(is_newer("1.0", "0.99.1"));
        assert!(is_newer("0.5.0", "0.5.0-dev"), "a release is newer than its dev build");
        assert!(!is_newer("0.4.0", "0.4.0"));
        assert!(!is_newer("0.3.9", "0.4.0"), "never a downgrade");
        assert!(!is_newer("0.5.0-dev", "0.5.0"));
        assert!(!is_newer("garbage", "0.4.0"));
        assert!(!is_newer("0.5.0", ""));
    }

    #[test]
    fn the_manifest_reads_leniently() {
        let m: Manifest = serde_json::from_str(
            r#"{"version":"0.5.0","notes":"https://x/notes","platforms":{"macos-universal":{"url":"https://x/a.zip","sha256":"ab","size":3}},"extra":1}"#,
        )
        .unwrap();
        assert_eq!(m.version, "0.5.0");
        assert_eq!(m.platforms["macos-universal"].size, 3);
    }

    #[test]
    fn the_requirement_names_the_team_and_the_app() {
        let r = requirement();
        assert!(r.starts_with("anchor apple generic") && r.contains("\"28P42HA77C\"") && r.contains("\"ai.wundur.app\""));
    }

    /// A download is checked against the manifest's size and checksum.
    #[test]
    fn a_download_with_the_wrong_checksum_is_refused() {
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for stream in listener.incoming().take(2) {
                let mut s = stream.unwrap();
                let mut buf = [0u8; 1024];
                let _ = s.read(&mut buf);
                let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello");
            }
        });
        let dir = std::env::temp_dir().join(format!("wundur-update-test-{}", std::process::id()));
        let url = format!("http://127.0.0.1:{port}/a.zip");
        let good = hex(&sha2::Sha256::digest(b"hello"));
        let ok = download(&Asset { url: url.clone(), sha256: good, size: 5 }, &dir).unwrap();
        assert_eq!(std::fs::read(&ok).unwrap(), b"hello");
        let bad = download(&Asset { url, sha256: "0".repeat(64), size: 5 }, &dir);
        assert!(bad.unwrap_err().contains("checksum"));
        assert!(!dir.join("update.zip").exists(), "a refused download is removed");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A real release zip: `UPDATE_TEST_ZIP=<zip> UPDATE_TEST_VERSION=<its version>` (run with
    /// `--ignored`). It stages as an update from an older version, and not from itself.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore]
    fn a_real_release_stages() {
        let zip = PathBuf::from(std::env::var("UPDATE_TEST_ZIP").unwrap());
        let version = std::env::var("UPDATE_TEST_VERSION").unwrap();
        let dir = std::env::temp_dir().join(format!("wundur-update-real-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let copy = dir.join("update.zip");
        std::fs::copy(&zip, &copy).unwrap();
        let m = Manifest { version: version.clone(), ..Default::default() };
        assert!(stage(&copy, &dir, &m, &version).unwrap_err().contains("isn't newer"));
        std::fs::copy(&zip, &copy).unwrap();
        let ready = stage(&copy, &dir, &m, "0.0.1").unwrap();
        assert!(ready.app.join("Contents/MacOS").is_dir());
        // The manifest lying about the version is caught by the bundle's own.
        std::fs::copy(&zip, &copy).unwrap();
        let lie = Manifest { version: "99.0.0".into(), ..Default::default() };
        assert!(stage(&copy, &dir, &lie, "0.0.1").unwrap_err().contains("says it is 99.0.0"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The helper waits for the process to end, swaps the bundle in and opens it.
    #[test]
    fn the_helper_swaps_the_bundle_after_the_app_quits() {
        let dir = std::env::temp_dir().join(format!("wundur-update-swap-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (app, new) = (dir.join("Applications/W.app"), dir.join("staged-1/W.app"));
        for (d, v) in [(&app, "old"), (&new, "new")] {
            std::fs::create_dir_all(d).unwrap();
            std::fs::write(d.join("v"), v).unwrap();
        }
        let opened = dir.join("opened");
        let mut quitting = std::process::Command::new("sleep").arg("0.5").spawn().unwrap();
        let mut helper = std::process::Command::new("/bin/sh")
            .args(["-c", SWAP, "wundur-update", &quitting.id().to_string()])
            .arg(&new)
            .arg(&app)
            .env("WUNDUR_UPDATE_OPEN", format!("touch {}", opened.display()))
            .spawn()
            .unwrap();
        assert_eq!(std::fs::read_to_string(app.join("v")).unwrap(), "old", "nothing moves while the app runs");
        quitting.wait().unwrap();
        assert!(helper.wait().unwrap().success());
        assert_eq!(std::fs::read_to_string(app.join("v")).unwrap(), "new");
        assert!(opened.exists(), "the new one is opened");
        let left: Vec<_> = std::fs::read_dir(dir.join("Applications")).unwrap().flatten().map(|e| e.file_name()).collect();
        assert_eq!(left.len(), 1, "the old bundle is removed: {left:?}");
        assert!(!dir.join("staged-1").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn an_unsigned_bundle_is_refused() {
        let dir = std::env::temp_dir().join(format!("wundur-update-unsigned-{}", std::process::id()));
        let app = dir.join(APP_NAME);
        std::fs::create_dir_all(app.join("Contents/MacOS")).unwrap();
        std::fs::write(app.join("Contents/Info.plist"), r#"<?xml version="1.0"?><plist version="1.0"><dict><key>CFBundleShortVersionString</key><string>9.9.9</string></dict></plist>"#).unwrap();
        let err = verify_app(&app, "9.9.9").unwrap_err();
        assert!(err.contains("isn't signed by Wundur"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
