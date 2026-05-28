use bytes::{BufMut, BytesMut};
use thiserror::Error;

// `FrameKind` and `FrameError` live inside the `frame` module — the repeated prefix
// is intentional so call sites read `frame::FrameKind` without losing clarity.
#[allow(clippy::module_name_repetitions)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum FrameKind {
    Request = 1,
    Response = 2,
    Event = 3,
    ErrorFrame = 4,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Frame<'a> {
    pub request_id: u64,
    pub kind: FrameKind,
    pub body: &'a [u8],
}

#[allow(clippy::module_name_repetitions)]
#[derive(Debug, Error)]
pub enum FrameError {
    #[error("truncated frame")]
    Truncated,
    #[error("bad magic")]
    BadMagic,
    #[error("unknown kind {0}")]
    UnknownKind(u8),
    #[error("length mismatch")]
    LengthMismatch,
}

const MAGIC: u32 = 0x4F52_4F4E; // "ORON" big-endian
pub const HEADER_LEN: usize = 4 + 1 + 8 + 4; // magic + kind + id + body_len

/// Maximum accepted frame body size on the wire (64 MiB).
///
/// Frame readers (`transport::read_frame_from`, `quic::QuicConnection::read_frame`)
/// reject any header advertising a body longer than this before allocating, so a
/// hostile peer cannot induce a multi-GiB allocation by sending a crafted length
/// header. 64 MiB is comfortably above any legitimate request or streamed event
/// payload the daemon emits today.
pub const MAX_FRAME_BYTES: usize = 64 * 1024 * 1024;

/// Encode a frame into a [`Vec<u8>`].
///
/// # Panics
///
/// Panics if `body.len()` exceeds `u32::MAX` (~4 GiB). This is not a
/// realistic constraint in practice; frames are expected to be small.
#[must_use]
pub fn encode(request_id: u64, kind: FrameKind, body: &[u8]) -> Vec<u8> {
    let mut out = BytesMut::with_capacity(HEADER_LEN + body.len());
    out.put_u32(MAGIC);
    out.put_u8(kind as u8);
    out.put_u64(request_id);
    out.put_u32(u32::try_from(body.len()).expect("body length must fit u32"));
    out.put_slice(body);
    out.to_vec()
}

/// Parse and validate a frame from a byte slice.
///
/// # Errors
///
/// Returns [`FrameError::Truncated`] if the slice is shorter than the minimum
/// header, [`FrameError::BadMagic`] if the magic bytes don't match,
/// [`FrameError::UnknownKind`] for an unrecognised kind byte, or
/// [`FrameError::LengthMismatch`] if the embedded body-length field does not
/// match the actual remaining bytes.
///
/// # Panics
///
/// Does not panic on well-formed input. The internal `expect` calls operate on
/// fixed-size slices that are guaranteed to be present after the length check.
pub fn validate(bytes: &[u8]) -> Result<Frame<'_>, FrameError> {
    if bytes.len() < HEADER_LEN {
        return Err(FrameError::Truncated);
    }
    let magic = u32::from_be_bytes(bytes[0..4].try_into().expect("4 bytes"));
    if magic != MAGIC {
        return Err(FrameError::BadMagic);
    }
    let kind = match bytes[4] {
        1 => FrameKind::Request,
        2 => FrameKind::Response,
        3 => FrameKind::Event,
        4 => FrameKind::ErrorFrame,
        x => return Err(FrameError::UnknownKind(x)),
    };
    let request_id = u64::from_be_bytes(bytes[5..13].try_into().expect("8 bytes"));
    // u32 → usize: safe on all platforms the project supports (32-bit minimum).
    #[allow(clippy::cast_possible_truncation)]
    let len = u32::from_be_bytes(bytes[13..17].try_into().expect("4 bytes")) as usize;
    if bytes.len() != HEADER_LEN + len {
        return Err(FrameError::LengthMismatch);
    }
    Ok(Frame {
        request_id,
        kind,
        body: &bytes[HEADER_LEN..HEADER_LEN + len],
    })
}
