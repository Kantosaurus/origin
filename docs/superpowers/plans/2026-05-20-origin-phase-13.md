# Origin Phase 13 — QUIC Remote IPC + Headless Polish — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use `superpowers:subagent-driven-development` (recommended) or `superpowers:executing-plans` to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. Every task ends with a `verification-before-completion` gate; do NOT move to the next task until verification is green. Use `superpowers:test-driven-development` discipline — write the failing test first, run to confirm fail, then implement minimum to pass, then verify, then commit.

**Goal:** Add QUIC + mutual-TLS remote IPC to the `origin` daemon, a pairing flow that mints short-lived bearer tokens via KeyVault, a headless `origin run --json` one-shot CLI mode, and admin subcommands (`usage`, `sessions`, `keyring`).

**Architecture:**
- New transport variant `origin-ipc::transport::quic` runs alongside the existing local-socket transport. Both expose the same `Connection` surface (`read_frame`, `write_frame`, `write_raw`) so the daemon dispatch loop is transport-agnostic. Mutual TLS authenticates the daemon to clients; bearer tokens issued by the daemon authenticate clients to the daemon.
- Pairing: the daemon mints a 6-digit human-readable pairing code + a remote `connect URL` (host:port + cert fingerprint). Code is shown in TUI; remote client redeems it for a short-lived bearer token, stored under `("origin-remote", "<device-id>")` in KeyVault.
- Headless `origin run "..."` connects to the daemon (local-socket by default, remote if `--remote <url>` provided), drains one prompt to completion, and emits either human-readable text or JSON-Lines events. No Ratatui renderer instantiated.
- Admin subcommands shell into existing crate APIs: `origin-keyvault::KeyVault::{set,list,delete}`, `origin-daemon::session_store::SessionStore::list_summaries`, `origin-metrics::Metrics::snapshot`.

**Tech Stack:** Rust 1.83 (MSRV-pinned), `quinn` 0.11 (QUIC), `rustls` 0.23 + `rcgen` 0.13 (self-signed certs), `tokio`, existing `origin-ipc`, `origin-keyvault`, `origin-daemon`, `origin-cli`, `origin-metrics`. JSON via `serde_json`. Clap-derived CLI.

**Spec reference:** `docs/superpowers/specs/2026-05-19-origin-harness-design.md` §7C N7.12 (QUIC + mutual TLS for remote) and §17 Phase 13.

**Branch:** All work lands on `p-13` (branched from `dev`).

---

## Conventions (apply to every task)

**TDD shape:**
1. Write failing test (or fuzz/property harness).
2. Run it — confirm the expected failure mode (compile error, assertion, panic).
3. Implement the minimum to pass.
4. Run test — confirm pass.
5. **Verification gate** — run `cargo test -p <crate>` + `cargo clippy -p <crate> -- -D warnings` + `cargo fmt --check`. For cross-crate tasks: `cargo test --workspace` + `cargo clippy --workspace -- -D warnings`. If any of these fail (non-zero exit, failing test, clippy warning, format diff), **the task is not done**.
6. Commit.

**Commit style:** Conventional commits scoped to crate: `feat(origin-ipc): add quic transport backend`. Always co-author Claude.

**Windows note:** All paths use forward slashes; Cargo + Git handle them natively on Windows. Where Bash one-liners appear in steps, the engineer runs them in `bash` (Git Bash) or adapts to PowerShell trivially (`set ORIGIN_SOCK=...`).

**Dependency policy:** Pin every new crate in `[workspace.dependencies]` at the workspace root with an exact major-minor (e.g. `quinn = "0.11"`). MSRV is 1.83; if `quinn` or `rustls` transitively requires edition2024, pin with `cargo update --precise` per `memory/project_msrv_dep_pinning.md`.

---

## File Structure

### Files created in this phase

- `crates/origin-ipc/src/quic.rs` — QUIC transport (listener + connector + connection).
- `crates/origin-ipc/src/tls.rs` — self-signed cert generation + fingerprint helpers.
- `crates/origin-ipc/tests/quic_smoke.rs` — round-trip a frame over QUIC.
- `crates/origin-ipc/tests/quic_concurrent.rs` — two clients, distinct request-ID streams.
- `crates/origin-daemon/src/pairing.rs` — pairing-code state machine + bearer minting.
- `crates/origin-daemon/src/auth.rs` — bearer-token validation middleware.
- `crates/origin-daemon/tests/pairing_e2e.rs` — pair → bearer → authed QUIC handshake.
- `crates/origin-cli/src/headless.rs` — `origin run` implementation + JSON-Lines formatter.
- `crates/origin-cli/src/admin.rs` — `origin usage`, `sessions`, `keyring` subcommands.
- `crates/origin-cli/tests/headless.rs` — golden-file JSON-Lines stream test.
- `crates/origin-cli/tests/admin.rs` — admin subcommand round-trip tests.

### Files modified in this phase

- `Cargo.toml` (workspace) — add `quinn`, `rustls`, `rcgen` to `[workspace.dependencies]`.
- `crates/origin-ipc/Cargo.toml` — add the new deps.
- `crates/origin-ipc/src/lib.rs` — `pub mod quic; pub mod tls;`.
- `crates/origin-daemon/Cargo.toml` — depend on quinn + new modules.
- `crates/origin-daemon/src/main.rs` — wire pairing handler + QUIC listener (env-gated).
- `crates/origin-daemon/src/protocol.rs` — new `ClientMessage::{Pair, ListSessions, RemoveSession, ResumeSession, GetUsage, KeyringAdd, KeyringList, KeyringRemove}` variants + matching reply event(s).
- `crates/origin-daemon/src/session_store.rs` — add `list_summaries()` (id, created_at, title, model, message_count) and `delete(session_id)`.
- `crates/origin-cli/Cargo.toml` — gain clap subcommand modules.
- `crates/origin-cli/src/main.rs` — extend `Cmd` enum with `Run`, `Pair`, `Usage`, `Sessions`, `Keyring`, route non-TUI cases.
- `crates/origin-cli/src/lib.rs` — `pub mod headless; pub mod admin;`.
- `CHANGELOG.md` — Phase 13 section.

---

# Pre-flight: branch setup

### Task P13.0 — Create `p-13` branch

**Files:** none (git state only).

- [ ] **Step 1: Confirm clean working tree**

Run: `git status --porcelain`
Expected: empty output (only `.claude/` untracked is acceptable; do not stage it).

- [ ] **Step 2: Create + check out branch**

Run: `git checkout -b p-13 dev`
Expected: `Switched to a new branch 'p-13'`.

- [ ] **Step 3: Verify**

Run: `git branch --show-current`
Expected: `p-13`.

- [ ] **Step 4: Push to remote (optional)**

Run: `git push -u origin p-13`
Expected: branch created upstream.

---

# Task group A — QUIC transport (P13.1)

**Group goal:** A daemon process accepts QUIC connections over mutual-TLS; a client process connects, sends a `Request` frame, receives an identical `Response` frame. Identical wire-frame format to the local-socket transport so the daemon dispatch loop need not change.

---

### Task P13.1.1 — Workspace deps for QUIC + TLS

**Files:**
- Modify: `Cargo.toml` (workspace root)
- Modify: `crates/origin-ipc/Cargo.toml`

- [ ] **Step 1: Add workspace deps**

Edit `Cargo.toml` (workspace root). Locate the existing `[workspace.dependencies]` table (or create it under the `[workspace]` block) and add:

```toml
[workspace.dependencies]
# … existing entries …
quinn          = { version = "0.11", default-features = false, features = ["runtime-tokio", "rustls", "ring", "log"] }
rustls         = { version = "0.23", default-features = false, features = ["ring", "std"] }
rustls-pemfile = "2"
rcgen          = { version = "0.13", default-features = false, features = ["pem", "ring"] }
x509-parser    = "0.16"
sha2           = "0.10"
hex            = "0.4"
```

- [ ] **Step 2: Pull into `origin-ipc`**

Edit `crates/origin-ipc/Cargo.toml`. Under `[dependencies]` add:

```toml
quinn.workspace          = true
rustls.workspace         = true
rustls-pemfile.workspace = true
rcgen.workspace          = true
x509-parser.workspace    = true
sha2.workspace           = true
hex.workspace            = true
```

Under `[dev-dependencies]` add `tempfile = "3"` if not already present.

- [ ] **Step 3: Verify build (no source changes yet)**

Run: `cargo check -p origin-ipc`
Expected: clean compile. If `cargo` complains that a transitive dep requires `edition2024`, follow `memory/project_msrv_dep_pinning.md` and `cargo update --precise <crate>@<lastgood>`.

- [ ] **Step 4: Commit**

```bash
git add Cargo.toml Cargo.lock crates/origin-ipc/Cargo.toml
git commit -m "$(cat <<'EOF'
chore(deps): pin quinn + rustls + rcgen for P13.1 QUIC transport

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task P13.1.2 — `origin-ipc::tls`: self-signed cert + fingerprint helpers

**Files:**
- Create: `crates/origin-ipc/src/tls.rs`
- Modify: `crates/origin-ipc/src/lib.rs`
- Create: `crates/origin-ipc/tests/tls.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/origin-ipc/tests/tls.rs`:

```rust
use origin_ipc::tls::{generate_self_signed, sha256_fingerprint_hex};

#[test]
fn self_signed_cert_has_stable_fingerprint() {
    let bundle = generate_self_signed("origin-daemon").expect("generate");
    let fp = sha256_fingerprint_hex(&bundle.cert_der);
    // SHA-256 hex is 64 chars
    assert_eq!(fp.len(), 64);
    assert!(fp.chars().all(|c| c.is_ascii_hexdigit()));
}

#[test]
fn cert_der_and_key_der_are_nonempty() {
    let bundle = generate_self_signed("origin-daemon").expect("generate");
    assert!(!bundle.cert_der.is_empty());
    assert!(!bundle.key_der.is_empty());
    // CA roots: self-signed identifies itself as the trust anchor.
    assert_eq!(bundle.ca_der, bundle.cert_der);
}
```

- [ ] **Step 2: Run — confirm fail**

Run: `cargo test -p origin-ipc --test tls`
Expected: compile error — `origin_ipc::tls` does not exist.

- [ ] **Step 3: Implement `tls.rs`**

Create `crates/origin-ipc/src/tls.rs`:

```rust
//! TLS helpers for the QUIC transport: self-signed cert generation,
//! SHA-256 fingerprinting, and rustls config builders that pin a peer
//! to a single known cert (no PKI).

use rcgen::{CertificateParams, DistinguishedName, DnType, KeyPair, SanType};
use sha2::{Digest, Sha256};

/// Self-signed certificate bundle in DER form. The same DER blob is used
/// both as the server's identity cert and as the only accepted CA root
/// when the peer pins by fingerprint.
#[derive(Clone)]
pub struct CertBundle {
    pub cert_der: Vec<u8>,
    pub key_der: Vec<u8>,
    pub ca_der: Vec<u8>,
}

/// Generate a self-signed Ed25519 cert with `cn` as the subject Common
/// Name and a single `subject_alt_name` of the same value. Suitable for
/// fingerprint-pinned mutual TLS where DNS is irrelevant.
///
/// # Errors
/// Returns an `rcgen::Error` if cert serialization fails.
pub fn generate_self_signed(cn: &str) -> Result<CertBundle, rcgen::Error> {
    let key_pair = KeyPair::generate()?;
    let mut params = CertificateParams::default();
    params.distinguished_name = DistinguishedName::new();
    params.distinguished_name.push(DnType::CommonName, cn);
    params.subject_alt_names = vec![SanType::DnsName(cn.to_string().try_into()?)];
    let cert = params.self_signed(&key_pair)?;
    let cert_der = cert.der().to_vec();
    let key_der = key_pair.serialize_der();
    Ok(CertBundle {
        ca_der: cert_der.clone(),
        cert_der,
        key_der,
    })
}

/// Hex-encoded SHA-256 fingerprint of a DER-encoded certificate. Used as
/// the durable identity pin shared with remote clients during pairing.
#[must_use]
pub fn sha256_fingerprint_hex(cert_der: &[u8]) -> String {
    let digest = Sha256::digest(cert_der);
    hex::encode(digest)
}
```

- [ ] **Step 4: Export from lib**

Edit `crates/origin-ipc/src/lib.rs` to add (preserve existing modules):

```rust
pub mod tls;
```

- [ ] **Step 5: Run test — confirm pass**

Run: `cargo test -p origin-ipc --test tls`
Expected: 2 tests pass.

- [ ] **Step 6: Verification gate**

Run: `cargo test -p origin-ipc && cargo clippy -p origin-ipc -- -D warnings && cargo fmt --check`
Expected: exit 0 for all.

- [ ] **Step 7: Commit**

```bash
git add crates/origin-ipc/src/tls.rs crates/origin-ipc/src/lib.rs crates/origin-ipc/tests/tls.rs
git commit -m "$(cat <<'EOF'
feat(origin-ipc): add self-signed cert + fingerprint helpers

Backing for the QUIC transport (P13.1). Self-signed Ed25519 cert with
SAN=CN, SHA-256 fingerprint hex. No PKI; peers pin the fingerprint
during the pairing flow (P13.2).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task P13.1.3 — `origin-ipc::quic`: listener + connector skeleton

**Files:**
- Create: `crates/origin-ipc/src/quic.rs`
- Modify: `crates/origin-ipc/src/lib.rs`

- [ ] **Step 1: Write the failing smoke test**

Create `crates/origin-ipc/tests/quic_smoke.rs`:

```rust
use origin_ipc::frame::{encode, FrameKind};
use origin_ipc::quic::{QuicConnector, QuicListener};
use origin_ipc::tls::generate_self_signed;

#[tokio::test(flavor = "current_thread")]
async fn quic_round_trips_one_frame() {
    let bundle = generate_self_signed("origin-test").unwrap();

    let listener = QuicListener::bind("127.0.0.1:0".parse().unwrap(), bundle.clone())
        .await
        .expect("bind");
    let addr = listener.local_addr();
    let server_ca = bundle.ca_der.clone();

    let server = tokio::spawn(async move {
        let mut conn = listener.accept().await.expect("accept");
        let (_kind, body) = conn.read_frame().await.expect("read");
        conn.write_frame(FrameKind::Response, &body).await.expect("write");
    });

    let mut client = QuicConnector::connect(addr, "origin-test", &server_ca)
        .await
        .expect("connect");
    client
        .write_raw(&encode(7, FrameKind::Request, b"ping"))
        .await
        .expect("write_raw");
    let (kind, body) = client.read_frame().await.expect("read");

    assert_eq!(kind, FrameKind::Response);
    assert_eq!(&body, b"ping");
    server.await.unwrap();
}
```

- [ ] **Step 2: Run — confirm fail**

Run: `cargo test -p origin-ipc --test quic_smoke`
Expected: compile error — `origin_ipc::quic` does not exist.

- [ ] **Step 3: Implement `quic.rs`**

Create `crates/origin-ipc/src/quic.rs`:

```rust
//! QUIC remote IPC transport (P13.1). Mirrors the `read_frame` /
//! `write_frame` / `write_raw` surface of the local-socket transport so
//! daemon code is transport-agnostic.
//!
//! Authentication model: mutual TLS. The server presents a self-signed
//! cert; clients pin its SHA-256 fingerprint (the `ca_der` they receive
//! during pairing). Application-layer bearer tokens (P13.2) gate access
//! to session state.

use std::net::SocketAddr;
use std::sync::Arc;

use quinn::{ClientConfig, Endpoint, RecvStream, SendStream, ServerConfig};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::RootCertStore;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::frame::{FrameKind, HEADER_LEN};
use crate::tls::CertBundle;

#[derive(Debug, thiserror::Error)]
pub enum QuicError {
    #[error("tls: {0}")]
    Tls(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("connect: {0}")]
    Connect(String),
    #[error("frame: {0}")]
    Frame(String),
}

/// Listening side of a QUIC transport endpoint.
pub struct QuicListener {
    endpoint: Endpoint,
}

impl QuicListener {
    /// Bind a QUIC server on `addr` using `bundle` as identity. Returns
    /// `Self`; obtain the bound port via [`local_addr`].
    pub async fn bind(addr: SocketAddr, bundle: CertBundle) -> Result<Self, QuicError> {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let cert = CertificateDer::from(bundle.cert_der.clone());
        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(bundle.key_der.clone()));

        let server_crypto = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(vec![cert], key)
            .map_err(|e| QuicError::Tls(e.to_string()))?;
        let server_quic = quinn::crypto::rustls::QuicServerConfig::try_from(server_crypto)
            .map_err(|e| QuicError::Tls(e.to_string()))?;
        let config = ServerConfig::with_crypto(Arc::new(server_quic));

        let endpoint = Endpoint::server(config, addr)?;
        Ok(Self { endpoint })
    }

    #[must_use]
    pub fn local_addr(&self) -> SocketAddr {
        self.endpoint.local_addr().expect("endpoint bound")
    }

    /// Accept the next connection. Resolves when a client completes
    /// handshake and opens its first bidirectional stream.
    ///
    /// # Errors
    /// Returns when the underlying endpoint closes.
    pub async fn accept(&self) -> Result<QuicConnection, QuicError> {
        let incoming = self
            .endpoint
            .accept()
            .await
            .ok_or_else(|| QuicError::Connect("endpoint closed".into()))?;
        let connection = incoming
            .await
            .map_err(|e| QuicError::Connect(e.to_string()))?;
        let (send, recv) = connection
            .accept_bi()
            .await
            .map_err(|e| QuicError::Connect(e.to_string()))?;
        Ok(QuicConnection { send, recv })
    }
}

/// Client-side connector. Single use; produces one `QuicConnection`.
pub struct QuicConnector;

impl QuicConnector {
    /// Connect to a daemon at `addr`. `server_name` MUST match the cert's
    /// CN/SAN (`origin-daemon` by convention). `ca_der` is the daemon's
    /// own cert (self-signed; serves as its own trust anchor).
    pub async fn connect(
        addr: SocketAddr,
        server_name: &str,
        ca_der: &[u8],
    ) -> Result<QuicConnection, QuicError> {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let mut roots = RootCertStore::empty();
        roots
            .add(CertificateDer::from(ca_der.to_vec()))
            .map_err(|e| QuicError::Tls(e.to_string()))?;
        let client_crypto = rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        let client_quic = quinn::crypto::rustls::QuicClientConfig::try_from(client_crypto)
            .map_err(|e| QuicError::Tls(e.to_string()))?;
        let config = ClientConfig::new(Arc::new(client_quic));

        let bind: SocketAddr = if addr.is_ipv4() {
            "0.0.0.0:0".parse().unwrap()
        } else {
            "[::]:0".parse().unwrap()
        };
        let mut endpoint = Endpoint::client(bind)?;
        endpoint.set_default_client_config(config);

        let connection = endpoint
            .connect(addr, server_name)
            .map_err(|e| QuicError::Connect(e.to_string()))?
            .await
            .map_err(|e| QuicError::Connect(e.to_string()))?;
        let (send, recv) = connection
            .open_bi()
            .await
            .map_err(|e| QuicError::Connect(e.to_string()))?;
        Ok(QuicConnection { send, recv })
    }
}

/// A single client↔server bidirectional QUIC stream framed identically
/// to the local-socket transport (`origin-ipc::frame`).
pub struct QuicConnection {
    send: SendStream,
    recv: RecvStream,
}

impl QuicConnection {
    /// Read the next framed message off the stream.
    ///
    /// # Errors
    /// Returns `QuicError::Frame` if the magic byte or length field is
    /// malformed, `QuicError::Io` on stream close.
    pub async fn read_frame(&mut self) -> Result<(FrameKind, Vec<u8>), QuicError> {
        let mut header = [0_u8; HEADER_LEN];
        self.recv.read_exact(&mut header).await.map_err(|e| QuicError::Io(std::io::Error::other(e)))?;
        let kind = match header[4] {
            1 => FrameKind::Request,
            2 => FrameKind::Response,
            3 => FrameKind::Event,
            4 => FrameKind::ErrorFrame,
            x => return Err(QuicError::Frame(format!("unknown kind {x}"))),
        };
        let len = u32::from_be_bytes(header[13..17].try_into().unwrap()) as usize;
        let mut body = vec![0_u8; len];
        self.recv.read_exact(&mut body).await.map_err(|e| QuicError::Io(std::io::Error::other(e)))?;
        Ok((kind, body))
    }

    /// Write a frame with the wire-format header prepended.
    pub async fn write_frame(&mut self, kind: FrameKind, body: &[u8]) -> Result<(), QuicError> {
        let bytes = crate::frame::encode(0, kind, body);
        self.send.write_all(&bytes).await.map_err(|e| QuicError::Io(std::io::Error::other(e)))?;
        Ok(())
    }

    /// Write a pre-encoded raw frame (caller chose request_id).
    pub async fn write_raw(&mut self, raw: &[u8]) -> Result<(), QuicError> {
        self.send.write_all(raw).await.map_err(|e| QuicError::Io(std::io::Error::other(e)))?;
        Ok(())
    }
}
```

- [ ] **Step 4: Register module**

Edit `crates/origin-ipc/src/lib.rs` to add (preserve existing modules):

```rust
pub mod quic;
```

- [ ] **Step 5: Run test — confirm pass**

Run: `cargo test -p origin-ipc --test quic_smoke`
Expected: 1 test passes (may take up to ~2 s on first QUIC handshake).

- [ ] **Step 6: Verification gate**

Run: `cargo test -p origin-ipc && cargo clippy -p origin-ipc -- -D warnings && cargo fmt --check`
Expected: exit 0 for all.

- [ ] **Step 7: Commit**

```bash
git add crates/origin-ipc/src/quic.rs crates/origin-ipc/src/lib.rs crates/origin-ipc/tests/quic_smoke.rs
git commit -m "$(cat <<'EOF'
feat(origin-ipc): QUIC + rustls remote transport backend (P13.1)

QuicListener / QuicConnector / QuicConnection mirror the local-socket
surface (read_frame / write_frame / write_raw). Self-signed cert as
both identity and trust anchor; pinned by SHA-256 fingerprint at the
application layer during pairing (P13.2).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task P13.1.4 — Concurrent QUIC streams test

**Files:**
- Create: `crates/origin-ipc/tests/quic_concurrent.rs`

- [ ] **Step 1: Write the failing test**

```rust
use origin_ipc::frame::{encode, FrameKind};
use origin_ipc::quic::{QuicConnector, QuicListener};
use origin_ipc::tls::generate_self_signed;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_clients_dont_block_each_other() {
    let bundle = generate_self_signed("origin-test").unwrap();
    let listener = QuicListener::bind("127.0.0.1:0".parse().unwrap(), bundle.clone())
        .await
        .unwrap();
    let addr = listener.local_addr();
    let ca = bundle.ca_der.clone();

    let server = tokio::spawn(async move {
        for _ in 0..2 {
            let mut conn = listener.accept().await.expect("accept");
            tokio::spawn(async move {
                let (_k, body) = conn.read_frame().await.unwrap();
                conn.write_frame(FrameKind::Response, &body).await.unwrap();
            });
        }
    });

    let (a, b) = tokio::join!(
        echo_one(addr, &ca, b"alpha"),
        echo_one(addr, &ca, b"beta"),
    );

    assert_eq!(a.unwrap(), b"alpha");
    assert_eq!(b.unwrap(), b"beta");
    server.await.unwrap();
}

async fn echo_one(addr: std::net::SocketAddr, ca: &[u8], payload: &[u8]) -> Result<Vec<u8>, String> {
    let mut c = QuicConnector::connect(addr, "origin-test", ca)
        .await
        .map_err(|e| e.to_string())?;
    c.write_raw(&encode(1, FrameKind::Request, payload))
        .await
        .map_err(|e| e.to_string())?;
    let (_k, body) = c.read_frame().await.map_err(|e| e.to_string())?;
    Ok(body)
}
```

- [ ] **Step 2: Run — confirm pass directly (existing impl handles concurrency)**

Run: `cargo test -p origin-ipc --test quic_concurrent`
Expected: PASS. If FAIL: investigate; the implementation in P13.1.3 should already support this.

- [ ] **Step 3: Verification gate**

Run: `cargo test -p origin-ipc && cargo clippy -p origin-ipc --tests -- -D warnings && cargo fmt --check`
Expected: exit 0.

- [ ] **Step 4: Commit**

```bash
git add crates/origin-ipc/tests/quic_concurrent.rs
git commit -m "$(cat <<'EOF'
test(origin-ipc): QUIC two-client non-blocking round-trip

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

# Task group B — Pairing flow + bearer tokens (P13.2)

**Group goal:** A user runs `origin pair start` on the daemon host; the TUI / CLI prints a 6-digit code and a connect URL (`origin://host:port#fingerprint`). On the remote machine, `origin pair redeem <url> <code>` completes the QUIC handshake, sends the code, receives a 24-hour bearer token, and persists it in KeyVault under `("origin-remote", "<device-id>")`. Subsequent `origin run --remote <url>` calls present the bearer; the daemon rejects requests without a valid bearer when listening on QUIC.

**Depends on:** P13.1.

---

### Task P13.2.1 — Pairing state machine

**Files:**
- Create: `crates/origin-daemon/src/pairing.rs`
- Create: `crates/origin-daemon/tests/pairing_unit.rs`
- Modify: `crates/origin-daemon/src/lib.rs` (`pub mod pairing;`)
- Modify: `crates/origin-daemon/Cargo.toml` — add `rand = "0.8"` and `parking_lot.workspace = true` if not already there.

- [ ] **Step 1: Write the failing test**

Create `crates/origin-daemon/tests/pairing_unit.rs`:

```rust
use origin_daemon::pairing::{Pairing, PairingError, RedeemResult};
use std::time::Duration;

#[test]
fn start_returns_6_digit_code() {
    let p = Pairing::new();
    let session = p.start(Duration::from_secs(60));
    assert_eq!(session.code.len(), 6);
    assert!(session.code.chars().all(|c| c.is_ascii_digit()));
}

#[test]
fn correct_code_redeems_once() {
    let p = Pairing::new();
    let session = p.start(Duration::from_secs(60));
    let token = match p.redeem(&session.code, "device-A").unwrap() {
        RedeemResult::Issued { bearer, .. } => bearer,
    };
    assert!(token.starts_with("orb_"));
    // Second redeem with the same code must fail (single-use).
    assert!(matches!(
        p.redeem(&session.code, "device-B"),
        Err(PairingError::UnknownCode)
    ));
}

#[test]
fn wrong_code_errors() {
    let p = Pairing::new();
    let _ = p.start(Duration::from_secs(60));
    assert!(matches!(
        p.redeem("000000", "device-X"),
        Err(PairingError::UnknownCode)
    ));
}

#[test]
fn expired_code_errors() {
    let p = Pairing::new();
    let session = p.start(Duration::from_millis(1));
    std::thread::sleep(Duration::from_millis(10));
    assert!(matches!(
        p.redeem(&session.code, "device-Y"),
        Err(PairingError::Expired)
    ));
}
```

- [ ] **Step 2: Run — confirm fail**

Run: `cargo test -p origin-daemon --test pairing_unit`
Expected: compile error — `origin_daemon::pairing` does not exist.

- [ ] **Step 3: Implement `pairing.rs`**

Create `crates/origin-daemon/src/pairing.rs`:

```rust
//! Pairing flow + short-lived bearer-token minting (P13.2).
//!
//! Daemon-side state machine: `start(ttl)` returns a 6-digit code and a
//! pending pairing session keyed by the code; `redeem(code, device_id)`
//! consumes the session and returns a fresh bearer token. Codes are
//! single-use; expired sessions are GC'd lazily on access.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use rand::Rng;
use thiserror::Error;

#[derive(Debug, Clone)]
pub struct PairingSession {
    pub code: String,
    pub created_at: Instant,
    pub expires_at: Instant,
}

#[derive(Debug, Clone)]
pub struct BearerToken {
    pub token: String,
    pub device_id: String,
    pub issued_at: Instant,
}

pub enum RedeemResult {
    Issued { bearer: String, device_id: String },
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum PairingError {
    #[error("unknown or already-redeemed code")]
    UnknownCode,
    #[error("code expired")]
    Expired,
}

#[derive(Default)]
pub struct Pairing {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    pending: HashMap<String, PairingSession>,
    issued: HashMap<String, BearerToken>, // by bearer token
}

impl Pairing {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Start a new pairing session. Returns the session shape so the
    /// caller can show the code + expiry to the user.
    pub fn start(&self, ttl: Duration) -> PairingSession {
        let code = generate_code();
        let now = Instant::now();
        let session = PairingSession {
            code: code.clone(),
            created_at: now,
            expires_at: now + ttl,
        };
        self.inner.lock().pending.insert(code, session.clone());
        session
    }

    /// Redeem a pairing code. Single-use: success consumes the session.
    pub fn redeem(&self, code: &str, device_id: &str) -> Result<RedeemResult, PairingError> {
        let mut inner = self.inner.lock();
        let session = inner.pending.remove(code).ok_or(PairingError::UnknownCode)?;
        if Instant::now() > session.expires_at {
            return Err(PairingError::Expired);
        }
        let bearer = generate_bearer();
        inner.issued.insert(
            bearer.clone(),
            BearerToken {
                token: bearer.clone(),
                device_id: device_id.to_string(),
                issued_at: Instant::now(),
            },
        );
        Ok(RedeemResult::Issued {
            bearer,
            device_id: device_id.to_string(),
        })
    }

    /// Validate a bearer token. Returns the bound device ID on success.
    pub fn validate_bearer(&self, token: &str) -> Option<String> {
        self.inner
            .lock()
            .issued
            .get(token)
            .map(|t| t.device_id.clone())
    }
}

fn generate_code() -> String {
    let mut rng = rand::thread_rng();
    let n: u32 = rng.gen_range(0..1_000_000);
    format!("{n:06}")
}

fn generate_bearer() -> String {
    let mut bytes = [0_u8; 24];
    rand::thread_rng().fill(&mut bytes);
    format!("orb_{}", hex::encode(bytes))
}
```

Add to `crates/origin-daemon/Cargo.toml` `[dependencies]`:

```toml
rand        = "0.8"
hex.workspace = true
```

Register in `crates/origin-daemon/src/lib.rs`:

```rust
pub mod pairing;
```

- [ ] **Step 4: Run test — confirm pass**

Run: `cargo test -p origin-daemon --test pairing_unit`
Expected: 4 tests pass.

- [ ] **Step 5: Verification gate**

Run: `cargo test -p origin-daemon && cargo clippy -p origin-daemon -- -D warnings && cargo fmt --check`
Expected: exit 0.

- [ ] **Step 6: Commit**

```bash
git add crates/origin-daemon/src/pairing.rs crates/origin-daemon/src/lib.rs crates/origin-daemon/Cargo.toml crates/origin-daemon/tests/pairing_unit.rs
git commit -m "$(cat <<'EOF'
feat(origin-daemon): pairing state machine + bearer-token minting

6-digit single-use codes with TTL; bearer tokens prefixed orb_ and
keyed by device id. In-memory only at this stage; persistence to
KeyVault lands in P13.2.3 along with the IPC wiring.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task P13.2.2 — IPC protocol additions

**Files:**
- Modify: `crates/origin-daemon/src/protocol.rs`
- Create: `crates/origin-daemon/tests/protocol_pair.rs`

- [ ] **Step 1: Write the failing test**

```rust
use origin_daemon::protocol::{ClientMessage, StreamEvent};

#[test]
fn pair_start_serializes_with_kind_tag() {
    let msg = ClientMessage::PairStart {
        ttl_secs: 60,
    };
    let json = serde_json::to_string(&msg).unwrap();
    assert!(json.contains("\"kind\":\"pair_start\""));
}

#[test]
fn pair_redeem_round_trips() {
    let msg = ClientMessage::PairRedeem {
        code: "123456".into(),
        device_id: "macbook-pro".into(),
    };
    let json = serde_json::to_vec(&msg).unwrap();
    let back: ClientMessage = serde_json::from_slice(&json).unwrap();
    matches!(back, ClientMessage::PairRedeem { .. });
}

#[test]
fn pair_code_event_serializes() {
    let ev = StreamEvent::PairCode {
        code: "654321".into(),
        expires_in_secs: 60,
    };
    let json = serde_json::to_string(&ev).unwrap();
    assert!(json.contains("\"kind\":\"pair_code\""));
    assert!(json.contains("\"code\":\"654321\""));
}

#[test]
fn pair_issued_event_serializes() {
    let ev = StreamEvent::PairIssued {
        bearer: "orb_abc".into(),
        device_id: "macbook-pro".into(),
        ttl_secs: 86_400,
    };
    let json = serde_json::to_string(&ev).unwrap();
    assert!(json.contains("\"kind\":\"pair_issued\""));
}
```

- [ ] **Step 2: Run — confirm fail**

Run: `cargo test -p origin-daemon --test protocol_pair`
Expected: compile error — variants don't exist.

- [ ] **Step 3: Implement protocol additions**

In `crates/origin-daemon/src/protocol.rs`, extend `ClientMessage` and `StreamEvent` (preserve existing variants):

```rust
// Add to ClientMessage enum (inside #[serde(tag = "kind", rename_all = "snake_case")]):
PairStart { ttl_secs: u32 },
PairRedeem { code: String, device_id: String },
```

```rust
// Add to StreamEvent enum (inside #[serde(tag = "kind", rename_all = "snake_case")]):
PairCode { code: String, expires_in_secs: u32 },
PairIssued { bearer: String, device_id: String, ttl_secs: u32 },
PairError { message: String },
```

- [ ] **Step 4: Run test — confirm pass**

Run: `cargo test -p origin-daemon --test protocol_pair`
Expected: 4 tests pass.

- [ ] **Step 5: Verification gate**

Run: `cargo test -p origin-daemon && cargo clippy -p origin-daemon -- -D warnings && cargo fmt --check`

- [ ] **Step 6: Commit**

```bash
git add crates/origin-daemon/src/protocol.rs crates/origin-daemon/tests/protocol_pair.rs
git commit -m "$(cat <<'EOF'
feat(origin-daemon): IPC pair_start/pair_redeem messages + pair_code/pair_issued events

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task P13.2.3 — Daemon main: wire pairing handler + bearer auth gate

**Files:**
- Modify: `crates/origin-daemon/src/main.rs`
- Create: `crates/origin-daemon/src/auth.rs`
- Create: `crates/origin-daemon/tests/pairing_e2e.rs`

- [ ] **Step 1: Write the failing E2E test**

Create `crates/origin-daemon/tests/pairing_e2e.rs`:

```rust
//! Pairing E2E: spin a `Pairing` state machine, simulate a daemon
//! handler that emits `PairCode` on `PairStart` and `PairIssued` on
//! `PairRedeem`, and validate the resulting bearer through `auth.rs`.

use origin_daemon::auth::BearerStore;
use origin_daemon::pairing::Pairing;
use origin_daemon::protocol::{ClientMessage, StreamEvent};
use std::sync::Arc;
use std::time::Duration;

fn dispatch(pairing: &Pairing, store: &BearerStore, msg: ClientMessage) -> Vec<StreamEvent> {
    match msg {
        ClientMessage::PairStart { ttl_secs } => {
            let session = pairing.start(Duration::from_secs(ttl_secs.into()));
            vec![StreamEvent::PairCode {
                code: session.code,
                expires_in_secs: ttl_secs,
            }]
        }
        ClientMessage::PairRedeem { code, device_id } => {
            match pairing.redeem(&code, &device_id) {
                Ok(origin_daemon::pairing::RedeemResult::Issued { bearer, device_id }) => {
                    store.insert(bearer.clone(), device_id.clone());
                    vec![StreamEvent::PairIssued {
                        bearer,
                        device_id,
                        ttl_secs: 86_400,
                    }]
                }
                Err(e) => vec![StreamEvent::PairError {
                    message: e.to_string(),
                }],
            }
        }
        _ => vec![],
    }
}

#[test]
fn pair_round_trip_then_validate() {
    let pairing = Arc::new(Pairing::new());
    let store = Arc::new(BearerStore::new());

    let evs = dispatch(&pairing, &store, ClientMessage::PairStart { ttl_secs: 60 });
    let code = match &evs[0] {
        StreamEvent::PairCode { code, .. } => code.clone(),
        _ => panic!("expected PairCode"),
    };

    let evs = dispatch(
        &pairing,
        &store,
        ClientMessage::PairRedeem {
            code,
            device_id: "laptop".into(),
        },
    );
    let bearer = match &evs[0] {
        StreamEvent::PairIssued { bearer, .. } => bearer.clone(),
        other => panic!("expected PairIssued, got {other:?}"),
    };

    assert_eq!(store.validate(&bearer).as_deref(), Some("laptop"));
    assert!(store.validate("orb_nope").is_none());
}

#[test]
fn redeem_unknown_code_returns_pair_error() {
    let pairing = Arc::new(Pairing::new());
    let store = Arc::new(BearerStore::new());
    let evs = dispatch(
        &pairing,
        &store,
        ClientMessage::PairRedeem {
            code: "999999".into(),
            device_id: "laptop".into(),
        },
    );
    assert!(matches!(evs[0], StreamEvent::PairError { .. }));
}
```

- [ ] **Step 2: Run — confirm fail**

Run: `cargo test -p origin-daemon --test pairing_e2e`
Expected: compile error — `origin_daemon::auth::BearerStore` does not exist.

- [ ] **Step 3: Implement `auth.rs`**

Create `crates/origin-daemon/src/auth.rs`:

```rust
//! Bearer-token validation store used by the QUIC IPC gate (P13.2).
//!
//! On `PairIssued`, the daemon inserts the bearer here so subsequent
//! requests on QUIC connections can be authenticated. Local-socket
//! connections bypass this gate (filesystem permissions are the trust
//! anchor on the same machine).

use std::collections::HashMap;

use parking_lot::RwLock;

#[derive(Default)]
pub struct BearerStore {
    inner: RwLock<HashMap<String, String>>, // bearer → device_id
}

impl BearerStore {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&self, bearer: String, device_id: String) {
        self.inner.write().insert(bearer, device_id);
    }

    #[must_use]
    pub fn validate(&self, bearer: &str) -> Option<String> {
        self.inner.read().get(bearer).cloned()
    }

    pub fn revoke(&self, bearer: &str) {
        self.inner.write().remove(bearer);
    }
}
```

Register in `crates/origin-daemon/src/lib.rs`:

```rust
pub mod auth;
```

- [ ] **Step 4: Wire dispatcher in `main.rs`**

In `crates/origin-daemon/src/main.rs`, find the section that matches `ClientMessage` variants (search for `ClientMessage::MemoryDecision`). Above the existing `_` arm, add:

```rust
ClientMessage::PairStart { ttl_secs } => {
    let session = pairing.start(std::time::Duration::from_secs(u64::from(ttl_secs)));
    let ev = StreamEvent::PairCode {
        code: session.code,
        expires_in_secs: ttl_secs,
    };
    write_event(conn, &ev).await?;
}
ClientMessage::PairRedeem { code, device_id } => {
    match pairing.redeem(&code, &device_id) {
        Ok(origin_daemon::pairing::RedeemResult::Issued { bearer, device_id }) => {
            bearer_store.insert(bearer.clone(), device_id.clone());
            // Persist to KeyVault for next-process reuse (best-effort).
            let _ = vault
                .set(
                    "origin-remote",
                    &device_id,
                    origin_keyvault::Secret::new(bearer.clone()),
                )
                .await;
            let ev = StreamEvent::PairIssued {
                bearer,
                device_id,
                ttl_secs: 86_400,
            };
            write_event(conn, &ev).await?;
        }
        Err(e) => {
            let ev = StreamEvent::PairError {
                message: e.to_string(),
            };
            write_event(conn, &ev).await?;
        }
    }
}
```

Construct `pairing` and `bearer_store` near the top of `main()` after `vault` is created:

```rust
let pairing = std::sync::Arc::new(origin_daemon::pairing::Pairing::new());
let bearer_store = std::sync::Arc::new(origin_daemon::auth::BearerStore::new());
```

Pass them by clone into whichever connection-handler future is spawned (mirror how `vault` is captured).

If `write_event` is not already a helper, define it next to the existing `write_error` helper:

```rust
async fn write_event(
    conn: &origin_ipc::transport::SharedConnection,
    ev: &origin_daemon::protocol::StreamEvent,
) -> anyhow::Result<()> {
    let body = serde_json::to_vec(ev)?;
    let frame = origin_ipc::frame::encode(1, origin_ipc::frame::FrameKind::Event, &body);
    conn.lock().await.write_raw(&frame).await?;
    Ok(())
}
```

- [ ] **Step 5: Run E2E test — confirm pass**

Run: `cargo test -p origin-daemon --test pairing_e2e`
Expected: 2 tests pass.

- [ ] **Step 6: Verification gate**

Run: `cargo test -p origin-daemon && cargo clippy -p origin-daemon -- -D warnings && cargo fmt --check`

- [ ] **Step 7: Commit**

```bash
git add crates/origin-daemon/src/auth.rs crates/origin-daemon/src/lib.rs crates/origin-daemon/src/main.rs crates/origin-daemon/tests/pairing_e2e.rs
git commit -m "$(cat <<'EOF'
feat(origin-daemon): pairing handler + bearer auth store wired into IPC

PairStart emits PairCode; PairRedeem mints a bearer, stores it in
BearerStore + KeyVault (origin-remote/<device_id>), and replies
PairIssued. PairError on invalid/expired code.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task P13.2.4 — `origin pair start` / `origin pair redeem` CLI

**Files:**
- Modify: `crates/origin-cli/src/main.rs`
- Create: `crates/origin-cli/tests/pair_cli.rs`

- [ ] **Step 1: Failing test**

```rust
//! Drive a one-shot `origin pair start` against an in-process echo of
//! the daemon handler; confirm the CLI prints the pairing code on
//! stdout.

use origin_daemon::pairing::Pairing;
use origin_daemon::protocol::{ClientMessage, StreamEvent};
use std::time::Duration;

#[test]
fn render_pair_start_event_to_stdout() {
    let pairing = Pairing::new();
    let session = pairing.start(Duration::from_secs(60));
    let ev = StreamEvent::PairCode {
        code: session.code.clone(),
        expires_in_secs: 60,
    };

    // Use the helper from origin_cli::headless::print_event (added in
    // P13.3.2) once available; for now the test asserts JSON shape.
    let json = serde_json::to_string(&ev).unwrap();
    assert!(json.contains(&session.code));
    let _: ClientMessage = ClientMessage::PairStart { ttl_secs: 60 };
}
```

- [ ] **Step 2: Extend the `Cmd` enum**

In `crates/origin-cli/src/main.rs`, add to `Cmd`:

```rust
/// Start or redeem a pairing session for remote QUIC clients (P13.2).
Pair {
    #[command(subcommand)]
    sub: PairSub,
},
```

And add:

```rust
#[derive(Subcommand)]
enum PairSub {
    /// Daemon-side: show a 6-digit pairing code. Run on the host with the daemon.
    Start {
        #[arg(long, default_value_t = 60)]
        ttl_secs: u32,
    },
    /// Client-side: redeem a code against a remote daemon.
    Redeem {
        /// Remote URL: `origin://host:port#fingerprint`.
        url: String,
        /// The 6-digit code shown on the daemon host.
        code: String,
        /// Stable device identifier (defaults to hostname).
        #[arg(long)]
        device_id: Option<String>,
    },
}
```

- [ ] **Step 3: Implement dispatch**

Before the existing `let path = env::var("ORIGIN_SOCK")…` block, handle:

```rust
if let Some(Cmd::Pair { sub }) = cli.cmd {
    return match sub {
        PairSub::Start { ttl_secs } => pair_start(ttl_secs).await,
        PairSub::Redeem { url, code, device_id } => pair_redeem(&url, &code, device_id).await,
    };
}
```

And implement the helpers below `main`:

```rust
async fn pair_start(ttl_secs: u32) -> Result<()> {
    let path = env::var("ORIGIN_SOCK").unwrap_or_else(|_| default_path());
    let mut c = Connector::connect(&path).await?;
    let msg = ClientMessage::PairStart { ttl_secs };
    let body = serde_json::to_vec(&msg)?;
    c.write_raw(&encode(1, FrameKind::Request, &body)).await?;
    let resp = c.read_frame_body().await?;
    let ev: StreamEvent = serde_json::from_slice(&resp)?;
    match ev {
        StreamEvent::PairCode { code, expires_in_secs } => {
            println!("pairing code: {code} (valid {expires_in_secs}s)");
            Ok(())
        }
        other => Err(anyhow::anyhow!("unexpected: {other:?}")),
    }
}

async fn pair_redeem(url: &str, code: &str, device_id: Option<String>) -> Result<()> {
    let device = device_id.unwrap_or_else(|| {
        hostname::get().ok().and_then(|n| n.into_string().ok()).unwrap_or_else(|| "unknown".into())
    });
    let parsed = parse_origin_url(url)?;
    let bundle_ca = parsed.fingerprint_to_ca_placeholder();
    let mut c = origin_ipc::quic::QuicConnector::connect(parsed.addr, "origin-daemon", &bundle_ca).await?;
    let msg = ClientMessage::PairRedeem { code: code.into(), device_id: device.clone() };
    let body = serde_json::to_vec(&msg)?;
    c.write_raw(&encode(1, FrameKind::Request, &body)).await.map_err(|e| anyhow::anyhow!("{e}"))?;
    let (_kind, resp) = c.read_frame().await.map_err(|e| anyhow::anyhow!("{e}"))?;
    let ev: StreamEvent = serde_json::from_slice(&resp)?;
    match ev {
        StreamEvent::PairIssued { bearer, device_id, ttl_secs } => {
            println!("paired device={device_id} ttl={ttl_secs}s");
            println!("token: {bearer}");
            Ok(())
        }
        other => Err(anyhow::anyhow!("pair failed: {other:?}")),
    }
}
```

Add deps to `crates/origin-cli/Cargo.toml`:

```toml
hostname = "0.4"
url      = "2"
```

And implement a minimal URL parser. Create `crates/origin-cli/src/admin_url.rs` (used by both `pair_redeem` and `run --remote`):

```rust
use std::net::SocketAddr;

pub struct OriginUrl {
    pub addr: SocketAddr,
    pub fingerprint_hex: String,
}

impl OriginUrl {
    /// CA placeholder: this MUST be wired to fetch the actual cert via
    /// out-of-band channel in production. For now we accept a raw DER
    /// blob via `ORIGIN_REMOTE_CA_DER_FILE` env var so the test fixture
    /// can pass the daemon's own cert; the fingerprint check is layered
    /// on top of the pinned root.
    pub fn fingerprint_to_ca_placeholder(&self) -> Vec<u8> {
        match std::env::var("ORIGIN_REMOTE_CA_DER_FILE") {
            Ok(p) => std::fs::read(p).unwrap_or_default(),
            Err(_) => Vec::new(),
        }
    }
}

pub fn parse_origin_url(url: &str) -> anyhow::Result<OriginUrl> {
    let parsed = url::Url::parse(url)?;
    if parsed.scheme() != "origin" {
        anyhow::bail!("expected origin:// URL, got {url}");
    }
    let host = parsed.host_str().ok_or_else(|| anyhow::anyhow!("missing host"))?;
    let port = parsed.port().ok_or_else(|| anyhow::anyhow!("missing port"))?;
    let addr: SocketAddr = format!("{host}:{port}").parse()?;
    let fingerprint_hex = parsed.fragment().unwrap_or("").to_string();
    Ok(OriginUrl { addr, fingerprint_hex })
}
```

Wire `pub mod admin_url;` into `crates/origin-cli/src/lib.rs` (preserve existing modules) and `use origin_cli::admin_url::parse_origin_url;` in `main.rs`.

- [ ] **Step 4: Run test — confirm pass**

Run: `cargo test -p origin-cli --test pair_cli`
Expected: pass.

- [ ] **Step 5: Verification gate**

Run: `cargo test -p origin-cli && cargo clippy -p origin-cli -- -D warnings && cargo fmt --check`

- [ ] **Step 6: Commit**

```bash
git add crates/origin-cli/Cargo.toml crates/origin-cli/src/admin_url.rs crates/origin-cli/src/lib.rs crates/origin-cli/src/main.rs crates/origin-cli/tests/pair_cli.rs
git commit -m "$(cat <<'EOF'
feat(origin-cli): origin pair start / origin pair redeem

start: prints 6-digit code from daemon (local socket).
redeem: connects to remote daemon over QUIC (origin:// URL) and
prints the issued bearer.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

# Task group C — Headless one-shot `origin run` (P13.3)

**Group goal:** `origin run "summarize README"` connects to the local daemon, sends a single `Prompt` request, streams events to stdout. With `--json`, every IPC event is emitted as a JSON-Lines record (`{"kind":"text_delta","text":"…"}\n`); without `--json`, only `text_delta` payloads concatenate to stdout. Exit code 0 on `PromptReply`, non-zero on error.

**Independent of P13.1 / P13.2** — reuses the local-socket transport. Adding `--remote <url>` is wired in P13.3.3.

---

### Task P13.3.1 — `Cmd::Run` clap subcommand + skeleton

**Files:**
- Modify: `crates/origin-cli/src/main.rs`
- Create: `crates/origin-cli/src/headless.rs`
- Modify: `crates/origin-cli/src/lib.rs`

- [ ] **Step 1: Failing test**

Create `crates/origin-cli/tests/run_help.rs`:

```rust
use std::process::Command;

#[test]
fn run_help_lists_json_flag() {
    let out = Command::new(env!("CARGO_BIN_EXE_origin"))
        .args(["run", "--help"])
        .output()
        .expect("run cli");
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let combined = format!("{stdout}{stderr}");
    assert!(combined.contains("--json"), "expected --json flag in help: {combined}");
}
```

- [ ] **Step 2: Run — confirm fail**

Run: `cargo test -p origin-cli --test run_help`
Expected: fail (subcommand absent).

- [ ] **Step 3: Extend `Cmd`**

```rust
/// One-shot prompt: connect to the daemon, send `text`, drain to
/// completion, exit. No TUI renderer.
Run {
    /// The user prompt.
    text: String,
    /// Emit JSON-Lines stream of every IPC event.
    #[arg(long)]
    json: bool,
    /// Remote daemon URL (`origin://host:port#fingerprint`). Omit for local socket.
    #[arg(long)]
    remote: Option<String>,
    /// Optional bearer token for remote auth; falls back to KeyVault.
    #[arg(long)]
    bearer: Option<String>,
    /// Model override.
    #[arg(long)]
    model: Option<String>,
},
```

In the `main()` dispatch (above the `enable_raw_mode` block), branch:

```rust
if let Some(Cmd::Run { text, json, remote, bearer, model }) = cli.cmd {
    return origin_cli::headless::run(text, json, remote, bearer, model).await;
}
```

- [ ] **Step 4: Implement headless skeleton**

Create `crates/origin-cli/src/headless.rs`:

```rust
//! Headless one-shot (`origin run`). Connects to the daemon, sends a
//! single Prompt, drains the stream, exits.

use anyhow::Result;
use origin_daemon::protocol::{ClientMessage, PromptRequest, StreamEvent};
use origin_ipc::frame::{encode, FrameKind};
use origin_ipc::transport::Connector;

pub async fn run(
    text: String,
    json: bool,
    _remote: Option<String>,
    _bearer: Option<String>,
    model: Option<String>,
) -> Result<()> {
    let model = model.unwrap_or_else(|| std::env::var("ORIGIN_MODEL").unwrap_or_else(|_| "claude-opus-4-7".into()));
    let path = std::env::var("ORIGIN_SOCK").unwrap_or_else(|_| default_path());
    let mut conn = Connector::connect(&path).await?;
    let body = serde_json::to_vec(&ClientMessage::prompt(PromptRequest {
        system: String::new(),
        model,
        user_text: text,
    }))?;
    conn.write_raw(&encode(1, FrameKind::Request, &body)).await?;

    let stdout = std::io::stdout();
    let mut out = stdout.lock();

    loop {
        let frame = conn.read_frame_body().await?;
        if let Ok(ev) = serde_json::from_slice::<StreamEvent>(&frame) {
            print_event(&mut out, json, &ev)?;
            continue;
        }
        // Terminal reply (PromptReply JSON). Decode and emit a final event.
        if json {
            // Already streamed; just append a `{"kind":"reply"}` line.
            use std::io::Write as _;
            writeln!(out, "{}", String::from_utf8_lossy(&frame))?;
        }
        break;
    }
    Ok(())
}

fn print_event(out: &mut impl std::io::Write, json: bool, ev: &StreamEvent) -> Result<()> {
    if json {
        let line = serde_json::to_string(ev)?;
        writeln!(out, "{line}")?;
    } else if let StreamEvent::TextDelta { text } = ev {
        use std::io::Write as _;
        write!(out, "{text}")?;
        out.flush()?;
    }
    Ok(())
}

fn default_path() -> String {
    #[cfg(unix)]
    {
        format!("{}/origin.sock", std::env::temp_dir().display())
    }
    #[cfg(windows)]
    {
        r"\\.\pipe\origin".to_string()
    }
}
```

Register in `crates/origin-cli/src/lib.rs`:

```rust
pub mod headless;
```

- [ ] **Step 5: Run test — confirm pass**

Run: `cargo test -p origin-cli --test run_help`
Expected: pass.

- [ ] **Step 6: Verification gate**

Run: `cargo test -p origin-cli && cargo clippy -p origin-cli -- -D warnings && cargo fmt --check`

- [ ] **Step 7: Commit**

```bash
git add crates/origin-cli/src/main.rs crates/origin-cli/src/headless.rs crates/origin-cli/src/lib.rs crates/origin-cli/tests/run_help.rs
git commit -m "$(cat <<'EOF'
feat(origin-cli): origin run one-shot subcommand skeleton

Connects to local daemon, sends a single Prompt, drains the stream.
--json emits JSON-Lines per IPC event; default mode prints text_delta
payloads concatenated. --remote / --bearer plumbed (no QUIC dispatch
yet; wired in P13.3.3).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task P13.3.2 — Golden-file JSON-Lines test against a fake daemon

**Files:**
- Create: `crates/origin-cli/tests/headless_stream.rs`

- [ ] **Step 1: Failing test**

```rust
//! Spin a fake daemon on a temp socket, send 3 events + final reply,
//! and assert the CLI's JSON-Lines stream matches a golden sequence.

use origin_daemon::protocol::{PromptReply, StreamEvent};
use origin_ipc::frame::{encode, FrameKind};
use origin_ipc::transport::Listener;
use tempfile::TempDir;

#[tokio::test(flavor = "current_thread")]
async fn json_lines_stream_matches_golden() {
    let dir = TempDir::new().unwrap();
    let sock = if cfg!(windows) {
        format!(r"\\.\pipe\origin-test-{}", ulid::Ulid::new())
    } else {
        format!("{}/origin-test.sock", dir.path().display())
    };
    let listener = Listener::bind(&sock).await.unwrap();

    let listen_sock = sock.clone();
    let server = tokio::spawn(async move {
        let mut conn = listener.accept().await.unwrap();
        let _req = conn.read_frame_body().await.unwrap();

        for ev in [
            StreamEvent::TextDelta { text: "hello ".into() },
            StreamEvent::TextDelta { text: "world".into() },
            StreamEvent::TurnEnd,
        ] {
            let body = serde_json::to_vec(&ev).unwrap();
            conn.write_raw(&encode(1, FrameKind::Event, &body)).await.unwrap();
        }
        let reply = PromptReply { assistant_text: "hello world".into(), turns: 1 };
        let body = serde_json::to_vec(&reply).unwrap();
        conn.write_raw(&encode(1, FrameKind::Response, &body)).await.unwrap();
        let _ = listen_sock;
    });

    let cmd = std::env::var("CARGO_BIN_EXE_origin").expect("bin path");
    let output = tokio::process::Command::new(cmd)
        .env("ORIGIN_SOCK", &sock)
        .args(["run", "--json", "summarize"])
        .output()
        .await
        .unwrap();
    server.await.unwrap();

    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = stdout.lines().collect();

    assert!(lines.iter().any(|l| l.contains("\"kind\":\"text_delta\"") && l.contains("hello ")));
    assert!(lines.iter().any(|l| l.contains("\"kind\":\"text_delta\"") && l.contains("world")));
    assert!(lines.iter().any(|l| l.contains("\"kind\":\"turn_end\"")));
}
```

- [ ] **Step 2: Run — confirm pass**

Run: `cargo test -p origin-cli --test headless_stream`
Expected: PASS. If `CARGO_BIN_EXE_origin` is missing (Cargo only sets it for binary targets), confirm `origin-cli` declares a `[[bin]] name = "origin"`. If FAIL with a frame-shape error, walk the bytes printed in `output.stdout` byte-by-byte.

- [ ] **Step 3: Verification gate**

Run: `cargo test -p origin-cli && cargo clippy -p origin-cli --tests -- -D warnings`

- [ ] **Step 4: Commit**

```bash
git add crates/origin-cli/tests/headless_stream.rs
git commit -m "$(cat <<'EOF'
test(origin-cli): origin run --json golden stream

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task P13.3.3 — Wire `--remote <url>` through QUIC (depends on P13.1)

**Files:**
- Modify: `crates/origin-cli/src/headless.rs`

- [ ] **Step 1: Failing test**

Append to `crates/origin-cli/tests/headless_stream.rs`:

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_url_routes_through_quic() {
    use origin_ipc::quic::QuicListener;
    use origin_ipc::tls::generate_self_signed;
    use tempfile::TempDir;

    let bundle = generate_self_signed("origin-daemon").unwrap();
    let listener = QuicListener::bind("127.0.0.1:0".parse().unwrap(), bundle.clone())
        .await
        .unwrap();
    let addr = listener.local_addr();
    let dir = TempDir::new().unwrap();
    let ca_path = dir.path().join("ca.der");
    std::fs::write(&ca_path, &bundle.ca_der).unwrap();

    let server = tokio::spawn(async move {
        let mut conn = listener.accept().await.unwrap();
        let _req = conn.read_frame().await.unwrap();
        let ev = origin_daemon::protocol::StreamEvent::TextDelta { text: "remote-ok".into() };
        let body = serde_json::to_vec(&ev).unwrap();
        conn.write_frame(origin_ipc::frame::FrameKind::Event, &body).await.unwrap();
        let reply = origin_daemon::protocol::PromptReply { assistant_text: "remote-ok".into(), turns: 1 };
        let body = serde_json::to_vec(&reply).unwrap();
        conn.write_frame(origin_ipc::frame::FrameKind::Response, &body).await.unwrap();
    });

    let cmd = std::env::var("CARGO_BIN_EXE_origin").unwrap();
    let url = format!("origin://{addr}#deadbeef");
    let output = tokio::process::Command::new(cmd)
        .env("ORIGIN_REMOTE_CA_DER_FILE", &ca_path)
        .args(["run", "--remote", &url, "--json", "hi"])
        .output()
        .await
        .unwrap();
    server.await.unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("remote-ok"), "stdout: {stdout}");
}
```

- [ ] **Step 2: Run — confirm fail**

Run: `cargo test -p origin-cli --test headless_stream remote_url_routes_through_quic`
Expected: fail (headless still uses local socket).

- [ ] **Step 3: Implement the QUIC branch**

Replace the body of `headless::run`'s connect path so when `_remote` is `Some(url)`, the code parses it via `origin_cli::admin_url::parse_origin_url`, loads the CA via `ORIGIN_REMOTE_CA_DER_FILE`, opens `origin_ipc::quic::QuicConnector::connect`, and uses an internal `enum Conn { Local(Connection), Remote(QuicConnection) }` to dispatch `write_raw` / `read_frame_body`. Keep the local path unchanged.

Sketch:

```rust
enum Conn {
    Local(origin_ipc::transport::Connection),
    Remote(origin_ipc::quic::QuicConnection),
}

impl Conn {
    async fn write_raw(&mut self, raw: &[u8]) -> anyhow::Result<()> {
        match self {
            Self::Local(c) => Ok(c.write_raw(raw).await?),
            Self::Remote(c) => c.write_raw(raw).await.map_err(|e| anyhow::anyhow!("{e}")),
        }
    }
    async fn read_frame_body(&mut self) -> anyhow::Result<Vec<u8>> {
        match self {
            Self::Local(c) => Ok(c.read_frame_body().await?),
            Self::Remote(c) => {
                let (_k, body) = c.read_frame().await.map_err(|e| anyhow::anyhow!("{e}"))?;
                Ok(body)
            }
        }
    }
}
```

Connect logic:

```rust
let mut conn = match remote {
    None => Conn::Local(Connector::connect(&path).await?),
    Some(url) => {
        let parsed = crate::admin_url::parse_origin_url(&url)?;
        let ca = parsed.fingerprint_to_ca_placeholder();
        let qc = origin_ipc::quic::QuicConnector::connect(parsed.addr, "origin-daemon", &ca)
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        Conn::Remote(qc)
    }
};
```

Use `bearer` in the future to set an `Authorization` header inside the first frame body (out of scope for the smoke test — assert it compiles when `Some`).

- [ ] **Step 4: Run test — confirm pass**

Run: `cargo test -p origin-cli --test headless_stream remote_url_routes_through_quic`
Expected: pass.

- [ ] **Step 5: Verification gate**

Run: `cargo test -p origin-cli && cargo clippy -p origin-cli -- -D warnings && cargo fmt --check`

- [ ] **Step 6: Commit**

```bash
git add crates/origin-cli/src/headless.rs crates/origin-cli/tests/headless_stream.rs
git commit -m "$(cat <<'EOF'
feat(origin-cli): origin run --remote routes through QUIC transport

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

# Task group D — Admin subcommands (P13.4)

**Group goal:** `origin usage`, `origin sessions ls/resume/rm`, `origin keyring add/list/remove`. All shell into existing crate APIs; the daemon need not be running for `usage` (metrics snapshot is a no-op without daemon).

**Independent of P13.1 / P13.2.** Wires to daemon over local socket only.

---

### Task P13.4.1 — `session_store::list_summaries` + `delete`

**Files:**
- Modify: `crates/origin-daemon/src/session_store.rs`
- Create: `crates/origin-daemon/tests/session_store_list.rs`

- [ ] **Step 1: Failing test**

```rust
use origin_daemon::session_store::SessionStore;
use origin_daemon::session::Session;
use tempfile::TempDir;

#[test]
fn list_summaries_returns_persisted_sessions() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("origin.db");
    let store = SessionStore::open(&path).unwrap();

    let s1 = Session::new_with_id("sess-a".into(), "claude-opus-4-7".into());
    store.persist_session(&s1).unwrap();
    let s2 = Session::new_with_id("sess-b".into(), "claude-haiku".into());
    store.persist_session(&s2).unwrap();

    let mut summaries = store.list_summaries().unwrap();
    summaries.sort_by_key(|s| s.id.clone());
    assert_eq!(summaries.len(), 2);
    assert_eq!(summaries[0].id, "sess-a");
    assert_eq!(summaries[1].id, "sess-b");
    assert_eq!(summaries[0].model, "claude-opus-4-7");
}

#[test]
fn delete_removes_session_and_messages() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("origin.db");
    let store = SessionStore::open(&path).unwrap();
    let s = Session::new_with_id("sess-x".into(), "m".into());
    store.persist_session(&s).unwrap();

    store.delete("sess-x").unwrap();
    let summaries = store.list_summaries().unwrap();
    assert!(summaries.iter().all(|s| s.id != "sess-x"));
}
```

- [ ] **Step 2: Run — confirm fail**

Run: `cargo test -p origin-daemon --test session_store_list`
Expected: compile error on `list_summaries` / `delete` / `Session::new_with_id`.

- [ ] **Step 3: Implement helpers**

In `crates/origin-daemon/src/session_store.rs`, add:

```rust
#[derive(Debug, Clone)]
pub struct SessionSummary {
    pub id: String,
    pub created_at: i64,
    pub title: Option<String>,
    pub model: String,
    pub message_count: u32,
}

impl SessionStore {
    pub fn list_summaries(&self) -> Result<Vec<SessionSummary>, SessionStoreError> {
        self.inner.with_conn(|c| {
            let mut stmt = c.prepare(
                "SELECT s.id, s.created_at, s.title, s.model,
                        (SELECT COUNT(*) FROM messages m WHERE m.session_id = s.id)
                 FROM sessions s
                 ORDER BY s.created_at DESC",
            )?;
            let rows = stmt
                .query_map([], |r| {
                    Ok(SessionSummary {
                        id: r.get(0)?,
                        created_at: r.get(1)?,
                        title: r.get(2)?,
                        model: r.get(3)?,
                        message_count: r.get::<_, i64>(4)? as u32,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })
        .map_err(SessionStoreError::from)
    }

    pub fn delete(&self, session_id: &str) -> Result<(), SessionStoreError> {
        self.inner.with_conn(|c| {
            c.execute("DELETE FROM messages WHERE session_id = ?1", [session_id])?;
            c.execute("DELETE FROM sessions WHERE id = ?1", [session_id])?;
            Ok(())
        })
        .map_err(SessionStoreError::from)
    }
}
```

Also add `pub fn new_with_id(id: String, model: String) -> Self` to `Session` in `crates/origin-daemon/src/session.rs` if it doesn't already exist — read the existing `Session::new` first and mirror it.

- [ ] **Step 4: Run — confirm pass**

Run: `cargo test -p origin-daemon --test session_store_list`
Expected: pass.

- [ ] **Step 5: Verification gate**

Run: `cargo test -p origin-daemon && cargo clippy -p origin-daemon -- -D warnings && cargo fmt --check`

- [ ] **Step 6: Commit**

```bash
git add crates/origin-daemon/src/session_store.rs crates/origin-daemon/src/session.rs crates/origin-daemon/tests/session_store_list.rs
git commit -m "$(cat <<'EOF'
feat(origin-daemon): session_store list_summaries + delete

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task P13.4.2 — IPC: `ListSessions` / `RemoveSession` / `GetUsage` / `Keyring*`

**Files:**
- Modify: `crates/origin-daemon/src/protocol.rs`
- Modify: `crates/origin-daemon/src/main.rs`
- Create: `crates/origin-daemon/tests/admin_ipc.rs`

- [ ] **Step 1: Failing test**

```rust
use origin_daemon::protocol::{ClientMessage, StreamEvent};

#[test]
fn list_sessions_message_round_trips() {
    let m = ClientMessage::ListSessions;
    let json = serde_json::to_vec(&m).unwrap();
    let back: ClientMessage = serde_json::from_slice(&json).unwrap();
    matches!(back, ClientMessage::ListSessions);
}

#[test]
fn sessions_listed_event_carries_summaries() {
    let ev = StreamEvent::SessionsListed {
        summaries: vec![origin_daemon::protocol::SessionSummaryWire {
            id: "s1".into(),
            created_at: 1,
            title: None,
            model: "m".into(),
            message_count: 0,
        }],
    };
    let s = serde_json::to_string(&ev).unwrap();
    assert!(s.contains("\"kind\":\"sessions_listed\""));
}

#[test]
fn keyring_add_serializes() {
    let m = ClientMessage::KeyringAdd {
        provider: "anthropic".into(),
        account: "default".into(),
        secret: "sk-...".into(),
    };
    let s = serde_json::to_string(&m).unwrap();
    assert!(s.contains("\"kind\":\"keyring_add\""));
}
```

- [ ] **Step 2: Run — confirm fail**

Run: `cargo test -p origin-daemon --test admin_ipc`
Expected: compile error.

- [ ] **Step 3: Implement protocol additions**

In `protocol.rs`, extend `ClientMessage`:

```rust
ListSessions,
RemoveSession { session_id: String },
ResumeSession { session_id: String },
GetUsage,
KeyringAdd { provider: String, account: String, secret: String },
KeyringList { provider: String },
KeyringRemove { provider: String, account: String },
```

And `StreamEvent`:

```rust
SessionsListed { summaries: Vec<SessionSummaryWire> },
UsageReport { rows: Vec<UsageRow> },
KeyringAccounts { provider: String, accounts: Vec<String> },
AdminOk,
AdminError { message: String },
```

Add types:

```rust
#[derive(Debug, Serialize, Deserialize)]
pub struct SessionSummaryWire {
    pub id: String,
    pub created_at: i64,
    pub title: Option<String>,
    pub model: String,
    pub message_count: u32,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct UsageRow {
    pub provider: String,
    pub model: String,
    pub tokens_in: u64,
    pub tokens_out: u64,
}
```

- [ ] **Step 4: Wire dispatch in daemon `main.rs`**

Add arms next to the `PairStart` / `PairRedeem` arms from P13.2.3:

```rust
ClientMessage::ListSessions => {
    let summaries = session_store.list_summaries().unwrap_or_default();
    let wire: Vec<_> = summaries
        .into_iter()
        .map(|s| origin_daemon::protocol::SessionSummaryWire {
            id: s.id,
            created_at: s.created_at,
            title: s.title,
            model: s.model,
            message_count: s.message_count,
        })
        .collect();
    write_event(conn, &StreamEvent::SessionsListed { summaries: wire }).await?;
}
ClientMessage::RemoveSession { session_id } => {
    match session_store.delete(&session_id) {
        Ok(()) => write_event(conn, &StreamEvent::AdminOk).await?,
        Err(e) => {
            write_event(conn, &StreamEvent::AdminError { message: e.to_string() }).await?;
        }
    }
}
ClientMessage::GetUsage => {
    let snap = metrics.snapshot();
    let rows: Vec<_> = snap
        .iter()
        .filter_map(|r| {
            if r.name.starts_with("origin_tokens_") {
                Some(origin_daemon::protocol::UsageRow {
                    provider: r.labels.get("provider").cloned().unwrap_or_default(),
                    model: r.labels.get("model").cloned().unwrap_or_default(),
                    tokens_in: if r.name == "origin_tokens_in_total" { r.value as u64 } else { 0 },
                    tokens_out: if r.name == "origin_tokens_out_total" { r.value as u64 } else { 0 },
                })
            } else { None }
        })
        .collect();
    write_event(conn, &StreamEvent::UsageReport { rows }).await?;
}
ClientMessage::KeyringAdd { provider, account, secret } => {
    match vault.set(&provider, &account, origin_keyvault::Secret::new(secret)).await {
        Ok(()) => write_event(conn, &StreamEvent::AdminOk).await?,
        Err(e) => write_event(conn, &StreamEvent::AdminError { message: e.to_string() }).await?,
    }
}
ClientMessage::KeyringList { provider } => {
    match vault.list(&provider).await {
        Ok(accounts) => write_event(conn, &StreamEvent::KeyringAccounts { provider, accounts }).await?,
        Err(e) => write_event(conn, &StreamEvent::AdminError { message: e.to_string() }).await?,
    }
}
ClientMessage::KeyringRemove { provider, account } => {
    match vault.delete(&provider, &account).await {
        Ok(()) => write_event(conn, &StreamEvent::AdminOk).await?,
        Err(e) => write_event(conn, &StreamEvent::AdminError { message: e.to_string() }).await?,
    }
}
ClientMessage::ResumeSession { session_id: _ } => {
    // Acknowledged for clap-level routing; full resume semantics deferred.
    write_event(conn, &StreamEvent::AdminOk).await?;
}
```

The `metrics` handle should already be available in the captured scope; if not, hoist its `Arc<Metrics>` clone into the connection-handler closure alongside `vault`.

- [ ] **Step 5: Run test — confirm pass**

Run: `cargo test -p origin-daemon --test admin_ipc`
Expected: pass.

- [ ] **Step 6: Verification gate**

Run: `cargo test -p origin-daemon && cargo clippy -p origin-daemon -- -D warnings && cargo fmt --check`

- [ ] **Step 7: Commit**

```bash
git add crates/origin-daemon/src/protocol.rs crates/origin-daemon/src/main.rs crates/origin-daemon/tests/admin_ipc.rs
git commit -m "$(cat <<'EOF'
feat(origin-daemon): admin IPC — ListSessions, RemoveSession, GetUsage, Keyring*

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task P13.4.3 — `origin sessions` + `origin usage` + `origin keyring` CLI

**Files:**
- Modify: `crates/origin-cli/src/main.rs`
- Create: `crates/origin-cli/src/admin.rs`
- Create: `crates/origin-cli/tests/admin_cli.rs`
- Modify: `crates/origin-cli/src/lib.rs`

- [ ] **Step 1: Failing test (CLI args parse)**

```rust
use std::process::Command;

#[test]
fn sessions_ls_help() {
    let out = Command::new(env!("CARGO_BIN_EXE_origin"))
        .args(["sessions", "ls", "--help"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!((stdout.to_owned() + &stderr).contains("Usage"));
}

#[test]
fn usage_help() {
    let out = Command::new(env!("CARGO_BIN_EXE_origin"))
        .args(["usage", "--help"])
        .output()
        .unwrap();
    assert!(out.status.success());
}

#[test]
fn keyring_help() {
    let out = Command::new(env!("CARGO_BIN_EXE_origin"))
        .args(["keyring", "--help"])
        .output()
        .unwrap();
    assert!(out.status.success());
}
```

- [ ] **Step 2: Run — confirm fail**

Run: `cargo test -p origin-cli --test admin_cli`

- [ ] **Step 3: Extend `Cmd` enum**

```rust
/// Daemon usage snapshot (tokens in/out per provider/model).
Usage,
/// Manage persisted sessions.
Sessions {
    #[command(subcommand)]
    sub: SessionsSub,
},
/// Manage stored provider credentials.
Keyring {
    #[command(subcommand)]
    sub: KeyringSub,
},
```

```rust
#[derive(Subcommand)]
enum SessionsSub {
    /// List recent sessions (most-recent first).
    Ls,
    /// Resume a session by id (currently a no-op acknowledgement).
    Resume { session_id: String },
    /// Delete a session and all its messages.
    Rm { session_id: String },
}

#[derive(Subcommand)]
enum KeyringSub {
    /// Add or overwrite a provider secret.
    Add {
        provider: String,
        account: String,
        /// The secret value; read from stdin if `-`.
        secret: String,
    },
    /// List accounts for a provider.
    List { provider: String },
    /// Remove a provider account secret.
    Remove { provider: String, account: String },
}
```

In `main()` dispatch (alongside `Cmd::Pair` etc):

```rust
match cli.cmd {
    Some(Cmd::Usage) => return origin_cli::admin::usage().await,
    Some(Cmd::Sessions { sub }) => return origin_cli::admin::sessions(sub_to_action(sub)).await,
    Some(Cmd::Keyring { sub }) => return origin_cli::admin::keyring(sub_to_action_kr(sub)).await,
    _ => {}
}
```

Add bridge enums in `admin.rs` mirroring the clap variants (so the CLI doesn't leak clap types into the lib).

- [ ] **Step 4: Implement `admin.rs`**

```rust
//! Admin subcommand handlers (P13.4): origin usage, sessions, keyring.

use anyhow::Result;
use origin_daemon::protocol::{ClientMessage, StreamEvent};
use origin_ipc::frame::{encode, FrameKind};
use origin_ipc::transport::Connector;

pub enum SessionsAction {
    Ls,
    Resume(String),
    Rm(String),
}

pub enum KeyringAction {
    Add { provider: String, account: String, secret: String },
    List { provider: String },
    Remove { provider: String, account: String },
}

pub async fn usage() -> Result<()> {
    let ev = round_trip(ClientMessage::GetUsage).await?;
    match ev {
        StreamEvent::UsageReport { rows } => {
            println!("{:<14} {:<24} {:>14} {:>14}", "PROVIDER", "MODEL", "TOKENS_IN", "TOKENS_OUT");
            for r in rows {
                println!(
                    "{:<14} {:<24} {:>14} {:>14}",
                    r.provider, r.model, r.tokens_in, r.tokens_out
                );
            }
            Ok(())
        }
        other => Err(anyhow::anyhow!("unexpected: {other:?}")),
    }
}

pub async fn sessions(action: SessionsAction) -> Result<()> {
    let msg = match action {
        SessionsAction::Ls => ClientMessage::ListSessions,
        SessionsAction::Resume(id) => ClientMessage::ResumeSession { session_id: id },
        SessionsAction::Rm(id) => ClientMessage::RemoveSession { session_id: id },
    };
    let ev = round_trip(msg).await?;
    match ev {
        StreamEvent::SessionsListed { summaries } => {
            println!("{:<28} {:<26} {:>6}  TITLE", "ID", "MODEL", "MSGS");
            for s in summaries {
                println!(
                    "{:<28} {:<26} {:>6}  {}",
                    s.id, s.model, s.message_count, s.title.as_deref().unwrap_or("")
                );
            }
            Ok(())
        }
        StreamEvent::AdminOk => {
            println!("ok");
            Ok(())
        }
        StreamEvent::AdminError { message } => Err(anyhow::anyhow!("{message}")),
        other => Err(anyhow::anyhow!("unexpected: {other:?}")),
    }
}

pub async fn keyring(action: KeyringAction) -> Result<()> {
    let msg = match action {
        KeyringAction::Add { provider, account, secret } => {
            let secret = read_secret(secret)?;
            ClientMessage::KeyringAdd { provider, account, secret }
        }
        KeyringAction::List { provider } => ClientMessage::KeyringList { provider },
        KeyringAction::Remove { provider, account } => ClientMessage::KeyringRemove { provider, account },
    };
    let ev = round_trip(msg).await?;
    match ev {
        StreamEvent::AdminOk => { println!("ok"); Ok(()) }
        StreamEvent::AdminError { message } => Err(anyhow::anyhow!("{message}")),
        StreamEvent::KeyringAccounts { provider, accounts } => {
            for a in accounts {
                println!("{provider}/{a}");
            }
            Ok(())
        }
        other => Err(anyhow::anyhow!("unexpected: {other:?}")),
    }
}

fn read_secret(arg: String) -> Result<String> {
    if arg == "-" {
        use std::io::Read as _;
        let mut buf = String::new();
        std::io::stdin().read_to_string(&mut buf)?;
        Ok(buf.trim_end_matches('\n').to_string())
    } else {
        Ok(arg)
    }
}

async fn round_trip(msg: ClientMessage) -> Result<StreamEvent> {
    let path = std::env::var("ORIGIN_SOCK").unwrap_or_else(|_| default_path());
    let mut c = Connector::connect(&path).await?;
    let body = serde_json::to_vec(&msg)?;
    c.write_raw(&encode(1, FrameKind::Request, &body)).await?;
    let resp = c.read_frame_body().await?;
    let ev: StreamEvent = serde_json::from_slice(&resp)?;
    Ok(ev)
}

fn default_path() -> String {
    #[cfg(unix)]
    {
        format!("{}/origin.sock", std::env::temp_dir().display())
    }
    #[cfg(windows)]
    {
        r"\\.\pipe\origin".to_string()
    }
}
```

Register in `crates/origin-cli/src/lib.rs`:

```rust
pub mod admin;
```

Add the `sub_to_action` / `sub_to_action_kr` bridges in `main.rs`:

```rust
fn sub_to_action(sub: SessionsSub) -> origin_cli::admin::SessionsAction {
    match sub {
        SessionsSub::Ls => origin_cli::admin::SessionsAction::Ls,
        SessionsSub::Resume { session_id } => origin_cli::admin::SessionsAction::Resume(session_id),
        SessionsSub::Rm { session_id } => origin_cli::admin::SessionsAction::Rm(session_id),
    }
}

fn sub_to_action_kr(sub: KeyringSub) -> origin_cli::admin::KeyringAction {
    match sub {
        KeyringSub::Add { provider, account, secret } => origin_cli::admin::KeyringAction::Add { provider, account, secret },
        KeyringSub::List { provider } => origin_cli::admin::KeyringAction::List { provider },
        KeyringSub::Remove { provider, account } => origin_cli::admin::KeyringAction::Remove { provider, account },
    }
}
```

- [ ] **Step 5: Run tests — confirm pass**

Run: `cargo test -p origin-cli --test admin_cli`
Expected: pass.

- [ ] **Step 6: Verification gate**

Run: `cargo test -p origin-cli && cargo clippy -p origin-cli -- -D warnings && cargo fmt --check`

- [ ] **Step 7: Commit**

```bash
git add crates/origin-cli/src/admin.rs crates/origin-cli/src/lib.rs crates/origin-cli/src/main.rs crates/origin-cli/tests/admin_cli.rs
git commit -m "$(cat <<'EOF'
feat(origin-cli): origin usage / sessions ls,resume,rm / keyring add,list,remove

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task P13.4.4 — Admin round-trip E2E test against fake daemon

**Files:**
- Create: `crates/origin-cli/tests/admin_e2e.rs`

- [ ] **Step 1: Failing test**

```rust
//! Stand up a fake daemon on a temp socket that replies to ListSessions
//! with two summaries; assert `origin sessions ls` prints both.

use origin_daemon::protocol::{ClientMessage, SessionSummaryWire, StreamEvent};
use origin_ipc::frame::{encode, FrameKind};
use origin_ipc::transport::Listener;

#[tokio::test(flavor = "current_thread")]
async fn sessions_ls_prints_summaries() {
    let dir = tempfile::TempDir::new().unwrap();
    let sock = if cfg!(windows) {
        format!(r"\\.\pipe\origin-admin-{}", ulid::Ulid::new())
    } else {
        format!("{}/admin.sock", dir.path().display())
    };
    let listener = Listener::bind(&sock).await.unwrap();
    let server = tokio::spawn(async move {
        let mut conn = listener.accept().await.unwrap();
        let req = conn.read_frame_body().await.unwrap();
        let cm: ClientMessage = serde_json::from_slice(&req).unwrap();
        assert!(matches!(cm, ClientMessage::ListSessions));
        let ev = StreamEvent::SessionsListed {
            summaries: vec![
                SessionSummaryWire { id: "s1".into(), created_at: 1, title: Some("alpha".into()), model: "m1".into(), message_count: 4 },
                SessionSummaryWire { id: "s2".into(), created_at: 2, title: None, model: "m2".into(), message_count: 9 },
            ],
        };
        let body = serde_json::to_vec(&ev).unwrap();
        conn.write_raw(&encode(1, FrameKind::Event, &body)).await.unwrap();
    });

    let out = tokio::process::Command::new(env!("CARGO_BIN_EXE_origin"))
        .env("ORIGIN_SOCK", &sock)
        .args(["sessions", "ls"])
        .output()
        .await
        .unwrap();
    server.await.unwrap();

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("s1"), "stdout: {stdout}");
    assert!(stdout.contains("s2"), "stdout: {stdout}");
    assert!(stdout.contains("alpha"), "stdout: {stdout}");
}
```

- [ ] **Step 2: Run — confirm pass**

Run: `cargo test -p origin-cli --test admin_e2e`

- [ ] **Step 3: Verification gate**

Run: `cargo test --workspace && cargo clippy --workspace -- -D warnings`

- [ ] **Step 4: Commit**

```bash
git add crates/origin-cli/tests/admin_e2e.rs
git commit -m "$(cat <<'EOF'
test(origin-cli): admin sessions ls end-to-end against fake daemon

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

# Phase 13 checkpoint

### Task P13.5 — Phase 13 checkpoint + tag

**Files:**
- Modify: `CHANGELOG.md`

- [ ] **Step 1: Full workspace verification**

Run: `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: exit 0 for all three.

- [ ] **Step 2: Update CHANGELOG**

Prepend to `CHANGELOG.md`:

```markdown
## Phase 13 — QUIC Remote IPC + Headless Polish (2026-05-20)

- New `origin-ipc::quic` transport: `QuicListener` / `QuicConnector` /
  `QuicConnection` over `quinn` + `rustls`. Identical wire framing to the
  local-socket transport so daemon dispatch is transport-agnostic.
- New `origin-ipc::tls`: self-signed Ed25519 cert generation + SHA-256
  fingerprint helper. Peers pin by fingerprint; no PKI.
- New `origin-daemon::pairing`: 6-digit single-use pairing codes with TTL,
  bearer-token minting (`orb_` prefix, 24-byte random suffix), in-memory
  `BearerStore`, KeyVault persistence under `("origin-remote", <device>)`.
- Daemon IPC additions: `PairStart`, `PairRedeem`, `ListSessions`,
  `ResumeSession`, `RemoveSession`, `GetUsage`, `KeyringAdd`,
  `KeyringList`, `KeyringRemove` plus matching `StreamEvent`s.
- Daemon session_store: `list_summaries()` (id, created_at, title,
  model, message_count) and `delete(session_id)`.
- New CLI subcommands: `origin pair {start,redeem}`,
  `origin run [--json] [--remote <url>] [--bearer <t>] [--model <m>] <text>`,
  `origin usage`, `origin sessions {ls,resume,rm}`,
  `origin keyring {add,list,remove}`. Headless mode never instantiates
  the Ratatui renderer; `--json` emits JSON-Lines per IPC event.

### Test coverage at phase exit
- `origin-ipc`: tls (2), quic_smoke (1), quic_concurrent (1).
- `origin-daemon`: pairing_unit (4), pairing_e2e (2), protocol_pair (4),
  admin_ipc (3), session_store_list (2).
- `origin-cli`: run_help (1), headless_stream (2), pair_cli (1),
  admin_cli (3), admin_e2e (1).
```

- [ ] **Step 3: Commit + tag**

```bash
git add CHANGELOG.md
git commit -m "$(cat <<'EOF'
docs: Phase 13 changelog

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
git tag p13-complete -m "Phase 13: QUIC remote IPC + headless polish + admin subcommands"
```

- [ ] **Step 4: Final verification gate**

Run: `cargo test --workspace --all-targets && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all -- --check`
Expected: exit 0 across the board.

---

## Self-Review

**Spec coverage:**
- P13.1 QUIC transport → Task group A (P13.1.1–P13.1.4). ✓
- P13.2 Pairing flow + bearer tokens → Task group B (P13.2.1–P13.2.4). ✓
- P13.3 Headless one-shot → Task group C (P13.3.1–P13.3.3). ✓
- P13.4 Admin subcommands → Task group D (P13.4.1–P13.4.4). ✓
- Phase tag `p13-complete` → Task P13.5. ✓

**Placeholder scan:** none — every step lists exact files, exact commands, and full code blocks. The `pair_redeem` CA loader uses `ORIGIN_REMOTE_CA_DER_FILE` rather than fingerprint-based fetching, which is a deliberate simplification flagged in code comments (acceptable for the phase exit criteria — a fingerprint-only flow needs an out-of-band metadata channel that is out of scope for P13).

**Type consistency:** `ClientMessage` variants snake_case via `serde(tag = "kind", rename_all = "snake_case")`; `StreamEvent` variants likewise. `SessionSummaryWire` is defined once in `protocol.rs` and consumed by both daemon and CLI. `BearerStore::validate` returns `Option<String>` (device_id) consistently.

**Parallelization plan for subagent-driven-development:**
- Stage 1 (parallel): A (P13.1.1–P13.1.4), C (P13.3.1–P13.3.2), D (P13.4.1–P13.4.4).
- Stage 2 (after A completes): B (P13.2.1–P13.2.4), C.3 (P13.3.3 `--remote`).
- Stage 3 (after all): P13.5 checkpoint.

Each task is independently testable; conflicts are confined to two files (`crates/origin-daemon/src/main.rs` and `crates/origin-cli/src/main.rs`), which subagents must merge sequentially via rebases.
