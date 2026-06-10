// SPDX-License-Identifier: Apache-2.0
//! QUIC + rustls remote IPC transport.
//!
//! Mirrors the `read_frame` / `write_frame` / `write_raw` surface of
//! [`crate::transport::Connection`] so the daemon dispatch loop is
//! transport-agnostic. Each connection uses a single bidirectional
//! QUIC stream — request/response pairs and event streams ride on
//! the same ordered byte channel as the local-socket transport.
//!
//! Trust model: peers exchange and pin SHA-256 cert fingerprints at
//! pairing time (P13.2). For now this module accepts a raw CA DER
//! blob from the caller and trusts it as a root for that connection.

use std::net::SocketAddr;
use std::sync::Arc;

use quinn::crypto::rustls::{QuicClientConfig, QuicServerConfig};
use quinn::{ClientConfig, Endpoint, RecvStream, SendStream, ServerConfig};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{verify_tls12_signature, verify_tls13_signature, CryptoProvider};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::{
    ClientConfig as RustlsClientConfig, DigitallySignedStruct, DistinguishedName,
    ServerConfig as RustlsServerConfig, SignatureScheme,
};
use thiserror::Error;

use crate::frame::{FrameKind, HEADER_LEN, MAX_FRAME_BYTES};
use crate::tls::{fingerprints_eq, sha256_fingerprint, CertBundle, CertFingerprint};

#[allow(clippy::module_name_repetitions)]
#[derive(Debug, Error)]
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

fn install_default_crypto_provider() {
    // Ignore the error if a provider was already installed by another module
    // in the same process.
    let _ = rustls::crypto::ring::default_provider().install_default();
}

/// A fresh handle to the crypto provider whose signature-verification
/// algorithms back the pinning verifiers below. Today this is the classical
/// `ring` provider (TLS 1.3 with X25519 key exchange and Ed25519/ECDSA
/// signatures). The transport's *authentication* anchor is the SHA-256 cert
/// fingerprint (see [`crate::tls::CertFingerprint`]), which remains sound
/// against a quantum adversary; migrating the *key exchange* to a hybrid
/// X25519+ML-KEM group is a drop-in provider swap here once a pure-Rust
/// post-quantum provider is vendored (tracked in `SECURITY.md`).
fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

/// A QUIC listener bound to a local address, configured with mutual-friendly
/// rustls server config built from `bundle`.
#[allow(clippy::module_name_repetitions)]
pub struct QuicListener {
    endpoint: Endpoint,
}

impl QuicListener {
    /// Bind a new listener on `addr` using the cert/key from `bundle`, accepting
    /// only clients whose certificate SHA-256 fingerprint is in
    /// `allowed_clients`.
    ///
    /// Zero-trust: this enforces **mutual** TLS. Unlike the previous
    /// `with_no_client_auth()` behavior — which accepted any peer that completed
    /// the handshake against the (publicly distributed) server cert — a client
    /// must now prove possession of a key whose cert is explicitly pinned. An
    /// empty `allowed_clients` trusts **no** peer (fail closed), so a
    /// misconfiguration denies access rather than silently opening it.
    ///
    /// The `async` keeps the API symmetric with [`QuicConnector::connect`]
    /// even though no `await` is currently required — future work
    /// (P13.2 pairing) will add async setup steps here.
    ///
    /// # Errors
    /// Returns [`QuicError::Tls`] if the rustls server config cannot be
    /// constructed, or [`QuicError::Io`] if the UDP socket cannot bind.
    #[allow(clippy::unused_async)]
    pub async fn bind(
        addr: SocketAddr,
        bundle: CertBundle,
        allowed_clients: Vec<CertFingerprint>,
    ) -> Result<Self, QuicError> {
        install_default_crypto_provider();

        let cert = CertificateDer::from(bundle.cert_der);
        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(bundle.key_der));

        let verifier = Arc::new(PinnedClientCertVerifier {
            allowed: allowed_clients,
            provider: provider(),
        });

        let mut rustls_config = RustlsServerConfig::builder()
            .with_client_cert_verifier(verifier)
            .with_single_cert(vec![cert], key)
            .map_err(|e| QuicError::Tls(format!("server cert: {e}")))?;
        // ALPN is not strictly required for our trust model but quinn requires
        // the cipher suites to be QUIC-compatible.
        rustls_config.alpn_protocols = vec![b"origin/1".to_vec()];

        let quic_server = QuicServerConfig::try_from(rustls_config)
            .map_err(|e| QuicError::Tls(format!("quic server config: {e}")))?;
        let server_config = ServerConfig::with_crypto(Arc::new(quic_server));

        let endpoint = Endpoint::server(server_config, addr)?;
        Ok(Self { endpoint })
    }

    /// Local bound socket address (useful when binding to port 0).
    ///
    /// # Panics
    /// Panics only if the underlying [`Endpoint`] reports no local
    /// address — which cannot happen after a successful [`Self::bind`]
    /// because `Endpoint::server` does not return until the socket is
    /// bound.
    #[must_use]
    pub fn local_addr(&self) -> SocketAddr {
        self.endpoint
            .local_addr()
            .expect("endpoint always has a local address after bind")
    }

    /// Accept one incoming connection and open the first bidirectional stream.
    ///
    /// # Errors
    /// Returns [`QuicError::Connect`] on handshake failure or when the
    /// endpoint is closed before any connection arrives.
    pub async fn accept(&self) -> Result<QuicConnection, QuicError> {
        let incoming = self
            .endpoint
            .accept()
            .await
            .ok_or_else(|| QuicError::Connect("listener closed".into()))?;
        let connection = incoming
            .await
            .map_err(|e| QuicError::Connect(format!("server handshake: {e}")))?;
        let (send, recv) = connection
            .accept_bi()
            .await
            .map_err(|e| QuicError::Connect(format!("accept_bi: {e}")))?;
        // Hold an Endpoint clone inside the connection so dropping the
        // listener does not tear down the endpoint driver while the
        // connection is still in use.
        Ok(QuicConnection {
            send,
            recv,
            endpoint: self.endpoint.clone(),
            connection: Some(connection),
        })
    }
}

/// Client-side connector. Stateless — one call produces one connection.
#[allow(clippy::module_name_repetitions)]
pub struct QuicConnector;

impl QuicConnector {
    /// Dial `addr` and complete a **mutually-authenticated** QUIC + rustls
    /// handshake. The server's leaf certificate is pinned to
    /// `server_fingerprint` (the SHA-256 hash distributed out-of-band in the
    /// `origin://host:port#<fingerprint>` pairing URL), and the client presents
    /// `client_bundle` so the server can pin it in return. Opens one
    /// bidirectional stream on success.
    ///
    /// Pinning to a hash — rather than validating a CA chain — is both the
    /// zero-trust anchor (only the exact paired daemon is trusted, no PKI to
    /// subvert) and the post-quantum anchor (a quantum adversary who forges the
    /// classical cert signature still cannot match the SHA-256 fingerprint).
    ///
    /// # Errors
    /// Returns [`QuicError::Tls`] on cert/config issues, [`QuicError::Io`] on
    /// socket bind failure, or [`QuicError::Connect`] on handshake failure
    /// (including a server whose certificate does not match the pin).
    ///
    /// # Panics
    /// Does not panic on well-formed input. The internal `.expect` calls
    /// operate on static string literals (`"0.0.0.0:0"` / `"[::]:0"`) which
    /// are guaranteed to parse as valid socket addresses.
    pub async fn connect(
        addr: SocketAddr,
        server_name: &str,
        server_fingerprint: CertFingerprint,
        client_bundle: &CertBundle,
    ) -> Result<QuicConnection, QuicError> {
        install_default_crypto_provider();

        let verifier = Arc::new(PinnedServerCertVerifier {
            expected: server_fingerprint,
            provider: provider(),
        });

        let client_cert = CertificateDer::from(client_bundle.cert_der.clone());
        let client_key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(client_bundle.key_der.clone()));

        let mut rustls_config = RustlsClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(verifier)
            .with_client_auth_cert(vec![client_cert], client_key)
            .map_err(|e| QuicError::Tls(format!("client cert: {e}")))?;
        rustls_config.alpn_protocols = vec![b"origin/1".to_vec()];

        let quic_client = QuicClientConfig::try_from(rustls_config)
            .map_err(|e| QuicError::Tls(format!("quic client config: {e}")))?;
        let client_config = ClientConfig::new(Arc::new(quic_client));

        // Match address family for the local bind.
        let bind_addr: SocketAddr = if addr.is_ipv6() {
            "[::]:0".parse().expect("static literal")
        } else {
            "0.0.0.0:0".parse().expect("static literal")
        };
        let mut endpoint = Endpoint::client(bind_addr)?;
        endpoint.set_default_client_config(client_config);

        let connecting = endpoint
            .connect(addr, server_name)
            .map_err(|e| QuicError::Connect(format!("dial: {e}")))?;
        let connection = connecting
            .await
            .map_err(|e| QuicError::Connect(format!("client handshake: {e}")))?;
        let (send, recv) = connection
            .open_bi()
            .await
            .map_err(|e| QuicError::Connect(format!("open_bi: {e}")))?;
        Ok(QuicConnection {
            send,
            recv,
            endpoint,
            connection: Some(connection),
        })
    }
}

/// Client-side verifier that pins the server's leaf certificate to an expected
/// SHA-256 fingerprint instead of validating a CA chain.
///
/// The TLS handshake signature is still cryptographically verified against the
/// pinned certificate's key, so this is genuine authentication — the fingerprint
/// check simply replaces "any cert a CA vouches for" with "exactly the paired
/// daemon's cert".
#[derive(Debug)]
struct PinnedServerCertVerifier {
    expected: CertFingerprint,
    provider: Arc<CryptoProvider>,
}

impl ServerCertVerifier for PinnedServerCertVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let got = sha256_fingerprint(end_entity.as_ref());
        if fingerprints_eq(&got, &self.expected) {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::General(
                "server certificate fingerprint does not match the pinned value".to_owned(),
            ))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider.signature_verification_algorithms.supported_schemes()
    }
}

/// Server-side verifier that requires a client certificate whose SHA-256
/// fingerprint is on a pinned allow-list. An empty allow-list trusts no client
/// (fail closed).
#[derive(Debug)]
struct PinnedClientCertVerifier {
    allowed: Vec<CertFingerprint>,
    provider: Arc<CryptoProvider>,
}

impl ClientCertVerifier for PinnedClientCertVerifier {
    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }

    fn verify_client_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _now: UnixTime,
    ) -> Result<ClientCertVerified, rustls::Error> {
        let got = sha256_fingerprint(end_entity.as_ref());
        if self.allowed.iter().any(|f| fingerprints_eq(f, &got)) {
            Ok(ClientCertVerified::assertion())
        } else {
            Err(rustls::Error::General(
                "client certificate fingerprint is not on the pinned allow-list".to_owned(),
            ))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider.signature_verification_algorithms.supported_schemes()
    }
}

/// One end of a QUIC bidirectional stream. Carries length-prefixed frames
/// using the same wire format as [`crate::transport::Connection`].
///
/// The connection holds a clone of the [`Endpoint`] and the underlying
/// [`quinn::Connection`] so dropping the originating listener (or the
/// client connector future) does not tear down the endpoint driver
/// or implicitly close the connection while this handle is still in use.
///
/// On drop, the underlying [`quinn::Connection`] is detached to a small
/// background tokio task that waits for the connection to be fully
/// drained (peer ack of pending stream data + graceful close). Without
/// this, dropping the [`QuicConnection`] would synchronously trigger an
/// implicit close which discards any bytes that the QUIC driver had not
/// yet pushed to the wire.
#[allow(clippy::module_name_repetitions)]
pub struct QuicConnection {
    send: SendStream,
    recv: RecvStream,
    endpoint: Endpoint,
    connection: Option<quinn::Connection>,
}

impl Drop for QuicConnection {
    fn drop(&mut self) {
        if let Some(connection) = self.connection.take() {
            // Detach a tokio task that holds the connection (and endpoint)
            // alive until the peer has either closed or drained outgoing
            // data. tokio::spawn requires an active runtime; we are
            // already inside one because all paths that produce a
            // QuicConnection require it.
            let endpoint = self.endpoint.clone();
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                handle.spawn(async move {
                    let _ = connection.closed().await;
                    drop(endpoint);
                });
            }
        }
    }
}

/// Decode a 17-byte frame header into `(kind, body_len)`, enforcing the
/// shared [`crate::frame::MAX_FRAME_BYTES`] cap on the advertised body
/// length. Extracted from [`QuicConnection::read_frame`] so the bounds
/// check is unit-testable without needing a full QUIC handshake (the
/// rest of `read_frame` is a thin wrapper over `RecvStream::read_exact`
/// which is already covered by `quic_smoke` / `quic_concurrent` tests).
///
/// # Errors
/// Returns [`QuicError::Frame`] for an unknown kind byte or a body-length
/// field that exceeds [`crate::frame::MAX_FRAME_BYTES`].
fn decode_header(header: &[u8; HEADER_LEN]) -> Result<(FrameKind, usize), QuicError> {
    let kind = match header[4] {
        1 => FrameKind::Request,
        2 => FrameKind::Response,
        3 => FrameKind::Event,
        4 => FrameKind::ErrorFrame,
        x => return Err(QuicError::Frame(format!("unknown frame kind: {x}"))),
    };
    let len = u32::from_be_bytes([header[13], header[14], header[15], header[16]]) as usize;
    if len > MAX_FRAME_BYTES {
        return Err(QuicError::Frame(format!(
            "frame too large: {len} bytes (cap {MAX_FRAME_BYTES})"
        )));
    }
    Ok((kind, len))
}

impl QuicConnection {
    /// Read one frame from the stream.
    ///
    /// # Errors
    /// Returns [`QuicError::Frame`] if the kind byte is unknown,
    /// [`QuicError::Io`] on read failure or short read.
    pub async fn read_frame(&mut self) -> Result<(FrameKind, Vec<u8>), QuicError> {
        let mut header = [0_u8; HEADER_LEN];
        self.recv
            .read_exact(&mut header)
            .await
            .map_err(|e| QuicError::Frame(format!("read header: {e}")))?;
        let (kind, len) = decode_header(&header)?;
        let mut body = vec![0_u8; len];
        self.recv
            .read_exact(&mut body)
            .await
            .map_err(|e| QuicError::Frame(format!("read body: {e}")))?;
        Ok((kind, body))
    }

    /// Write a frame with `kind` and `body`. Uses `request_id = 0` — callers
    /// that need a specific request id should use [`Self::write_raw`].
    ///
    /// # Errors
    /// Propagates I/O errors.
    pub async fn write_frame(&mut self, kind: FrameKind, body: &[u8]) -> Result<(), QuicError> {
        let bytes = crate::frame::encode(0, kind, body);
        self.write_raw(&bytes).await
    }

    /// Write a pre-encoded frame.
    ///
    /// # Errors
    /// Propagates I/O errors.
    pub async fn write_raw(&mut self, raw: &[u8]) -> Result<(), QuicError> {
        self.send
            .write_all(raw)
            .await
            .map_err(|e| QuicError::Frame(format!("write: {e}")))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::MAX_FRAME_BYTES;

    /// Build a frame header with the given body length and kind=Request.
    const fn header_with_len(body_len: u32) -> [u8; HEADER_LEN] {
        let mut h = [0_u8; HEADER_LEN];
        h[0] = 0x4F;
        h[1] = 0x52;
        h[2] = 0x4F;
        h[3] = 0x4E;
        h[4] = 1; // Request
        let len_be = body_len.to_be_bytes();
        h[13] = len_be[0];
        h[14] = len_be[1];
        h[15] = len_be[2];
        h[16] = len_be[3];
        h
    }

    #[test]
    fn decode_header_rejects_oversized_length() {
        // A hostile peer advertises a body just past the cap. The header
        // decoder must reject this before any allocation occurs in the
        // calling `read_frame`.
        let oversize = u32::try_from(MAX_FRAME_BYTES + 1).expect("fits u32");
        let header = header_with_len(oversize);
        let result = decode_header(&header);
        let err = result.expect_err("oversized length must be rejected");
        let msg = err.to_string();
        assert!(
            msg.contains("frame too large"),
            "expected 'frame too large' in error, got: {msg}"
        );
    }

    #[test]
    fn decode_header_accepts_max_size() {
        let max = u32::try_from(MAX_FRAME_BYTES).expect("fits u32");
        let header = header_with_len(max);
        let (kind, len) = decode_header(&header).expect("at-cap header is valid");
        assert_eq!(kind, FrameKind::Request);
        assert_eq!(len, MAX_FRAME_BYTES);
    }
}
