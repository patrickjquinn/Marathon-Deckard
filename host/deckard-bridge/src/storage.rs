//! The user's folders that are also Android's shared storage (the LXC hook
//! deckard-shared-folders binds them over /data/media/0/...). MediaProvider,
//! in group media_rw, reads and writes them through ACLs; changes made on
//! the host are handed to it to index (`media_scan` to DeckardSystem).

use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::io;
use std::mem::MaybeUninit;
use std::os::fd::OwnedFd;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use deckard_core::acl;
use deckard_host_tools::shared_folders::{self, MEDIA_RW};
use rustix::fs::inotify::{self, CreateFlags, ReadFlags, WatchFlags};
use serde_json::{Value, json};

use crate::Outbound;

/// How long a folder stays quiet before Android is asked to index it.
pub const SETTLE: Duration = Duration::from_secs(3);
pub const MAX_PATHS: usize = 64;
const RECENT: Duration = Duration::from_secs(2 * 3 + 2);
const QUEUE: usize = 256;
const ANDROID_ROOT: &str = "/storage/emulated/0";

/// One shared folder: where it is on the host, and its name in Android.
#[derive(Clone, Debug)]
pub struct Folder {
    pub host: PathBuf,
    pub android: &'static str,
}

/// The XDG user dirs and ~/DCIM, as the hook maps them.
#[must_use]
pub fn user_folders(home: &Path) -> Vec<Folder> {
    let folders = shared_folders::host_folders(home);
    folders
        .into_iter()
        .map(|(android, host)| Folder { host, android })
        .collect()
}

fn clean(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

/// `/storage/emulated/0/...` for a host path in a folder.
#[must_use]
pub fn android_path(folders: &[Folder], host: &Path) -> Option<String> {
    let host = clean(host);
    let (folder, rest) = folders
        .iter()
        .find_map(|folder| Some((folder, host.strip_prefix(&folder.host).ok()?)))?;
    let mut path = Path::new(ANDROID_ROOT).join(folder.android);
    if !rest.as_os_str().is_empty() {
        path.push(rest);
    }
    path.to_str().map(str::to_owned)
}

enum Event {
    Changed(i32),
    Gone(i32),
    Overflow,
    Connected(bool),
}

/// The shared-storage worker; cheap to clone.
#[derive(Clone)]
pub struct SharedStorage {
    events: SyncSender<Event>,
}

impl SharedStorage {
    /// Applies the ACLs (and, once per `stamp`, to everything in the
    /// folders), then watches them and sends `media_scan` with `send`.
    ///
    /// # Errors
    ///
    /// No inotify instance could be made.
    pub fn start(folders: Vec<Folder>, stamp: PathBuf, send: Arc<Outbound>) -> io::Result<Self> {
        let inotify = Arc::new(inotify::init(CreateFlags::CLOEXEC)?);
        let (events, received) = mpsc::sync_channel(QUEUE);
        let reader = (Arc::clone(&inotify), events.clone());
        thread::spawn(move || read(&reader.0, &reader.1));
        let worker = Worker {
            folders,
            stamp,
            send,
            inotify,
            watched: HashMap::new(),
            dirty: BTreeSet::new(),
            connected: false,
        };
        thread::spawn(move || worker.run(&received));
        Ok(Self { events })
    }

    /// DeckardSystem came or went; when it comes, every folder is indexed.
    pub fn set_connected(&self, connected: bool) {
        let _ = self.events.send(Event::Connected(connected));
    }
}

fn read(inotify: &OwnedFd, events: &SyncSender<Event>) {
    let mut buffer = [MaybeUninit::uninit(); 4096];
    let mut reader = inotify::Reader::new(inotify, &mut buffer);
    loop {
        let event = match reader.next() {
            Ok(event) => event,
            Err(rustix::io::Errno::INTR) => continue,
            Err(error) => {
                eprintln!("deckard-bridge: shared storage watch failed: {error}");
                return;
            }
        };
        let flags = event.events();
        let event = if flags.contains(ReadFlags::QUEUE_OVERFLOW) {
            Event::Overflow
        } else if flags.contains(ReadFlags::IGNORED) {
            Event::Gone(event.wd())
        } else {
            Event::Changed(event.wd())
        };
        if events.send(event).is_err() {
            return;
        }
    }
}

fn walk(root: &Path) {
    let mut stack = vec![root.to_owned()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        let entries = entries.flatten();
        for entry in entries {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() || kind.is_file() {
                let _ = acl::grant(&entry.path(), MEDIA_RW);
            }
            if kind.is_dir() {
                stack.push(entry.path());
            }
        }
    }
}

struct Worker {
    folders: Vec<Folder>,
    stamp: PathBuf,
    send: Arc<Outbound>,
    inotify: Arc<OwnedFd>,
    watched: HashMap<i32, PathBuf>,
    dirty: BTreeSet<PathBuf>,
    connected: bool,
}

impl Worker {
    fn run(mut self, events: &Receiver<Event>) {
        self.prepare();
        let mut settle: Option<Instant> = None;
        loop {
            let event = match settle {
                Some(at) => events.recv_timeout(at.saturating_duration_since(Instant::now())),
                None => events.recv().map_err(|_| RecvTimeoutError::Disconnected),
            };
            match event {
                Ok(Event::Changed(wd)) => {
                    if let Some(dir) = self.watched.get(&wd).cloned() {
                        self.changed(dir);
                        settle = Some(Instant::now() + SETTLE);
                    }
                }
                Ok(Event::Gone(wd)) => {
                    self.watched.remove(&wd);
                }
                Ok(Event::Overflow) => {
                    self.dirty
                        .extend(self.folders.iter().map(|f| f.host.clone()));
                    self.watch();
                    settle = Some(Instant::now() + SETTLE);
                }
                Ok(Event::Connected(connected)) => {
                    self.connected = connected;
                    if connected {
                        self.dirty
                            .extend(self.folders.iter().map(|f| f.host.clone()));
                        self.flush();
                    }
                }
                Err(RecvTimeoutError::Timeout) => {
                    settle = None;
                    self.flush();
                }
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
    }

    fn prepare(&mut self) {
        let first = !self.stamp.exists();
        let mut ok = true;
        for folder in &self.folders {
            if !folder.host.is_dir() && fs::create_dir_all(&folder.host).is_err() {
                continue;
            }
            if let Err(error) = acl::grant(&folder.host, MEDIA_RW) {
                let host = folder.host.display();
                eprintln!(
                    "deckard-bridge: no ACL on {host} ({error}); Android may not write there"
                );
                ok = false;
                continue;
            }
            if first {
                walk(&folder.host);
            }
        }
        if first && ok {
            let parent = self.stamp.parent().unwrap_or(Path::new("/"));
            if let Err(error) =
                fs::create_dir_all(parent).and_then(|()| fs::write(&self.stamp, "1\n"))
            {
                eprintln!("deckard-bridge: shared storage stamp: {error}");
            }
        }
        self.watch();
    }

    fn watch(&mut self) {
        let flags = WatchFlags::ATTRIB
            | WatchFlags::MOVE
            | WatchFlags::CREATE
            | WatchFlags::DELETE
            | WatchFlags::DELETE_SELF
            | WatchFlags::ONLYDIR
            | WatchFlags::DONT_FOLLOW;
        let mut dirs = Vec::new();
        for folder in &self.folders {
            dirs.push(folder.host.clone());
            let Ok(entries) = fs::read_dir(&folder.host) else {
                continue;
            };
            let entries = entries.flatten();
            for entry in entries {
                if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                    dirs.push(entry.path());
                }
            }
        }
        for dir in dirs {
            if let Ok(wd) = inotify::add_watch(&*self.inotify, &dir, flags) {
                self.watched.insert(wd, dir);
            }
        }
    }

    /// New entries from the host get the ACL too; what Android made has
    /// the folder's default already.
    fn changed(&mut self, dir: PathBuf) {
        let recent = SystemTime::now() - RECENT;
        let recent = recent
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        if let Ok(entries) = fs::read_dir(&dir) {
            let entries = entries.flatten();
            for entry in entries {
                let Ok(metadata) = entry.metadata() else {
                    continue;
                };
                let ctime = u64::try_from(metadata.ctime()).unwrap_or(0);
                if !metadata.is_symlink() && ctime >= recent {
                    let _ = acl::grant(&entry.path(), MEDIA_RW);
                }
            }
        }
        self.dirty.insert(dir);
        self.watch();
    }

    fn flush(&mut self) {
        if !self.connected || self.dirty.is_empty() {
            return;
        }
        let dirty = std::mem::take(&mut self.dirty);
        let mut paths = Vec::new();
        for dir in &dirty {
            if let Some(path) = android_path(&self.folders, dir)
                && paths.len() < MAX_PATHS
            {
                paths.push(path);
            }
        }
        if paths.is_empty() {
            return;
        }
        if let Value::Object(message) = json!({"t": "media_scan", "paths": paths}) {
            (self.send)(message);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_to_android_paths() {
        let folders = [
            Folder {
                host: "/home/u/Pictures".into(),
                android: "Pictures",
            },
            Folder {
                host: "/home/u/Videos".into(),
                android: "Movies",
            },
        ];
        let path = |p: &str| android_path(&folders, Path::new(p));
        assert_eq!(
            path("/home/u/Pictures").as_deref(),
            Some("/storage/emulated/0/Pictures")
        );
        assert_eq!(
            path("/home/u/Videos/a/b.mp4").as_deref(),
            Some("/storage/emulated/0/Movies/a/b.mp4")
        );
        assert_eq!(path("/home/u/Pictures/../.ssh"), None);
        assert_eq!(path("/home/u/PicturesX"), None);
    }
}
