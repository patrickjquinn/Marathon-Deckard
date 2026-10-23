//! One Android MediaSession per MPRIS2 bus connection.

use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::io;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, mpsc::Sender};
use std::time::Instant;

use deckard_core::messages::{self, Media};
use image::ImageFormat;
use serde_json::{Map, Value, json};
use zbus::blocking::{Connection, connection::Builder};
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Str, Value as DbusValue};

const PATH: &str = "/org/mpris/MediaPlayer2";
const PLAYER: &str = "org.mpris.MediaPlayer2.Player";
const PROPERTIES: &str = "org.freedesktop.DBus.Properties";
const MAX_PLAYERS: usize = 8;
static ART_SERIAL: AtomicU64 = AtomicU64::new(0);

pub type BridgeCommand = Value;

#[derive(Debug)]
pub enum Error {
    InvalidMedia(messages::MessageError),
    TooManyPlayers,
    Bus(zbus::Error),
    Io(io::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidMedia(error) => write!(f, "invalid media message: {error:?}"),
            Self::TooManyPlayers => write!(f, "at most eight media players are supported"),
            Self::Bus(error) => write!(f, "D-Bus error: {error}"),
            Self::Io(error) => write!(f, "I/O error: {error}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<zbus::Error> for Error {
    fn from(error: zbus::Error) -> Self {
        Self::Bus(error)
    }
}

impl From<io::Error> for Error {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

struct State {
    media: Media,
    track: u64,
    since: Instant,
    art_path: Option<PathBuf>,
}

impl State {
    fn position_us(&self) -> i64 {
        let mut position = self.media.position_ms;
        if self.media.status == "Playing" {
            let elapsed = self.since.elapsed().as_millis() as f64 * self.media.speed;
            position = position.saturating_add(elapsed as i64);
        }
        if self.media.duration_ms > 0 {
            position = position.min(self.media.duration_ms);
        }
        position.max(0).saturating_mul(1000)
    }

    fn can_seek(&self) -> bool {
        self.media.can_seek && self.media.duration_ms > 0
    }

    fn rate(&self) -> f64 {
        if self.media.speed > 0.0 {
            self.media.speed
        } else {
            1.0
        }
    }

    fn track_id(&self) -> String {
        format!(
            "/org/marathonos/deckard/session{}/track{}",
            self.media.session_id, self.track
        )
    }

    fn metadata(&self) -> HashMap<String, OwnedValue> {
        let mut metadata = HashMap::new();
        if let Ok(track) = OwnedObjectPath::try_from(self.track_id())
            && let Ok(value) = OwnedValue::try_from(DbusValue::from(track))
        {
            metadata.insert("mpris:trackid".into(), value);
        }
        metadata.insert(
            "xesam:title".into(),
            OwnedValue::from(Str::from(self.media.title.clone())),
        );
        if self.media.duration_ms > 0 {
            metadata.insert(
                "mpris:length".into(),
                OwnedValue::from(self.media.duration_ms.saturating_mul(1000)),
            );
        }
        if !self.media.artist.is_empty()
            && let Ok(value) =
                OwnedValue::try_from(DbusValue::from(vec![self.media.artist.clone()]))
        {
            metadata.insert("xesam:artist".into(), value);
        }
        if !self.media.album.is_empty() {
            metadata.insert(
                "xesam:album".into(),
                OwnedValue::from(Str::from(self.media.album.clone())),
            );
        }
        if let Some(path) = &self.art_path {
            metadata.insert(
                "mpris:artUrl".into(),
                OwnedValue::from(Str::from(format!("file://{}", path.display()))),
            );
        }
        metadata
    }
}

fn lock(state: &Arc<Mutex<State>>) -> MutexGuard<'_, State> {
    state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

struct Root(Arc<Mutex<State>>);

#[zbus::interface(name = "org.mpris.MediaPlayer2")]
impl Root {
    #[zbus(name = "Raise")]
    fn raise(&self) {}

    #[zbus(name = "Quit")]
    fn quit(&self) {}

    #[zbus(property)]
    fn can_quit(&self) -> bool {
        false
    }

    #[zbus(property)]
    fn can_raise(&self) -> bool {
        false
    }

    #[zbus(property)]
    fn has_track_list(&self) -> bool {
        false
    }

    #[zbus(property)]
    fn identity(&self) -> String {
        lock(&self.0).media.label.clone()
    }

    #[zbus(property)]
    fn desktop_entry(&self) -> String {
        format!("waydroid.{}", lock(&self.0).media.package.as_str())
    }

    #[zbus(property)]
    fn supported_uri_schemes(&self) -> Vec<String> {
        Vec::new()
    }

    #[zbus(property)]
    fn supported_mime_types(&self) -> Vec<String> {
        Vec::new()
    }
}

struct Player {
    state: Arc<Mutex<State>>,
    commands: Sender<BridgeCommand>,
}

impl Player {
    fn send(&self, command: &str, position_ms: i64) {
        let sid = lock(&self.state).media.session_id;
        let _ = self.commands.send(json!({
            "t": "media_cmd", "sid": sid, "cmd": command, "position": position_ms
        }));
    }
}

#[zbus::interface(name = "org.mpris.MediaPlayer2.Player")]
impl Player {
    #[zbus(name = "Next")]
    fn next(&self) {
        self.send("next", 0);
    }

    #[zbus(name = "Previous")]
    fn previous(&self) {
        self.send("previous", 0);
    }

    #[zbus(name = "Pause")]
    fn pause(&self) {
        self.send("pause", 0);
    }

    #[zbus(name = "PlayPause")]
    fn play_pause(&self) {
        self.send("playpause", 0);
    }

    #[zbus(name = "Stop")]
    fn stop(&self) {
        self.send("stop", 0);
    }

    #[zbus(name = "Play")]
    fn play(&self) {
        self.send("play", 0);
    }

    #[zbus(name = "Seek")]
    fn seek(&self, offset_us: i64) {
        let state = lock(&self.state);
        let position = state
            .can_seek()
            .then(|| state.position_us().saturating_add(offset_us).max(0) / 1000);
        drop(state);
        if let Some(position) = position {
            self.send("seek", position);
        }
    }

    #[zbus(name = "SetPosition")]
    fn set_position(&self, track: OwnedObjectPath, position_us: i64) {
        let state = lock(&self.state);
        let allowed = state.can_seek() && track.as_str() == state.track_id() && position_us >= 0;
        drop(state);
        if allowed {
            self.send("seek", position_us / 1000);
        }
    }

    #[zbus(name = "OpenUri")]
    fn open_uri(&self, _uri: &str) {}

    #[zbus(signal)]
    async fn seeked(
        ctxt: &zbus::object_server::SignalEmitter<'_>,
        position_us: i64,
    ) -> zbus::Result<()>;

    #[zbus(property)]
    fn playback_status(&self) -> String {
        lock(&self.state).media.status.clone()
    }

    #[zbus(property)]
    fn rate(&self) -> f64 {
        lock(&self.state).rate()
    }

    #[zbus(property)]
    fn set_rate(&mut self, _rate: f64) {}

    #[zbus(property)]
    fn metadata(&self) -> HashMap<String, OwnedValue> {
        lock(&self.state).metadata()
    }

    #[zbus(property)]
    fn volume(&self) -> f64 {
        1.0
    }

    #[zbus(property)]
    fn set_volume(&mut self, _volume: f64) {}

    #[zbus(property)]
    fn position(&self) -> i64 {
        lock(&self.state).position_us()
    }

    #[zbus(property)]
    fn minimum_rate(&self) -> f64 {
        lock(&self.state).rate()
    }

    #[zbus(property)]
    fn maximum_rate(&self) -> f64 {
        lock(&self.state).rate()
    }

    #[zbus(property)]
    fn can_go_next(&self) -> bool {
        lock(&self.state).media.can_next
    }

    #[zbus(property)]
    fn can_go_previous(&self) -> bool {
        lock(&self.state).media.can_previous
    }

    #[zbus(property)]
    fn can_play(&self) -> bool {
        lock(&self.state).media.can_play
    }

    #[zbus(property)]
    fn can_pause(&self) -> bool {
        lock(&self.state).media.can_pause
    }

    #[zbus(property)]
    fn can_seek(&self) -> bool {
        lock(&self.state).can_seek()
    }

    #[zbus(property)]
    fn can_control(&self) -> bool {
        true
    }
}

struct ActivePlayer {
    connection: Connection,
    state: Arc<Mutex<State>>,
}

impl Drop for ActivePlayer {
    fn drop(&mut self) {
        if let Some(path) = &lock(&self.state).art_path {
            let _ = fs::remove_file(path);
        }
    }
}

pub struct MprisAdapter {
    address: Option<String>,
    art_dir: PathBuf,
    commands: Sender<BridgeCommand>,
    players: HashMap<i32, ActivePlayer>,
}

impl MprisAdapter {
    pub fn new_session(art_dir: impl Into<PathBuf>, commands: Sender<BridgeCommand>) -> Self {
        Self::new_at(None, art_dir, commands)
    }

    pub fn new_at(
        address: Option<&str>,
        art_dir: impl Into<PathBuf>,
        commands: Sender<BridgeCommand>,
    ) -> Self {
        Self {
            address: address.map(str::to_owned),
            art_dir: art_dir.into(),
            commands,
            players: HashMap::new(),
        }
    }

    pub fn active_players(&self) -> usize {
        self.players.len()
    }

    pub fn handle(&mut self, kind: &str, message: &Map<String, Value>) -> Result<(), Error> {
        match kind {
            "media_gone" => {
                let sid = message.get("sid").and_then(Value::as_i64).unwrap_or(0);
                if let Ok(sid) = i32::try_from(sid) {
                    self.players.remove(&sid);
                }
            }
            "media" => {
                let media = messages::media(message).map_err(Error::InvalidMedia)?;
                self.upsert(media)?;
            }
            _ => {}
        }
        Ok(())
    }

    pub fn clear(&mut self) {
        self.players.clear();
    }

    fn connect(&self, name: &str, state: Arc<Mutex<State>>) -> Result<Connection, Error> {
        let builder = if let Some(address) = &self.address {
            Builder::address(address.as_str())?
        } else {
            Builder::session()?
        };
        Ok(builder
            .name(name)?
            .serve_at(PATH, Root(Arc::clone(&state)))?
            .serve_at(
                PATH,
                Player {
                    state,
                    commands: self.commands.clone(),
                },
            )?
            .build()?)
    }

    fn upsert(&mut self, media: Media) -> Result<(), Error> {
        if let Some(player) = self.players.get_mut(&media.session_id) {
            update_player(player, media, &self.art_dir)?;
            return Ok(());
        }
        if self.players.len() >= MAX_PLAYERS {
            return Err(Error::TooManyPlayers);
        }
        let sid = media.session_id;
        let name = format!(
            "org.mpris.MediaPlayer2.waydroid_{}.instance{sid}",
            media.package.as_str().replace('.', "_")
        );
        let state = Arc::new(Mutex::new(State {
            media,
            track: 1,
            since: Instant::now(),
            art_path: None,
        }));
        let initial_art = lock(&state).media.art.clone();
        if let Some(art) = initial_art {
            let path = write_art(&self.art_dir, sid, &art)?;
            lock(&state).art_path = path;
        }
        let connection = match self.connect(&name, Arc::clone(&state)) {
            Ok(connection) => connection,
            Err(error) => {
                if let Some(path) = &lock(&state).art_path {
                    let _ = fs::remove_file(path);
                }
                return Err(error);
            }
        };
        self.players.insert(sid, ActivePlayer { connection, state });
        Ok(())
    }
}

fn write_art(directory: &Path, sid: i32, png: &[u8]) -> Result<Option<PathBuf>, Error> {
    if !png.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Ok(None);
    }
    let Ok((width, height)) =
        image::ImageReader::with_format(Cursor::new(png), ImageFormat::Png).into_dimensions()
    else {
        return Ok(None);
    };
    if width == 0 || height == 0 || width > 4096 || height > 4096 {
        return Ok(None);
    }
    let Ok(image) = image::load_from_memory_with_format(png, ImageFormat::Png) else {
        return Ok(None);
    };
    let image = if width.max(height) > 512 {
        image.resize(512, 512, image::imageops::FilterType::Triangle)
    } else {
        image
    };
    fs::create_dir_all(directory)?;
    let serial = ART_SERIAL.fetch_add(1, Ordering::Relaxed);
    let path = directory.join(format!("{sid}-{serial}.png"));
    let mut temp = tempfile::NamedTempFile::new_in(directory)?;
    image
        .to_rgba8()
        .write_to(&mut temp, ImageFormat::Png)
        .map_err(io::Error::other)?;
    temp.persist(&path).map_err(|error| error.error)?;
    Ok(Some(path))
}

fn update_player(player: &mut ActivePlayer, media: Media, art_dir: &Path) -> Result<(), Error> {
    let replacement_art = media
        .art
        .as_ref()
        .map(|art| write_art(art_dir, media.session_id, art))
        .transpose()?;
    let mut state = lock(&player.state);
    let expected = state.position_us();
    let new_track = media.title != state.media.title
        || media.artist != state.media.artist
        || media.album != state.media.album
        || media.duration_ms != state.media.duration_ms;
    if new_track {
        state.track += 1;
    }
    let old_art = state.art_path.clone();
    let has_replacement = replacement_art.is_some();
    if let Some(art) = replacement_art {
        state.art_path = art;
    }
    let old = std::mem::replace(&mut state.media, media);
    state.since = Instant::now();
    let mut changed = HashMap::new();
    if new_track || state.art_path != old_art {
        changed.insert("Metadata".to_owned(), OwnedValue::from(state.metadata()));
    }
    if state.media.status != old.status {
        changed.insert(
            "PlaybackStatus".to_owned(),
            OwnedValue::from(Str::from(state.media.status.clone())),
        );
    }
    if state.media.speed != old.speed {
        changed.insert("Rate".to_owned(), OwnedValue::from(state.rate()));
    }
    for (key, new, previous) in [
        ("CanPlay", state.media.can_play, old.can_play),
        ("CanPause", state.media.can_pause, old.can_pause),
        ("CanGoNext", state.media.can_next, old.can_next),
        ("CanGoPrevious", state.media.can_previous, old.can_previous),
    ] {
        if new != previous {
            changed.insert(key.to_owned(), OwnedValue::from(new));
        }
    }
    changed.insert("CanSeek".to_owned(), OwnedValue::from(state.can_seek()));
    let seeked = (!new_track && (state.position_us() - expected).abs() > 1_500_000)
        .then(|| state.position_us());
    drop(state);
    if has_replacement && let Some(path) = &old_art {
        let _ = fs::remove_file(path);
    }
    player.connection.emit_signal(
        None::<&str>,
        PATH,
        PROPERTIES,
        "PropertiesChanged",
        &(PLAYER, changed, Vec::<String>::new()),
    )?;
    if let Some(position) = seeked {
        player
            .connection
            .emit_signal(None::<&str>, PATH, PLAYER, "Seeked", &(position,))?;
    }
    Ok(())
}
