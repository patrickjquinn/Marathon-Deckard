//! Linux Unix-socket transport for Deckard's three Android bridge channels.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rustix::net::sockopt::socket_peercred;
use serde_json::{Map, Value};

use crate::framing::{self, Decoder};
use crate::ids::PackageName;

const MAX_PACKAGES_LIST_SIZE: u64 = 4 << 20;
const READ_CHUNK_SIZE: usize = 8192;
const WRITE_TIMEOUT: Duration = Duration::from_secs(2);

fn protocol_error(error: crate::Error) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error)
}

fn package_uid(path: &Path, package: &PackageName) -> io::Result<u32> {
    let mut contents = String::new();
    File::open(path)?
        .take(MAX_PACKAGES_LIST_SIZE + 1)
        .read_to_string(&mut contents)?;
    if contents.len() as u64 > MAX_PACKAGES_LIST_SIZE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Android packages.list exceeds limit",
        ));
    }

    let mut uid = None;
    for line in contents.lines() {
        let mut fields = line.split_ascii_whitespace();
        if fields.next() != Some(package.as_str()) {
            continue;
        }
        let parsed = fields
            .next()
            .and_then(|value| value.parse::<u32>().ok())
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "invalid package UID"))?;
        if uid.replace(parsed).is_some() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "duplicate package entry",
            ));
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

    pub fn accept(&mut self) -> io::Result<BridgeSession> {
        let (stream, _) = self.listener.accept()?;
        let actual_uid = socket_peercred(&stream)?.uid.as_raw();
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
            let _ = previous.shutdown(std::net::Shutdown::Both);
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
            let _ = active.shutdown(std::net::Shutdown::Both);
        }
        if fs::symlink_metadata(&self.socket_path)
            .is_ok_and(|metadata| (metadata.dev(), metadata.ino()) == self.socket_identity)
        {
            let _ = fs::remove_file(&self.socket_path);
        }
    }
}

pub struct BridgeSession {
    stream: UnixStream,
    decoder: Decoder,
    generation: u64,
}

impl BridgeSession {
    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.stream.set_read_timeout(timeout)
    }

    /// Drains coalesced frames before reading more bytes. Each socket read
    /// is capped to the decoder's remaining capacity.
    pub fn read_message(&mut self) -> io::Result<Option<Map<String, Value>>> {
        loop {
            match self.decoder.push(&[], Instant::now()) {
                Ok(Some(message)) => return Ok(Some(message)),
                Ok(None) => {}
                Err(error) => {
                    self.close();
                    return Err(protocol_error(error));
                }
            }
            let capacity = self.decoder.remaining_capacity().min(READ_CHUNK_SIZE);
            if capacity == 0 {
                self.close();
                return Err(protocol_error(crate::Error::BufferLimit));
            }
            let mut bytes = [0_u8; READ_CHUNK_SIZE];
            let count = self.stream.read(&mut bytes[..capacity])?;
            if count == 0 {
                self.decoder.finish().map_err(protocol_error)?;
                return Ok(None);
            }
            match self.decoder.push(&bytes[..count], Instant::now()) {
                Ok(Some(message)) => return Ok(Some(message)),
                Ok(None) => {}
                Err(error) => {
                    self.close();
                    return Err(protocol_error(error));
                }
            }
        }
    }

    pub fn send_message(&mut self, message: &Map<String, Value>) -> io::Result<()> {
        send(&mut self.stream, message)
    }

    /// A sender for another thread, so writes need not wait for a read.
    pub fn writer(&self) -> io::Result<BridgeWriter> {
        Ok(BridgeWriter {
            stream: self.stream.try_clone()?,
        })
    }

    pub fn close(&self) {
        let _ = self.stream.shutdown(std::net::Shutdown::Both);
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
    pub fn send_message(&mut self, message: &Map<String, Value>) -> io::Result<()> {
        send(&mut self.stream, message)
    }

    /// Also ends the session's reads.
    pub fn close(&self) {
        let _ = self.stream.shutdown(std::net::Shutdown::Both);
    }
}

/// One bounded frame per call. The deadline applies to the whole frame,
/// including peers that consume a few bytes at a time. A failed write
/// closes the connection.
fn send(stream: &mut UnixStream, message: &Map<String, Value>) -> io::Result<()> {
    let frame = framing::encode(message).map_err(protocol_error)?;
    let result = write_frame(stream, &frame);
    if result.is_err() {
        let _ = stream.shutdown(std::net::Shutdown::Both);
    }
    result
}

fn write_frame(stream: &mut UnixStream, frame: &[u8]) -> io::Result<()> {
    let deadline = Instant::now() + WRITE_TIMEOUT;
    let mut written = 0;
    while written < frame.len() {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .filter(|remaining| !remaining.is_zero())
            .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "bridge write timed out"))?;
        stream.set_write_timeout(Some(remaining))?;
        match stream.write(&frame[written..]) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "bridge peer closed",
                ));
            }
            Ok(count) => written += count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "tests construct local socket fixtures")]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::ErrorKind;

    fn setup() -> (tempfile::TempDir, PathBuf, PathBuf, PackageName) {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("deckard/bridge.sock");
        let packages = dir.path().join("packages.list");
        let package = PackageName::try_from("org.marathonos.deckard.bridge").unwrap();
        fs::write(
            &packages,
            format!(
                "{} {} 0 /data/user/0\n",
                package.as_str(),
                rustix::process::geteuid().as_raw()
            ),
        )
        .unwrap();
        (dir, socket, packages, package)
    }

    fn object() -> Map<String, Value> {
        json!({"t":"hello","v":1}).as_object().unwrap().clone()
    }

    #[test]
    fn authenticates_and_exchanges_coalesced_frames() {
        let (_dir, socket, packages, package) = setup();
        let mut listener = BridgeListener::bind(&socket, &packages, package).unwrap();
        let mut client = UnixStream::connect(&socket).unwrap();
        let mut session = listener.accept().unwrap();
        let frame = framing::encode(&object()).unwrap();
        client.write_all(&[frame.clone(), frame].concat()).unwrap();
        assert_eq!(session.read_message().unwrap().unwrap()["t"], "hello");
        assert_eq!(session.read_message().unwrap().unwrap()["v"], 1);

        session.send_message(&object()).unwrap();
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
        fs::write(
            &packages,
            "org.marathonos.deckard.bridge 4294967294 0 /data/user/0\n",
        )
        .unwrap();
        let _denied_client = UnixStream::connect(&socket).unwrap();
        assert_eq!(
            listener.accept().err().unwrap().kind(),
            ErrorKind::PermissionDenied
        );
        assert_eq!(first.generation(), 1);
        fs::write(
            &packages,
            format!(
                "org.marathonos.deckard.bridge {} 0 /data/user/0\n",
                rustix::process::geteuid().as_raw()
            ),
        )
        .unwrap();
        let _next_client = UnixStream::connect(&socket).unwrap();
        let next = listener.accept().unwrap();
        assert_eq!(next.generation(), 2);
        assert!(first.read_message().unwrap().is_none());
    }

    #[test]
    fn rejects_truncated_and_oversized_input() {
        let (_dir, socket, packages, package) = setup();
        let mut listener = BridgeListener::bind(&socket, &packages, package).unwrap();
        let mut client = UnixStream::connect(&socket).unwrap();
        let mut session = listener.accept().unwrap();
        client.write_all(&[0, 0, 0, 10, b'{']).unwrap();
        client.shutdown(std::net::Shutdown::Write).unwrap();
        assert_eq!(
            session.read_message().err().unwrap().kind(),
            ErrorKind::InvalidData
        );

        let mut next_client = UnixStream::connect(&socket).unwrap();
        let mut next = listener.accept().unwrap();
        next_client
            .write_all(&((framing::MAX_FRAME_SIZE + 1) as u32).to_be_bytes())
            .unwrap();
        assert_eq!(
            next.read_message().err().unwrap().kind(),
            ErrorKind::InvalidData
        );
    }

    #[test]
    fn rejects_duplicate_package_records_and_non_socket_paths() {
        let (_dir, socket, packages, package) = setup();
        let record = format!(
            "{} {} 0 /data/user/0\n",
            package.as_str(),
            rustix::process::geteuid().as_raw()
        );
        fs::write(&packages, record.repeat(2)).unwrap();
        assert_eq!(
            package_uid(&packages, &package).err().unwrap().kind(),
            ErrorKind::InvalidData
        );
        fs::create_dir_all(socket.parent().unwrap()).unwrap();
        fs::write(&socket, "not a socket").unwrap();
        assert_eq!(
            BridgeListener::bind(&socket, &packages, package)
                .err()
                .unwrap()
                .kind(),
            ErrorKind::AlreadyExists
        );
    }
}
