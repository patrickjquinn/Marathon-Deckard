//! The user's folders that are also Android's shared storage (the LXC hook
//! deckard-shared-folders binds them over /data/media/0/...). MediaProvider,
//! in group media_rw, reads and writes them through ACLs; changes made on
//! the host are handed to it to index (`media_scan` to DeckardSystem).

use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::iter;
use std::mem::MaybeUninit;
use std::os::fd::OwnedFd;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use deckard_core::acl;
use deckard_host_tools::shared_folders::MEDIA_RW;
use rustix::fs::inotify::{self, CreateFlags, ReadFlags, WatchFlags};
use serde_json::{Value, json};

use crate::Outbound;

/// How long a folder stays quiet before Android is asked to index it.
pub const SETTLE: Duration = Duration::from_secs(3);
const MAX_PATHS: usize = 64;
const RECENT: Duration = Duration::from_secs(2 * 3 + 2);
const QUEUE: usize = 256;

/// A shared folder: its Android name and its host path, as
/// `deckard_host_tools::shared_folders::host_folders` lists them.
pub type Folder = (&'static str, PathBuf);

/// `/storage/emulated/0/...` for a host path in a folder.
#[must_use]
pub fn android_path(folders: &[Folder], host: &Path) -> Option<String> {
    if host.components().any(|part| part == Component::ParentDir) {
        return None;
    }
    let mut found = folders.iter();
    let (android, rest) =
        found.find_map(|(name, dir)| Some((name, host.strip_prefix(dir).ok()?)))?;
    let rest = rest.to_str()?;
    let separator = if rest.is_empty() { "" } else { "/" };
    Some(format!("/storage/emulated/0/{android}{separator}{rest}"))
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
    /// Without inotify the folders are still indexed when Android connects.
    #[must_use]
    pub fn start(folders: Vec<Folder>, stamp: PathBuf, send: Arc<Outbound>) -> Self {
        let (events, received) = mpsc::sync_channel(QUEUE);
        let inotify = match inotify::init(CreateFlags::CLOEXEC) {
            Ok(inotify) => {
                let (inotify, sender) = (Arc::new(inotify), events.clone());
                let reader = Arc::clone(&inotify);
                thread::spawn(move || read(&reader, &sender));
                Some(inotify)
            }
            Err(error) => {
                eprintln!("deckard-bridge: shared storage isn't watched: {error}");
                None
            }
        };
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
        Self { events }
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
            Err(error) => return eprintln!("deckard-bridge: shared storage watch failed: {error}"),
        };
        let event = match event.events() {
            flags if flags.contains(ReadFlags::QUEUE_OVERFLOW) => Event::Overflow,
            flags if flags.contains(ReadFlags::IGNORED) => Event::Gone(event.wd()),
            _ => Event::Changed(event.wd()),
        };
        if events.send(event).is_err() {
            return;
        }
    }
}

/// `dir`'s entries other than symlinks, with their metadata.
fn entries(dir: &Path) -> Vec<(PathBuf, fs::Metadata)> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let entries = entries.flatten();
    let entries = entries.filter_map(|entry| Some((entry.path(), entry.metadata().ok()?)));
    entries
        .filter(|(_, metadata)| !metadata.is_symlink())
        .collect()
}

fn walk(root: &Path) {
    let mut stack = vec![root.to_owned()];
    while let Some(dir) = stack.pop() {
        for (path, metadata) in entries(&dir) {
            if metadata.is_dir() || metadata.is_file() {
                let _ = acl::grant(&path, MEDIA_RW);
            }
            if metadata.is_dir() {
                stack.push(path);
            }
        }
    }
}

struct Worker {
    folders: Vec<Folder>,
    stamp: PathBuf,
    send: Arc<Outbound>,
    inotify: Option<Arc<OwnedFd>>,
    watched: HashMap<i32, PathBuf>,
    dirty: BTreeSet<PathBuf>,
    connected: bool,
}

impl Worker {
    fn run(mut self, events: &Receiver<Event>) {
        self.prepare();
        let mut settle: Option<Instant> = None;
        loop {
            let first = match settle {
                Some(at) => events.recv_timeout(at.saturating_duration_since(Instant::now())),
                None => events.recv().map_err(|_| RecvTimeoutError::Disconnected),
            };
            let first = match first {
                Ok(event) => event,
                Err(RecvTimeoutError::Timeout) => {
                    settle = None;
                    self.flush();
                    continue;
                }
                Err(RecvTimeoutError::Disconnected) => return,
            };
            // What is already queued is one change per folder, as Qt's watcher had it.
            let batch = iter::once(first).chain(events.try_iter().take(QUEUE));
            let mut changed = BTreeSet::new();
            let mut rescan = false;
            for event in batch {
                let all = self.folders.iter().map(|(_, dir)| dir.clone());
                match event {
                    Event::Changed(wd) => changed.extend(self.watched.get(&wd).cloned()),
                    Event::Gone(wd) => drop(self.watched.remove(&wd)),
                    Event::Overflow => {
                        self.dirty.extend(all);
                        rescan = true;
                    }
                    Event::Connected(connected) => {
                        self.connected = connected;
                        if connected {
                            self.dirty.extend(all);
                            self.flush();
                        }
                    }
                }
            }
            if changed.is_empty() && !rescan {
                continue;
            }
            for dir in changed {
                self.changed(dir);
            }
            self.watch();
            settle = Some(Instant::now() + SETTLE);
        }
    }

    fn prepare(&mut self) {
        let first = !self.stamp.exists();
        let mut ok = true;
        let mut granted = Vec::new();
        for (_, dir) in &self.folders {
            if !dir.is_dir() && fs::create_dir_all(dir).is_err() {
                continue;
            }
            if let Err(error) = acl::grant(dir, MEDIA_RW) {
                let dir = dir.display();
                eprintln!("deckard-bridge: no ACL on {dir} ({error}); Android may not write there");
                ok = false;
            } else if first {
                granted.push(dir);
            }
        }
        for dir in granted {
            walk(dir);
        }
        let parent = self.stamp.parent().unwrap_or(Path::new("/"));
        let write = || fs::create_dir_all(parent).and_then(|()| fs::write(&self.stamp, "1\n"));
        if first
            && ok
            && let Err(error) = write()
        {
            eprintln!("deckard-bridge: shared storage stamp: {error}");
        }
        self.watch();
    }

    fn watch(&mut self) {
        let Some(inotify) = &self.inotify else {
            return;
        };
        let flags = WatchFlags::ATTRIB | WatchFlags::MOVE | WatchFlags::CREATE | WatchFlags::DELETE;
        let flags = flags | WatchFlags::DELETE_SELF | WatchFlags::ONLYDIR | WatchFlags::DONT_FOLLOW;
        let mut dirs = Vec::new();
        for (_, dir) in &self.folders {
            let subfolders = entries(dir)
                .into_iter()
                .filter(|(_, metadata)| metadata.is_dir());
            dirs.push(dir.clone());
            dirs.extend(subfolders.map(|(path, _)| path));
        }
        for dir in dirs {
            if let Ok(wd) = inotify::add_watch(&**inotify, &dir, flags) {
                self.watched.insert(wd, dir);
            }
        }
    }

    /// New entries from the host get the ACL too; what Android made has
    /// the folder's default already.
    fn changed(&mut self, dir: PathBuf) {
        let recent = (SystemTime::now() - RECENT).duration_since(UNIX_EPOCH);
        let recent = recent.map_or(0, |since| since.as_secs());
        for (path, metadata) in entries(&dir) {
            if u64::try_from(metadata.ctime()).is_ok_and(|ctime| ctime >= recent) {
                let _ = acl::grant(&path, MEDIA_RW);
            }
        }
        self.dirty.insert(dir);
    }

    fn flush(&mut self) {
        if !self.connected || self.dirty.is_empty() {
            return;
        }
        let dirty = std::mem::take(&mut self.dirty);
        let paths = dirty
            .iter()
            .filter_map(|dir| android_path(&self.folders, dir));
        let paths: Vec<String> = paths.take(MAX_PATHS).collect();
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
            ("Pictures", "/home/u/Pictures".into()),
            ("Movies", "/home/u/Videos".into()),
        ];
        let path = |p: &str| android_path(&folders, Path::new(p)).unwrap_or_default();
        assert_eq!(path("/home/u/Pictures"), "/storage/emulated/0/Pictures");
        assert_eq!(
            path("/home/u/Videos/a/b.mp4"),
            "/storage/emulated/0/Movies/a/b.mp4"
        );
        assert_eq!(path("/home/u/Pictures/../.ssh"), "");
        assert_eq!(path("/home/u/PicturesX"), "");
    }
}
