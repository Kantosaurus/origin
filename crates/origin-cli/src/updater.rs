//! Fully-automatic GitHub-release auto-updater for the `origin` binary.
//!
//! Every invocation of the CLI:
//!
//! 1. Calls [`apply_staged_if_present`] at startup. If a `<exe>.new` file is
//!    sitting next to the running binary (left there by a previous run's
//!    background check), it is renamed over the current executable. On
//!    Windows the live process keeps using the now-renamed `.old` file, so
//!    the swap is safe for a running process — the NEXT invocation picks up
//!    the new binary.
//! 2. Spawns [`run_background_check`] on the tokio runtime without awaiting
//!    it. The task hits `api.github.com`, compares versions, downloads the
//!    matching platform asset + its cosign sig bundle, verifies with the
//!    `cosign` CLI, and stages the result as `<exe>.new`. Failures are
//!    logged via `tracing::warn!` and never surface to the user.
//!
//! No knobs, no banners, no opt-out — by design. A 24h on-disk cache at
//! `$ORIGIN_HOME/.origin/update_check.json` (falling back to `~/.origin/`)
//! prevents hammering the GitHub API.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// How long a successful update-check result is reused before re-querying.
///
/// 24 hours — short enough to pick up new releases the day after publish,
/// long enough to stay well under GitHub's 60/hr unauthenticated rate
/// limit for typical CLI usage patterns.
pub const UPDATE_CHECK_TTL_SECS: i64 = 86_400;

/// GitHub repository slug the updater pulls releases from. Hardcoded so a
/// hostile `$ORIGIN_*` env var can never redirect the binary's auto-update
/// to a third-party mirror.
const RELEASES_REPO: &str = "Kantosaurus/origin";

/// HTTP timeout for the latest-release GET. Five seconds is long enough for
/// flaky links but short enough that a hung network never delays the user.
const HTTP_TIMEOUT_SECS: u64 = 5;

/// HTTP timeout for downloading a release asset. Asset binaries top out at a
/// few tens of MB; 60s covers slow links without blocking the next invocation
/// for an unreasonable time.
const DOWNLOAD_TIMEOUT_SECS: u64 = 60;

#[derive(Debug, Error)]
pub enum UpdateError {
    #[error("network: {0}")]
    Network(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("bad response: {0}")]
    BadResponse(String),
    #[error("no asset matches current target ({0})")]
    NoMatchingAsset(String),
    #[error("cosign not found on PATH — install cosign to enable auto-updates")]
    CosignMissing,
    #[error("cosign signature verification failed: {0}")]
    SignatureFailed(String),
    #[error("unsupported platform: {0}")]
    UnsupportedPlatform(String),
}

impl From<reqwest::Error> for UpdateError {
    fn from(e: reqwest::Error) -> Self {
        Self::Network(e.to_string())
    }
}

/// On-disk cache entry. Written by [`write_cache`], read by
/// [`cached_latest`]. The shape is forward-compatible: extra fields added in
/// future versions will be ignored by older binaries, and missing fields
/// produce a cache miss rather than a panic.
#[derive(Debug, Serialize, Deserialize)]
struct CacheEntry {
    /// Unix epoch seconds at write time.
    checked_at: i64,
    /// The latest version string fetched from GitHub. Stored without any
    /// leading `v` prefix to keep the format stable regardless of how the
    /// upstream tags releases.
    latest_version: String,
}

/// Resolve the cache file path. Honors `$ORIGIN_HOME` for tests and
/// alternate-root installs, matching the convention used by
/// `crates/origin-cli/src/config.rs::path`. Returns `None` only when
/// neither `$ORIGIN_HOME` nor a home directory can be resolved — in
/// practice every supported platform satisfies one of these.
fn cache_path() -> Option<PathBuf> {
    let home = std::env::var_os("ORIGIN_HOME")
        .map(PathBuf::from)
        .or_else(dirs::home_dir)?;
    Some(home.join(".origin").join("update_check.json"))
}

/// Current unix-epoch seconds. Wrapped so tests don't have to read the
/// system clock and so a clock-skewed system can't underflow the math.
fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

/// Strip a single leading `v` or `V` prefix, returning the version string
/// callers can hand to `semver::Version::parse`.
fn strip_v_prefix(s: &str) -> &str {
    s.strip_prefix('v').or_else(|| s.strip_prefix('V')).unwrap_or(s)
}

/// Read the on-disk cache.
///
/// Returns `Some(version)` (without a `v` prefix) when the cache exists,
/// parses, and was written within `ttl_secs`. Any other case — missing
/// file, parse error, stale entry — returns `None` so the caller falls
/// back to a live GitHub query.
#[must_use]
pub fn cached_latest(ttl_secs: i64) -> Option<String> {
    let path = cache_path()?;
    let bytes = std::fs::read(&path).ok()?;
    let entry: CacheEntry = serde_json::from_slice(&bytes).ok()?;
    let age = now_secs().saturating_sub(entry.checked_at);
    if age >= 0 && age < ttl_secs {
        Some(entry.latest_version)
    } else {
        None
    }
}

/// Write a cache entry.
///
/// Best-effort: any IO failure is logged via `tracing::warn!` and
/// swallowed — a missing or unwritable cache only costs an extra GitHub
/// query next invocation.
pub fn write_cache(version: &str) {
    if let Err(e) = write_cache_inner(version) {
        tracing::warn!("updater: write cache failed: {e}");
    }
}

/// Inner body so `write_cache` itself stays trivially-low cognitive
/// complexity; lets the public entry centralize logging and short-circuit
/// via `?`.
fn write_cache_inner(version: &str) -> Result<(), String> {
    let path = cache_path().ok_or_else(|| "no home directory".to_string())?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create cache dir: {e}"))?;
    }
    let entry = CacheEntry {
        checked_at: now_secs(),
        latest_version: strip_v_prefix(version).to_string(),
    };
    let buf = serde_json::to_vec(&entry).map_err(|e| format!("serialize: {e}"))?;
    std::fs::write(&path, buf).map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok(())
}

/// Compare two semver-ish version strings.
///
/// Returns `true` only when `latest` parses to a strictly greater semver
/// than `current`. Both sides accept an optional leading `v` (so `v0.1.0`
/// and `0.1.0` compare the same). On parse failure of either side, returns
/// `false` — the safer default: an unparseable version never triggers an
/// update.
#[must_use]
pub fn is_newer(current: &str, latest: &str) -> bool {
    let c = strip_v_prefix(current.trim());
    let l = strip_v_prefix(latest.trim());
    match (parse_semver(c), parse_semver(l)) {
        (Some(cv), Some(lv)) => lv > cv,
        _ => false,
    }
}

/// Minimal semver triple parser — enough for `MAJOR.MINOR.PATCH[-pre]`
/// comparison without taking a dep on `semver`. Pre-release suffixes are
/// stripped before parsing so `0.1.0-rc1` is treated as `0.1.0`.
fn parse_semver(s: &str) -> Option<(u64, u64, u64)> {
    let core = s.split('-').next()?.split('+').next()?;
    let mut parts = core.split('.');
    let major: u64 = parts.next()?.parse().ok()?;
    let minor: u64 = parts.next()?.parse().ok()?;
    let patch: u64 = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some((major, minor, patch))
}

/// Map the current build host's OS+ARCH to the released asset filename.
///
/// The release workflow (`.github/workflows/release.yml`) stages binaries
/// as `origin-<target-triple>[.exe]`, e.g. `origin-x86_64-pc-windows-msvc.exe`.
///
/// # Errors
/// [`UpdateError::UnsupportedPlatform`] when the host doesn't match one of
/// the six published targets (`x86_64`/`aarch64` cross `linux-musl` /
/// `apple-darwin` / `pc-windows-msvc`).
pub fn current_target_asset_name() -> Result<String, UpdateError> {
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;
    let (triple, ext) = match (os, arch) {
        ("linux", "x86_64") => ("x86_64-unknown-linux-musl", ""),
        ("linux", "aarch64") => ("aarch64-unknown-linux-musl", ""),
        ("macos", "x86_64") => ("x86_64-apple-darwin", ""),
        ("macos", "aarch64") => ("aarch64-apple-darwin", ""),
        ("windows", "x86_64") => ("x86_64-pc-windows-msvc", ".exe"),
        ("windows", "aarch64") => ("aarch64-pc-windows-msvc", ".exe"),
        _ => return Err(UpdateError::UnsupportedPlatform(format!("{os}/{arch}"))),
    };
    Ok(format!("origin-{triple}{ext}"))
}

/// User-Agent string sent on every GitHub API request. GitHub rejects
/// API calls without a UA. Format: `origin-cli/<package-version>`.
fn user_agent() -> String {
    format!("origin-cli/{}", env!("CARGO_PKG_VERSION"))
}

#[derive(Deserialize)]
struct GhRelease {
    tag_name: String,
    assets: Vec<GhAsset>,
}

#[derive(Deserialize)]
struct GhAsset {
    name: String,
    browser_download_url: String,
}

/// GET `https://api.github.com/repos/<RELEASES_REPO>/releases/latest` and
/// return the `tag_name` field (e.g. `"v0.1.0"`). Honors `HTTP_TIMEOUT_SECS`.
///
/// # Errors
/// [`UpdateError::Network`] on transport failure or non-2xx status;
/// [`UpdateError::BadResponse`] when the JSON shape doesn't match.
pub async fn fetch_latest_tag() -> Result<String, UpdateError> {
    let url = format!("https://api.github.com/repos/{RELEASES_REPO}/releases/latest");
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(HTTP_TIMEOUT_SECS))
        .user_agent(user_agent())
        .build()
        .map_err(|e| UpdateError::Network(e.to_string()))?;
    let resp = client
        .get(&url)
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .send()
        .await?;
    if !resp.status().is_success() {
        return Err(UpdateError::BadResponse(format!(
            "GET {url} returned {}",
            resp.status()
        )));
    }
    let release: GhRelease = resp
        .json()
        .await
        .map_err(|e| UpdateError::BadResponse(format!("decode releases JSON: {e}")))?;
    if release.tag_name.is_empty() {
        return Err(UpdateError::BadResponse("empty tag_name".into()));
    }
    let _ = release.assets; // Suppress dead-code lint; assets are consumed by `fetch_release` below.
    Ok(release.tag_name)
}

/// Hit the GitHub API and return the full release struct so callers can
/// also see the asset list (used by the download step to find the matching
/// `<name>` + `<name>.sig` pair). Separate from `fetch_latest_tag` so the
/// public API surface stays minimal — tests only need the tag.
async fn fetch_release() -> Result<GhRelease, UpdateError> {
    let url = format!("https://api.github.com/repos/{RELEASES_REPO}/releases/latest");
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(HTTP_TIMEOUT_SECS))
        .user_agent(user_agent())
        .build()
        .map_err(|e| UpdateError::Network(e.to_string()))?;
    let resp = client
        .get(&url)
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .send()
        .await?;
    if !resp.status().is_success() {
        return Err(UpdateError::BadResponse(format!(
            "GET {url} returned {}",
            resp.status()
        )));
    }
    resp.json::<GhRelease>()
        .await
        .map_err(|e| UpdateError::BadResponse(format!("decode releases JSON: {e}")))
}

/// Download `url` into `dest`. Uses a longer timeout than the API check so
/// slow links can complete the binary fetch without artificially failing.
async fn download_to(url: &str, dest: &std::path::Path) -> Result<(), UpdateError> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(DOWNLOAD_TIMEOUT_SECS))
        .user_agent(user_agent())
        .build()
        .map_err(|e| UpdateError::Network(e.to_string()))?;
    let resp = client.get(url).send().await?;
    if !resp.status().is_success() {
        return Err(UpdateError::BadResponse(format!(
            "GET {url} returned {}",
            resp.status()
        )));
    }
    let bytes = resp.bytes().await?;
    std::fs::write(dest, &bytes)?;
    Ok(())
}

/// Verify a downloaded asset against its cosign keyless `.sig` bundle.
/// Shells out to the `cosign` CLI rather than pulling in the heavy
/// `sigstore` Rust crate. Returns:
///
/// - [`UpdateError::CosignMissing`] if `cosign` isn't on PATH.
/// - [`UpdateError::SignatureFailed`] if the binary executes but exits non-zero.
fn cosign_verify(bundle: &std::path::Path, blob: &std::path::Path) -> Result<(), UpdateError> {
    let cosign = which::which("cosign").map_err(|_| UpdateError::CosignMissing)?;
    let output = std::process::Command::new(cosign)
        .arg("verify-blob")
        .arg("--bundle")
        .arg(bundle)
        .arg(blob)
        .output()
        .map_err(|e| UpdateError::SignatureFailed(format!("cosign spawn failed: {e}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        return Err(UpdateError::SignatureFailed(stderr));
    }
    Ok(())
}

/// Locate the running executable's path. Used by [`apply_staged_if_present`]
/// and [`run_background_check`] to derive the `<exe>.new` / `<exe>.old`
/// neighbor paths.
fn current_exe() -> Result<PathBuf, UpdateError> {
    std::env::current_exe().map_err(UpdateError::Io)
}

/// If a file exists at `<current_exe>.new`, atomically swap it into the
/// running binary's spot:
///
/// 1. Rename the current exe to `<exe>.old` (Windows lets the live process
///    keep using the renamed file).
/// 2. Rename `<exe>.new` to the original exe path.
///
/// Returns `Ok(true)` when a swap occurred, `Ok(false)` when no staged
/// file was found, and `Err` on IO failure mid-swap. Callers are expected
/// to ignore the error and continue — a partial swap is auto-healed on
/// the next invocation (either by re-detecting `.new` or by the OS leaving
/// `.old` to be cleaned up).
///
/// # Errors
/// [`UpdateError::Io`] on rename / canonicalize failures.
pub fn apply_staged_if_present() -> Result<bool, UpdateError> {
    let exe = current_exe()?;
    let staged = staged_path(&exe);
    if !staged.exists() {
        return Ok(false);
    }
    let old = old_path(&exe);
    // Best-effort cleanup of any prior `.old` so the rename below doesn't
    // collide with a stale file from an earlier swap.
    let _ = std::fs::remove_file(&old);
    std::fs::rename(&exe, &old)?;
    if let Err(e) = std::fs::rename(&staged, &exe) {
        // Try to undo the first rename so the user isn't left without a
        // binary. The undo is best-effort; if it fails the user can
        // recover by renaming `<exe>.old` back manually.
        let _ = std::fs::rename(&old, &exe);
        return Err(UpdateError::Io(e));
    }
    tracing::info!(
        "updater: swapped staged binary into place; previous binary preserved at {}",
        old.display()
    );
    Ok(true)
}

/// Helper: file name with `.{suffix}` appended (or `origin.{suffix}` if
/// the path has no file name component, which shouldn't happen in
/// practice but we degrade gracefully).
fn neighbor_path(exe: &std::path::Path, suffix: &str) -> PathBuf {
    let mut p = exe.to_path_buf();
    let name = exe
        .file_name()
        .map_or_else(|| "origin".to_string(), |n| n.to_string_lossy().into_owned());
    p.set_file_name(format!("{name}.{suffix}"));
    p
}

/// `<exe>.new` neighbor path.
fn staged_path(exe: &std::path::Path) -> PathBuf {
    neighbor_path(exe, "new")
}

/// `<exe>.old` neighbor path.
fn old_path(exe: &std::path::Path) -> PathBuf {
    neighbor_path(exe, "old")
}

/// Full background pass.
///
/// Checks the cache, fetches the latest tag, downloads + cosign-verifies
/// the platform asset, and stages it as `<exe>.new`. Logs every failure
/// mode via `tracing::warn!` and never panics — this runs from a detached
/// tokio task and must not surface errors to the user's command.
pub async fn run_background_check() {
    if let Err(e) = run_background_check_inner().await {
        tracing::warn!("updater: background check failed: {e}");
    }
}

/// Inner body so the public entry can centralize error logging. Keeps the
/// public signature `-> ()` (a detached task) while the inner logic uses
/// `?` for ergonomic short-circuiting.
async fn run_background_check_inner() -> Result<(), UpdateError> {
    let current = env!("CARGO_PKG_VERSION");

    // Cache hit: still write log line + skip download. We compare against
    // the cached version too so a cached "no update" answer short-circuits
    // without a network round trip.
    if let Some(cached) = cached_latest(UPDATE_CHECK_TTL_SECS) {
        if !is_newer(current, &cached) {
            return Ok(());
        }
        // Cache says there's a newer version, but we still need to fetch
        // the release struct to learn the asset URLs. Fall through.
    }

    let release = fetch_release().await?;
    let latest = release.tag_name.clone();
    write_cache(&latest);

    if !is_newer(current, &latest) {
        return Ok(());
    }

    let asset_name = current_target_asset_name()?;
    let sig_name = format!("{asset_name}.sig");

    let asset = release
        .assets
        .iter()
        .find(|a| a.name == asset_name)
        .ok_or_else(|| UpdateError::NoMatchingAsset(asset_name.clone()))?;
    let sig = release
        .assets
        .iter()
        .find(|a| a.name == sig_name)
        .ok_or_else(|| UpdateError::NoMatchingAsset(sig_name.clone()))?;

    tracing::info!(
        "updater: update available (current={current} latest={latest}); downloading {asset_name}"
    );

    // Stage into a temp dir adjacent to the exe so the final rename stays
    // on the same filesystem (rename across filesystems is not atomic on
    // Windows and can fail on Linux).
    let exe = current_exe()?;
    let parent = exe
        .parent()
        .ok_or_else(|| UpdateError::Io(std::io::Error::new(std::io::ErrorKind::Other, "exe has no parent")))?;
    let download_path = parent.join(format!("{asset_name}.download"));
    let sig_path = parent.join(sig_name);

    download_to(&asset.browser_download_url, &download_path).await?;
    download_to(&sig.browser_download_url, &sig_path).await?;

    if let Err(e) = cosign_verify(&sig_path, &download_path) {
        // Clean up unverified files before bubbling the error. Leaving an
        // unverified binary on disk would invite later auto-staging code
        // to pick it up.
        let _ = std::fs::remove_file(&download_path);
        let _ = std::fs::remove_file(&sig_path);
        return Err(e);
    }
    // Sig is no longer needed once verification passed; keep the staged
    // binary only.
    let _ = std::fs::remove_file(&sig_path);

    let staged = staged_path(&exe);
    std::fs::rename(&download_path, &staged)?;
    tracing::info!(
        "updater: staged {} for next invocation (will swap on startup)",
        staged.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    use tokio::sync::Mutex;

    // All cache tests mutate the process-global `ORIGIN_HOME` env var.
    // cargo test runs in parallel by default, so without serialization they
    // race. A tokio Mutex is async-aware and safe to hold across awaits
    // (the workspace `clippy::await_holding_lock` lint flags std::sync::Mutex
    // here). Pattern lifted from `crates/origin-browser/src/web_search.rs`.
    static ENV_LOCK: Mutex<()> = Mutex::const_new(());

    #[test]
    fn is_newer_recognizes_patch_bump() {
        assert!(is_newer("0.0.1", "0.0.2"));
    }

    #[test]
    fn is_newer_recognizes_minor_bump() {
        assert!(is_newer("0.0.9", "0.1.0"));
    }

    #[test]
    fn is_newer_handles_v_prefix() {
        assert!(is_newer("v0.0.1", "v0.0.2"));
        assert!(is_newer("0.0.1", "v0.0.2"));
        assert!(is_newer("v0.0.1", "0.0.2"));
    }

    #[test]
    fn is_newer_returns_false_for_equal() {
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(!is_newer("v0.1.0", "0.1.0"));
    }

    #[test]
    fn is_newer_returns_false_for_older() {
        assert!(!is_newer("0.2.0", "0.1.0"));
        assert!(!is_newer("v1.0.0", "v0.9.9"));
    }

    #[test]
    fn is_newer_returns_false_on_unparseable() {
        // Parse failure must NEVER trigger an update — safer default.
        assert!(!is_newer("garbage", "0.1.0"));
        assert!(!is_newer("0.1.0", "also garbage"));
        assert!(!is_newer("not.a.version", "0.0.1"));
    }

    #[test]
    #[allow(clippy::panic)] // test ergonomics: bail loudly on an unsupported test host
    fn current_target_asset_name_includes_target_triple() {
        let name = current_target_asset_name().expect("supported test host");
        // Assert presence of an OS substring matching the test host.
        let os = std::env::consts::OS;
        let needle = match os {
            "windows" => "windows",
            "linux" => "linux",
            "macos" => "darwin",
            other => panic!("unexpected test host OS: {other}"),
        };
        assert!(name.contains(needle), "asset {name} should contain {needle}");
        assert!(name.starts_with("origin-"), "asset {name} should start with origin-");
    }

    #[tokio::test]
    async fn cache_round_trips() {
        let _g = ENV_LOCK.lock().await;
        let tmp = tempdir().expect("tempdir");
        std::env::set_var("ORIGIN_HOME", tmp.path());

        // Empty cache returns None.
        assert!(cached_latest(UPDATE_CHECK_TTL_SECS).is_none());

        // Round-trip a write.
        write_cache("v0.1.0");
        let v = cached_latest(UPDATE_CHECK_TTL_SECS).expect("cache hit");
        assert_eq!(v, "0.1.0", "v-prefix should be stripped on write");

        // Within-TTL hit.
        let v = cached_latest(60).expect("within ttl");
        assert_eq!(v, "0.1.0");

        // Stale TTL miss: passing 0 means "expire everything".
        assert!(
            cached_latest(0).is_none(),
            "TTL of 0 should always miss"
        );

        std::env::remove_var("ORIGIN_HOME");
    }

    #[test]
    fn strip_v_prefix_handles_both_cases() {
        assert_eq!(strip_v_prefix("v1.2.3"), "1.2.3");
        assert_eq!(strip_v_prefix("V1.2.3"), "1.2.3");
        assert_eq!(strip_v_prefix("1.2.3"), "1.2.3");
        assert_eq!(strip_v_prefix(""), "");
    }

    #[test]
    fn parse_semver_strips_prerelease() {
        assert_eq!(parse_semver("0.1.0-rc1"), Some((0, 1, 0)));
        assert_eq!(parse_semver("1.2.3"), Some((1, 2, 3)));
        assert_eq!(parse_semver("1.2"), None);
        assert_eq!(parse_semver("1.2.3.4"), None);
    }

    #[test]
    fn staged_and_old_paths_append_suffix() {
        let p = std::path::Path::new("/tmp/origin");
        assert_eq!(staged_path(p), std::path::PathBuf::from("/tmp/origin.new"));
        assert_eq!(old_path(p), std::path::PathBuf::from("/tmp/origin.old"));

        let pe = std::path::Path::new("C:/bin/origin.exe");
        assert_eq!(
            staged_path(pe),
            std::path::PathBuf::from("C:/bin/origin.exe.new")
        );
        assert_eq!(
            old_path(pe),
            std::path::PathBuf::from("C:/bin/origin.exe.old")
        );
    }
}
