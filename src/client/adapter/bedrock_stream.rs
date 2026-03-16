//! AWS Event Stream binary frame decoder for Bedrock streaming.
//!
//! The Bedrock Converse Streaming API returns responses in the AWS Event
//! Stream binary framing format (`application/vnd.amazon.eventstream`).
//! Each frame has the following layout:
//!
//! ```text
//! [total_byte_length: u32]   - big-endian, includes all fields
//! [headers_byte_length: u32] - big-endian
//! [prelude_crc: u32]         - CRC-32 over the first 8 bytes
//! [headers: variable]        - typed key-value pairs
//! [payload: variable]        - message body (JSON for content events)
//! [message_crc: u32]         - CRC-32 over everything before this field
//! ```

use bytes::{Buf, BytesMut};

// ---------------------------------------------------------------------------
// CRC-32 (ISO 3309 / ITU-T V.42) – polynomial 0x04C11DB7
//
// AWS EventStream uses standard CRC-32, NOT CRC-32 (Castagnoli).
// Reversed polynomial: 0xEDB88320
// ---------------------------------------------------------------------------

/// Precomputed CRC-32 lookup table for byte-at-a-time computation.
const CRC32_TABLE: [u32; 256] = {
    let mut table = [0u32; 256];
    let mut i: usize = 0;
    while i < 256 {
        let mut crc = i as u32;
        let mut j = 0;
        while j < 8 {
            if crc & 1 != 0 {
                crc = (crc >> 1) ^ 0xEDB8_8320; // reversed polynomial (standard CRC-32)
            } else {
                crc >>= 1;
            }
            j += 1;
        }
        table[i] = crc;
        i += 1;
    }
    table
};

/// Compute CRC-32 over the given byte slice.
fn crc32(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &byte in data {
        let index = ((crc ^ byte as u32) & 0xFF) as usize;
        crc = CRC32_TABLE[index] ^ (crc >> 8);
    }
    crc ^ 0xFFFF_FFFF
}

/// Minimum frame size: 8 (prelude) + 4 (prelude CRC) + 4 (message CRC) = 16
const MIN_FRAME_SIZE: usize = 16;

/// A decoded AWS Event Stream message.
#[derive(Debug, Clone)]
pub struct EventStreamMessage {
    pub headers: Vec<EventStreamHeader>,
    pub payload: Vec<u8>,
}

/// A single header within an Event Stream frame.
#[derive(Debug, Clone)]
pub struct EventStreamHeader {
    pub name: String,
    pub value: HeaderValue,
}

/// Header value types. We only decode the types actually used by Bedrock.
#[derive(Debug, Clone)]
pub enum HeaderValue {
    String(String),
    #[allow(dead_code)]
    Bytes(Vec<u8>),
    Other,
}

impl EventStreamMessage {
    /// Look up a header by name and return its string value if present.
    pub fn header_str(&self, name: &str) -> Option<&str> {
        self.headers.iter().find_map(|h| {
            if h.name == name {
                if let HeaderValue::String(ref s) = h.value {
                    Some(s.as_str())
                } else {
                    None
                }
            } else {
                None
            }
        })
    }

    /// Return the payload as a UTF-8 string.
    pub fn payload_str(&self) -> Option<&str> {
        std::str::from_utf8(&self.payload).ok()
    }
}

/// Incremental decoder that buffers incoming bytes and yields complete
/// [`EventStreamMessage`]s as they become available.
#[derive(Debug)]
pub struct AwsEventStreamDecoder {
    buf: BytesMut,
}

impl AwsEventStreamDecoder {
    pub fn new() -> Self {
        Self {
            buf: BytesMut::with_capacity(8192),
        }
    }

    /// Feed raw bytes from the network into the decoder.
    pub fn push(&mut self, data: &[u8]) {
        self.buf.extend_from_slice(data);
    }

    /// Try to decode the next complete message from the buffer.
    ///
    /// Returns `Ok(Some(msg))` when a full frame is available,
    /// `Ok(None)` when more data is needed, or `Err` on a malformed frame.
    pub fn decode(&mut self) -> Result<Option<EventStreamMessage>, DecodeError> {
        if self.buf.len() < MIN_FRAME_SIZE {
            return Ok(None);
        }

        // Peek at prelude without advancing the cursor.
        let total_length =
            u32::from_be_bytes([self.buf[0], self.buf[1], self.buf[2], self.buf[3]]) as usize;
        let headers_length =
            u32::from_be_bytes([self.buf[4], self.buf[5], self.buf[6], self.buf[7]]) as usize;

        if total_length < MIN_FRAME_SIZE {
            return Err(DecodeError::InvalidFrameLength(total_length));
        }

        // Wait for the full frame to arrive.
        if self.buf.len() < total_length {
            return Ok(None);
        }

        // Split the complete frame from the buffer.
        let frame = self.buf.split_to(total_length);

        // Validate prelude CRC (bytes 8..12 cover bytes 0..8).
        let prelude_crc_expected = u32::from_be_bytes([frame[8], frame[9], frame[10], frame[11]]);
        let prelude_crc_computed = crc32(&frame[..8]);
        if prelude_crc_computed != prelude_crc_expected {
            return Err(DecodeError::PreludeCrcMismatch {
                expected: prelude_crc_expected,
                computed: prelude_crc_computed,
            });
        }

        // Validate message CRC (last 4 bytes cover everything before them).
        let message_crc_expected = u32::from_be_bytes([
            frame[total_length - 4],
            frame[total_length - 3],
            frame[total_length - 2],
            frame[total_length - 1],
        ]);
        let message_crc_computed = crc32(&frame[..total_length - 4]);
        if message_crc_computed != message_crc_expected {
            return Err(DecodeError::MessageCrcMismatch {
                expected: message_crc_expected,
                computed: message_crc_computed,
            });
        }

        // Skip prelude (8 bytes) and prelude CRC (4 bytes).
        let header_start = 12;
        let header_end = header_start + headers_length;
        if header_end > total_length.saturating_sub(4) {
            return Err(DecodeError::InvalidHeaderLength(headers_length));
        }

        let headers = decode_headers(&frame[header_start..header_end])?;

        // Payload sits between headers and the trailing message CRC (4 bytes).
        let payload_end = total_length - 4;
        let payload = frame[header_end..payload_end].to_vec();

        Ok(Some(EventStreamMessage { headers, payload }))
    }

    /// Drain all complete messages currently buffered.
    pub fn decode_all(&mut self) -> Result<Vec<EventStreamMessage>, DecodeError> {
        let mut messages = Vec::new();
        while let Some(msg) = self.decode()? {
            messages.push(msg);
        }
        Ok(messages)
    }
}

fn decode_headers(mut data: &[u8]) -> Result<Vec<EventStreamHeader>, DecodeError> {
    let mut headers = Vec::new();

    while !data.is_empty() {
        if data.is_empty() {
            break;
        }

        // Header name: [name_length: u8] [name: utf8]
        let name_len = data[0] as usize;
        data.advance(1);
        if data.len() < name_len {
            return Err(DecodeError::TruncatedHeader);
        }
        let name = std::str::from_utf8(&data[..name_len])
            .map_err(|_| DecodeError::InvalidUtf8)?
            .to_string();
        data.advance(name_len);

        // Header value type: [type: u8]
        if data.is_empty() {
            return Err(DecodeError::TruncatedHeader);
        }
        let value_type = data[0];
        data.advance(1);

        let value = match value_type {
            // Type 7 = String: [value_length: u16] [value: utf8]
            7 => {
                if data.len() < 2 {
                    return Err(DecodeError::TruncatedHeader);
                }
                let value_len = u16::from_be_bytes([data[0], data[1]]) as usize;
                data.advance(2);
                if data.len() < value_len {
                    return Err(DecodeError::TruncatedHeader);
                }
                let s = std::str::from_utf8(&data[..value_len])
                    .map_err(|_| DecodeError::InvalidUtf8)?
                    .to_string();
                data.advance(value_len);
                HeaderValue::String(s)
            }
            // Type 6 = Bytes: [value_length: u16] [value: bytes]
            6 => {
                if data.len() < 2 {
                    return Err(DecodeError::TruncatedHeader);
                }
                let value_len = u16::from_be_bytes([data[0], data[1]]) as usize;
                data.advance(2);
                if data.len() < value_len {
                    return Err(DecodeError::TruncatedHeader);
                }
                let b = data[..value_len].to_vec();
                data.advance(value_len);
                HeaderValue::Bytes(b)
            }
            // Type 0 = Bool true (no payload)
            0 => HeaderValue::Other,
            // Type 1 = Bool false (no payload)
            1 => HeaderValue::Other,
            // Type 2 = Byte: 1 byte
            2 => {
                if data.is_empty() {
                    return Err(DecodeError::TruncatedHeader);
                }
                data.advance(1);
                HeaderValue::Other
            }
            // Type 3 = Short: 2 bytes
            3 => {
                if data.len() < 2 {
                    return Err(DecodeError::TruncatedHeader);
                }
                data.advance(2);
                HeaderValue::Other
            }
            // Type 4 = Int: 4 bytes
            4 => {
                if data.len() < 4 {
                    return Err(DecodeError::TruncatedHeader);
                }
                data.advance(4);
                HeaderValue::Other
            }
            // Type 5 = Long: 8 bytes
            5 => {
                if data.len() < 8 {
                    return Err(DecodeError::TruncatedHeader);
                }
                data.advance(8);
                HeaderValue::Other
            }
            // Type 8 = Timestamp: 8 bytes
            8 => {
                if data.len() < 8 {
                    return Err(DecodeError::TruncatedHeader);
                }
                data.advance(8);
                HeaderValue::Other
            }
            // Type 9 = UUID: 16 bytes
            9 => {
                if data.len() < 16 {
                    return Err(DecodeError::TruncatedHeader);
                }
                data.advance(16);
                HeaderValue::Other
            }
            other => {
                return Err(DecodeError::UnknownHeaderType(other));
            }
        };

        headers.push(EventStreamHeader { name, value });
    }

    Ok(headers)
}

/// Errors that can occur during Event Stream frame decoding.
#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error("invalid frame total length: {0}")]
    InvalidFrameLength(usize),
    #[error("invalid header length: {0}")]
    InvalidHeaderLength(usize),
    #[error("prelude CRC-32 mismatch (expected {expected:#010x}, computed {computed:#010x})")]
    PreludeCrcMismatch { expected: u32, computed: u32 },
    #[error("message CRC-32 mismatch (expected {expected:#010x}, computed {computed:#010x})")]
    MessageCrcMismatch { expected: u32, computed: u32 },
    #[error("truncated header data")]
    TruncatedHeader,
    #[error("invalid UTF-8 in header")]
    InvalidUtf8,
    #[error("unknown header value type: {0}")]
    UnknownHeaderType(u8),
}

/// Build a minimal AWS Event Stream frame from headers and a payload.
///
/// Used in tests to construct realistic binary frames without needing
/// real Bedrock responses.
#[cfg(test)]
fn build_frame(headers: &[(&str, &str)], payload: &[u8]) -> Vec<u8> {
    let mut header_bytes = Vec::new();
    for (name, value) in headers {
        let name_bytes = name.as_bytes();
        header_bytes.push(name_bytes.len() as u8);
        header_bytes.extend_from_slice(name_bytes);
        // Type 7 = String
        header_bytes.push(7);
        let value_bytes = value.as_bytes();
        header_bytes.extend_from_slice(&(value_bytes.len() as u16).to_be_bytes());
        header_bytes.extend_from_slice(value_bytes);
    }

    // total_length = 8 (prelude) + 4 (prelude CRC) + headers + payload + 4 (message CRC)
    let total_length: u32 = (16 + header_bytes.len() + payload.len()) as u32;
    let headers_length: u32 = header_bytes.len() as u32;

    let mut frame = Vec::new();
    frame.extend_from_slice(&total_length.to_be_bytes());
    frame.extend_from_slice(&headers_length.to_be_bytes());

    // Prelude CRC-32 over bytes 0..8 (total_length + headers_length).
    let prelude_crc = crc32(&frame[..8]);
    frame.extend_from_slice(&prelude_crc.to_be_bytes());

    frame.extend_from_slice(&header_bytes);
    frame.extend_from_slice(payload);

    // Message CRC-32 over everything before this field.
    let message_crc = crc32(&frame);
    frame.extend_from_slice(&message_crc.to_be_bytes());

    frame
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_single_frame() {
        let payload = br#"{"contentBlockDelta":{"contentBlockIndex":0,"delta":{"text":"Hello"}}}"#;
        let frame = build_frame(
            &[
                (":message-type", "event"),
                (":event-type", "contentBlockDelta"),
            ],
            payload,
        );

        let mut decoder = AwsEventStreamDecoder::new();
        decoder.push(&frame);

        let msg = decoder.decode().unwrap().expect("should decode a message");
        assert_eq!(msg.header_str(":message-type"), Some("event"));
        assert_eq!(msg.header_str(":event-type"), Some("contentBlockDelta"));
        assert_eq!(
            msg.payload_str().unwrap(),
            std::str::from_utf8(payload).unwrap()
        );
    }

    #[test]
    fn decode_multiple_frames() {
        let frame1 = build_frame(
            &[(":message-type", "event"), (":event-type", "messageStart")],
            br#"{"role":"assistant"}"#,
        );
        let frame2 = build_frame(
            &[
                (":message-type", "event"),
                (":event-type", "contentBlockDelta"),
            ],
            br#"{"contentBlockIndex":0,"delta":{"text":"Hi"}}"#,
        );

        let mut decoder = AwsEventStreamDecoder::new();
        decoder.push(&frame1);
        decoder.push(&frame2);

        let messages = decoder.decode_all().unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].header_str(":event-type"), Some("messageStart"));
        assert_eq!(
            messages[1].header_str(":event-type"),
            Some("contentBlockDelta")
        );
    }

    #[test]
    fn decode_partial_frame() {
        let payload = br#"{"test":true}"#;
        let frame = build_frame(
            &[(":message-type", "event"), (":event-type", "test")],
            payload,
        );

        let mut decoder = AwsEventStreamDecoder::new();

        // Feed first half
        let mid = frame.len() / 2;
        decoder.push(&frame[..mid]);
        assert!(decoder.decode().unwrap().is_none());

        // Feed second half
        decoder.push(&frame[mid..]);
        let msg = decoder
            .decode()
            .unwrap()
            .expect("should decode after full data");
        assert_eq!(msg.header_str(":event-type"), Some("test"));
    }

    #[test]
    fn decode_empty_payload() {
        let frame = build_frame(
            &[(":message-type", "event"), (":event-type", "messageStop")],
            b"",
        );

        let mut decoder = AwsEventStreamDecoder::new();
        decoder.push(&frame);

        let msg = decoder
            .decode()
            .unwrap()
            .expect("should decode empty payload");
        assert_eq!(msg.header_str(":event-type"), Some("messageStop"));
        assert!(msg.payload.is_empty());
    }

    #[test]
    fn decode_insufficient_data_returns_none() {
        let mut decoder = AwsEventStreamDecoder::new();
        decoder.push(&[0, 0, 0, 20]); // Only partial prelude
        assert!(decoder.decode().unwrap().is_none());
    }

    #[test]
    fn decode_invalid_frame_length() {
        let mut decoder = AwsEventStreamDecoder::new();
        // total_length = 5 which is less than MIN_FRAME_SIZE
        let mut frame = vec![0u8; 16];
        frame[0..4].copy_from_slice(&5u32.to_be_bytes());
        decoder.push(&frame);
        assert!(decoder.decode().is_err());
    }

    #[test]
    fn decode_exception_message() {
        let payload = br#"{"message":"throttled"}"#;
        let frame = build_frame(
            &[
                (":message-type", "exception"),
                (":exception-type", "throttlingException"),
            ],
            payload,
        );

        let mut decoder = AwsEventStreamDecoder::new();
        decoder.push(&frame);

        let msg = decoder.decode().unwrap().unwrap();
        assert_eq!(msg.header_str(":message-type"), Some("exception"));
        assert_eq!(
            msg.header_str(":exception-type"),
            Some("throttlingException")
        );
    }

    #[test]
    fn decode_back_to_back_frames_single_push() {
        let frame1 = build_frame(&[(":message-type", "event"), (":event-type", "a")], b"{}");
        let frame2 = build_frame(&[(":message-type", "event"), (":event-type", "b")], b"{}");

        let mut combined = Vec::new();
        combined.extend_from_slice(&frame1);
        combined.extend_from_slice(&frame2);

        let mut decoder = AwsEventStreamDecoder::new();
        decoder.push(&combined);

        let messages = decoder.decode_all().unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].header_str(":event-type"), Some("a"));
        assert_eq!(messages[1].header_str(":event-type"), Some("b"));
    }

    #[test]
    fn test_prelude_crc_mismatch() {
        let mut frame = build_frame(
            &[(":message-type", "event"), (":event-type", "test")],
            b"{}",
        );
        // Corrupt the prelude CRC (bytes 8..12).
        frame[8] ^= 0xFF;

        let mut decoder = AwsEventStreamDecoder::new();
        decoder.push(&frame);

        let err = decoder.decode().unwrap_err();
        assert!(
            matches!(err, DecodeError::PreludeCrcMismatch { .. }),
            "expected PreludeCrcMismatch, got: {err:?}"
        );
    }

    #[test]
    fn test_message_crc_mismatch() {
        let mut frame = build_frame(
            &[(":message-type", "event"), (":event-type", "test")],
            b"{}",
        );
        // Corrupt the message CRC (last 4 bytes).
        let len = frame.len();
        frame[len - 1] ^= 0xFF;

        let mut decoder = AwsEventStreamDecoder::new();
        decoder.push(&frame);

        let err = decoder.decode().unwrap_err();
        assert!(
            matches!(err, DecodeError::MessageCrcMismatch { .. }),
            "expected MessageCrcMismatch, got: {err:?}"
        );
    }
}
