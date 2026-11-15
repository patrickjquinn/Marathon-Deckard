//! deckard-camera: hands the host's cameras to Deckard's Android camera HAL
//! (docs/camera.md). libcamera owns the sensors; frames reach Android as
//! dma-bufs over `/data/deckard/camera.sock`.

mod probe;
mod session;
mod wire;

use std::env;
use std::fs::{self, Permissions};
use std::io;
use std::os::fd::OwnedFd;
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use deckard_host_tools::names;
use gstreamer as gst;
use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::net::{
    AddressFamily, SocketAddrUnix, SocketFlags, SocketType, accept_with, bind, listen, socket_with,
    sockopt,
};

/// Android's `AID_CAMERASERVER`, the HAL's UID. The container shares the
/// host's UIDs.
const CAMERASERVER: u32 = 1047;
/// How often the socket's path is checked: an Android data reset removes it.
const CHECK: Duration = Duration::from_secs(5);

type Error = Box<dyn std::error::Error>;

fn data_dir() -> Result<PathBuf, Error> {
    let data_home = dirs::data_dir().ok_or("no XDG data directory")?;
    let data = env::var_os("DECKARD_DATA").map_or_else(
        || data_home.join(names::USER_STATE).join("data"),
        PathBuf::from,
    );
    Ok(data)
}

fn identity(path: &Path) -> Option<(u64, u64)> {
    fs::symlink_metadata(path)
        .ok()
        .map(|metadata| (metadata.dev(), metadata.ino()))
}

/// Binds the socket at `path`, replacing a stale one.
fn listen_at(path: &Path) -> Result<OwnedFd, Error> {
    if fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_socket()) {
        fs::remove_file(path)?;
    }
    let listener = socket_with(
        AddressFamily::UNIX,
        SocketType::SEQPACKET,
        SocketFlags::CLOEXEC,
        None,
    )?;
    bind(&listener, &SocketAddrUnix::new(path)?)?;
    fs::set_permissions(path, Permissions::from_mode(0o666))?;
    listen(&listener, 4)?;
    Ok(listener)
}

/// Serves the socket until its path stops naming it (an Android data reset).
fn serve(listener: &OwnedFd, path: &Path, found: &mut Found) -> Result<(), Error> {
    let ours = identity(path);
    let timeout = Timespec::try_from(CHECK)?;
    loop {
        let mut ready = [PollFd::new(listener, PollFlags::IN)];
        if poll(&mut ready, Some(&timeout))? == 0 {
            if identity(path) != ours {
                return Ok(());
            }
            continue;
        }
        let socket = accept_with(listener, SocketFlags::CLOEXEC)?;
        let uid = sockopt::socket_peercred(&socket)?.uid.as_raw();
        if uid != CAMERASERVER {
            eprintln!("deckard-camera: refused UID {uid}");
            continue;
        }
        // libcamera lists no cameras while another process streams from the
        // shared media device, so that is probed again; cameras it lists
        // without a usable size stay so, and Android asks every 2 s.
        if found.cameras.is_empty() && !found.listed {
            *found = Found::probe();
        }
        let cameras = Arc::clone(&found.cameras);
        thread::spawn(move || {
            if let Err(failure) = session::serve(socket, &cameras)
                && failure.kind() != io::ErrorKind::BrokenPipe
            {
                eprintln!("deckard-camera: connection: {failure}");
            }
        });
    }
}

/// The last probe, shared with the connections.
struct Found {
    cameras: Arc<Vec<probe::Camera>>,
    listed: bool,
}

impl Found {
    fn probe() -> Self {
        let probe = probe::cameras();
        Self {
            cameras: Arc::new(probe.cameras),
            listed: probe.listed,
        }
    }
}

fn main() -> Result<(), Error> {
    let path = data_dir()?.join("deckard/camera.sock");
    gst::init()?;
    let mut found = Found::probe();
    loop {
        if path.parent().is_some_and(Path::is_dir) {
            let listener = listen_at(&path)?;
            eprintln!(
                "deckard-camera: {} camera(s) on {}",
                found.cameras.len(),
                path.display()
            );
            serve(&listener, &path, &mut found)?;
        } else {
            thread::sleep(CHECK);
        }
    }
}
