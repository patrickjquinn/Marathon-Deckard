use std::time::Instant;

use serde_json::{Map, Value};

use crate::Error;

pub const MAX_FRAME_SIZE: usize = 1 << 20;
pub const MAX_BUFFER_SIZE: usize = MAX_FRAME_SIZE + 4;
pub const BURST: u32 = 200;
pub const PER_SECOND: u32 = 50;
pub const MAX_DROPPED: u32 = 1000;

pub struct TokenBucket {
    tokens: f64,
    last: Instant,
}

impl TokenBucket {
    pub fn new(now: Instant) -> Self {
        Self {
            tokens: f64::from(BURST),
            last: now,
        }
    }

    pub fn take(&mut self, now: Instant) -> bool {
        let elapsed = now.saturating_duration_since(self.last);
        self.last = self.last.max(now);
        self.tokens =
            (self.tokens + elapsed.as_secs_f64() * f64::from(PER_SECOND)).min(f64::from(BURST));
        if self.tokens < 1.0 {
            return false;
        }
        self.tokens -= 1.0;
        true
    }
}

pub struct Decoder {
    buffer: Vec<u8>,
    limiter: TokenBucket,
    dropped: u32,
    failed: bool,
}

impl Decoder {
    pub fn new(now: Instant) -> Self {
        Self {
            buffer: Vec::new(),
            limiter: TokenBucket::new(now),
            dropped: 0,
            failed: false,
        }
    }

    pub fn push(
        &mut self,
        bytes: &[u8],
        now: Instant,
    ) -> Result<Option<Map<String, Value>>, Error> {
        if self.failed {
            return Err(Error::InvalidFrame);
        }
        let result = self.push_inner(bytes, now);
        if result.is_err() {
            self.failed = true;
            self.buffer.clear();
        }
        result
    }

    fn push_inner(
        &mut self,
        bytes: &[u8],
        now: Instant,
    ) -> Result<Option<Map<String, Value>>, Error> {
        if bytes.len() > MAX_BUFFER_SIZE.saturating_sub(self.buffer.len()) {
            return Err(Error::BufferLimit);
        }
        self.buffer.extend_from_slice(bytes);
        while self.buffer.len() >= 4 {
            let length = u32::from_be_bytes([
                self.buffer[0],
                self.buffer[1],
                self.buffer[2],
                self.buffer[3],
            ]) as usize;
            if length == 0 || length > MAX_FRAME_SIZE {
                return Err(Error::InvalidFrame);
            }
            if self.buffer.len() < length + 4 {
                return Ok(None);
            }
            let message: Value = serde_json::from_slice(&self.buffer[4..4 + length])
                .map_err(|_| Error::InvalidFrame)?;
            self.buffer.drain(..4 + length);
            let Value::Object(object) = message else {
                return Err(Error::InvalidFrame);
            };
            if !valid_type(&object) {
                return Err(Error::InvalidFrame);
            }
            if self.limiter.take(now) {
                return Ok(Some(object));
            }
            self.dropped += 1;
            if self.dropped > MAX_DROPPED {
                return Err(Error::Flood);
            }
        }
        Ok(None)
    }

    pub fn finish(&self) -> Result<(), Error> {
        if self.failed || !self.buffer.is_empty() {
            Err(Error::InvalidFrame)
        } else {
            Ok(())
        }
    }

    pub fn dropped(&self) -> u32 {
        self.dropped
    }

    pub fn remaining_capacity(&self) -> usize {
        MAX_BUFFER_SIZE.saturating_sub(self.buffer.len())
    }
}

fn valid_type(object: &Map<String, Value>) -> bool {
    object
        .get("t")
        .and_then(Value::as_str)
        .is_some_and(|value| !value.is_empty() && value.encode_utf16().count() <= 32)
}

pub fn encode(object: &Map<String, Value>) -> Result<Vec<u8>, Error> {
    if !valid_type(object) {
        return Err(Error::InvalidFrame);
    }
    let body = serde_json::to_vec(object).map_err(|_| Error::InvalidFrame)?;
    if body.len() > MAX_FRAME_SIZE {
        return Err(Error::InvalidFrame);
    }
    let length = u32::try_from(body.len()).map_err(|_| Error::InvalidFrame)?;
    let mut frame = Vec::with_capacity(body.len() + 4);
    frame.extend_from_slice(&length.to_be_bytes());
    frame.extend_from_slice(&body);
    Ok(frame)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "tests construct protocol fixtures")]
mod tests {
    use super::*;
    use serde_json::json;
    use std::time::Duration;

    fn frame() -> Vec<u8> {
        encode(json!({"t": "hello", "v": 1}).as_object().unwrap()).unwrap()
    }

    #[test]
    fn fragmented_header_and_body_roundtrip() {
        let now = Instant::now();
        let mut decoder = Decoder::new(now);
        let bytes = frame();
        for byte in &bytes[..bytes.len() - 1] {
            assert!(decoder.push(&[*byte], now).unwrap().is_none());
        }
        assert_eq!(
            decoder
                .push(&bytes[bytes.len() - 1..], now)
                .unwrap()
                .unwrap()["v"],
            1
        );
        assert!(decoder.finish().is_ok());
    }

    #[test]
    fn coalesced_frames_are_drained_without_losing_messages() {
        let now = Instant::now();
        let mut decoder = Decoder::new(now);
        let bytes = [frame(), frame()].concat();
        assert!(decoder.push(&bytes, now).unwrap().is_some());
        assert!(decoder.push(&[], now).unwrap().is_some());
        assert!(decoder.push(&[], now).unwrap().is_none());
    }

    #[test]
    fn invalid_frames_poison_the_connection() {
        let now = Instant::now();
        for body in [
            b"[]".as_slice(),
            b"{}",
            b"{\"t\":42}",
            b"{\"t\":\"\"}",
            b"{bad",
        ] {
            let mut decoder = Decoder::new(now);
            let bytes = [(body.len() as u32).to_be_bytes().as_slice(), body].concat();
            assert_eq!(decoder.push(&bytes, now), Err(Error::InvalidFrame));
            assert_eq!(decoder.push(&frame(), now), Err(Error::InvalidFrame));
        }
        for size in [0, (MAX_FRAME_SIZE + 1) as u32] {
            assert!(Decoder::new(now).push(&size.to_be_bytes(), now).is_err());
        }
    }

    #[test]
    fn bounds_partial_buffers_and_detects_truncated_eof() {
        let now = Instant::now();
        let mut decoder = Decoder::new(now);
        assert!(decoder.push(&[0, 0], now).unwrap().is_none());
        assert!(decoder.finish().is_err());
        assert_eq!(
            decoder.push(&vec![0; MAX_BUFFER_SIZE], now),
            Err(Error::BufferLimit)
        );
    }

    #[test]
    fn bounded_reads_preserve_a_large_frame_followed_by_another_frame() {
        let now = Instant::now();
        let large = encode(
            json!({"t": "large", "text": "x".repeat(MAX_FRAME_SIZE - 100)})
                .as_object()
                .unwrap(),
        )
        .unwrap();
        let bytes = [large, frame()].concat();
        let mut decoder = Decoder::new(now);
        let mut position = 0;
        let mut received = Vec::new();
        while position < bytes.len() {
            let count = decoder
                .remaining_capacity()
                .min(8192)
                .min(bytes.len() - position);
            assert!(count > 0);
            let mut message = decoder
                .push(&bytes[position..position + count], now)
                .unwrap();
            position += count;
            while let Some(object) = message {
                received.push(object["t"].as_str().unwrap().to_owned());
                message = decoder.push(&[], now).unwrap();
            }
        }
        assert_eq!(received, ["large", "hello"]);
        assert!(decoder.finish().is_ok());
    }

    #[test]
    fn limiter_refills_and_disconnects_after_one_thousand_drops() {
        let now = Instant::now();
        let mut decoder = Decoder::new(now);
        for _ in 0..BURST {
            assert!(decoder.push(&frame(), now).unwrap().is_some());
        }
        for _ in 0..MAX_DROPPED {
            assert!(decoder.push(&frame(), now).unwrap().is_none());
        }
        assert_eq!(decoder.dropped(), MAX_DROPPED);
        assert_eq!(decoder.push(&frame(), now), Err(Error::Flood));
        let mut limiter = TokenBucket::new(now);
        for _ in 0..BURST {
            assert!(limiter.take(now));
        }
        assert!(!limiter.take(now));
        assert!(limiter.take(now + Duration::from_millis(20)));
        assert!(!limiter.take(now));
    }
}
