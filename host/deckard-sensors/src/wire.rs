//! The sensors socket's messages (docs/sensors.md). One SOCK_SEQPACKET
//! datagram per message: a little-endian `u32` type, then its fields,
//! unpadded. The HAL's copy is `hardware/deckard/sensors/Sensors.cpp` in the
//! image.

pub const VERSION: u32 = 1;

pub const HELLO: u32 = 1;
pub const ACTIVATE: u32 = 2;
pub const BATCH: u32 = 3;

pub const SENSOR: u32 = 101;
pub const SENSORS_END: u32 = 102;
pub const EVENT: u32 = 103;

/// Bytes of a sensor's name in `SENSOR`, NUL-padded.
pub const NAME: usize = 48;

#[derive(Debug, PartialEq, Eq)]
pub enum Request {
    Hello { version: u32 },
    Activate { handle: u32, enabled: bool },
    Batch { handle: u32, period_ns: i64 },
}

fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

fn i64_at(bytes: &[u8], at: usize) -> Option<i64> {
    Some(i64::from_le_bytes(bytes.get(at..at + 8)?.try_into().ok()?))
}

impl Request {
    /// A request, if `bytes` is exactly one.
    #[must_use]
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        let request = match u32_at(bytes, 0)? {
            HELLO if bytes.len() == 8 => Self::Hello {
                version: u32_at(bytes, 4)?,
            },
            ACTIVATE if bytes.len() == 12 => Self::Activate {
                handle: u32_at(bytes, 4)?,
                enabled: u32_at(bytes, 8)? != 0,
            },
            BATCH if bytes.len() == 16 => Self::Batch {
                handle: u32_at(bytes, 4)?,
                period_ns: i64_at(bytes, 8)?,
            },
            _ => return None,
        };
        Some(request)
    }
}

/// Builds a message from its type and fields.
#[derive(Default)]
pub struct Message(Vec<u8>);

impl Message {
    #[must_use]
    pub fn new(kind: u32) -> Self {
        Self(kind.to_le_bytes().to_vec())
    }

    #[must_use]
    pub fn u32(mut self, value: u32) -> Self {
        self.0.extend_from_slice(&value.to_le_bytes());
        self
    }

    #[must_use]
    pub fn i32(mut self, value: i32) -> Self {
        self.0.extend_from_slice(&value.to_le_bytes());
        self
    }

    #[must_use]
    pub fn i64(mut self, value: i64) -> Self {
        self.0.extend_from_slice(&value.to_le_bytes());
        self
    }

    #[must_use]
    pub fn f32(mut self, value: f32) -> Self {
        self.0.extend_from_slice(&value.to_le_bytes());
        self
    }

    /// `text` in `NAME` bytes: cut at a character boundary, NUL-padded, so
    /// at least one NUL ends it.
    #[must_use]
    pub fn name(mut self, text: &str) -> Self {
        let mut end = text.len().min(NAME - 1);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        let mut field = [0_u8; NAME];
        field[..end].copy_from_slice(&text.as_bytes()[..end]);
        self.0.extend_from_slice(&field);
        self
    }

    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.0
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "fixed test messages")]
mod tests {
    use super::*;

    #[test]
    fn requests_parse_only_at_their_exact_length() {
        let batch = Message::new(BATCH).u32(3).i64(20_000_000);
        assert_eq!(
            Request::parse(batch.bytes()),
            Some(Request::Batch {
                handle: 3,
                period_ns: 20_000_000
            })
        );
        assert_eq!(Request::parse(&batch.bytes()[..12]), None);
        let activate = Message::new(ACTIVATE).u32(1).u32(1);
        assert_eq!(
            Request::parse(activate.bytes()),
            Some(Request::Activate {
                handle: 1,
                enabled: true
            })
        );
        assert_eq!(Request::parse(Message::new(HELLO).bytes()), None);
        assert_eq!(Request::parse(Message::new(EVENT).u32(1).bytes()), None);
    }

    #[test]
    fn names_are_cut_at_a_character_and_end_in_nul() {
        let long = "é".repeat(40);
        let message = Message::new(SENSOR).name(&long);
        let field = &message.bytes()[4..];
        assert_eq!(field.len(), NAME);
        assert_eq!(field[NAME - 1], 0);
        let text = field.split(|&b| b == 0).next().unwrap();
        assert_eq!(std::str::from_utf8(text).map(str::len), Ok(46));
    }
}
