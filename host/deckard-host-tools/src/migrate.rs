//! Moves Android from Waydroid's host names to Deckard's, or back: a
//! journaled transaction whose steps are each idempotent, so an interrupted
//! run resumes where it stopped.
//!
//! As root it moves the system state, then runs itself as each user with
//! Android state; as a user it moves that user's state and the Shell's. Data
//! moves by renaming within its parent directory, so ownership, modes, xattrs
//! and ACLs stay with the inode.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

use crate::{files, names};

const JOURNAL_VERSION: u32 = 2;
/// Version 1 journals predate the Android image's own rename: a complete one
/// resumes to take [`Step::Names`] and [`Step::Properties`].
const OLDEST_JOURNAL_VERSION: u32 = 1;
const SYSTEM_JOURNAL: &str = "/var/lib/deckard-migration.json";
const LEGACY_WORK: &str = "/var/lib/waydroid";
const LEGACY_CONTAINER: &str = "waydroid";
const LEGACY_BRIDGE: &str = "waydroid0";
const LEGACY_RUN: &str = "/run/waydroid-lxc";
const LEGACY_APP_ID_PREFIX: &str = "waydroid.";
const LEGACY_USER_STATE: &str = "waydroid";
const SHELL: &str = "/usr/bin/marathon-shell-bin";
/// The instance's files in the work directory, by their legacy name.
const INSTANCE_FILES: [(&str, &str); 3] = [
    ("waydroid.cfg", "deckard.cfg"),
    ("waydroid_base.prop", "deckard_base.prop"),
    ("waydroid.prop", "deckard.prop"),
];
const LEGACY_SECCOMP: &str = "waydroid.seccomp";
const SECCOMP: &str = "deckard.seccomp";
/// Android property prefixes, legacy first; the longest match wins.
const PROPERTY_PREFIXES: [(&str, &str); 4] = [
    ("ro.product.waydroid.", "ro.product.deckard."),
    ("persist.waydroid.", "persist.deckard."),
    ("ro.waydroid.", "ro.deckard."),
    ("waydroid.", "deckard."),
];
/// Files a start remakes, by their path in the tree: tmpfiles' seed and the
/// bridge's copies of the Shell's state.
const REMADE: [&str; 4] = [
    "waydroid.cfg",
    "deckard.cfg",
    "data/deckard/doze",
    "data/deckard/palette",
];
const SYSTEM_PROCESSES: [&str; 1] = ["/usr/libexec/deckard-containerd"];
const USER_PROCESSES: [&str; 3] = [
    SHELL,
    "/usr/libexec/deckard-bridge",
    "/usr/libexec/deckard-runtime",
];

/// Which way the names move.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    /// From Waydroid's names to Deckard's.
    Forward,
    /// From Deckard's names back to Waydroid's.
    Rollback,
}

/// Whose state a run moves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    /// `/var/lib`, then every user's, each as that user.
    System,
    /// The calling user's.
    User,
}

/// A step of the transaction, as the journal records it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum Step {
    Work,
    Lxc,
    Names,
    LxcConfig,
    Config,
    Properties,
    Data,
    Desktop,
    Shell,
}

/// What a step does.
enum Action {
    /// Renames a tree within its directory.
    Move(PathBuf, PathBuf),
    /// Renames files, each replacing a copy a start seeded under its new name.
    Rename(Vec<(PathBuf, PathBuf)>),
    /// Rewrites each file through the map.
    Rewrite(Vec<PathBuf>, fn(&str, Direction) -> String),
    /// Moves the desktop entries in this home.
    Entries(PathBuf),
    /// Has the Shell move its stores.
    ShellIds,
}

/// A moved tree's identity: the same inode means its contents moved intact.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Root {
    dev: u64,
    ino: u64,
    uid: u32,
    gid: u32,
    mode: u32,
}

#[derive(Debug, Serialize, Deserialize)]
struct Journal {
    version: u32,
    direction: Direction,
    /// Steps finished in `direction`, in order.
    done: Vec<Step>,
    complete: bool,
    /// Each moved tree, by its step.
    roots: BTreeMap<Step, Root>,
    /// Android packages and their UIDs, from `packages.list`.
    apps: BTreeMap<String, u32>,
}

/// `(old, new)` for `direction`.
fn pair<'a>(direction: Direction, legacy: &'a str, deckard: &'a str) -> (&'a str, &'a str) {
    match direction {
        Direction::Forward => (legacy, deckard),
        Direction::Rollback => (deckard, legacy),
    }
}

/// `value` under the first `from` of `map` moved under its `to`.
fn map_path(value: &str, map: &[(String, String)]) -> Option<String> {
    for (from, to) in map {
        if let Some(rest) = value.strip_prefix(from.as_str())
            && (rest.is_empty() || rest.starts_with('/'))
        {
            return Some(format!("{to}{rest}"));
        }
    }
    None
}

fn system_paths(direction: Direction) -> Vec<(String, String)> {
    let lxc = |work: &str, name: &str| format!("{work}/lxc/{name}");
    let forward = [
        (
            lxc(LEGACY_WORK, LEGACY_CONTAINER),
            lxc(names::WORK, names::CONTAINER),
        ),
        (LEGACY_WORK.to_owned(), names::WORK.to_owned()),
        (LEGACY_RUN.to_owned(), names::RUN.to_owned()),
    ];
    let mut map = Vec::new();
    for (legacy, deckard) in forward {
        let (from, to) = pair(direction, &legacy, &deckard);
        map.push((from.to_owned(), to.to_owned()));
    }
    map
}

/// An LXC configuration with its paths, name and bridge moved.
fn map_lxc(text: &str, direction: Direction) -> String {
    let paths = system_paths(direction);
    let (from_name, to_name) = pair(direction, LEGACY_CONTAINER, names::CONTAINER);
    let (from_bridge, to_bridge) = pair(direction, LEGACY_BRIDGE, names::BRIDGE);
    let mut result = String::new();
    for line in text.lines() {
        let mapped = line.split_once('=').and_then(|(key, value)| {
            let (key, value) = (key.trim(), value.trim());
            let value = match key {
                "lxc.rootfs.path" | "lxc.include" => map_path(value, &paths)?,
                "lxc.seccomp.profile" => {
                    let value = map_path(value, &paths).unwrap_or_else(|| value.to_owned());
                    let (from, to) = pair(direction, LEGACY_SECCOMP, SECCOMP);
                    match value.strip_suffix(from) {
                        Some(directory) => format!("{directory}{to}"),
                        None => value,
                    }
                }
                "lxc.mount.entry" => {
                    let (source, rest) = value.split_once(' ')?;
                    format!("{} {rest}", map_path(source, &paths)?)
                }
                "lxc.uts.name" if value == from_name => to_name.to_owned(),
                "lxc.net.0.link" if value == from_bridge => to_bridge.to_owned(),
                _ => return None,
            };
            Some(format!("{key} = {value}"))
        });
        result.push_str(mapped.as_deref().unwrap_or(line));
        result.push('\n');
    }
    result
}

/// `waydroid.cfg` with `images_path` moved.
fn map_config(text: &str, direction: Direction) -> String {
    let paths = system_paths(direction);
    let mut result = String::new();
    for line in text.lines() {
        let mapped = line.split_once('=').and_then(|(key, value)| {
            (key.trim() == "images_path")
                .then(|| map_path(value.trim(), &paths))
                .flatten()
                .map(|value| format!("images_path = {value}"))
        });
        result.push_str(mapped.as_deref().unwrap_or(line));
        result.push('\n');
    }
    result
}

/// An instance file with its `[waydroid]` section and Android property names
/// moved.
fn map_properties(text: &str, direction: Direction) -> String {
    let (from_section, to_section) = pair(direction, "[waydroid]", "[deckard]");
    let mut result = String::new();
    for line in text.lines() {
        let trimmed = line.trim_start();
        let mapped = if trimmed.eq_ignore_ascii_case(from_section) {
            Some(to_section.to_owned())
        } else {
            PROPERTY_PREFIXES.iter().find_map(|&(legacy, deckard)| {
                let (from, to) = pair(direction, legacy, deckard);
                let rest = trimmed.strip_prefix(from)?;
                Some(format!("{}{to}{rest}", &line[..line.len() - trimmed.len()]))
            })
        };
        result.push_str(mapped.as_deref().unwrap_or(line));
        result.push('\n');
    }
    result
}

/// Renames `from` over `to`: the legacy file is the instance's state, a `to`
/// beside it only what a start seeded.
fn rename_over(from: &Path, to: &Path) -> io::Result<()> {
    if files::found(fs::symlink_metadata(from))?.is_none() {
        return Ok(());
    }
    fs::rename(from, to)?;
    fs::File::open(to.parent().unwrap_or(Path::new("/")))?.sync_all()
}

/// Whether the tree at `path` holds only what a start makes: directories,
/// sockets and [`REMADE`].
fn fresh(root: &Path, path: &Path) -> io::Result<bool> {
    let kind = fs::symlink_metadata(path)?.file_type();
    if !kind.is_dir() {
        let remade = kind.is_file() && REMADE.iter().any(|name| root.join(name) == path);
        return Ok(remade || kind.is_socket());
    }
    for entry in fs::read_dir(path)? {
        if !fresh(root, &entry?.path())? {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Renames `from` to `to`; done when only `to` exists. A fresh `to`, made by
/// a start while an earlier move had failed, gives way.
fn rename(from: &Path, to: &Path) -> io::Result<()> {
    let source = files::found(fs::symlink_metadata(from))?;
    let target = files::found(fs::symlink_metadata(to))?;
    match (source, target) {
        (None, _) => return Ok(()),
        (Some(_), Some(_)) if fresh(to, to)? => fs::remove_dir_all(to)?,
        (Some(_), Some(_)) => {
            let message = format!("both {} and {} exist", from.display(), to.display());
            return Err(io::Error::other(message));
        }
        (Some(_), None) => {}
    }
    fs::rename(from, to)?;
    fs::File::open(to.parent().unwrap_or(Path::new("/")))?.sync_all()
}

/// Rewrites `path` through `map` when it exists and changes.
fn rewrite(path: &Path, map: impl Fn(&str) -> String) -> io::Result<()> {
    let Some(metadata) = files::found(fs::symlink_metadata(path))? else {
        return Ok(());
    };
    let text = fs::read_to_string(path)?;
    let next = map(&text);
    if next == text {
        return Ok(());
    }
    files::replace(
        path,
        next.as_bytes(),
        metadata.permissions().mode() & 0o7777,
    )
}

fn root(path: &Path) -> io::Result<Option<Root>> {
    Ok(files::found(fs::symlink_metadata(path))?.map(|m| Root {
        dev: m.dev(),
        ino: m.ino(),
        uid: m.uid(),
        gid: m.gid(),
        mode: m.mode(),
    }))
}

/// The first of `executables` running, as far as this process can see.
fn running(executables: &[&str]) -> Option<String> {
    for entry in fs::read_dir("/proc").ok()?.flatten() {
        let Ok(exe) = fs::read_link(entry.path().join("exe")) else {
            continue;
        };
        if let Some(found) = executables.iter().find(|wanted| exe == Path::new(wanted)) {
            return Some((*found).to_owned());
        }
    }
    None
}

fn android_apps(data: &Path) -> BTreeMap<String, u32> {
    let text = fs::read_to_string(data.join("system/packages.list")).unwrap_or_default();
    let mut apps = BTreeMap::new();
    for line in text.lines() {
        let mut fields = line.split(' ');
        if let (Some(name), Some(Ok(uid))) = (fields.next(), fields.next().map(str::parse)) {
            apps.insert(name.to_owned(), uid);
        }
    }
    apps
}

/// One scope's places and steps.
struct Plan {
    journal: PathBuf,
    /// Steps in forward order.
    steps: Vec<(Step, Action)>,
    /// Android's data before and after the move.
    data: Option<(PathBuf, PathBuf)>,
}

impl Plan {
    fn system(direction: Direction) -> Self {
        let (from, to) = pair(direction, LEGACY_WORK, names::WORK);
        let lxc = Path::new(names::WORK).join("lxc");
        let (from_name, to_name) = pair(direction, LEGACY_CONTAINER, names::CONTAINER);
        let instance = lxc.join(names::CONTAINER);
        let mut lxc_files = Vec::new();
        for file in ["config", "config_nodes", "config_session"] {
            lxc_files.push(instance.join(file));
        }
        let work = Path::new(names::WORK);
        let mut renames = Vec::new();
        for (legacy, deckard) in INSTANCE_FILES
            .into_iter()
            .chain([(LEGACY_SECCOMP, SECCOMP)])
        {
            let (from, to) = pair(direction, legacy, deckard);
            let directory = if legacy == LEGACY_SECCOMP {
                &instance
            } else {
                work
            };
            renames.push((directory.join(from), directory.join(to)));
        }
        // Deckard's names, which both directions find when these run.
        let properties = vec![
            names::CONFIG.into(),
            work.join(INSTANCE_FILES[1].1),
            work.join("deckard-props.list"),
        ];
        Self {
            journal: PathBuf::from(SYSTEM_JOURNAL),
            steps: vec![
                (Step::Work, Action::Move(from.into(), to.into())),
                (
                    Step::Lxc,
                    Action::Move(lxc.join(from_name), lxc.join(to_name)),
                ),
                (Step::Names, Action::Rename(renames)),
                (Step::LxcConfig, Action::Rewrite(lxc_files, map_lxc)),
                (
                    Step::Config,
                    Action::Rewrite(vec![names::CONFIG.into()], map_config),
                ),
                (
                    Step::Properties,
                    Action::Rewrite(properties, map_properties),
                ),
            ],
            data: None,
        }
    }

    fn user(direction: Direction, home: &Path) -> Self {
        let share = home.join(".local/share");
        let (from, to) = pair(direction, LEGACY_USER_STATE, names::USER_STATE);
        let data = (share.join(from).join("data"), share.join(to).join("data"));
        Self {
            journal: home.join(".local/state/deckard-migration.json"),
            steps: vec![
                (Step::Data, Action::Move(share.join(from), share.join(to))),
                (Step::Desktop, Action::Entries(home.to_owned())),
                (Step::Shell, Action::ShellIds),
            ],
            data: Some(data),
        }
    }
}

fn load(path: &Path) -> io::Result<Option<Journal>> {
    let Some(text) = files::found(fs::read(path))? else {
        return Ok(None);
    };
    let journal: Journal = serde_json::from_slice(&text).map_err(io::Error::other)?;
    if !(OLDEST_JOURNAL_VERSION..=JOURNAL_VERSION).contains(&journal.version) {
        return Err(io::Error::other("unknown migration journal version"));
    }
    Ok(Some(journal))
}

fn save(path: &Path, journal: &Journal) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let text = serde_json::to_vec_pretty(journal).map_err(io::Error::other)?;
    files::replace(path, &text, 0o600)
}

/// The desktop entries of Android apps under the other prefix, with their
/// icons moved with the user's state.
fn move_entries(home: &Path, direction: Direction) -> io::Result<()> {
    let applications = home.join(".local/share/applications");
    let Some(entries) = files::found(fs::read_dir(&applications))? else {
        return Ok(());
    };
    let (from, to) = pair(direction, LEGACY_APP_ID_PREFIX, names::APP_ID_PREFIX);
    let share = home.join(".local/share");
    let (old, new) = pair(direction, LEGACY_USER_STATE, names::USER_STATE);
    let icons = [(
        share.join(old).to_string_lossy().into_owned(),
        share.join(new).to_string_lossy().into_owned(),
    )];
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let Some(package) = name
            .to_str()
            .and_then(|name| name.strip_prefix(from)?.strip_suffix(".desktop"))
        else {
            continue;
        };
        if !entry.file_type()?.is_file() {
            continue;
        }
        let mut text = String::new();
        for line in fs::read_to_string(entry.path())?.lines() {
            let icon = line.strip_prefix("Icon=").and_then(|v| map_path(v, &icons));
            match icon {
                Some(icon) => text.push_str(&format!("Icon={icon}")),
                None => text.push_str(line),
            }
            text.push('\n');
        }
        let mode = entry.metadata()?.permissions().mode() & 0o7777;
        files::replace(
            &applications.join(format!("{to}{package}.desktop")),
            text.as_bytes(),
            mode,
        )?;
        fs::remove_file(entry.path())?;
    }
    fs::File::open(&applications)?.sync_all()
}

/// The Shell's own stores, which only the Shell can read.
fn move_shell_ids(direction: Direction) -> io::Result<()> {
    if !Path::new(SHELL).exists() {
        return Ok(());
    }
    let (from, to) = pair(direction, LEGACY_APP_ID_PREFIX, names::APP_ID_PREFIX);
    let status = Command::new(SHELL)
        .args(["--rename-app-ids", from, to])
        .status()?;
    if !status.success() {
        return Err(io::Error::other(format!(
            "the Shell's id rename failed: {status}"
        )));
    }
    Ok(())
}

fn check_stopped(scope: Scope) -> io::Result<()> {
    let busy = match scope {
        Scope::System => {
            let cgroups = ["lxc.payload.waydroid", "lxc.payload.deckard"];
            let live = cgroups
                .iter()
                .any(|name| Path::new("/sys/fs/cgroup").join(name).exists());
            let bridges = [LEGACY_BRIDGE, names::BRIDGE];
            let up = bridges
                .iter()
                .any(|name| Path::new("/sys/class/net").join(name).exists());
            running(&SYSTEM_PROCESSES)
                .or_else(|| live.then(|| "an Android container".to_owned()))
                .or_else(|| up.then(|| "Android's network".to_owned()))
        }
        Scope::User => running(&USER_PROCESSES),
    };
    match busy {
        Some(what) => Err(io::Error::other(format!(
            "{what} is running; stop it first"
        ))),
        None => Ok(()),
    }
}

/// Runs one scope's transaction in `direction`, resuming its journal.
fn transact(direction: Direction, home: Option<&Path>) -> io::Result<()> {
    let plan = match home {
        Some(home) => Plan::user(direction, home),
        None => Plan::system(direction),
    };
    let previous = load(&plan.journal)?;
    if previous
        .as_ref()
        .is_some_and(|j| j.complete && j.direction == direction && j.version == JOURNAL_VERSION)
    {
        return Ok(());
    }
    check_stopped(if home.is_some() {
        Scope::User
    } else {
        Scope::System
    })?;
    let mut journal = match previous {
        Some(mut journal) if journal.direction == direction => {
            if journal.version < JOURNAL_VERSION {
                journal.version = JOURNAL_VERSION;
                journal.complete = false;
                if let Some((from, to)) = &plan.data {
                    journal.apps = android_apps(if from.exists() { from } else { to });
                }
            }
            journal
        }
        _ => Journal {
            version: JOURNAL_VERSION,
            direction,
            done: Vec::new(),
            complete: false,
            roots: BTreeMap::new(),
            apps: plan.data.as_ref().map_or_else(BTreeMap::new, |(from, to)| {
                android_apps(if from.exists() { from } else { to })
            }),
        },
    };
    save(&plan.journal, &journal)?;
    let mut order: Vec<&(Step, Action)> = plan.steps.iter().collect();
    if direction == Direction::Rollback {
        order.reverse();
    }
    for (step, action) in order {
        if journal.done.contains(step) {
            continue;
        }
        match action {
            Action::Move(from, to) => {
                if !journal.roots.contains_key(step)
                    && let Some(found) = root(from)?
                {
                    journal.roots.insert(*step, found);
                    save(&plan.journal, &journal)?;
                }
                rename(from, to)?;
                let arrived = root(to)?;
                let intact = journal
                    .roots
                    .get(step)
                    .is_none_or(|moved| arrived.as_ref() == Some(moved));
                if root(from)?.is_some() || !intact {
                    let message =
                        format!("{} did not move intact to {}", from.display(), to.display());
                    return Err(io::Error::other(message));
                }
            }
            Action::Rewrite(paths, map) => {
                for path in paths {
                    rewrite(path, |text| map(text, direction))?;
                }
            }
            Action::Rename(pairs) => {
                for (from, to) in pairs {
                    rename_over(from, to)?;
                }
            }
            Action::Entries(home) => move_entries(home, direction)?,
            Action::ShellIds => move_shell_ids(direction)?,
        }
        journal.done.push(*step);
        save(&plan.journal, &journal)?;
    }
    if let Some((_, data)) = &plan.data
        && android_apps(data) != journal.apps
    {
        return Err(io::Error::other("Android's app list changed in the move"));
    }
    journal.complete = true;
    save(&plan.journal, &journal)
}

/// Users who may have Android state: (uid, gid, home).
fn users() -> io::Result<Vec<(u32, u32, PathBuf)>> {
    let mut users = Vec::new();
    for line in fs::read_to_string("/etc/passwd")?.lines() {
        let fields: Vec<&str> = line.split(':').collect();
        let (Some(uid), Some(gid), Some(home)) = (fields.get(2), fields.get(3), fields.get(5))
        else {
            continue;
        };
        let (Ok(uid), Ok(gid)) = (uid.parse::<u32>(), gid.parse::<u32>()) else {
            continue;
        };
        let home = PathBuf::from(home);
        let share = home.join(".local/share");
        let state = [LEGACY_USER_STATE, names::USER_STATE]
            .iter()
            .any(|name| share.join(name).exists());
        if (1000..65534).contains(&uid) && home.is_absolute() && state {
            users.push((uid, gid, home));
        }
    }
    Ok(users)
}

/// Runs the transaction in `direction` for `scope`: as root the system's,
/// then each user's as that user.
///
/// # Errors
/// Android or the Shell is running, two names are both taken, a step fails,
/// or a tree did not arrive intact. The journal keeps the finished steps.
pub fn run(direction: Direction, scope: Scope) -> io::Result<()> {
    if scope == Scope::User {
        let home = std::env::var_os("HOME").ok_or_else(|| io::Error::other("HOME is not set"))?;
        return transact(direction, Some(Path::new(&home)));
    }
    if direction == Direction::Forward {
        transact(direction, None)?;
    }
    let exe = std::env::current_exe()?;
    let argument = match direction {
        Direction::Forward => "forward",
        Direction::Rollback => "rollback",
    };
    for (uid, gid, home) in users()? {
        let status = Command::new(&exe)
            .args([argument, "--user"])
            .env_clear()
            .env("HOME", &home)
            .env("PATH", "/usr/bin:/bin")
            .current_dir(&home)
            .uid(uid)
            .gid(gid)
            .status()?;
        if !status.success() {
            let message = format!("migration for uid {uid} failed: {status}");
            return Err(io::Error::other(message));
        }
    }
    if direction == Direction::Rollback {
        transact(direction, None)?;
    }
    Ok(())
}
