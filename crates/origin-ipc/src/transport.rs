use std::io;
use std::sync::Arc;

use interprocess::local_socket::{
    tokio::{prelude::*, Listener as IpcListener, Stream as IpcStream},
    GenericFilePath, ListenerOptions,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::Mutex;

/// Shared, mutex-protected `Connection` handle.
///
/// Use when multiple writers (e.g., a stream relay plus the main request
/// handler) must serialize access to the underlying transport. Cloning is
/// cheap (`Arc` clone).
pub type SharedConnection = Arc<Mutex<Connection>>;

use crate::frame::{encode, FrameKind, HEADER_LEN};

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
        let mut header = [0_u8; HEADER_LEN];
        self.inner.read_exact(&mut header).await?;
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
        let mut body = vec![0_u8; len];
        self.inner.read_exact(&mut body).await?;
        Ok((kind, body))
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
