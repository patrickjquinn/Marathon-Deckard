//! Android MediaSessions as MPRIS2 players, one session-bus connection each.

use std::collections::HashMap;
use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use deckard_core::messages::{self, Media};
use deckard_core::png_guard;
use deckard_core::{json, keel};
use serde_json::{Map, Value};
use tempfile::TempPath;
use zbus::blocking::Connection;
use zbus::blocking::connection::Builder;
use zbus::zvariant::{ObjectPath, OwnedObjectPath, Value as Variant};

const PATH: &str = "/org/mpris/MediaPlayer2";
const PLAYER: &str = "org.mpris.MediaPlayer2.Player";
const MAX_PLAYERS: usize = 8;
const MAX_ART_BYTES: usize = 768 * 1024;
const ART_SIDE: u32 = 512;

type Command = Map<String, Value>;
type Sink = Arc<dyn Fn(Command) + Send + Sync>;
type Changes = HashMap<&'static str, Variant<'static>>;

struct Root {
    identity: String,
    desktop_entry: String,
}

impl Root {
    fn new(media: &Media) -> Self {
        Self {
            identity: media.label.clone(),
            desktop_entry: keel::app_id(media.package.as_str()),
        }
    }
}

#[zbus::interface(name = "org.mpris.MediaPlayer2")]
impl Root {
    fn raise(&self) {}

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
        self.identity.clone()
    }

    #[zbus(property)]
    fn desktop_entry(&self) -> String {
        self.desktop_entry.clone()
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
    media: Media,
    track: u64,
    since: Instant,
    art_url: Option<String>,
    send: Sink,
}

impl Player {
    fn send(&self, command: &str, position_ms: i64) {
        let message = Map::from_iter([
            ("t".to_owned(), Value::from("media_cmd")),
            ("sid".to_owned(), Value::from(self.media.session_id)),
            ("cmd".to_owned(), Value::from(command)),
            ("position".to_owned(), Value::from(position_ms)),
        ]);
        (self.send)(message);
    }

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

    fn seekable(&self) -> bool {
        self.media.can_seek && self.media.duration_ms > 0
    }

    fn speed(&self) -> f64 {
        if self.media.speed > 0.0 {
            self.media.speed
        } else {
            1.0
        }
    }

    fn track_id(&self) -> ObjectPath<'static> {
        let path = format!(
            "/org/marathonos/deckard/session{}/track{}",
            self.media.session_id, self.track
        );
        ObjectPath::from_string_unchecked(path)
    }

    fn track_metadata(&self) -> Changes {
        let mut metadata = Changes::from([
            ("mpris:trackid", Variant::from(self.track_id())),
            ("xesam:title", Variant::from(self.media.title.clone())),
        ]);
        if self.media.duration_ms > 0 {
            let length = self.media.duration_ms.saturating_mul(1000);
            metadata.insert("mpris:length", Variant::from(length));
        }
        if !self.media.artist.is_empty() {
            let artists = vec![self.media.artist.clone()];
            metadata.insert("xesam:artist", Variant::from(artists));
        }
        if !self.media.album.is_empty() {
            metadata.insert("xesam:album", Variant::from(self.media.album.clone()));
        }
        if let Some(url) = &self.art_url {
            metadata.insert("mpris:artUrl", Variant::from(url.clone()));
        }
        metadata
    }

    /// Takes a new session state; `art` is `None` when the message left art
    /// unchanged. Returns the changed properties and any `Seeked` position.
    fn apply(&mut self, media: Media, art: Option<Option<String>>) -> (Changes, Option<i64>) {
        let expected = self.position_us();
        let new_track = media.title != self.media.title
            || media.artist != self.media.artist
            || media.album != self.media.album
            || media.duration_ms != self.media.duration_ms;
        if new_track {
            self.track += 1;
        }
        let art_changed = art.is_some_and(|url| {
            let changed = url != self.art_url;
            self.art_url = url;
            changed
        });
        let old = std::mem::replace(&mut self.media, media);
        self.since = Instant::now();
        let mut changed = Changes::new();
        if new_track || art_changed {
            changed.insert("Metadata", Variant::from(self.track_metadata()));
        }
        if self.media.status != old.status {
            changed.insert("PlaybackStatus", Variant::from(self.media.status.clone()));
        }
        if self.media.speed != old.speed {
            changed.insert("Rate", Variant::from(self.speed()));
        }
        let flags = [
            ("CanPlay", self.media.can_play, old.can_play),
            ("CanPause", self.media.can_pause, old.can_pause),
            ("CanGoNext", self.media.can_next, old.can_next),
            ("CanGoPrevious", self.media.can_previous, old.can_previous),
        ];
        for (name, now, before) in flags {
            if now != before {
                changed.insert(name, Variant::from(now));
            }
        }
        changed.insert("CanSeek", Variant::from(self.seekable()));
        let position = self.position_us();
        let seeked = (!new_track && (position - expected).abs() > 1_500_000).then_some(position);
        (changed, seeked)
    }
}

#[zbus::interface(name = "org.mpris.MediaPlayer2.Player")]
impl Player {
    fn next(&self) {
        self.send("next", 0);
    }

    fn previous(&self) {
        self.send("previous", 0);
    }

    fn pause(&self) {
        self.send("pause", 0);
    }

    fn play_pause(&self) {
        self.send("playpause", 0);
    }

    fn stop(&self) {
        self.send("stop", 0);
    }

    fn play(&self) {
        self.send("play", 0);
    }

    fn seek(&self, offset_us: i64) {
        if self.seekable() {
            let target = self.position_us().saturating_add(offset_us).max(0);
            self.send("seek", target / 1000);
        }
    }

    fn set_position(&self, track: OwnedObjectPath, position_us: i64) {
        if self.seekable() && *track == self.track_id() && position_us >= 0 {
            self.send("seek", position_us / 1000);
        }
    }

    fn open_uri(&self, _uri: &str) {}

    #[zbus(signal)]
    async fn seeked(
        emitter: &zbus::object_server::SignalEmitter<'_>,
        position: i64,
    ) -> zbus::Result<()>;

    #[zbus(property)]
    fn playback_status(&self) -> String {
        self.media.status.clone()
    }

    #[zbus(property)]
    fn rate(&self) -> f64 {
        self.speed()
    }

    #[zbus(property)]
    fn set_rate(&mut self, _rate: f64) {}

    #[zbus(property)]
    fn metadata(&self) -> Changes {
        self.track_metadata()
    }

    #[zbus(property)]
    fn volume(&self) -> f64 {
        1.0
    }

    #[zbus(property)]
    fn set_volume(&mut self, _volume: f64) {}

    #[zbus(property)]
    fn position(&self) -> i64 {
        self.position_us()
    }

    #[zbus(property)]
    fn minimum_rate(&self) -> f64 {
        self.speed()
    }

    #[zbus(property)]
    fn maximum_rate(&self) -> f64 {
        self.speed()
    }

    #[zbus(property)]
    fn can_go_next(&self) -> bool {
        self.media.can_next
    }

    #[zbus(property)]
    fn can_go_previous(&self) -> bool {
        self.media.can_previous
    }

    #[zbus(property)]
    fn can_play(&self) -> bool {
        self.media.can_play
    }

    #[zbus(property)]
    fn can_pause(&self) -> bool {
        self.media.can_pause
    }

    #[zbus(property)]
    fn can_seek(&self) -> bool {
        self.seekable()
    }

    #[zbus(property)]
    fn can_control(&self) -> bool {
        true
    }
}

/// A player's connection, and its art, which is deleted with it.
struct ActivePlayer {
    connection: Connection,
    art: Option<TempPath>,
}

fn art_url(path: &Option<TempPath>) -> Option<String> {
    let path = path.as_deref()?;
    Some(format!("file://{}", path.display()))
}

/// Owns every player of one Android connection and the art cache directory.
pub struct MprisAdapter {
    address: Option<String>,
    art_dir: PathBuf,
    art_serial: u64,
    players: HashMap<i32, ActivePlayer>,
    send: Sink,
}

impl MprisAdapter {
    /// Players connect to the bus at `address`, or the session bus when it is
    /// `None`, and drop their commands until [`Self::clear`] gives them a
    /// sink. Art left in `art_dir` by an earlier run is deleted.
    ///
    /// # Errors
    ///
    /// The art directory cannot be emptied or created.
    pub fn new(address: Option<&str>, art_dir: impl Into<PathBuf>) -> io::Result<Self> {
        let art_dir = art_dir.into();
        if let Err(error) = fs::remove_dir_all(&art_dir)
            && error.kind() != io::ErrorKind::NotFound
        {
            return Err(error);
        }
        fs::create_dir_all(&art_dir)?;
        Ok(Self {
            address: address.map(str::to_owned),
            art_dir,
            art_serial: 0,
            players: HashMap::new(),
            send: Arc::new(|_| {}),
        })
    }

    /// The number of registered players.
    #[must_use]
    pub fn active_players(&self) -> usize {
        self.players.len()
    }

    /// Applies a `media` or `media_gone` message; other types are ignored.
    ///
    /// # Errors
    ///
    /// The session is malformed, over the player limit, or its player could
    /// not be registered, updated or given its art.
    pub fn handle(&mut self, message: &Map<String, Value>) -> zbus::Result<()> {
        match json::string(message, "t") {
            "media_gone" => {
                let sid = message.get("sid").and_then(Value::as_i64).unwrap_or(0);
                if let Ok(sid) = i32::try_from(sid) {
                    self.players.remove(&sid);
                }
                Ok(())
            }
            "media" => {
                let media = messages::media(message).map_err(|error| {
                    zbus::Error::Failure(format!("invalid media message: {error:?}"))
                })?;
                if self.players.contains_key(&media.session_id) {
                    self.update(media)
                } else {
                    self.insert(media)
                }
            }
            _ => Ok(()),
        }
    }

    /// Removes every player; later players hand their `media_cmd` messages
    /// to `send`, which must not block.
    pub fn clear(&mut self, send: impl Fn(Command) + Send + Sync + 'static) {
        self.players.clear();
        self.send = Arc::new(send);
    }

    fn insert(&mut self, media: Media) -> zbus::Result<()> {
        if self.players.len() >= MAX_PLAYERS {
            let error = format!("more than {MAX_PLAYERS} media sessions");
            return Err(zbus::Error::Failure(error));
        }
        let sid = media.session_id;
        let name = format!(
            "org.mpris.MediaPlayer2.deckard_{}.instance{sid}",
            media.package.as_str().replace('.', "_")
        );
        let art = match &media.art {
            Some(png) => self.write_art(sid, png)?,
            None => None,
        };
        let root = Root::new(&media);
        let player = Player {
            media,
            track: 1,
            since: Instant::now(),
            art_url: art_url(&art),
            send: Arc::clone(&self.send),
        };
        let connection = self.connect(&name, root, player)?;
        self.players.insert(sid, ActivePlayer { connection, art });
        Ok(())
    }

    fn connect(&self, name: &str, root: Root, player: Player) -> zbus::Result<Connection> {
        let builder = match &self.address {
            Some(address) => Builder::address(address.as_str())?,
            None => Builder::session()?,
        };
        builder
            .name(name)?
            .serve_at(PATH, root)?
            .serve_at(PATH, player)?
            .build()
    }

    fn update(&mut self, media: Media) -> zbus::Result<()> {
        let sid = media.session_id;
        let art = match &media.art {
            Some(png) => Some(self.write_art(sid, png)?),
            None => None,
        };
        let Some(active) = self.players.get_mut(&sid) else {
            return Ok(());
        };
        let url = art.as_ref().map(art_url);
        if let Some(art) = art {
            active.art = art;
        }
        let server = active.connection.object_server();
        *server.interface::<_, Root>(PATH)?.get_mut() = Root::new(&media);
        let (changed, seeked) = server
            .interface::<_, Player>(PATH)?
            .get_mut()
            .apply(media, url);
        active.connection.emit_signal(
            None::<&str>,
            PATH,
            "org.freedesktop.DBus.Properties",
            "PropertiesChanged",
            &(PLAYER, changed, Vec::<String>::new()),
        )?;
        if let Some(position) = seeked {
            active
                .connection
                .emit_signal(None::<&str>, PATH, PLAYER, "Seeked", &(position,))?;
        }
        Ok(())
    }

    /// Writes bounded, re-encoded art; `None` when the PNG is empty or unsafe.
    fn write_art(&mut self, sid: i32, png: &[u8]) -> io::Result<Option<TempPath>> {
        let Some(png) = png_guard::sanitize(png, MAX_ART_BYTES, ART_SIDE) else {
            return Ok(None);
        };
        fs::create_dir_all(&self.art_dir)?;
        self.art_serial += 1;
        let path = self.art_dir.join(format!("{sid}-{}.png", self.art_serial));
        let mut file = tempfile::Builder::new()
            .permissions(fs::Permissions::from_mode(0o666))
            .tempfile_in(&self.art_dir)?;
        file.write_all(&png)?;
        file.persist(&path).map_err(|error| error.error)?;
        Ok(Some(TempPath::try_from_path(path)?))
    }
}
