use std::io;
use std::sync::Arc;

use interprocess::local_socket::{
    tokio::{prelude::*, Listener as IpcListener, Stream as IpcStream},
    GenericFilePath, ListenerOptions,
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::sync::Mutex;

/// Shared, mutex-protected `Connection` handle.
///
/// Use when multiple writers (e.g., a stream relay plus the main request
/// handler) must serialize access to the underlying transport. Cloning is
/// cheap (`Arc` clone).
pub type SharedConnection = Arc<Mutex<Connection>>;

use crate::frame::{encode, FrameKind, HEADER_LEN, MAX_FRAME_BYTES};

#[allow(clippy::module_name_repetitions)]
pub struct Listener {
    inner: IpcListener,
}

pub struct Connector;

#[allow(clippy::module_name_repetitions)]
pub struct Connection {
    inner: IpcStream,
}

impl Listener {
    /// Bind a listener at the given path / named-pipe name.
    ///
    /// # Errors
    /// Returns an `io::Error` if the address is invalid or the listener cannot
    /// be created (e.g., name in use).
    // `create_tokio` is synchronous under the hood, but the public API is `async`
    // so callers can uniformly `.await` it alongside other async transport operations.
    #[allow(clippy::unused_async)]
    pub async fn bind(path: &str) -> io::Result<Self> {
        let name = path
            .to_fs_name::<GenericFilePath>()
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        let inner = ListenerOptions::new().name(name).create_tokio()?;
        Ok(Self { inner })
    }

    /// Accept the next incoming connection.
    ///
    /// # Errors
    /// Propagates I/O errors from the underlying transport.
    pub async fn accept(&self) -> io::Result<Connection> {
        let inner = self.inner.accept().await?;
        Ok(Connection { inner })
    }
}

impl Connector {
    /// Connect to a listener.
    ///
    /// # Errors
    /// Propagates I/O errors from the underlying transport.
    pub async fn connect(path: &str) -> io::Result<Connection> {
        let name = path
            .to_fs_name::<GenericFilePath>()
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        let inner = IpcStream::connect(name).await?;
        Ok(Connection { inner })
    }
}

impl Connection {
    /// Read the next frame body. The framing header is consumed but discarded;
    /// callers receive only the payload bytes.
    ///
    /// # Errors
    /// Returns an error if the connection closes mid-frame or the length field
    /// is malformed.
    pub async fn read_frame_body(&mut self) -> io::Result<Vec<u8>> {
        let (_kind, body) = self.read_frame().await?;
        Ok(body)
    }

    /// Read the next frame, returning its `FrameKind` and body bytes. Used by
    /// callers that need to distinguish `Event` from `Response` frames (e.g.,
    /// the CLI's streaming response loop and the daemon's ordering tests).
    ///
    /// # Errors
    /// Returns an error if the connection closes mid-frame, the magic bytes
    /// don't match, the length field is malformed, or the kind byte is
    /// unknown.
    pub async fn read_frame(&mut self) -> io::Result<(FrameKind, Vec<u8>)> {
        read_frame_from(&mut self.inner).await
    }

    /// Write a frame with `kind` and `body`. `request_id` is zero — the
    /// caller can use `write_raw` with a pre-built frame for non-zero ids.
    ///
    /// # Errors
    /// Propagates I/O errors.
    pub async fn write_frame(&mut self, kind: FrameKind, body: &[u8]) -> io::Result<()> {
        let bytes = encode(0, kind, body);
        self.inner.write_all(&bytes).await?;
        self.inner.flush().await?;
        Ok(())
    }

    /// Write a pre-encoded frame (e.g., one built with a non-zero `request_id`).
    ///
    /// # Errors
    /// Propagates I/O errors.
    pub async fn write_raw(&mut self, raw: &[u8]) -> io::Result<()> {
        self.inner.write_all(raw).await?;
        self.inner.flush().await?;
        Ok(())
    }
}

/// Read one length-prefixed frame from any async reader.
///
/// Extracted from [`Connection::read_frame`] so the framing logic can be
/// exercised in isolation against an in-memory reader (see this module's
/// tests). Enforces [`crate::frame::MAX_FRAME_BYTES`] on the advertised body
/// length so a hostile peer cannot induce a multi-GiB allocation via a
/// crafted length header.
///
/// # Errors
/// Returns [`io::ErrorKind::InvalidData`] for an unknown frame kind or a
/// body-length field exceeding [`crate::frame::MAX_FRAME_BYTES`], or any
/// underlying I/O error from the reader.
pub async fn read_frame_from<R: AsyncRead + Unpin>(
    reader: &mut R,
) -> io::Result<(FrameKind, Vec<u8>)> {
    let mut header = [0_u8; HEADER_LEN];
    reader.read_exact(&mut header).await?;
    // header layout (must match crate::frame::encode):
    //   [0..4]  magic
    //   [4]     kind
    //   [5..13] request_id
    //   [13..17] body length (big-endian u32)
    let kind = match header[4] {
        1 => FrameKind::Request,
        2 => FrameKind::Response,
        3 => FrameKind::Event,
        4 => FrameKind::ErrorFrame,
        x => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unknown frame kind: {x}"),
            ))
        }
    };
    let len = u32::from_be_bytes([header[13], header[14], header[15], header[16]]) as usize;
    if len > MAX_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("frame too large: {len} bytes (cap {MAX_FRAME_BYTES})"),
        ));
    }
    let mut body = vec![0_u8; len];
    reader.read_exact(&mut body).await?;
    Ok((kind, body))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::{MAX_FRAME_BYTES, HEADER_LEN as FRAME_HEADER_LEN};
    use tokio::io::AsyncWriteExt;

    /// Build a raw frame header with the given body length. Body bytes are
    /// not sent — the reader should reject the header before any allocation.
    fn malicious_header(body_len: u32) -> [u8; FRAME_HEADER_LEN] {
        let mut h = [0_u8; FRAME_HEADER_LEN];
        // magic = 0x4F524F4E "ORON" big-endian
        h[0] = 0x4F;
        h[1] = 0x52;
        h[2] = 0x4F;
        h[3] = 0x4E;
        h[4] = 1; // kind = Request
        // request_id (8 bytes) = 0, already zeroed
        let len_be = body_len.to_be_bytes();
        h[13] = len_be[0];
        h[14] = len_be[1];
        h[15] = len_be[2];
        h[16] = len_be[3];
        h
    }

    #[tokio::test]
    async fn read_frame_rejects_oversized_length_without_allocating() {
        let (mut client, mut server) = tokio::io::duplex(64);
        // Advertise a body slightly larger than the cap, then close — without
        // a cap check the reader would try to allocate ~64 MiB+1 and then
        // block on `read_exact` for the body bytes that will never arrive.
        let oversize = u32::try_from(MAX_FRAME_BYTES + 1).expect("fits u32");
        let header = malicious_header(oversize);
        let writer = tokio::spawn(async move {
            client.write_all(&header).await.expect("write header");
            // Close the write half so a non-rejecting reader would observe
            // EOF on the body read rather than hanging forever.
            client.shutdown().await.expect("shutdown");
            drop(client);
        });

        let result = read_frame_from(&mut server).await;
        writer.await.expect("writer task");
        let err = result.expect_err("oversized length must be rejected");
        assert_eq!(
            err.kind(),
            io::ErrorKind::InvalidData,
            "expected InvalidData, got {err:?}"
        );
        let msg = err.to_string();
        assert!(
            msg.contains("frame too large"),
            "expected 'frame too large' in error message, got: {msg}"
        );
    }
}
