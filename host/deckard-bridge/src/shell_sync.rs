//! Marathon state mirrored into Android's `/data/deckard`, where its
//! deckard-watch service applies it: the ambience palette (empty when
//! Settings switches Android's theming off) and Android's display power,
//! which Android's own Doze follows.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::process;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use deckard_core::palette::{self, Color, Theme};
use deckard_core::runtime_state::SHELL_BUS_NAME;
use deckard_core::{ids, keel, sync};
use zbus::Connection;
use zbus::MatchRule;
use zbus::OwnedVariant as OwnedValue;
use zbus::runtime::traits::TaskHandle as _;

use crate::intents;
use crate::location::LocationSync;

const POWER_PATH: &str = "/org/marathonos/Shell/Power";
const POWER_INTERFACE: &str = "org.marathonos.Shell.Power1";
const AMBIENCE_PATH: &str = "/org/marathonos/Shell/Ambience";
const AMBIENCE_INTERFACE: &str = "org.marathonos.Shell.Ambience1";
const HIGHLIGHT: &str = "/desktop/jolla/theme/color/highlight";
const SETTINGS_PATH: &str = "/org/marathonos/Shell/Settings";
const SETTINGS_INTERFACE: &str = "org.marathonos.Shell.Settings1";
const ANDROID_THEMING: &str = "androidTheming";

type State = HashMap<String, OwnedValue>;

/// Replaces `path` by a rename, which deckard-watch sees as one `moved_to`.
/// Returns whether the content changed.
fn replace(path: &Path, content: &str) -> io::Result<bool> {
    if fs::read(path).is_ok_and(|current| current == content.as_bytes()) {
        return Ok(false);
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension(format!("{}.tmp", process::id()));
    fs::write(&temporary, content)?;
    fs::rename(&temporary, path).inspect_err(|_| {
        let _ = fs::remove_file(&temporary);
    })?;
    Ok(true)
}

fn write(path: &Path, content: &str, done: &str) {
    match replace(path, content) {
        Ok(true) => eprintln!("deckard-bridge: {done}"),
        Ok(false) => {}
        Err(error) => eprintln!("deckard-bridge: cannot write {}: {error}", path.display()),
    }
}

/// What Android's display should be.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Display {
    On,
    Off,
    /// Left to the composer HAL, which puts Android to sleep once none of
    /// its cards has had focus for a while, and keeps their last frames.
    Idle,
}

impl Display {
    /// Off with Marathon's screen, on while an Android app is in front.
    #[must_use]
    pub fn wanted(screen_on: bool, android_in_front: bool) -> Self {
        match (screen_on, android_in_front) {
            (false, _) => Self::Off,
            (true, true) => Self::On,
            (true, false) => Self::Idle,
        }
    }
}

#[derive(Default)]
struct DisplayState {
    screen_on: bool,
    in_front: bool,
    written: Option<Display>,
    wakes: u64,
}

/// Android's display power, written to `/data/deckard/display` for
/// deckard-watch: `on <n>` (a larger `n` for every wake), `off` or `idle`.
pub struct DisplaySync {
    path: PathBuf,
    location: Option<LocationSync>,
    state: Mutex<DisplayState>,
}

static DISPLAY: OnceLock<Arc<DisplaySync>> = OnceLock::new();

impl DisplaySync {
    /// Starts with the screen on and no Android app in front; `location`
    /// hears whether Android is in view.
    #[must_use]
    pub fn new(path: &Path, location: Option<LocationSync>) -> Self {
        // A restarted bridge must not repeat its predecessor's last `on <n>`.
        let wakes = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_secs());
        let state = DisplayState {
            screen_on: true,
            wakes,
            ..DisplayState::default()
        };
        Self {
            path: path.to_owned(),
            location,
            state: Mutex::new(state),
        }
    }

    /// Marathon's screen went on or off.
    pub fn screen(&self, on: bool) {
        let mut state = sync::lock(&self.state);
        state.screen_on = on;
        self.apply(&mut state, false);
    }

    /// The Shell's front app changed (`""` for none).
    pub fn foreground(&self, app: &str) {
        let pkg = app.strip_prefix(keel::APP_ID_PREFIX);
        let mut state = sync::lock(&self.state);
        state.in_front = pkg.is_some_and(ids::is_package_name);
        self.apply(&mut state, false);
    }

    /// Android is about to open a window for Marathon to show: it has to be
    /// awake to draw it. Nothing while the screen is off.
    pub fn wake(&self) {
        let mut state = sync::lock(&self.state);
        self.apply(&mut state, true);
    }

    fn apply(&self, state: &mut DisplayState, wake: bool) {
        if let Some(location) = &self.location {
            location.set_in_view(state.screen_on && state.in_front);
        }
        let wake = wake && state.screen_on;
        let wanted = if wake {
            Display::On
        } else {
            Display::wanted(state.screen_on, state.in_front)
        };
        if state.written == Some(wanted) && !wake {
            return;
        }
        let content = match wanted {
            Display::On => {
                state.wakes += 1;
                format!("on {}\n", state.wakes)
            }
            Display::Off => "off\n".to_owned(),
            Display::Idle => "idle\n".to_owned(),
        };
        write(&self.path, &content, &format!("Android display {wanted:?}"));
        state.written = Some(wanted);
    }
}

/// Wakes Android's display before it opens a window (see [`DisplaySync::wake`]).
pub fn wake_display() {
    if let Some(display) = DISPLAY.get() {
        display.wake();
    }
}

/// Follows Power1's screen state and the Shell's front app into `display`.
///
/// # Errors
/// Returns the D-Bus error that prevented the subscription.
pub fn sync_display(bus: &Connection, display: Arc<DisplaySync>) -> zbus::Result<()> {
    let _ = DISPLAY.set(Arc::clone(&display));
    let rule = crate::signals(SHELL_BUS_NAME, POWER_PATH, POWER_INTERFACE)?;
    let rule = rule.member("StateChanged").build()?;
    let screen = Arc::clone(&display);
    crate::relay(bus, rule, 8, move |signal| {
        let state = signal.body().deserialize::<State>();
        if let Some(Ok(dozing)) = state
            .ok()
            .and_then(|state| state.get("dozing").map(bool::try_from))
        {
            screen.screen(!dozing);
        }
        true
    })?;
    let follower = bus.clone();
    let follow = async move {
        let follow = intents::follow_foreground(&follower, |app| {
            display.foreground(app);
            ControlFlow::Continue(())
        });
        if let Err(error) = follow.await {
            eprintln!("deckard-bridge: foreground watch for the display failed: {error}");
        }
    };
    bus.spawn("deckard-display".to_owned(), follow).detach();
    Ok(())
}

/// Settings' switch for Android's theming, on for a Shell without the
/// switch; none when the Shell cannot be asked.
fn android_theming(bus: &Connection) -> Option<bool> {
    let reply = bus.call_method(
        Some(SHELL_BUS_NAME),
        SETTINGS_PATH,
        Some(SETTINGS_INTERFACE),
        "GetState",
        &(),
    );
    match zbus::block_on(reply).and_then(|reply| reply.body().deserialize::<State>()) {
        Ok(state) => Some(
            state
                .get(ANDROID_THEMING)
                .and_then(|on| bool::try_from(on).ok())
                .unwrap_or(true),
        ),
        Err(zbus::Error::MethodError(name, _, _))
            if name
                .as_str()
                .starts_with("org.freedesktop.DBus.Error.Unknown") =>
        {
            Some(true)
        }
        // zbus hands the standard names over as typed errors.
        Err(zbus::Error::FDO(error))
            if matches!(
                *error,
                zbus::fdo::Error::UnknownObject(_)
                    | zbus::fdo::Error::UnknownInterface(_)
                    | zbus::fdo::Error::UnknownMethod(_)
                    | zbus::fdo::Error::UnknownProperty(_)
            ) =>
        {
            Some(true)
        }
        Err(error) => {
            eprintln!("deckard-bridge: cannot read the settings: {error}");
            None
        }
    }
}

fn refresh_palette(bus: &Connection, path: &Path) {
    match android_theming(bus) {
        Some(true) => {}
        Some(false) => {
            write(path, "", "Android's own palette");
            return;
        }
        None => return,
    }
    let reply = bus.call_method(
        Some(SHELL_BUS_NAME),
        AMBIENCE_PATH,
        Some(AMBIENCE_INTERFACE),
        "GetAmbience",
        &(),
    );
    let ambience = match zbus::block_on(reply).and_then(|reply| reply.body().deserialize::<State>())
    {
        Ok(ambience) => ambience,
        Err(error) => {
            eprintln!("deckard-bridge: cannot read the ambience: {error}");
            return;
        }
    };
    let Some(highlight) = ambience
        .get(HIGHLIGHT)
        .and_then(|value| <&str>::try_from(value).ok())
        .and_then(|value| Color::parse(value).ok())
    else {
        return;
    };
    let content = palette::runtime_file(highlight, Theme::default());
    let done = format!("palette for highlight {}", highlight.hex());
    write(path, &content, &done);
}

/// Writes Android's 65 system colours for the current ambience highlight to
/// `path`, or nothing while Android's theming is off, again whenever the
/// ambience, that switch or the Shell's bus owner changes.
///
/// # Errors
/// Returns the D-Bus error that prevented the subscription.
pub fn sync_palette(bus: &Connection, path: &Path) -> zbus::Result<()> {
    let ambience = crate::signals(SHELL_BUS_NAME, AMBIENCE_PATH, AMBIENCE_INTERFACE)?;
    // The Shell sends settings signals from another of its connections, so
    // the rule cannot name the Shell; the switch is read back from it anyway.
    let theming = MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .path(SETTINGS_PATH)
        .interface(SETTINGS_INTERFACE)
        .member("PropertyChanged")
        .add_arg(ANDROID_THEMING);
    let rules: Vec<(MatchRule<'static>, crate::Relevant)> = vec![
        (ambience.member("AmbienceChanged").build()?, |_| true),
        (theming.build()?, |_| true),
        (crate::owner_changes(SHELL_BUS_NAME)?, |_| true),
    ];
    let (bus, path) = (bus.clone(), path.to_owned());
    crate::follow(&bus.clone(), rules, move || refresh_palette(&bus, &path))?;
    Ok(())
}
