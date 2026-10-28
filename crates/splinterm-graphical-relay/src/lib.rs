#![forbid(unsafe_code)]

//! Bounded binary framing for the SSH graphical-relay transport.
//!
//! This is an outer byte-channel protocol. It never parses or rewrites the
//! private daemon protocol carried in [`Frame::Data`].

use std::io::ErrorKind;

use anyhow::{Context, Result, bail};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

mod client;
mod fair_queue;

pub use client::{ClientMultiplexer, LogicalChannel};
pub use fair_queue::{
    FairData, FairDataChannel, FairDataPermit, FairDataReceiver, FairDataSender, fair_data_queue,
};

/// Exact graphical relay protocol version.
pub const VERSION: u16 = 1;
/// Maximum opaque bytes carried by one data frame.
pub const MAX_DATA_BYTES: usize = 16 * 1024;
/// Maximum diagnostic bytes carried by a rejection or session error.
pub const MAX_DIAGNOSTIC_BYTES: usize = 1024;
/// Maximum Splints retained by one daemon topology and therefore one Window.
pub const MAX_NATIVE_WINDOW_SPLINTS: usize = 256;
/// Observation plus controller channels retained for each mapped Splint.
pub const RETAINED_CHANNELS_PER_SPLINT: usize = 2;
/// Topology, focus/identity, and bounded transient Window service allowance,
/// plus the [`CONSENT_RESPONDER_CHANNELS`] a remote Window's agent access
/// prompt responder holds for its lifetime (ADR 0015), so it never takes a
/// slot the other services count on.
pub const FIXED_WINDOW_SERVICE_CHANNELS: usize = 8 + CONSENT_RESPONDER_CHANNELS;
/// The long-lived channels a remote Window's consent responder holds: one.
pub const CONSENT_RESPONDER_CHANNELS: usize = 1;
/// Hard maximum channels for the largest supported native Window topology.
pub const MAX_LOGICAL_CHANNELS: usize =
    MAX_NATIVE_WINDOW_SPLINTS * RETAINED_CHANNELS_PER_SPLINT + FIXED_WINDOW_SERVICE_CHANNELS;
/// Maximum queued outgoing data per logical channel.
pub const MAX_CHANNEL_QUEUED_BYTES: usize = 64 * 1024;
/// Maximum queued incoming data per logical channel.
///
/// This matches the supported private-frame ceiling without parsing the opaque
/// payload, allowing one fragmented logical record to survive a delayed consumer.
pub const MAX_INCOMING_CHANNEL_QUEUED_BYTES: usize = 16 * 1024 * 1024;
/// Maximum queued data across a complete graphical relay session.
pub const MAX_SESSION_QUEUED_BYTES: usize = MAX_LOGICAL_CHANNELS * MAX_CHANNEL_QUEUED_BYTES;

const MAGIC: [u8; 4] = *b"SPGR";
const HEADER_BYTES: usize = 16;
const KIND_HELLO: u8 = 1;
const KIND_HELLO_ACK: u8 = 2;
const KIND_OPEN_CHANNEL: u8 = 3;
const KIND_CHANNEL_OPENED: u8 = 4;
const KIND_CHANNEL_REJECTED: u8 = 5;
const KIND_DATA: u8 = 6;
const KIND_HALF_CLOSE: u8 = 7;
const KIND_CLOSE_CHANNEL: u8 = 8;
const KIND_SESSION_ERROR: u8 = 9;

/// One validated outer graphical-relay frame.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Frame {
    Hello,
    HelloAck,
    OpenChannel { channel_id: u32 },
    ChannelOpened { channel_id: u32 },
    ChannelRejected { channel_id: u32, reason: String },
    Data { channel_id: u32, bytes: Vec<u8> },
    HalfClose { channel_id: u32 },
    CloseChannel { channel_id: u32 },
    SessionError { reason: String },
}

impl Frame {
    fn parts(&self) -> Result<(u8, u32, &[u8])> {
        let (kind, channel_id, payload) = match self {
            Self::Hello => (KIND_HELLO, 0, &[][..]),
            Self::HelloAck => (KIND_HELLO_ACK, 0, &[][..]),
            Self::OpenChannel { channel_id } => (KIND_OPEN_CHANNEL, *channel_id, &[][..]),
            Self::ChannelOpened { channel_id } => (KIND_CHANNEL_OPENED, *channel_id, &[][..]),
            Self::ChannelRejected { channel_id, reason } => {
                validate_diagnostic(reason)?;
                (KIND_CHANNEL_REJECTED, *channel_id, reason.as_bytes())
            }
            Self::Data { channel_id, bytes } => {
                if bytes.is_empty() || bytes.len() > MAX_DATA_BYTES {
                    bail!("graphical relay data length is outside bounds");
                }
                (KIND_DATA, *channel_id, bytes.as_slice())
            }
            Self::HalfClose { channel_id } => (KIND_HALF_CLOSE, *channel_id, &[][..]),
            Self::CloseChannel { channel_id } => (KIND_CLOSE_CHANNEL, *channel_id, &[][..]),
            Self::SessionError { reason } => {
                validate_diagnostic(reason)?;
                (KIND_SESSION_ERROR, 0, reason.as_bytes())
            }
        };
        validate_channel(kind, channel_id)?;
        Ok((kind, channel_id, payload))
    }
}

fn validate_diagnostic(value: &str) -> Result<()> {
    if value.is_empty() || value.len() > MAX_DIAGNOSTIC_BYTES || value.chars().any(char::is_control)
    {
        bail!("graphical relay diagnostic is outside bounds");
    }
    Ok(())
}

fn validate_channel(kind: u8, channel_id: u32) -> Result<()> {
    let session_frame = matches!(kind, KIND_HELLO | KIND_HELLO_ACK | KIND_SESSION_ERROR);
    if session_frame != (channel_id == 0) {
        bail!("graphical relay frame has an invalid channel identity");
    }
    Ok(())
}

/// Writes one complete bounded frame.
///
/// # Errors
///
/// Returns an error for invalid frame fields or output failure.
pub async fn write_frame<W>(writer: &mut W, frame: &Frame) -> Result<()>
where
    W: AsyncWrite + Unpin + ?Sized,
{
    let (kind, channel_id, payload) = frame.parts()?;
    write_parts(writer, kind, channel_id, payload).await
}

/// Writes one data frame without copying its permit-bound payload.
///
/// # Errors
///
/// Returns an error for invalid channel/data bounds or output failure.
pub async fn write_data_frame<W>(writer: &mut W, channel_id: u32, payload: &[u8]) -> Result<()>
where
    W: AsyncWrite + Unpin + ?Sized,
{
    if payload.is_empty() || payload.len() > MAX_DATA_BYTES {
        bail!("graphical relay data length is outside bounds");
    }
    validate_channel(KIND_DATA, channel_id)?;
    write_parts(writer, KIND_DATA, channel_id, payload).await
}

async fn write_parts<W>(writer: &mut W, kind: u8, channel_id: u32, payload: &[u8]) -> Result<()>
where
    W: AsyncWrite + Unpin + ?Sized,
{
    let payload_len =
        u32::try_from(payload.len()).context("graphical relay payload is too large")?;
    let mut header = [0_u8; HEADER_BYTES];
    header[..4].copy_from_slice(&MAGIC);
    header[4..6].copy_from_slice(&VERSION.to_be_bytes());
    header[6] = kind;
    header[7] = 0;
    header[8..12].copy_from_slice(&channel_id.to_be_bytes());
    header[12..16].copy_from_slice(&payload_len.to_be_bytes());
    writer.write_all(&header).await?;
    writer.write_all(payload).await?;
    writer.flush().await?;
    Ok(())
}

/// Reads one complete frame, returning `None` only for clean EOF between frames.
///
/// # Errors
///
/// Returns an error for corrupt, incompatible, truncated, or oversized framing.
pub async fn read_frame<R>(reader: &mut R) -> Result<Option<Frame>>
where
    R: AsyncRead + Unpin + ?Sized,
{
    let mut header = [0_u8; HEADER_BYTES];
    let first = reader.read(&mut header[..1]).await?;
    if first == 0 {
        return Ok(None);
    }
    reader.read_exact(&mut header[1..]).await.map_err(|error| {
        if error.kind() == ErrorKind::UnexpectedEof {
            anyhow::anyhow!("graphical relay header is truncated")
        } else {
            error.into()
        }
    })?;
    if header[..4] != MAGIC {
        bail!("graphical relay magic is invalid");
    }
    if u16::from_be_bytes(header[4..6].try_into()?) != VERSION {
        bail!("graphical relay version is incompatible");
    }
    if header[7] != 0 {
        bail!("graphical relay reserved flags are nonzero");
    }
    let kind = header[6];
    let channel_id = u32::from_be_bytes(header[8..12].try_into()?);
    validate_channel(kind, channel_id)?;
    let payload_len = usize::try_from(u32::from_be_bytes(header[12..16].try_into()?))?;
    let maximum = match kind {
        KIND_DATA => MAX_DATA_BYTES,
        KIND_CHANNEL_REJECTED | KIND_SESSION_ERROR => MAX_DIAGNOSTIC_BYTES,
        KIND_HELLO | KIND_HELLO_ACK | KIND_OPEN_CHANNEL | KIND_CHANNEL_OPENED | KIND_HALF_CLOSE
        | KIND_CLOSE_CHANNEL => 0,
        _ => bail!("graphical relay frame kind is invalid"),
    };
    if payload_len > maximum || (kind == KIND_DATA && payload_len == 0) {
        bail!("graphical relay payload length is outside bounds");
    }
    let mut payload = vec![0_u8; payload_len];
    reader.read_exact(&mut payload).await.map_err(|error| {
        if error.kind() == ErrorKind::UnexpectedEof {
            anyhow::anyhow!("graphical relay payload is truncated")
        } else {
            error.into()
        }
    })?;
    let diagnostic = || -> Result<String> {
        let value = String::from_utf8(payload.clone())
            .context("graphical relay diagnostic is not UTF-8")?;
        validate_diagnostic(&value)?;
        Ok(value)
    };
    Ok(Some(match kind {
        KIND_HELLO => Frame::Hello,
        KIND_HELLO_ACK => Frame::HelloAck,
        KIND_OPEN_CHANNEL => Frame::OpenChannel { channel_id },
        KIND_CHANNEL_OPENED => Frame::ChannelOpened { channel_id },
        KIND_CHANNEL_REJECTED => Frame::ChannelRejected {
            channel_id,
            reason: diagnostic()?,
        },
        KIND_DATA => Frame::Data {
            channel_id,
            bytes: payload,
        },
        KIND_HALF_CLOSE => Frame::HalfClose { channel_id },
        KIND_CLOSE_CHANNEL => Frame::CloseChannel { channel_id },
        KIND_SESSION_ERROR => Frame::SessionError {
            reason: diagnostic()?,
        },
        _ => unreachable!("validated frame kind"),
    }))
}

/// Allocates monotonically increasing nonzero channel identities without reuse.
#[derive(Debug, Default)]
pub struct ChannelIdAllocator {
    last: u32,
}

impl ChannelIdAllocator {
    /// Returns the next session-unique channel ID.
    ///
    /// # Errors
    ///
    /// Returns an error after exhausting the nonzero `u32` identity space.
    pub fn allocate(&mut self) -> Result<u32> {
        self.last = self
            .last
            .checked_add(1)
            .context("graphical relay channel ID space exhausted")?;
        Ok(self.last)
    }

    pub(crate) const fn last_issued(&self) -> u32 {
        self.last
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn fragmented_and_coalesced_frames_round_trip() {
        let frames = [
            Frame::Hello,
            Frame::OpenChannel { channel_id: 1 },
            Frame::Data {
                channel_id: 1,
                bytes: vec![7; MAX_DATA_BYTES],
            },
            Frame::HalfClose { channel_id: 1 },
        ];
        let (mut encoded_writer, mut encoded_reader) = tokio::io::duplex(128 * 1024);
        let writer = tokio::spawn(async move {
            for frame in frames {
                write_frame(&mut encoded_writer, &frame).await.unwrap();
            }
        });
        assert_eq!(
            read_frame(&mut encoded_reader).await.unwrap(),
            Some(Frame::Hello)
        );
        assert_eq!(
            read_frame(&mut encoded_reader).await.unwrap(),
            Some(Frame::OpenChannel { channel_id: 1 })
        );
        assert!(matches!(
            read_frame(&mut encoded_reader).await.unwrap(),
            Some(Frame::Data { channel_id: 1, bytes }) if bytes.len() == MAX_DATA_BYTES
        ));
        assert_eq!(
            read_frame(&mut encoded_reader).await.unwrap(),
            Some(Frame::HalfClose { channel_id: 1 })
        );
        writer.await.unwrap();
    }

    #[tokio::test]
    async fn decoder_accepts_a_frame_fragmented_across_single_byte_capacity() {
        let (mut writer, mut reader) = tokio::io::duplex(1);
        let send = tokio::spawn(async move {
            write_frame(
                &mut writer,
                &Frame::Data {
                    channel_id: 9,
                    bytes: b"fragmented".to_vec(),
                },
            )
            .await
            .unwrap();
        });
        assert_eq!(
            read_frame(&mut reader).await.unwrap(),
            Some(Frame::Data {
                channel_id: 9,
                bytes: b"fragmented".to_vec(),
            })
        );
        send.await.unwrap();
    }

    #[tokio::test]
    async fn invalid_magic_version_flags_and_lengths_fail_closed() {
        let valid = |kind: u8, channel: u32, length: u32| {
            let mut header = [0_u8; HEADER_BYTES];
            header[..4].copy_from_slice(&MAGIC);
            header[4..6].copy_from_slice(&VERSION.to_be_bytes());
            header[6] = kind;
            header[8..12].copy_from_slice(&channel.to_be_bytes());
            header[12..16].copy_from_slice(&length.to_be_bytes());
            header
        };
        let mut cases = Vec::new();
        let mut magic = valid(KIND_HELLO, 0, 0);
        magic[0] ^= 1;
        cases.push(magic);
        let mut version = valid(KIND_HELLO, 0, 0);
        version[5] = VERSION.to_be_bytes()[1].saturating_add(1);
        cases.push(version);
        let mut flags = valid(KIND_HELLO, 0, 0);
        flags[7] = 1;
        cases.push(flags);
        cases.push(valid(
            KIND_DATA,
            1,
            u32::try_from(MAX_DATA_BYTES + 1).unwrap(),
        ));
        cases.push(valid(KIND_OPEN_CHANNEL, 0, 0));
        for bytes in cases {
            let mut input = &bytes[..];
            assert!(read_frame(&mut input).await.is_err());
        }
    }

    #[test]
    fn channel_ids_are_nonzero_monotonic_and_frames_enforce_identity() {
        let mut allocator = ChannelIdAllocator::default();
        assert_eq!(allocator.allocate().unwrap(), 1);
        assert_eq!(allocator.allocate().unwrap(), 2);
        assert!(Frame::OpenChannel { channel_id: 0 }.parts().is_err());
        assert!(
            Frame::SessionError {
                reason: "bounded".into()
            }
            .parts()
            .is_ok()
        );
    }
}
