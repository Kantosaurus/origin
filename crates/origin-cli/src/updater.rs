// SPDX-License-Identifier: Apache-2.0
//! Fully-automatic **npm-channel** auto-updater for the `origin` binary.
//!
//! Update decisions are driven entirely by npm: the *installed* version is read
//! from the npm package metadata shipped next to the binary, and the *latest*
//! version is read from the npm registry. There is **no `cosign` dependency** —
//! downloads are integrity-checked against the release's `SHA256SUMS` manifest
//! with the built-in `sha2` hasher, so auto-update works with zero external
//! tools installed.
//!
//! Every invocation of the CLI:
//!
//! 1. Calls [`apply_staged_if_present`] at startup. If a `<exe>.new` file is
//!    sitting next to the running binary (left there by a previous run's
//!    background check), it is renamed over the current executable. On Windows
//!    the live process keeps using the now-renamed `.old` file, so the swap is
//!    safe for a running process. This is a fast, local rename — no network.
//! 2. Calls [`spawn_background_update_worker`], which spawns a **detached child
//!    process** that performs the network check + download + stage in the
//!    background, then returns immediately. Startup is never blocked on the
//!    network, and the foreground process never re-execs mid-session: a freshly
//!    downloaded binary is staged as `<exe>.new` and swapped in by the *next*
//!    launch's apply step (1). The worker (the binary re-invoked with
//!    [`SELF_UPDATE_WORKER_ENV`] set) runs [`check_and_stage_blocking`], which
//!    guards on install type: auto-update only runs for binaries distributed via
//!    npm (the running exe lives under a `node_modules` tree). Dev/source builds
//!    (cargo `target/`), `cargo install` (`~/.cargo/bin`), and direct downloads
//!    are left untouched so a local build is never clobbered, and the installed
//!    version is read from the adjacent npm `package.json`. It then checks the
//!    npm registry (`registry.npmjs.org/<pkg>/latest`) for the latest published
//!    version; when newer, it downloads the matching platform asset from the
//!    GitHub release for that version (where the npm channel also sources its
//!    binaries), verifies its SHA-256 against the release `SHA256SUMS`, and
//!    stages the result as `<exe>.new`. Failures are logged via `tracing::warn!`
//!    and degraded to `Ok(false)` so offline / network-flaky users still run.
//!
//! Because the check is fully off the startup hot path (a detached worker, not a
//! blocking call), the on-disk cache TTL is short ([`UPDATE_CHECK_TTL_SECS`], 1h)
//! — it only throttles how often a worker is spawned, so a same-day release is
//! picked up within ~an hour of publish rather than the up-to-24h lag a blocking
//! check forced. The cache lives at `$ORIGIN_HOME/.origin/update_check.json`
//! (falling back to `~/.origin/`).
//!
//! Setting `ORIGIN_NO_UPDATE=1` (any value) short-circuits the apply, the worker
//! spawn, and the worker's own network check — the binary then behaves as if the
//! updater were absent.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// How long a successful update-check result is reused before re-querying.
///
/// 1 hour. The check now runs in a *detached background worker* off the startup
/// hot path (see [`spawn_background_update_worker`]), so this TTL no longer
/// trades startup latency for freshness — it only rate-limits how often a worker
/// is spawned across launches. A short value means a same-day release is picked
/// up within ~an hour, fixing the up-to-24h lag the old blocking check forced
/// (a 24h TTL once masked a release that published minutes after the last
/// check). Because the gate is evaluated once per launch, real-world spawn
/// frequency stays low even at 1h. Keep this small; a regression test guards it.
pub const UPDATE_CHECK_TTL_SECS: i64 = 3_600;

// Compile-time guard against re-introducing the up-to-24h lag: the background
// worker makes a short TTL cheap, so keep it within 1h. Bumping it back toward
// 24h (which once masked v0.9.5 for ~a day) fails the build right here.
const _: () = assert!(UPDATE_CHECK_TTL_SECS > 0 && UPDATE_CHECK_TTL_SECS <= 3_600);

/// Environment variable that marks a process as the detached background update
/// worker.
///
/// When set, the CLI performs a single check + download + stage and exits
/// *without* parsing args or re-exec'ing — the staged binary is applied by the
/// next foreground launch's [`apply_staged_if_present`]. Set only by
/// [`build_worker_command`]; never meant to be set by a user.
pub const SELF_UPDATE_WORKER_ENV: &str = "ORIGIN_SELF_UPDATE_WORKER";

/// GitHub repository slug the binary *assets* are downloaded from. Hardcoded so
/// a hostile `$ORIGIN_*` env var can never redirect the binary's auto-update to
/// a third-party mirror. (npm only supplies the version number; the bytes come
/// from this release, sha256-verified.)
const RELEASES_REPO: &str = "Kantosaurus/origin";

/// npm registry base URL the *latest version* is read from.
const NPM_REGISTRY: &str = "https://registry.npmjs.org";

/// Scoped npm package whose published version drives the update decision.
const NPM_PACKAGE: &str = "@kantosaurus/origin";

/// HTTP timeout for the latest-version GET. Five seconds is long enough for
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
    #[error("checksum verification failed: {0}")]
    ChecksumFailed(String),
    #[error("unsupported platform: {0}")]
    UnsupportedPlatform(String),
}

impl From<reqwest::Error> for UpdateError {
    fn from(e: reqwest::Error) -> Self {
        Self::Network(e.to_string())
    }
}

/// On-disk cache entry. Written by [`write_cache`], read by [`cached_latest`].
/// The shape is forward-compatible: extra fields added in future versions are
/// ignored by older binaries, and missing fields produce a cache miss rather
/// than a panic.
#[derive(Debug, Serialize, Deserialize)]
struct CacheEntry {
    /// Unix epoch seconds at write time.
    checked_at: i64,
    /// The latest version string fetched from npm. Stored without any leading
    /// `v` prefix to keep the format stable.
    latest_version: String,
}

/// Resolve the cache file path. Honors `$ORIGIN_HOME` for tests and
/// alternate-root installs, matching the convention used by
/// `crates/origin-cli/src/config.rs::path`. Returns `None` only when neither
/// `$ORIGIN_HOME` nor a home directory can be resolved.
fn cache_path() -> Option<PathBuf> {
    let home = std::env::var_os("ORIGIN_HOME")
        .map(PathBuf::from)
        .or_else(dirs::home_dir)?;
    Some(home.join(".origin").join("update_check.json"))
}

/// Resolve the persistent update-log path (`$ORIGIN_HOME/.origin/update.log`,
/// falling back to `~/.origin/`). Sibling of [`cache_path`].
fn update_log_path() -> Option<PathBuf> {
    let home = std::env::var_os("ORIGIN_HOME")
        .map(PathBuf::from)
        .or_else(dirs::home_dir)?;
    Some(home.join(".origin").join("update.log"))
}

/// Append one timestamped line to the persistent update log.
///
/// The background worker is otherwise COMPLETELY silent — it runs detached with
/// null stdio, and the CLI installs no `tracing` subscriber, so every
/// `tracing::warn!`/`eprintln!` in the worker is dropped. That blackout is why a
/// worker that checks-but-never-stages (a transient download failure) leaves no
/// trace and can't be diagnosed after the fact. This log fixes that: every check
/// records whether an update was found and whether the download/stage succeeded
/// or why it failed.
///
/// Best-effort: any IO error is swallowed — the log is for diagnosis, never
/// load-bearing for the update itself.
pub fn update_log(msg: &str) {
    use std::io::Write as _;
    let Some(path) = update_log_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(f, "{} {msg}", now_secs());
    }
}

/// Retry an async fallible operation up to `attempts` times, sleeping `backoff`
/// between tries; returns the first `Ok`, else the last `Err`. `attempts` is
/// clamped to at least 1.
///
/// The multi-binary stage is all-or-nothing: if ANY of the three downloads
/// fails, nothing is staged and (because the worker is silent) the user is left
/// on the old version with no signal. A single transient blip — a flaky link, or
/// a scanning proxy resetting a large transfer — is exactly that failure mode. A
/// bounded retry turns those transient failures into successes.
async fn retry_async<T, E, F, Fut>(attempts: u32, backoff: std::time::Duration, mut f: F) -> Result<T, E>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, E>>,
{
    let tries = attempts.max(1);
    let mut last_err: Option<E> = None;
    for i in 0..tries {
        match f().await {
            Ok(v) => return Ok(v),
            Err(e) => {
                last_err = Some(e);
                if i + 1 < tries {
                    tokio::time::sleep(backoff).await;
                }
            }
        }
    }
    Err(last_err.expect("the loop runs at least once, so an Err was recorded"))
}

/// Current unix-epoch seconds. Wrapped so tests don't have to read the system
/// clock and so a clock-skewed system can't underflow the math.
fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

/// Strip a single leading `v` or `V` prefix.
fn strip_v_prefix(s: &str) -> &str {
    s.strip_prefix('v').or_else(|| s.strip_prefix('V')).unwrap_or(s)
}

/// Read the on-disk cache.
///
/// Returns `Some(version)` (without a `v` prefix) when the cache exists,
/// parses, and was written within `ttl_secs`. Any other case — missing file,
/// parse error, stale entry — returns `None` so the caller falls back to a live
/// npm query.
#[must_use]
pub fn cached_latest(ttl_secs: i64) -> Option<String> {
    let path = cache_path()?;
    let bytes = std::fs::read(&path).ok()?;
    let entry: CacheEntry = serde_json::from_slice(&bytes).ok()?;
    let age = now_secs().saturating_sub(entry.checked_at);
    // Reject a non-semver cached value so a corrupt or tampered cache file can't
    // wedge the check (is_newer treats an unparseable version as "not newer").
    let parseable = parse_semver(strip_v_prefix(entry.latest_version.trim())).is_some();
    if age >= 0 && age < ttl_secs && parseable {
        Some(entry.latest_version)
    } else {
        None
    }
}

/// Write a cache entry.
///
/// Best-effort: any IO failure is logged via `tracing::warn!` and swallowed — a
/// missing or unwritable cache only costs an extra npm query next invocation.
pub fn write_cache(version: &str) {
    if let Err(e) = write_cache_inner(version) {
        tracing::warn!("updater: write cache failed: {e}");
    }
}

/// Inner body so `write_cache` itself stays trivially-low cognitive complexity.
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
/// Returns `true` only when `latest` parses to a strictly greater semver than
/// `current`. Both sides accept an optional leading `v`. On parse failure of
/// either side, returns `false` — the safer default: an unparseable version
/// never triggers an update.
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

/// Map the current build host's OS+ARCH to its released target triple and binary
/// extension. Shared by every per-binary asset-name builder.
///
/// # Errors
/// [`UpdateError::UnsupportedPlatform`] when the host doesn't match a published
/// target.
fn target_triple_ext() -> Result<(&'static str, &'static str), UpdateError> {
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;
    Ok(match (os, arch) {
        ("linux", "x86_64") => ("x86_64-unknown-linux-gnu", ""),
        ("linux", "aarch64") => ("aarch64-unknown-linux-gnu", ""),
        ("macos", "x86_64") => ("x86_64-apple-darwin", ""),
        ("macos", "aarch64") => ("aarch64-apple-darwin", ""),
        ("windows", "x86_64") => ("x86_64-pc-windows-msvc", ".exe"),
        ("windows", "aarch64") => ("aarch64-pc-windows-msvc", ".exe"),
        _ => return Err(UpdateError::UnsupportedPlatform(format!("{os}/{arch}"))),
    })
}

/// Map the current build host's OS+ARCH to the released **CLI** asset filename.
///
/// The release workflow (`.github/workflows/release.yml`) stages binaries as
/// `origin-<target-triple>[.exe]`, e.g. `origin-x86_64-pc-windows-msvc.exe`.
///
/// # Errors
/// [`UpdateError::UnsupportedPlatform`] when the host doesn't match one of the
/// published targets.
pub fn current_target_asset_name() -> Result<String, UpdateError> {
    let (triple, ext) = target_triple_ext()?;
    Ok(format!("origin-{triple}{ext}"))
}

/// One binary the self-updater keeps in lockstep: its GitHub-release asset
/// basename and the on-disk path the staged `.new` is applied over.
struct UpdateTarget {
    /// Release asset filename, e.g. `origin-daemon-x86_64-pc-windows-msvc.exe`.
    asset_name: String,
    /// Destination path the staged `.new` is swapped over.
    dest: PathBuf,
}

/// The full set of binaries one update swaps as a unit: the CLI (the running
/// `cli_exe`) plus its sibling `origin-daemon` and `origin-supervisor` in the
/// same `bin/` directory (the npm platform-package layout, where the daemon and
/// supervisor live alongside the CLI).
///
/// Ordered **daemon, supervisor, CLI** so the apply step swaps the companions
/// before the CLI — the CLI is the binary the next launch re-runs and the one
/// whose newer mtime makes `ensure_daemon_running` restart the daemon, so its
/// companions must already be in place. Fixing only the CLI (the old behavior)
/// left a self-updated install running a stale daemon indefinitely.
///
/// Falls back to the CLI alone when the exe has no parent directory, preserving
/// single-binary behavior for unusual layouts.
///
/// # Errors
/// [`UpdateError::UnsupportedPlatform`] on an unpublished host.
fn update_targets(cli_exe: &Path) -> Result<Vec<UpdateTarget>, UpdateError> {
    let (triple, ext) = target_triple_ext()?;
    let cli = UpdateTarget {
        asset_name: format!("origin-{triple}{ext}"),
        dest: cli_exe.to_path_buf(),
    };
    let Some(dir) = cli_exe.parent() else {
        return Ok(vec![cli]);
    };
    Ok(vec![
        UpdateTarget {
            asset_name: format!("origin-daemon-{triple}{ext}"),
            dest: dir.join(format!("origin-daemon{ext}")),
        },
        UpdateTarget {
            asset_name: format!("origin-supervisor-{triple}{ext}"),
            dest: dir.join(format!("origin-supervisor{ext}")),
        },
        cli,
    ])
}

/// `https://github.com/<repo>/releases/download/v<version>/<asset>` — the exact
/// URL the npm channel's `download.js` uses. npm supplies the version; the
/// bytes come from the GitHub release and are sha256-verified.
fn release_asset_url(version: &str, asset_name: &str) -> String {
    let v = strip_v_prefix(version.trim());
    format!("https://github.com/{RELEASES_REPO}/releases/download/v{v}/{asset_name}")
}

/// User-Agent string sent on every HTTP request. Format:
/// `origin-cli/<package-version>`.
fn user_agent() -> String {
    format!("origin-cli/{}", env!("CARGO_PKG_VERSION"))
}

// ── installed-version discovery (npm-channel guard) ───────────────────────────

/// Minimal `package.json` shape — `name` (to confirm it is origin's own npm
/// package) and `version`.
#[derive(Deserialize)]
struct PackageMeta {
    #[serde(default)]
    name: String,
    #[serde(default)]
    version: String,
}

/// npm registry `…/latest` manifest — we only read `version`.
#[derive(Deserialize)]
struct NpmManifest {
    version: String,
}

/// The npm-installed version of the running binary, or `None` when this is NOT
/// an npm-managed install (and therefore must not auto-update).
///
/// Returns `None` for dev/source builds, `cargo install`, and direct
/// downloads — auto-update is scoped to the npm distribution channel so a local
/// build is never clobbered and an unknown version never loops.
fn installed_npm_version() -> Option<String> {
    npm_version_for_exe(&current_exe().ok()?)
}

/// Pure core of [`installed_npm_version`], split out so it's testable without
/// stubbing `current_exe`.
fn npm_version_for_exe(exe: &Path) -> Option<String> {
    find_origin_package_json(exe).map(|(_, v)| v)
}

/// Locate origin's OWN `package.json` by walking up from `exe`, returning its
/// path and version. The binary must live under a `node_modules` tree (the npm
/// install marker); the package must be `@kantosaurus/origin` or one of its
/// `@kantosaurus/origin-<platform>` binary packages with a parseable semver — so
/// the walk can't latch onto a stray parent-project `package.json` or trust a
/// spoofed version (an unparseable version would wedge `is_newer`, which treats
/// it as "not newer").
fn find_origin_package_json(exe: &Path) -> Option<(PathBuf, String)> {
    if !exe.components().any(|c| c.as_os_str() == "node_modules") {
        return None;
    }
    let mut dir = exe.parent();
    while let Some(d) = dir {
        let pj = d.join("package.json");
        if let Ok(bytes) = std::fs::read(&pj) {
            if let Ok(meta) = serde_json::from_slice::<PackageMeta>(&bytes) {
                let is_origin = meta.name == NPM_PACKAGE || meta.name.starts_with("@kantosaurus/origin-");
                let v = meta.version.trim();
                if is_origin && parse_semver(strip_v_prefix(v)).is_some() {
                    return Some((pj, v.to_string()));
                }
            }
        }
        dir = d.parent();
    }
    None
}

/// After staging an updated binary, write the new version into origin's own
/// `package.json` so the next launch's installed-version read matches the
/// swapped-in binary.
///
/// This is what makes the self-updater the safe default: npm rewrites
/// `package.json` only on its own install/update, but a self-update swaps just
/// the binary. Without recording the new version here, the version source would
/// keep reporting the pre-update version and the updater would re-download the
/// same release on every launch. Best-effort — a failure only costs one
/// redundant check next run (the staged-binary swap itself is unaffected).
fn record_staged_version(exe: &Path, version: &str) {
    let Some((pj, _)) = find_origin_package_json(exe) else {
        return;
    };
    let Ok(bytes) = std::fs::read(&pj) else {
        return;
    };
    let Ok(mut json) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return;
    };
    let Some(obj) = json.as_object_mut() else {
        return;
    };
    obj.insert(
        "version".to_string(),
        serde_json::Value::String(strip_v_prefix(version.trim()).to_string()),
    );
    if let Ok(buf) = serde_json::to_vec_pretty(&json) {
        let _ = std::fs::write(&pj, buf);
    }
}

/// GET `https://registry.npmjs.org/<pkg>/latest` and return its `version`.
///
/// # Errors
/// [`UpdateError::Network`] on transport failure; [`UpdateError::BadResponse`]
/// on non-2xx status or a JSON shape without a non-empty `version`.
pub async fn fetch_latest_npm_version() -> Result<String, UpdateError> {
    let url = format!("{NPM_REGISTRY}/{NPM_PACKAGE}/latest");
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(HTTP_TIMEOUT_SECS))
        .user_agent(user_agent())
        .build()
        .map_err(|e| UpdateError::Network(e.to_string()))?;
    let resp = client
        .get(&url)
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .await?;
    if !resp.status().is_success() {
        return Err(UpdateError::BadResponse(format!(
            "GET {url} returned {}",
            resp.status()
        )));
    }
    let manifest: NpmManifest = resp
        .json()
        .await
        .map_err(|e| UpdateError::BadResponse(format!("decode npm manifest JSON: {e}")))?;
    if manifest.version.trim().is_empty() {
        return Err(UpdateError::BadResponse("empty npm version".into()));
    }
    Ok(manifest.version)
}

// ── download + verify + stage ─────────────────────────────────────────────────

/// Download `url` fully into memory. Uses a longer timeout than the version
/// check so slow links can complete the binary fetch without artificially
/// failing. Returning bytes (rather than writing a file) lets us verify the
/// SHA-256 of exactly what we'll stage, closing the verify→stage swap window.
/// How many times each asset download is attempted before giving up, and the
/// pause between tries. The all-or-nothing stage means one transient failure
/// aborts the whole update, so a small bounded retry materially raises the odds
/// all three binaries land in one worker run.
const DOWNLOAD_ATTEMPTS: u32 = 3;
const DOWNLOAD_RETRY_BACKOFF: std::time::Duration = std::time::Duration::from_secs(2);

/// Download `url` fully into memory, retrying transient failures (see
/// [`retry_async`] / [`DOWNLOAD_ATTEMPTS`]).
async fn download_bytes(url: &str) -> Result<Vec<u8>, UpdateError> {
    retry_async(DOWNLOAD_ATTEMPTS, DOWNLOAD_RETRY_BACKOFF, || download_once(url)).await
}

/// One download attempt: build a client, GET, and read the body. A non-2xx
/// status or transport error is returned as an error so [`retry_async`] can
/// retry it.
async fn download_once(url: &str) -> Result<Vec<u8>, UpdateError> {
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
    Ok(resp.bytes().await?.to_vec())
}

/// Compute the lowercase hex SHA-256 of `bytes` using the built-in `sha2`
/// hasher. No external tool required.
fn sha256_hex_bytes(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(64);
    for b in digest {
        use std::fmt::Write as _;
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// Extract the expected lowercase SHA-256 for `file_name` from `sha256sum`-style
/// manifest text. Each line is `<64-hex><spaces>[*]<name>` — matching GNU
/// coreutils' text (`  `) and binary (` *`) formats, and the shape produced by
/// the release workflow's `SHA256SUMS` step. Comparison is on the basename so a
/// manifest entry of either `origin-<triple>` or `dist/origin-<triple>`
/// resolves. Returns `None` when the manifest has no matching row.
fn expected_hash_for(sums_text: &str, file_name: &str) -> Option<String> {
    for line in sums_text.lines() {
        let line = line.trim();
        let mut it = line.splitn(2, char::is_whitespace);
        let hex = it.next()?;
        let rest = it.next()?.trim_start();
        if hex.len() != 64 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            continue;
        }
        let name = rest.strip_prefix('*').unwrap_or(rest);
        let base = Path::new(name)
            .file_name()
            .map_or(name, |n| n.to_str().unwrap_or(name));
        if base == file_name {
            return Some(hex.to_ascii_lowercase());
        }
    }
    None
}

/// Download the release's `SHA256SUMS` manifest text. Best-effort: returns
/// `None` when the release doesn't publish one or it's unreachable.
async fn fetch_checksums(version: &str) -> Option<String> {
    let v = strip_v_prefix(version.trim());
    let url = format!("https://github.com/{RELEASES_REPO}/releases/download/v{v}/SHA256SUMS");
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(HTTP_TIMEOUT_SECS))
        .user_agent(user_agent())
        .build()
        .ok()?;
    let resp = client.get(&url).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    resp.text().await.ok()
}

/// Verify freshly-downloaded `bytes` against the release `SHA256SUMS`.
///
/// Verification is **mandatory** now that `cosign` is gone: an unverifiable
/// download (no manifest, or no matching entry) is rejected rather than staged,
/// so a missing/garbled `SHA256SUMS` never lets an unchecked binary be swapped
/// in.
///
/// # Errors
/// [`UpdateError::ChecksumFailed`] when no manifest entry exists or the digest
/// doesn't match.
fn verify_sha256_bytes(bytes: &[u8], checksums: Option<&str>, asset_name: &str) -> Result<(), UpdateError> {
    let expected = checksums
        .and_then(|text| expected_hash_for(text, asset_name))
        .ok_or_else(|| {
            UpdateError::ChecksumFailed(format!(
                "no SHA256SUMS entry for {asset_name}; refusing to stage an unverified binary"
            ))
        })?;
    let actual = sha256_hex_bytes(bytes);
    if !actual.eq_ignore_ascii_case(&expected) {
        return Err(UpdateError::ChecksumFailed(format!(
            "expected {expected}, computed {actual}"
        )));
    }
    tracing::info!("updater: verified {asset_name} via SHA256SUMS checksum");
    Ok(())
}

/// Locate the running executable's path. Used to derive the `<exe>.new` /
/// `<exe>.old` neighbor paths and to read the adjacent npm `package.json`.
fn current_exe() -> Result<PathBuf, UpdateError> {
    std::env::current_exe().map_err(UpdateError::Io)
}

/// Swap in any binaries staged by a prior background worker — the CLI **and** its
/// `origin-daemon`/`origin-supervisor` siblings — by renaming each `<dest>.new`
/// over `<dest>`.
///
/// Companions are applied before the CLI (the [`update_targets`] order) so the
/// daemon/supervisor binaries are current by the time the CLI's newer mtime makes
/// `ensure_daemon_running` restart the daemon. Each binary is an independent
/// no-op when its `.new` is absent, so a CLI-only stage (or a missing sibling)
/// still works. Returns `Ok(true)` when at least one swap occurred.
///
/// **Partial-apply safety:** the CLI is the *last* target, so it is swapped only
/// after its companions succeed — we never advance the CLI past an old daemon
/// (the dangerous "new CLI / old daemon" skew). If a swap fails mid-sequence the
/// call returns `Err`, but the unapplied `.new` files persist and the next launch
/// re-applies them (and `apply_one_staged` restores a dest it moved aside), so the
/// state self-corrects rather than corrupting.
///
/// # Errors
/// [`UpdateError::Io`] on a rename failure mid-swap.
pub fn apply_staged_if_present() -> Result<bool, UpdateError> {
    if std::env::var_os("ORIGIN_NO_UPDATE").is_some() {
        return Ok(false);
    }
    let cli_exe = current_exe()?;
    let targets = update_targets(&cli_exe).unwrap_or_else(|e| {
        tracing::debug!("updater: update_targets failed ({e}); applying the CLI alone");
        vec![UpdateTarget {
            asset_name: String::new(),
            dest: cli_exe.clone(),
        }]
    });
    let mut applied = false;
    for t in &targets {
        applied |= apply_one_staged(&t.dest)?;
    }
    if applied {
        update_log("applied staged update on launch");
    }
    Ok(applied)
}

/// Atomically swap a single `<dest>.new` over `dest`:
///
/// 1. Rename the current `dest` to `<dest>.old` (Windows lets a live process keep
///    using the renamed file, so this is safe even while the daemon runs).
/// 2. Rename `<dest>.new` to `dest`.
///
/// Returns `Ok(false)` when no `.new` is staged. When `dest` itself is missing
/// (a sibling that was never installed) the `.new` is moved into place directly.
///
/// # Errors
/// [`UpdateError::Io`] on a rename failure mid-swap (the original is restored).
fn apply_one_staged(dest: &Path) -> Result<bool, UpdateError> {
    let staged = staged_path(dest);
    if !staged.exists() {
        return Ok(false);
    }
    let old = old_path(dest);
    let _ = std::fs::remove_file(&old);
    let moved_aside = dest.exists();
    if moved_aside {
        std::fs::rename(dest, &old)?;
    }
    if let Err(e) = std::fs::rename(&staged, dest) {
        if moved_aside {
            if let Err(re) = std::fs::rename(&old, dest) {
                // Both renames failed: `dest` is momentarily absent, but `.new`
                // still exists, so the next launch's apply moves it into place
                // directly (the `dest.exists()` branch). Log loudly all the same.
                tracing::warn!(
                    "updater: staged-swap of {} failed and rollback also failed ({re}); \
                     next launch will re-apply from {}.new",
                    dest.display(),
                    dest.display()
                );
            }
        }
        return Err(UpdateError::Io(e));
    }
    tracing::info!(
        "updater: swapped staged binary {} into place; previous preserved at {}",
        dest.display(),
        old.display()
    );
    Ok(true)
}

/// Helper: file name with `.{suffix}` appended (or `origin.{suffix}` if the
/// path has no file name component).
fn neighbor_path(exe: &Path, suffix: &str) -> PathBuf {
    let mut p = exe.to_path_buf();
    let name = exe
        .file_name()
        .map_or_else(|| "origin".to_string(), |n| n.to_string_lossy().into_owned());
    p.set_file_name(format!("{name}.{suffix}"));
    p
}

/// `<exe>.new` neighbor path.
fn staged_path(exe: &Path) -> PathBuf {
    neighbor_path(exe, "new")
}

/// `<exe>.old` neighbor path.
fn old_path(exe: &Path) -> PathBuf {
    neighbor_path(exe, "old")
}

/// Synchronous update check that resolves to `Ok(true)` iff a new binary was
/// downloaded, verified, and staged this call.
///
/// Returns `Ok(false)` when there's nothing to do (not an npm install, cache
/// fresh, no newer release, already up-to-date, or `ORIGIN_NO_UPDATE` is set).
/// Network and verification failures all resolve to `Ok(false)` (with a
/// `tracing::warn`) so the caller can fall through to running the current
/// binary.
///
/// # Errors
/// Currently never; the signature reserves room for future failure modes.
pub async fn check_and_stage_blocking() -> Result<bool, UpdateError> {
    if std::env::var_os("ORIGIN_NO_UPDATE").is_some() {
        return Ok(false);
    }
    update_log("worker: check started");
    Ok(check_and_stage_inner().await)
}

// ── background (non-blocking) update worker ───────────────────────────────────

/// Pure policy for [`spawn_background_update_worker`]: should a background update
/// worker be spawned this launch?
///
/// Split out from the side-effecting spawn so the decision is unit-testable
/// without touching the network, the filesystem, or spawning a process.
///
/// - `disabled`: `ORIGIN_NO_UPDATE` is set → never update.
/// - `is_npm_install`: the running binary is an npm-managed install → only these
///   auto-update (a dev/source build is left untouched).
/// - `cache_fresh`: a check ran within [`UPDATE_CHECK_TTL_SECS`] → skip, so rapid
///   relaunches don't each spawn a worker. The TTL is the only throttle now that
///   the check is off the startup hot path.
#[must_use]
const fn should_spawn_background_worker(disabled: bool, is_npm_install: bool, cache_fresh: bool) -> bool {
    !disabled && is_npm_install && !cache_fresh
}

// Compile-time guard: the spawn policy is exhaustively pinned, so a future edit
// that flips the logic fails the build rather than slipping past as a flaky test.
const _: () = {
    assert!(should_spawn_background_worker(false, true, false)); // enabled + npm + stale → spawn
    assert!(!should_spawn_background_worker(true, true, false)); // disabled → no spawn
    assert!(!should_spawn_background_worker(false, false, false)); // not an npm install → no spawn
    assert!(!should_spawn_background_worker(false, true, true)); // recent check cached → no spawn
};

/// Build (but don't spawn) the detached background-worker command: re-invokes the
/// current binary with [`SELF_UPDATE_WORKER_ENV`] set and all stdio detached to
/// null so it never writes to the user's terminal. Split out so the command shape
/// is unit-testable without actually spawning a process.
///
/// On Windows the process is created `DETACHED_PROCESS | CREATE_NO_WINDOW` so it
/// has no console and survives the foreground process exiting. On unix, null
/// stdio plus reparenting-to-init on parent exit keeps it running in the
/// background; a spawn that doesn't outlive a fast one-shot command simply
/// degrades to "no update this run" (the next launch retries).
fn build_worker_command(exe: &Path) -> std::process::Command {
    use std::process::Stdio;
    let mut cmd = std::process::Command::new(exe);
    cmd.env(SELF_UPDATE_WORKER_ENV, "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        // DETACHED_PROCESS (0x0000_0008) | CREATE_NO_WINDOW (0x0800_0000): no
        // inherited console, no flashing window, not tied to the parent console.
        cmd.creation_flags(0x0000_0008 | 0x0800_0000);
    }
    cmd
}

/// Spawn a detached background worker to check + download + stage an update.
///
/// Fire-and-forget: returns immediately after spawning, without blocking startup
/// or re-exec'ing mid-session. The staged binary is applied by the next launch's
/// [`apply_staged_if_present`].
///
/// No-op when updates are disabled (`ORIGIN_NO_UPDATE`), this isn't an npm
/// install, or a recent check is still cached. A spawn failure is logged at
/// `debug` and ignored — the next launch retries.
pub fn spawn_background_update_worker() {
    let disabled = std::env::var_os("ORIGIN_NO_UPDATE").is_some();
    let is_npm_install = installed_npm_version().is_some();
    let cache_fresh = cached_latest(UPDATE_CHECK_TTL_SECS).is_some();
    if !should_spawn_background_worker(disabled, is_npm_install, cache_fresh) {
        return;
    }
    let exe = match current_exe() {
        Ok(p) => p,
        Err(e) => {
            tracing::debug!("updater: background worker not spawned (no current_exe): {e}");
            return;
        }
    };
    match build_worker_command(&exe).spawn() {
        // Drop the handle without waiting — we intentionally never reap it; it
        // outlives us (reparented to init on unix / detached on windows).
        Ok(child) => tracing::debug!("updater: spawned background update worker (pid {})", child.id()),
        Err(e) => tracing::debug!("updater: background worker spawn failed: {e}"),
    }
}

/// Print a user-visible failure message + log via `tracing::warn!`, then return
/// `false` so the caller can `return` directly.
fn skip_with_warn(stage: &str, err: impl std::fmt::Display) -> bool {
    eprintln!("Update check failed ({err}); continuing with current version.");
    tracing::warn!("updater: {stage} failed: {err}");
    // Persist the reason: this is the funnel every failure path flows through,
    // and the only durable trace the silent background worker leaves.
    update_log(&format!("FAILED ({stage}): {err}"));
    false
}

/// Inner body returning `bool` so the public entry stays trivially shaped.
async fn check_and_stage_inner() -> bool {
    // Install-type guard + version source in one: only npm-distributed binaries
    // auto-update, and their installed version comes from the adjacent npm
    // package metadata. A dev/source build or non-npm install returns `None`
    // and is left untouched.
    let Some(current) = installed_npm_version() else {
        return false;
    };

    // Cache hit: if we recently saw the same-or-older latest version, skip the
    // network round trip entirely.
    if let Some(cached) = cached_latest(UPDATE_CHECK_TTL_SECS) {
        if !is_newer(&current, &cached) {
            return false;
        }
    } else {
        eprintln!("Checking for updates…");
    }

    let latest = match fetch_latest_npm_version().await {
        Ok(v) => v,
        Err(e) => return skip_with_warn("fetch npm version", e),
    };
    write_cache(&latest);

    if !is_newer(&current, &latest) {
        update_log(&format!("up to date (installed={current}, latest={latest})"));
        return false;
    }

    let exe = match current_exe() {
        Ok(p) => p,
        Err(e) => return skip_with_warn("current_exe", e),
    };
    let targets = match update_targets(&exe) {
        Ok(t) => t,
        Err(e) => return skip_with_warn("update_targets", e),
    };

    eprintln!("origin {current} → {latest}: downloading…");
    update_log(&format!(
        "update available {current} -> {latest}; downloading {} binaries",
        targets.len()
    ));
    tracing::info!(
        "updater: update available (current={current} latest={latest}); downloading {} binaries",
        targets.len()
    );

    // Pull the checksum manifest so verification can run; with cosign gone this
    // is the sole integrity gate and is mandatory for every binary.
    let checksums = fetch_checksums(&latest).await;

    stage_all_targets(&targets, checksums.as_deref(), &latest).await
}

/// Download + verify + stage the whole CLI/daemon/supervisor set **all-or-nothing**.
///
/// Every asset is downloaded and SHA-256-verified IN FULL before *any* `.new` is
/// staged, so a partial download can never produce a CLI-newer-than-daemon
/// mismatch — the exact skew this fix exists to prevent (a CLI-only self-update
/// once left the daemon frozen at an old version). On any download/verify failure
/// nothing is staged; on a stage-write failure the already-staged `.new` files
/// are removed so the next launch cannot apply a half-update.
async fn stage_all_targets(targets: &[UpdateTarget], checksums: Option<&str>, version: &str) -> bool {
    // Phase 1 — download + verify every binary into memory (no disk writes yet).
    let mut verified: Vec<(&UpdateTarget, Vec<u8>)> = Vec::with_capacity(targets.len());
    for t in targets {
        let url = release_asset_url(version, &t.asset_name);
        match download_and_verify(&url, checksums, &t.asset_name).await {
            Ok(bytes) => verified.push((t, bytes)),
            Err(e) => return skip_with_warn(&format!("download/verify {}", t.asset_name), e),
        }
    }

    // Phase 2 — only now that ALL are verified, stage each as `<dest>.new`.
    let mut staged: Vec<PathBuf> = Vec::with_capacity(verified.len());
    for (t, bytes) in &verified {
        match stage_verified_bytes(bytes, &t.asset_name, &t.dest) {
            Ok(path) => staged.push(path),
            Err(e) => {
                // Roll back the already-staged `.new` files so the next launch
                // can't apply a half-update. A removal that itself fails is
                // logged (the leftover would be re-applied, but the all-or-nothing
                // download means it's a verified binary, not a partial download).
                for p in &staged {
                    if let Err(re) = std::fs::remove_file(p) {
                        tracing::warn!(
                            "updater: cleanup of partially-staged {} failed: {re}",
                            p.display()
                        );
                    }
                }
                return skip_with_warn(&format!("stage {}", t.asset_name), e);
            }
        }
    }

    // One package.json rewrite for the whole set (keyed off the CLI, the last
    // target) so the next launch doesn't re-download the same release in a loop.
    record_staged_version(&targets[targets.len() - 1].dest, version);
    eprintln!("Update staged; will apply on next launch.");
    update_log(&format!(
        "staged {} binaries for v{}; applies on next launch",
        staged.len(),
        strip_v_prefix(version.trim())
    ));
    tracing::info!(
        "updater: staged {} binaries for swap-in on next launch",
        staged.len()
    );
    true
}

/// Download `asset_url` and verify its SHA-256 against the release manifest
/// (mandatory — a missing/garbled `SHA256SUMS` entry rejects the download).
/// Returns the verified bytes; writes nothing to disk.
///
/// # Errors
/// Network/HTTP failure, or [`UpdateError::ChecksumFailed`] on a verify miss.
async fn download_and_verify(
    asset_url: &str,
    checksums: Option<&str>,
    asset_name: &str,
) -> Result<Vec<u8>, UpdateError> {
    let bytes = download_bytes(asset_url).await?;
    verify_sha256_bytes(&bytes, checksums, asset_name)?;
    Ok(bytes)
}

/// Write already-verified `bytes` to a temp neighbor of `dest`, mark it
/// executable on unix, then atomically rename to `<dest>.new`. Returns the staged
/// path. Cleans up the temp on any failure.
///
/// # Errors
/// [`UpdateError::Io`] on write / chmod / rename failure, or when `dest` has no
/// parent directory.
fn stage_verified_bytes(bytes: &[u8], asset_name: &str, dest: &Path) -> Result<PathBuf, UpdateError> {
    let parent = dest
        .parent()
        .ok_or_else(|| UpdateError::Io(std::io::Error::other("dest has no parent directory")))?;
    let tmp = parent.join(format!("{asset_name}.download"));
    if let Err(e) = std::fs::write(&tmp, bytes) {
        let _ = std::fs::remove_file(&tmp);
        return Err(UpdateError::Io(e));
    }
    // The downloaded release asset must be executable on unix once swapped in;
    // `std::fs::write` creates a 0644 file, so set the mode here (matching the
    // npm channel's `download.js`). No-op on Windows.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if let Err(e) = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755)) {
            let _ = std::fs::remove_file(&tmp);
            return Err(UpdateError::Io(e));
        }
    }
    let staged = staged_path(dest);
    if let Err(e) = std::fs::rename(&tmp, &staged) {
        let _ = std::fs::remove_file(&tmp);
        return Err(UpdateError::Io(e));
    }
    Ok(staged)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    use tokio::sync::Mutex;

    // All cache tests mutate the process-global `ORIGIN_HOME` env var. cargo
    // test runs in parallel by default, so without serialization they race. A
    // tokio Mutex is async-aware and safe to hold across awaits.
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
        assert!(!is_newer("garbage", "0.1.0"));
        assert!(!is_newer("0.1.0", "also garbage"));
        assert!(!is_newer("not.a.version", "0.0.1"));
    }

    #[test]
    #[allow(clippy::panic)]
    fn current_target_asset_name_includes_target_triple() {
        let name = current_target_asset_name().expect("supported test host");
        let os = std::env::consts::OS;
        let needle = match os {
            "windows" => "windows",
            "linux" => "linux",
            "macos" => "darwin",
            other => panic!("unexpected test host OS: {other}"),
        };
        assert!(name.contains(needle), "asset {name} should contain {needle}");
        assert!(
            name.starts_with("origin-"),
            "asset {name} should start with origin-"
        );
    }

    #[test]
    fn release_asset_url_points_at_versioned_github_release() {
        assert_eq!(
            release_asset_url("0.9.0", "origin-x86_64-pc-windows-msvc.exe"),
            "https://github.com/Kantosaurus/origin/releases/download/v0.9.0/origin-x86_64-pc-windows-msvc.exe"
        );
        // A leading `v` on the version is normalized so we never emit `vv0.9.0`.
        assert_eq!(
            release_asset_url("v0.9.0", "origin-x86_64-unknown-linux-gnu"),
            "https://github.com/Kantosaurus/origin/releases/download/v0.9.0/origin-x86_64-unknown-linux-gnu"
        );
    }

    #[test]
    fn npm_manifest_parses_version() {
        // The `…/latest` registry endpoint returns far more than `version`; the
        // parser must ignore the rest.
        let raw = r#"{"name":"@kantosaurus/origin","version":"0.9.0","dist":{"tarball":"x"}}"#;
        let m: NpmManifest = serde_json::from_str(raw).expect("parse");
        assert_eq!(m.version, "0.9.0");
    }

    #[test]
    fn npm_version_for_exe_reads_adjacent_package_json() {
        // Mirror the npm platform-package layout:
        //   <tmp>/node_modules/pkg/package.json   (version)
        //   <tmp>/node_modules/pkg/bin/origin(.exe)
        let tmp = tempdir().expect("tempdir");
        let pkg = tmp.path().join("node_modules").join("pkg");
        let bin = pkg.join("bin");
        std::fs::create_dir_all(&bin).expect("mkdir");
        std::fs::write(
            pkg.join("package.json"),
            r#"{"name":"@kantosaurus/origin-win32-x64","version":"1.2.3"}"#,
        )
        .expect("write package.json");
        let exe = bin.join("origin");
        std::fs::write(&exe, b"binary").expect("write exe");

        assert_eq!(npm_version_for_exe(&exe).as_deref(), Some("1.2.3"));
    }

    #[test]
    fn npm_version_for_exe_rejects_foreign_package_and_garbage_version() {
        // The version source must be origin's OWN package.json with a valid
        // semver — a stray foreign package.json or a spoofed garbage version is
        // ignored (so a tampered file can't block updates).
        let tmp = tempdir().expect("tempdir");
        let pkg = tmp.path().join("node_modules").join("x");
        let bin = pkg.join("bin");
        std::fs::create_dir_all(&bin).expect("mkdir");
        let exe = bin.join("origin");
        std::fs::write(&exe, b"binary").expect("write exe");
        let pj = pkg.join("package.json");

        std::fs::write(&pj, r#"{"name":"some-other-pkg","version":"1.2.3"}"#).expect("w");
        assert!(
            npm_version_for_exe(&exe).is_none(),
            "foreign package must be ignored"
        );

        std::fs::write(&pj, r#"{"name":"@kantosaurus/origin","version":"garbage"}"#).expect("w");
        assert!(
            npm_version_for_exe(&exe).is_none(),
            "non-semver version must be ignored"
        );

        std::fs::write(&pj, r#"{"name":"@kantosaurus/origin","version":"0.9.0"}"#).expect("w");
        assert_eq!(npm_version_for_exe(&exe).as_deref(), Some("0.9.0"));
    }

    #[test]
    fn record_staged_version_breaks_the_self_update_loop() {
        // npm install at 0.9.0, then a self-update stages 0.10.0. The installed
        // version read MUST then report 0.10.0 (not the un-rewritten 0.9.0), or
        // the updater would re-download 0.10.0 on every launch forever.
        let tmp = tempdir().expect("tempdir");
        let pkg = tmp
            .path()
            .join("node_modules")
            .join("@kantosaurus")
            .join("origin-win32-x64");
        let bin = pkg.join("bin");
        std::fs::create_dir_all(&bin).expect("mkdir");
        std::fs::write(
            pkg.join("package.json"),
            r#"{"name":"@kantosaurus/origin-win32-x64","version":"0.9.0","bin":{"origin":"bin/origin.exe"}}"#,
        )
        .expect("write package.json");
        let exe = bin.join("origin.exe");
        std::fs::write(&exe, b"binary").expect("write exe");

        assert_eq!(npm_version_for_exe(&exe).as_deref(), Some("0.9.0"));

        record_staged_version(&exe, "0.10.0");

        assert_eq!(
            npm_version_for_exe(&exe).as_deref(),
            Some("0.10.0"),
            "installed-version read must reflect the staged version"
        );
        // Unrelated package.json fields must survive the rewrite.
        let raw = std::fs::read_to_string(pkg.join("package.json")).expect("read");
        let json: serde_json::Value = serde_json::from_str(&raw).expect("parse");
        assert_eq!(json["version"], "0.10.0");
        assert!(json.get("bin").is_some(), "unrelated fields must be preserved");
    }

    #[test]
    fn npm_version_for_exe_is_none_outside_node_modules() {
        // A dev/source build (cargo target dir, no node_modules) must NOT
        // auto-update — the guard returns None even if a package.json exists.
        let tmp = tempdir().expect("tempdir");
        let dir = tmp.path().join("target").join("debug");
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(dir.join("package.json"), r#"{"version":"9.9.9"}"#).expect("write");
        let exe = dir.join("origin");
        std::fs::write(&exe, b"binary").expect("write exe");

        assert!(
            npm_version_for_exe(&exe).is_none(),
            "a build outside node_modules must not be treated as an npm install"
        );
    }

    #[tokio::test]
    async fn cache_round_trips() {
        let _g = ENV_LOCK.lock().await;
        let tmp = tempdir().expect("tempdir");
        std::env::set_var("ORIGIN_HOME", tmp.path());

        assert!(cached_latest(UPDATE_CHECK_TTL_SECS).is_none());

        write_cache("v0.1.0");
        let v = cached_latest(UPDATE_CHECK_TTL_SECS).expect("cache hit");
        assert_eq!(v, "0.1.0", "v-prefix should be stripped on write");

        let v = cached_latest(60).expect("within ttl");
        assert_eq!(v, "0.1.0");

        assert!(cached_latest(0).is_none(), "TTL of 0 should always miss");

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

    // Host-specific binary extension, matching `target_triple_ext`.
    const HOST_EXT: &str = if cfg!(windows) { ".exe" } else { "" };

    #[test]
    fn update_targets_covers_cli_daemon_supervisor_as_siblings() {
        let dir = if cfg!(windows) {
            Path::new("C:/app/bin")
        } else {
            Path::new("/opt/app/bin")
        };
        let cli = dir.join(format!("origin{HOST_EXT}"));
        let targets = update_targets(&cli).expect("supported test host");

        assert_eq!(targets.len(), 3, "CLI + daemon + supervisor");
        // CLI is LAST so the apply step swaps companions before it.
        assert_eq!(targets[2].dest, cli, "CLI must be the final target");
        // Dests are siblings in the CLI's directory with the local (un-tripled) names.
        assert_eq!(targets[0].dest, dir.join(format!("origin-daemon{HOST_EXT}")));
        assert_eq!(targets[1].dest, dir.join(format!("origin-supervisor{HOST_EXT}")));
        for t in &targets {
            assert_eq!(t.dest.parent(), cli.parent(), "all binaries share one dir");
        }
        // Asset names are the triple-suffixed release names, distinct per binary.
        assert!(targets[0].asset_name.starts_with("origin-daemon-"));
        assert!(targets[1].asset_name.starts_with("origin-supervisor-"));
        assert!(
            targets[2].asset_name.starts_with("origin-")
                && !targets[2].asset_name.starts_with("origin-daemon-")
                && !targets[2].asset_name.starts_with("origin-supervisor-")
        );
        assert_eq!(
            targets[2].asset_name,
            current_target_asset_name().expect("host asset")
        );
    }

    #[test]
    fn update_targets_handles_bare_and_rootless_paths() {
        // A bare relative exe still derives daemon/supervisor as siblings — its
        // parent is Some("") (not None) — so the trio stays in lockstep.
        let bare = update_targets(Path::new("origin")).expect("supported test host");
        assert_eq!(bare.len(), 3);
        assert_eq!(bare[2].dest, Path::new("origin"));
        assert_eq!(bare[0].dest, Path::new(&format!("origin-daemon{HOST_EXT}")));

        // A path with no parent at all degrades to single-binary behavior.
        let rootless = update_targets(Path::new("")).expect("supported test host");
        assert_eq!(rootless.len(), 1);
    }

    #[test]
    fn stage_and_apply_round_trips_all_three_binaries() {
        // Seed all three "installed" binaries, stage fresh bytes for each, apply,
        // and confirm every dest holds its NEW content — the end-to-end swap the
        // background worker + next-launch apply perform for the whole trio.
        let tmp = tempdir().expect("tempdir");
        let dir = tmp.path();
        let cli = dir.join(format!("origin{HOST_EXT}"));
        let targets = update_targets(&cli).expect("supported test host");

        for t in &targets {
            std::fs::write(&t.dest, b"OLD").expect("seed old binary");
        }

        // Stage distinct new bytes for each dest (label by basename).
        for t in &targets {
            let name = t
                .dest
                .file_name()
                .expect("dest name")
                .to_string_lossy()
                .into_owned();
            let staged = stage_verified_bytes(format!("NEW:{name}").as_bytes(), &t.asset_name, &t.dest)
                .expect("stage");
            assert!(staged.exists(), "{} should be staged", t.asset_name);
            assert_eq!(staged, staged_path(&t.dest));
        }

        // Apply every staged .new (CLI applied last per target order).
        let mut applied = 0;
        for t in &targets {
            if apply_one_staged(&t.dest).expect("apply") {
                applied += 1;
            }
        }
        assert_eq!(applied, 3, "all three swapped");

        for t in &targets {
            let name = t
                .dest
                .file_name()
                .expect("dest name")
                .to_string_lossy()
                .into_owned();
            assert_eq!(
                std::fs::read(&t.dest).expect("read dest"),
                format!("NEW:{name}").into_bytes()
            );
        }

        // A second apply is a no-op — no leftover .new files.
        for t in &targets {
            assert!(
                !apply_one_staged(&t.dest).expect("re-apply"),
                "no .new should remain"
            );
        }
    }

    #[test]
    fn apply_one_staged_moves_new_into_place_when_dest_missing() {
        // A staged sibling whose original was never installed is moved in directly.
        let tmp = tempdir().expect("tempdir");
        let dest = tmp.path().join(format!("origin-daemon{HOST_EXT}"));
        std::fs::write(staged_path(&dest), b"FRESH").expect("stage");
        assert!(!dest.exists(), "precondition: dest missing");

        assert!(apply_one_staged(&dest).expect("apply"));
        assert_eq!(std::fs::read(&dest).expect("read dest"), b"FRESH");
        assert!(!staged_path(&dest).exists());
    }

    #[tokio::test]
    async fn env_var_bypass_short_circuits_check() {
        let _g = ENV_LOCK.lock().await;
        std::env::set_var("ORIGIN_NO_UPDATE", "1");
        let result = check_and_stage_blocking().await;
        std::env::remove_var("ORIGIN_NO_UPDATE");
        assert!(matches!(result, Ok(false)), "bypass should return Ok(false)");
    }

    #[tokio::test]
    async fn env_var_bypass_short_circuits_apply() {
        let _g = ENV_LOCK.lock().await;
        std::env::set_var("ORIGIN_NO_UPDATE", "1");
        let result = apply_staged_if_present();
        std::env::remove_var("ORIGIN_NO_UPDATE");
        assert!(matches!(result, Ok(false)), "bypass should return Ok(false)");
    }

    #[test]
    fn staged_and_old_paths_append_suffix() {
        let p = Path::new("/tmp/origin");
        assert_eq!(staged_path(p), PathBuf::from("/tmp/origin.new"));
        assert_eq!(old_path(p), PathBuf::from("/tmp/origin.old"));

        let pe = Path::new("C:/bin/origin.exe");
        assert_eq!(staged_path(pe), PathBuf::from("C:/bin/origin.exe.new"));
        assert_eq!(old_path(pe), PathBuf::from("C:/bin/origin.exe.old"));
    }

    // NOTE: the TTL-is-short and the spawn-policy truth-table checks are enforced
    // as module-level compile-time `const _` assertions (stronger than a test — a
    // regression fails the build), so they intentionally have no `#[test]` here.

    #[test]
    fn build_worker_command_sets_worker_env_and_targets_exe() {
        use std::ffi::OsStr;
        let exe = Path::new("/some/dir/origin");
        let cmd = build_worker_command(exe);
        assert_eq!(cmd.get_program(), OsStr::new("/some/dir/origin"));
        let has_worker_env = cmd
            .get_envs()
            .any(|(k, v)| k == OsStr::new(SELF_UPDATE_WORKER_ENV) && v == Some(OsStr::new("1")));
        assert!(
            has_worker_env,
            "worker command must set {SELF_UPDATE_WORKER_ENV}=1"
        );
    }

    #[test]
    fn sha256_hex_bytes_matches_known_vector() {
        // SHA-256("abc") NIST test vector — confirms lowercase zero-padded hex.
        assert_eq!(
            sha256_hex_bytes(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn expected_hash_for_parses_both_manifest_formats() {
        let h = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        let text = format!(
            "{h}  origin-x86_64-unknown-linux-gnu\n\
             {h} *origin-x86_64-pc-windows-msvc.exe\n\
             {h}  dist/origin-aarch64-apple-darwin\n"
        );
        assert_eq!(
            expected_hash_for(&text, "origin-x86_64-unknown-linux-gnu").as_deref(),
            Some(h)
        );
        assert_eq!(
            expected_hash_for(&text, "origin-x86_64-pc-windows-msvc.exe").as_deref(),
            Some(h)
        );
        assert_eq!(
            expected_hash_for(&text, "origin-aarch64-apple-darwin").as_deref(),
            Some(h)
        );
        assert_eq!(expected_hash_for(&text, "origin-not-present"), None);
    }

    #[test]
    fn expected_hash_for_ignores_malformed_lines() {
        let text = "# comment\nnot-a-hash  origin-foo\n";
        assert_eq!(expected_hash_for(text, "origin-foo"), None);
    }

    #[test]
    fn verify_sha256_bytes_is_mandatory_and_matches_manifest() {
        let bytes = b"abc";
        let h = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        let sums = format!("{h}  origin-test-asset\n");

        // Matching manifest entry → ok (case-insensitive digest).
        assert!(verify_sha256_bytes(bytes, Some(&sums), "origin-test-asset").is_ok());
        let upper = format!("{}  origin-test-asset\n", h.to_uppercase());
        assert!(verify_sha256_bytes(bytes, Some(&upper), "origin-test-asset").is_ok());

        // NO manifest → rejected (never stage an unverified binary).
        assert!(matches!(
            verify_sha256_bytes(bytes, None, "origin-test-asset"),
            Err(UpdateError::ChecksumFailed(_))
        ));

        // Manifest present but no entry for this asset → rejected.
        let other = format!("{h}  some-other-asset\n");
        assert!(matches!(
            verify_sha256_bytes(bytes, Some(&other), "origin-test-asset"),
            Err(UpdateError::ChecksumFailed(_))
        ));

        // Manifest present but mismatching digest → rejected.
        let bad = format!("{}  origin-test-asset\n", "0".repeat(64));
        assert!(matches!(
            verify_sha256_bytes(bytes, Some(&bad), "origin-test-asset"),
            Err(UpdateError::ChecksumFailed(_))
        ));
    }

    // ── diagnostic log + download retry (observability + resilience) ──────────

    #[tokio::test]
    async fn update_log_appends_timestamped_lines() {
        // The background worker is otherwise silent; this persistent log is the
        // only trace of why an auto-update did or didn't happen.
        let _g = ENV_LOCK.lock().await;
        let tmp = tempdir().expect("tempdir");
        std::env::set_var("ORIGIN_HOME", tmp.path());

        update_log("worker: check started");
        update_log("FAILED (download/verify origin-x): network: reset");

        let log = tmp.path().join(".origin").join("update.log");
        let contents = std::fs::read_to_string(&log).expect("log file written");
        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(lines.len(), 2, "two appended lines, got: {contents:?}");
        assert!(lines[0].contains("worker: check started"));
        assert!(lines[1].contains("FAILED (download/verify origin-x)"));
        // Each line is `<epoch> <msg>` — confirm the timestamp prefix parses.
        assert!(
            lines[0]
                .split(' ')
                .next()
                .and_then(|t| t.parse::<i64>().ok())
                .is_some(),
            "each line is prefixed with an epoch timestamp"
        );

        std::env::remove_var("ORIGIN_HOME");
    }

    #[tokio::test]
    async fn retry_async_succeeds_after_transient_failures() {
        use std::cell::Cell;
        // Fails twice (transient), then succeeds — the exact "flaky download that
        // aborts the all-or-nothing stage" failure mode the retry exists to fix.
        let calls = Cell::new(0u32);
        let result: Result<&str, &str> = retry_async(5, std::time::Duration::ZERO, || {
            let n = calls.get() + 1;
            calls.set(n);
            async move {
                if n < 3 {
                    Err("transient reset")
                } else {
                    Ok("downloaded")
                }
            }
        })
        .await;
        assert_eq!(result, Ok("downloaded"));
        assert_eq!(calls.get(), 3, "retried until the 3rd attempt succeeded");
    }

    #[tokio::test]
    async fn retry_async_gives_up_after_attempts_and_returns_last_error() {
        use std::cell::Cell;
        let calls = Cell::new(0u32);
        let result: Result<&str, String> = retry_async(3, std::time::Duration::ZERO, || {
            let n = calls.get() + 1;
            calls.set(n);
            async move { Err(format!("fail {n}")) }
        })
        .await;
        assert_eq!(result, Err("fail 3".to_string()), "returns the LAST error");
        assert_eq!(calls.get(), 3, "exactly `attempts` tries, no more");
    }

    #[tokio::test]
    async fn retry_async_clamps_zero_attempts_to_one() {
        use std::cell::Cell;
        let calls = Cell::new(0u32);
        let result: Result<&str, &str> = retry_async(0, std::time::Duration::ZERO, || {
            calls.set(calls.get() + 1);
            async move { Ok("once") }
        })
        .await;
        assert_eq!(result, Ok("once"));
        assert_eq!(calls.get(), 1, "attempts=0 still runs exactly once");
    }
}
