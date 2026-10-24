//! Deckard's Android bridge sockets: one authenticated Android process at a
//! time, exchanging length-prefixed JSON objects (protocol/bridge.md).

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::net::Shutdown;
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{Map, Value};

use crate::Error;
use crate::ids::PackageName;

/// The largest frame body, in bytes.
pub const MAX_FRAME_SIZE: usize = 1 << 20;
/// Frames a peer may send at once before the rate limit applies.
pub const BURST: u32 = 200;
/// Rate-limited frames dropped before a peer is cut off as a flood.
pub const MAX_DROPPED: u32 = 1000;
const PER_SECOND: u32 = 50;
const MAX_BUFFER_SIZE: usize = MAX_FRAME_SIZE + 4;
const MAX_PACKAGES_LIST_SIZE: u64 = 4 << 20;
const READ_CHUNK_SIZE: usize = 8192;
const WRITE_TIMEOUT: Duration = Duration::from_secs(2);

fn invalid_data(error: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error)
}

fn valid_type(object: &Map<String, Value>) -> bool {
    let kind = object.get("t").and_then(Value::as_str).unwrap_or("");
    !kind.is_empty() && kind.encode_utf16().count() <= 32
}

/// A frame for `object`, which must have a short string `t`.
///
/// # Errors
/// `object` has no valid `t` or is over [`MAX_FRAME_SIZE`].
pub fn encode(object: &Map<String, Value>) -> Result<Vec<u8>, Error> {
    let body = serde_json::to_vec(object).map_err(|_| Error::InvalidFrame)?;
    let length = u32::try_from(body.len()).map_err(|_| Error::InvalidFrame)?;
    if !valid_type(object) || body.len() > MAX_FRAME_SIZE {
        return Err(Error::InvalidFrame);
    }
    let mut frame = length.to_be_bytes().to_vec();
    frame.extend_from_slice(&body);
    Ok(frame)
}

/// Splits a byte stream into frames under a token-bucket rate limit.
struct Decoder {
    buffer: Vec<u8>,
    tokens: f64,
    last: Instant,
    dropped: u32,
}

impl Decoder {
    fn new(now: Instant) -> Self {
        Self {
            buffer: Vec::new(),
            tokens: f64::from(BURST),
            last: now,
            dropped: 0,
        }
    }

    fn take_token(&mut self, now: Instant) -> bool {
        let elapsed = now.saturating_duration_since(self.last).as_secs_f64();
        self.last = self.last.max(now);
        self.tokens = (self.tokens + elapsed * f64::from(PER_SECOND)).min(f64::from(BURST));
        let allowed = self.tokens >= 1.0;
        if allowed {
            self.tokens -= 1.0;
        }
        allowed
    }

    /// Appends `bytes` and returns the next frame the rate limit lets through.
    fn push(&mut self, bytes: &[u8], now: Instant) -> Result<Option<Map<String, Value>>, Error> {
        if bytes.len() > self.capacity() {
            return Err(Error::BufferLimit);
        }
        self.buffer.extend_from_slice(bytes);
        while let Some(header) = self.buffer.first_chunk::<4>() {
            let length = u32::from_be_bytes(*header) as usize;
            if length == 0 || length > MAX_FRAME_SIZE {
                return Err(Error::InvalidFrame);
            }
            let Some(body) = self.buffer.get(4..4 + length) else {
                return Ok(None);
            };
            let message: Map<String, Value> =
                serde_json::from_slice(body).map_err(|_| Error::InvalidFrame)?;
            self.buffer.drain(..4 + length);
            if !valid_type(&message) {
                return Err(Error::InvalidFrame);
            }
            if self.take_token(now) {
                return Ok(Some(message));
            }
            self.dropped += 1;
            if self.dropped > MAX_DROPPED {
                return Err(Error::Flood);
            }
        }
        Ok(None)
    }

    fn capacity(&self) -> usize {
        MAX_BUFFER_SIZE.saturating_sub(self.buffer.len())
    }
}

fn package_uid(path: &Path, package: &PackageName) -> io::Result<u32> {
    let mut contents = String::new();
    File::open(path)?
        .take(MAX_PACKAGES_LIST_SIZE + 1)
        .read_to_string(&mut contents)?;
    if contents.len() as u64 > MAX_PACKAGES_LIST_SIZE {
        return Err(invalid_data("Android packages.list exceeds limit"));
    }
    let mut uid = None;
    for line in contents.lines() {
        let mut fields = line.split_ascii_whitespace();
        if fields.next() != Some(package.as_str()) {
            continue;
        }
        let parsed = fields.next().and_then(|value| value.parse::<u32>().ok());
        let parsed = parsed.ok_or_else(|| invalid_data("invalid package UID"))?;
        if uid.replace(parsed).is_some() {
            return Err(invalid_data("duplicate package entry"));
        }
    }
    uid.ok_or_else(|| io::Error::new(io::ErrorKind::PermissionDenied, "package not installed"))
}

/// Accepts one authenticated Android process at a time. A newly accepted
/// process closes the previous connection, including its blocked read.
pub struct BridgeListener {
    listener: UnixListener,
    socket_path: PathBuf,
    socket_identity: (u64, u64),
    packages_list: PathBuf,
    package: PackageName,
    active: Option<UnixStream>,
    generation: u64,
}

impl BridgeListener {
    /// Binds `socket_path`, mode 0666 in a 0755 directory, for the UID that
    /// `packages_list` gives `package`.
    ///
    /// # Errors
    /// The directory or socket cannot be made, or something other than a
    /// socket of ours is at `socket_path`.
    pub fn bind(
        socket_path: impl AsRef<Path>,
        packages_list: impl AsRef<Path>,
        package: PackageName,
    ) -> io::Result<Self> {
        let socket_path = socket_path.as_ref().to_path_buf();
        let parent = socket_path
            .parent()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "socket has no parent"))?;
        fs::create_dir_all(parent)?;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o755))?;
        match fs::symlink_metadata(&socket_path) {
            Ok(metadata)
                if metadata.file_type().is_socket()
                    && metadata.uid() == rustix::process::geteuid().as_raw() =>
            {
                fs::remove_file(&socket_path)?;
            }
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "socket path is not an owned socket",
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let listener = UnixListener::bind(&socket_path)?;
        fs::set_permissions(&socket_path, fs::Permissions::from_mode(0o666))?;
        let metadata = fs::symlink_metadata(&socket_path)?;
        Ok(Self {
            listener,
            socket_path,
            socket_identity: (metadata.dev(), metadata.ino()),
            packages_list: packages_list.as_ref().to_path_buf(),
            package,
            active: None,
            generation: 0,
        })
    }

    /// Waits for the next peer whose UID is the package's, closing the
    /// previous one.
    ///
    /// # Errors
    /// Accepting failed, or the peer is not the package.
    pub fn accept(&mut self) -> io::Result<BridgeSession> {
        let (stream, _) = self.listener.accept()?;
        let actual_uid = rustix::net::sockopt::socket_peercred(&stream)?.uid.as_raw();
        let expected_uid = package_uid(&self.packages_list, &self.package)?;
        if actual_uid != expected_uid {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("bridge peer UID {actual_uid} does not match {expected_uid}"),
            ));
        }
        let active = stream.try_clone()?;
        self.generation = self
            .generation
            .checked_add(1)
            .ok_or_else(|| io::Error::other("bridge generation exhausted"))?;
        if let Some(previous) = self.active.replace(active) {
            let _ = previous.shutdown(Shutdown::Both);
        }
        Ok(BridgeSession {
            stream,
            decoder: Decoder::new(Instant::now()),
            generation: self.generation,
        })
    }
}

impl Drop for BridgeListener {
    fn drop(&mut self) {
        if let Some(active) = self.active.take() {
            let _ = active.shutdown(Shutdown::Both);
        }
        if fs::symlink_metadata(&self.socket_path)
            .is_ok_and(|metadata| (metadata.dev(), metadata.ino()) == self.socket_identity)
        {
            let _ = fs::remove_file(&self.socket_path);
        }
    }
}

/// One accepted peer.
pub struct BridgeSession {
    stream: UnixStream,
    decoder: Decoder,
    generation: u64,
}

impl BridgeSession {
    /// Counts the listener's accepted peers, from 1.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// The next message, or `None` at a clean end of stream. Coalesced
    /// frames are drained before more is read; a protocol error closes the
    /// session.
    ///
    /// # Errors
    /// The read failed or timed out, or the peer broke the protocol.
    pub fn read_message(&mut self) -> io::Result<Option<Map<String, Value>>> {
        let mut bytes = [0_u8; READ_CHUNK_SIZE];
        let mut count = 0;
        loop {
            match self.decoder.push(&bytes[..count], Instant::now()) {
                Ok(Some(message)) => return Ok(Some(message)),
                Ok(None) => {}
                Err(error) => {
                    self.close();
                    return Err(invalid_data(error));
                }
            }
            let capacity = self.decoder.capacity().min(READ_CHUNK_SIZE);
            count = self.stream.read(&mut bytes[..capacity])?;
            if count == 0 {
                return match self.decoder.buffer.is_empty() {
                    true => Ok(None),
                    false => Err(invalid_data(Error::InvalidFrame)),
                };
            }
        }
    }

    /// # Errors
    /// See [`BridgeWriter::send_message`].
    pub fn send_message(&mut self, message: &Map<String, Value>) -> io::Result<()> {
        send(&mut self.stream, message)
    }

    /// A sender for another thread, so writes need not wait for a read.
    ///
    /// # Errors
    /// The socket cannot be duplicated.
    pub fn writer(&self) -> io::Result<BridgeWriter> {
        Ok(BridgeWriter {
            stream: self.stream.try_clone()?,
        })
    }

    fn close(&self) {
        let _ = self.stream.shutdown(Shutdown::Both);
    }
}

impl Drop for BridgeSession {
    fn drop(&mut self) {
        self.close();
    }
}

/// The sending half of a [`BridgeSession`].
pub struct BridgeWriter {
    stream: UnixStream,
}

impl BridgeWriter {
    /// Sends one frame within a two-second deadline for the whole frame. A
    /// failed write closes the connection.
    ///
    /// # Errors
    /// The message is not a valid frame, or the write failed or timed out.
    pub fn send_message(&mut self, message: &Map<String, Value>) -> io::Result<()> {
        send(&mut self.stream, message)
    }

    /// Also ends the session's reads.
    pub fn close(&self) {
        let _ = self.stream.shutdown(Shutdown::Both);
    }
}

fn send(stream: &mut UnixStream, message: &Map<String, Value>) -> io::Result<()> {
    let frame = encode(message).map_err(invalid_data)?;
    let result = write_frame(stream, &frame);
    if result.is_err() {
        let _ = stream.shutdown(Shutdown::Both);
    }
    result
}

fn write_frame(stream: &mut UnixStream, mut frame: &[u8]) -> io::Result<()> {
    let deadline = Instant::now() + WRITE_TIMEOUT;
    while !frame.is_empty() {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .filter(|remaining| !remaining.is_zero())
            .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "bridge write timed out"))?;
        stream.set_write_timeout(Some(remaining))?;
        match stream.write(frame) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "bridge peer closed",
                ));
            }
            Ok(count) => frame = &frame[count..],
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "tests construct protocol and socket fixtures"
)]
mod tests {
    use super::*;
    use serde_json::json;

    fn object(value: Value) -> Map<String, Value> {
        value.as_object().unwrap().clone()
    }

    fn frame() -> Vec<u8> {
        encode(&object(json!({"t": "hello", "v": 1}))).unwrap()
    }

    fn setup() -> (tempfile::TempDir, PathBuf, PathBuf, PackageName) {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("deckard/bridge.sock");
        let packages = dir.path().join("packages.list");
        let package = PackageName::try_from("org.marathonos.deckard.bridge").unwrap();
        let uid = rustix::process::geteuid().as_raw();
        fs::write(
            &packages,
            format!("{} {uid} 0 /data/user/0\n", package.as_str()),
        )
        .unwrap();
        (dir, socket, packages, package)
    }

    #[test]
    fn fragmented_and_coalesced_frames_decode() {
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
        assert!(
            decoder
                .push(&[frame(), frame()].concat(), now)
                .unwrap()
                .is_some()
        );
        assert!(decoder.push(&[], now).unwrap().is_some());
        assert!(decoder.push(&[], now).unwrap().is_none());
    }

    #[test]
    fn invalid_frames_are_rejected() {
        let now = Instant::now();
        for body in [
            b"[]".as_slice(),
            b"{}",
            b"{\"t\":42}",
            b"{\"t\":\"\"}",
            b"{bad",
        ] {
            let bytes = [(body.len() as u32).to_be_bytes().as_slice(), body].concat();
            assert_eq!(
                Decoder::new(now).push(&bytes, now),
                Err(Error::InvalidFrame)
            );
        }
        for size in [0, (MAX_FRAME_SIZE + 1) as u32] {
            assert!(Decoder::new(now).push(&size.to_be_bytes(), now).is_err());
        }
        let mut decoder = Decoder::new(now);
        assert!(decoder.push(&[0, 0], now).unwrap().is_none());
        assert_eq!(
            decoder.push(&vec![0; MAX_BUFFER_SIZE], now),
            Err(Error::BufferLimit)
        );
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
        assert_eq!(decoder.push(&frame(), now), Err(Error::Flood));
        let mut limiter = Decoder::new(now);
        for _ in 0..BURST {
            assert!(limiter.take_token(now));
        }
        assert!(!limiter.take_token(now));
        assert!(limiter.take_token(now + Duration::from_millis(20)));
    }

    #[test]
    fn exchanges_a_large_frame_followed_by_coalesced_frames() {
        let (_dir, socket, packages, package) = setup();
        let mut listener = BridgeListener::bind(&socket, &packages, package).unwrap();
        let mut client = UnixStream::connect(&socket).unwrap();
        let mut session = listener.accept().unwrap();
        let large = json!({"t": "large", "text": "x".repeat(MAX_FRAME_SIZE - 100)});
        let large = encode(&object(large)).unwrap();
        let writer = std::thread::spawn(move || {
            client
                .write_all(&[large, frame(), frame()].concat())
                .unwrap();
            client
        });
        assert_eq!(session.read_message().unwrap().unwrap()["t"], "large");
        assert_eq!(session.read_message().unwrap().unwrap()["t"], "hello");
        assert_eq!(session.read_message().unwrap().unwrap()["v"], 1);
        let mut client = writer.join().unwrap();

        session
            .send_message(&object(json!({"t": "hello"})))
            .unwrap();
        let mut header = [0_u8; 4];
        client.read_exact(&mut header).unwrap();
        let mut body = vec![0_u8; u32::from_be_bytes(header) as usize];
        client.read_exact(&mut body).unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&body).unwrap()["t"],
            "hello"
        );
    }

    #[test]
    fn denies_wrong_uid_without_displacing_current_session() {
        let (_dir, socket, packages, package) = setup();
        let mut listener = BridgeListener::bind(&socket, &packages, package).unwrap();
        let _first_client = UnixStream::connect(&socket).unwrap();
        let mut first = listener.accept().unwrap();
        let record = fs::read_to_string(&packages).unwrap();
        fs::write(&packages, "org.marathonos.deckard.bridge 4294967294 0 /\n").unwrap();
        let _denied_client = UnixStream::connect(&socket).unwrap();
        let denied = listener.accept().err().unwrap();
        assert_eq!(denied.kind(), io::ErrorKind::PermissionDenied);
        fs::write(&packages, record).unwrap();
        let _next_client = UnixStream::connect(&socket).unwrap();
        assert_eq!(listener.accept().unwrap().generation(), 2);
        assert!(first.read_message().unwrap().is_none());
    }

    #[test]
    fn rejects_truncated_input_duplicate_records_and_foreign_paths() {
        let (_dir, socket, packages, package) = setup();
        let mut listener = BridgeListener::bind(&socket, &packages, package.clone()).unwrap();
        let mut client = UnixStream::connect(&socket).unwrap();
        let mut session = listener.accept().unwrap();
        client.write_all(&[0, 0, 0, 10, b'{']).unwrap();
        client.shutdown(Shutdown::Write).unwrap();
        let truncated = session.read_message().err().unwrap();
        assert_eq!(truncated.kind(), io::ErrorKind::InvalidData);
        drop(listener);

        let record = fs::read_to_string(&packages).unwrap();
        fs::write(&packages, record.repeat(2)).unwrap();
        let duplicate = package_uid(&packages, &package).err().unwrap();
        assert_eq!(duplicate.kind(), io::ErrorKind::InvalidData);
        fs::write(&socket, "not a socket").unwrap();
        let foreign = BridgeListener::bind(&socket, &packages, package)
            .err()
            .unwrap();
        assert_eq!(foreign.kind(), io::ErrorKind::AlreadyExists);
    }
}
