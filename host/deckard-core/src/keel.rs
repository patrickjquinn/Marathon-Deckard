//! Android apps as Keel Actions providers (protocol/bridge.md, Keel Actions):
//! what DeckardBridge reports about an app, the Keel manifest it is served
//! with, and the screen text Pilot reads.

use std::collections::HashMap;
use std::fmt::Write;
use std::sync::LazyLock;

use regex::Regex;
use serde::Serialize;
use serde_json::{Map, Value, json};

use crate::ids;
use crate::json;
use crate::validate;

pub use deckard_host_tools::names::APP_ID_PREFIX;

const MAX_LINKS: usize = 16;
const MAX_SHORTCUTS: usize = 15;
const MAX_NODES: usize = 400;
const MAX_TOOL_NAME: usize = 127;
/// The role of a node by its class name (any of `|`), first match first.
const ROLES: &[(&str, &str)] = &[
    ("CheckBox", "checkbox"),
    ("Switch|ToggleButton", "switch"),
    ("RadioButton", "radio"),
    ("Button", "button"),
    ("ImageView", "image"),
    ("WebView", "web"),
    ("SeekBar|Slider", "slider"),
    ("ProgressBar", "progress"),
    (
        "RecyclerView|ListView|GridView|ScrollView|ViewPager",
        "list",
    ),
    ("TextView", "text"),
];
const TAPPABLE: &[&str] = &["button", "field", "checkbox", "switch", "radio", "item"];

fn pattern(source: &str) -> Regex {
    Regex::new(source).unwrap_or_else(|error| panic!("invalid fixed Keel pattern: {error}"))
}

static SCHEME: LazyLock<Regex> = LazyLock::new(|| pattern(r"^[a-z][a-z0-9+.-]{0,31}$"));
static HOST: LazyLock<Regex> = LazyLock::new(|| pattern(r"^[a-z0-9*][a-z0-9.*-]{0,252}$"));
static LINK: LazyLock<Regex> =
    LazyLock::new(|| pattern(r"^([A-Za-z][A-Za-z0-9+.-]{0,31}):[^ \t\n\x0B\x0C\r]+$"));
/// Schemes that reach files, other apps' data or code, not an app's links.
static BLOCKED: LazyLock<Regex> = LazyLock::new(|| {
    pattern(
        r"^(?:file|content|javascript|data|intent|android-app|about|jar|blob|android\.resource)$",
    )
});
/// Shortcut labels that act at once or touch money, messages or data.
static RISKY: LazyLock<Regex> = LazyLock::new(|| {
    pattern(
        r"(?i)(?-u:\b)(delete|remove|erase|wipe|clear|reset|send|pay|payment|buy|order|purchase|transfer|call|dial|post|publish|record|uninstall|block|report|sign out|log out|logout)(?-u:\b)",
    )
});

/// Node flags as DeckardBridge's `ScreenReader` sends them.
mod flags {
    pub const CLICKABLE: i64 = 1;
    pub const LONG_CLICKABLE: i64 = 2;
    pub const EDITABLE: i64 = 4;
    pub const CHECKABLE: i64 = 8;
    pub const CHECKED: i64 = 16;
    pub const SELECTED: i64 = 32;
    pub const DISABLED: i64 = 64;
    pub const FOCUSED: i64 = 128;
    pub const SCROLLABLE: i64 = 256;
    pub const PASSWORD: i64 = 512;
    pub const HEADING: i64 = 1024;
    pub const ALL: i64 = 2047;
}

/// A `VIEW` + `BROWSABLE` intent filter's scheme and host.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Link {
    #[serde(rename = "s")]
    pub scheme: String,
    #[serde(rename = "h")]
    pub host: String,
}

/// A launcher shortcut, from DeckardSystem.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Shortcut {
    pub id: String,
    pub label: String,
    #[serde(rename = "long")]
    pub long_label: String,
}

/// One launchable app; serialises as an `apps.json` entry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct App {
    pub pkg: String,
    pub label: String,
    pub links: Vec<Link>,
    pub share: bool,
    /// `gms`, `android` or empty.
    pub search: String,
    pub shortcuts: Vec<Shortcut>,
}

/// A manifest and the shortcut id behind each `shortcut.*` action.
#[derive(Clone, Debug, PartialEq)]
pub struct Manifest {
    pub json: Value,
    pub shortcuts: HashMap<String, String>,
}

/// A validated `screen` reply, one rendered line per node.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Screen {
    pub pkg: String,
    pub snapshot: i64,
    pub title: String,
    lines: Vec<String>,
}

/// A screen as text, at most the asked length.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rendered {
    pub text: String,
    pub truncated: bool,
}

/// An element id from `screen.read`: `<snapshot>.<index>`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NodeRef {
    pub snapshot: i64,
    pub index: i64,
}

fn units(value: &str) -> usize {
    value.encode_utf16().count()
}

/// A JSON number that is a whole `i32`, as Qt's `toInt` reads it.
fn int(object: &Map<String, Value>, key: &str) -> Option<i64> {
    let value = object.get(key)?.as_f64()?;
    let whole =
        value.fract() == 0.0 && (f64::from(i32::MIN)..=f64::from(i32::MAX)).contains(&value);
    whole.then_some(value as i64)
}

/// The shell's app id, and the Keel bus name, of `pkg`.
#[must_use]
pub fn app_id(pkg: &str) -> String {
    format!("{APP_ID_PREFIX}{pkg}")
}

/// Validates an `app_info` (or an `apps.json` entry). `None` for a bad
/// package name or one of Deckard's own.
#[must_use]
pub fn app(message: &Map<String, Value>) -> Option<App> {
    let pkg = json::string(message, "pkg");
    if !ids::is_package_name(pkg)
        || units(&app_id(pkg)) > 255
        || pkg.starts_with("org.marathonos.deckard.")
    {
        return None;
    }
    let mut links = Vec::new();
    let declared = json::array(message, "links")
        .iter()
        .filter_map(Value::as_object);
    for link in declared {
        let link = Link {
            scheme: json::string(link, "s").to_lowercase(),
            host: json::string(link, "h").to_lowercase(),
        };
        let valid = SCHEME.is_match(&link.scheme)
            && !BLOCKED.is_match(&link.scheme)
            && (link.host.is_empty() || HOST.is_match(&link.host));
        if valid && links.len() < MAX_LINKS && !links.contains(&link) {
            links.push(link);
        }
    }
    let search = json::string(message, "search");
    Some(App {
        pkg: pkg.to_owned(),
        label: validate::label(json::string(message, "label"), pkg),
        links,
        share: json::boolean(message, "share"),
        search: if matches!(search, "gms" | "android") {
            search.to_owned()
        } else {
            String::new()
        },
        shortcuts: shortcuts(json::array(message, "shortcuts")),
    })
}

/// Validates a shortcut list: ids go back to Android as they came, so an id
/// that needs cleaning is dropped, not rewritten.
#[must_use]
pub fn shortcuts(list: &[Value]) -> Vec<Shortcut> {
    let mut out = Vec::new();
    let listed = list.iter().filter_map(Value::as_object);
    for shortcut in listed {
        let id = json::string(shortcut, "id");
        let label = validate::simplified(&json::text(shortcut, "label", 64));
        let clean = !id.is_empty() && units(id) <= 128 && validate::clean_text(id, 128) == id;
        let taken = out.iter().any(|known: &Shortcut| known.id == id);
        if !clean || label.is_empty() || out.len() >= MAX_SHORTCUTS || taken {
            continue;
        }
        out.push(Shortcut {
            id: id.to_owned(),
            label,
            long_label: validate::simplified(&json::text(shortcut, "long", 256)),
        });
    }
    out
}

/// The Keel action a shortcut becomes: `shortcut.<word>`, unique in `taken`.
fn shortcut_action(id: &str, taken: &[String]) -> String {
    let mut word = String::new();
    let lower = id.to_lowercase();
    for character in lower.chars() {
        if character.is_ascii_lowercase() || character.is_ascii_digit() {
            word.push(character);
        } else if !word.is_empty() && !word.ends_with('_') {
            word.push('_');
        }
    }
    let mut word = word.trim_end_matches('_').to_owned();
    if !word.starts_with(|character: char| character.is_ascii_lowercase()) {
        word.insert(0, 's');
    }
    word.truncate(40);
    let word = word.trim_end_matches('_');
    let mut action = format!("shortcut.{word}");
    let mut n = 2;
    while taken.contains(&action) {
        action = format!("shortcut.{word}_{n}");
        n += 1;
    }
    action
}

/// The host of a `scheme://authority` URI, without user info or port.
fn web_host(rest: &str) -> &str {
    let authority = rest.strip_prefix("//").unwrap_or("");
    let authority = authority.split(['/', '?', '#']).next().unwrap_or("");
    let host = authority.rsplit('@').next().unwrap_or("");
    match host.find(']') {
        Some(end) if host.starts_with('[') => &host[1..end],
        _ => host.split(':').next().unwrap_or(""),
    }
}

/// Whether `app.show` may hand `uri` to Android.
///
/// # Errors
///
/// The reason, for the caller's `InvalidArguments`.
pub fn link_allowed(uri: &str) -> Result<(), String> {
    let scheme = LINK
        .captures(uri)
        .and_then(|parts| parts.get(1))
        .map(|scheme| scheme.as_str());
    let Some(scheme) = scheme.filter(|_| units(uri) <= 2048) else {
        return Err("link must be a URI of at most 2048 characters".into());
    };
    // The shape has no whitespace, so cleaning changes it only for a control
    // or format character.
    if validate::clean_text(uri, 2048) != uri {
        return Err("link has control characters".into());
    }
    let scheme = scheme.to_lowercase();
    if BLOCKED.is_match(&scheme) {
        return Err(format!("{scheme}: links are not opened in apps"));
    }
    let rest = &uri[scheme.len() + 1..];
    if matches!(scheme.as_str(), "http" | "https") && web_host(rest).is_empty() {
        return Err("a web link needs a host".into());
    }
    Ok(())
}

fn string_schema(max_length: usize, description: &str) -> Value {
    json!({"type": "string", "maxLength": max_length, "description": description})
}

fn object_schema(properties: Value, required: &[&str]) -> Value {
    let mut schema =
        json!({"type": "object", "additionalProperties": false, "properties": properties});
    if !required.is_empty() {
        schema["required"] = json!(required);
    }
    schema
}

const DESTRUCTIVE: u8 = 1;
const IDEMPOTENT: u8 = 2;
const OPEN_WORLD: u8 = 4;
const READ_ONLY: u8 = 8;
const CONFIRM: u8 = 16;
const UNTRUSTED: u8 = 32;

/// A tool from `spec` (title, description, schemas and, for a tool that is
/// not destructive, `because`: why not).
fn tool(app_id: &str, action: &str, hints: u8, mut spec: Value) -> Value {
    let has = |hint: u8| hints & hint != 0;
    let result = if spec.get("outputSchema").is_some() {
        "object"
    } else {
        "none"
    };
    let mut meta = json!({
        "org.shipwright.keel/action": action, "org.shipwright.keel/confirm": has(CONFIRM),
        "org.shipwright.keel/result": result, "org.shipwright.keel/sensitive": false,
        "org.shipwright.keel/timeoutMs": 25000,
    });
    if has(UNTRUSTED) {
        meta["org.shipwright.keel/untrusted"] = json!(true);
    }
    if let Some(because) = spec.as_object_mut().and_then(|spec| spec.remove("because")) {
        meta["org.shipwright.keel/notDestructiveBecause"] = because;
    }
    spec["_meta"] = meta;
    spec["annotations"] = json!({
        "destructiveHint": has(DESTRUCTIVE), "idempotentHint": has(IDEMPOTENT),
        "openWorldHint": has(OPEN_WORLD), "readOnlyHint": has(READ_ONLY),
    });
    spec["name"] = json!(format!("{app_id}/{action}"));
    spec
}

/// The Keel manifest `Describe` answers for `app`.
#[must_use]
pub fn manifest(app: &App) -> Manifest {
    let id = app_id(&app.pkg);
    let label = &app.label;
    let tool = |action: &str, hints: u8, spec: Value| tool(&id, action, hints, spec);
    let mut tools = Vec::new();

    let mut examples = Vec::new();
    let shown = app.links.iter().take(8);
    for link in shown {
        let separator = if link.host.is_empty() { ":" } else { "://" };
        examples.push(format!("{}{separator}{}", link.scheme, link.host));
    }
    let mut show = json!({});
    let mut show_text = format!("Open {label}, an Android app, and bring it to the front.");
    if !examples.is_empty() {
        show["link"] = string_schema(2048, "A link or deep link for the app to open");
        let examples = examples.join(", ");
        show_text = format!(
            "{show_text} Given a link, opens it in the app; it handles links like {examples}."
        );
    }
    tools.push(tool("app.show", IDEMPOTENT, json!({
        "title": format!("Open {label}"), "description": show_text, "inputSchema": object_schema(show, &[]),
        "because": "it only shows the app, or a link in it",
    })));
    if app.share {
        let input = json!({
            "text": string_schema(20000, "The text to share"),
            "subject": string_schema(200, "A subject, for apps that use one"),
        });
        tools.push(tool("share.text", OPEN_WORLD, json!({
            "title": format!("Share to {label}"),
            "description": format!("Open {label}'s share screen with the text (and an optional subject) filled in. Nothing leaves the app until the person goes on from there."),
            "inputSchema": object_schema(input, &["text"]),
            "because": "it opens the app's share screen; the person decides whether to send",
        })));
    }
    if !app.search.is_empty() {
        let input = json!({"query": string_schema(500, "What to search for")});
        tools.push(tool(
            "app.search",
            IDEMPOTENT,
            json!({
                "title": format!("Search in {label}"),
                "description": format!("Search inside {label} and show its results for the query."),
                "inputSchema": object_schema(input, &["query"]),
            }),
        ));
    }

    let mut actions: Vec<String> = Vec::new();
    let mut shortcuts = HashMap::new();
    for shortcut in &app.shortcuts {
        let action = shortcut_action(&shortcut.id, &actions);
        let risky = RISKY.is_match(&format!("{} {}", shortcut.label, shortcut.long_label));
        let what = Some(&shortcut.long_label).filter(|long| !long.is_empty());
        let what = what.unwrap_or(&shortcut.label);
        let (hints, confirm) = match risky {
            true => (
                CONFIRM | DESTRUCTIVE,
                " It may act at once, so the person confirms it.",
            ),
            false => (0, ""),
        };
        let mut spec = json!({
            "title": format!("{} ({label})", shortcut.label),
            "description": format!("{what}: a shortcut of {label}, an Android app; opens the app there.{confirm}"),
            "inputSchema": object_schema(json!({}), &[]),
        });
        if !risky {
            spec["because"] = json!("it opens a screen of the app");
        }
        tools.push(tool(&action, hints, spec));
        shortcuts.insert(action.clone(), shortcut.id.clone());
        actions.push(action);
    }

    let output = json!({
        "app": {"type": "string", "maxLength": 264}, "label": {"type": "string", "maxLength": 128},
        "title": {"type": "string", "maxLength": 256}, "snapshot": {"type": "integer", "minimum": 1},
        "screen": {"type": "string", "maxLength": 20000}, "truncated": {"type": "boolean"},
    });
    tools.push(tool("screen.read", READ_ONLY | IDEMPOTENT | UNTRUSTED, json!({
        "title": format!("Read {label}'s screen"),
        "description": format!("Read what {label} shows on screen now, as text: one line per element with its id ([snapshot.node]), role, text, description, hint and state. Password fields are never included. Only while the app is in front and the phone is unlocked; ui.tap and ui.type take the ids."),
        "inputSchema": object_schema(json!({}), &[]),
        "outputSchema": object_schema(output, &["app", "snapshot", "screen"]),
    })));
    let node = string_schema(
        24,
        "An element id from the latest screen.read, such as 3.17",
    );
    let text = string_schema(4000, "The field's new text");
    tools.push(tool("ui.tap", CONFIRM | DESTRUCTIVE, json!({
        "title": format!("Tap in {label}"),
        "description": format!("Tap an element of {label}'s screen, by its id from screen.read, as the person would. The person confirms every tap."),
        "inputSchema": object_schema(json!({"node": node}), &["node"]),
    })));
    tools.push(tool("ui.type", CONFIRM | DESTRUCTIVE, json!({
        "title": format!("Type in {label}"),
        "description": format!("Replace the text of an editable field of {label}'s screen (an id from screen.read) with the given text, as the person would type it. Never for password fields. The person confirms it."),
        "inputSchema": object_schema(json!({"node": node, "text": text}), &["node", "text"]),
    })));

    // keel-mcp drops a tool whose MCP name (with __) is over 128 characters.
    tools.retain(|tool| units(tool["name"].as_str().unwrap_or("")) <= MAX_TOOL_NAME);
    Manifest {
        json: json!({
            "appId": id, "context": {"uri": format!("keel://{id}/context")},
            "entities": [], "prompts": [], "tools": tools, "version": 1,
        }),
        shortcuts,
    }
}

/// Checks Invoke's arguments against a tool's input schema, whose every
/// parameter is a bounded string. `None` when they fit.
#[must_use]
pub fn invalid(schema: &Value, args: &Map<String, Value>) -> Option<String> {
    for (name, value) in args {
        let Some(property) = schema["properties"].get(name) else {
            return Some(format!("unknown argument {}", validate::truncate(name, 64)));
        };
        let max = property["maxLength"].as_u64().unwrap_or(0);
        if value.as_str().is_none_or(|value| units(value) as u64 > max) {
            return Some(format!(
                "{name} must be a string of at most {max} characters"
            ));
        }
    }
    let required = schema["required"].as_array().map_or(&[][..], Vec::as_slice);
    let missing = |name: &&str| json::string(args, name).trim().is_empty();
    let name = required.iter().filter_map(Value::as_str).find(missing)?;
    Some(format!("missing argument {name}"))
}

/// Validates a `screen` reply. `None` for a bad package or snapshot.
#[must_use]
pub fn screen(message: &Map<String, Value>) -> Option<Screen> {
    let pkg = json::string(message, "pkg");
    let snapshot = int(message, "snap").filter(|snapshot| *snapshot >= 1)?;
    if !ids::is_package_name(pkg) {
        return None;
    }
    let mut lines = Vec::new();
    let sent = json::array(message, "nodes")
        .iter()
        .filter_map(Value::as_object);
    for node in sent {
        if lines.len() >= MAX_NODES {
            break;
        }
        lines.extend(line(snapshot, node));
    }
    Some(Screen {
        pkg: pkg.to_owned(),
        snapshot,
        title: validate::simplified(&json::text(message, "title", 256)),
        lines,
    })
}

/// The role `screen.read` gives a node.
fn role(class: &str, flags: i64) -> &'static str {
    let class = class.rsplit('.').next().unwrap_or("");
    if flags & flags::EDITABLE != 0 || class.contains("EditText") {
        return "field";
    }
    let matches = |classes: &str| classes.split('|').any(|name| class.contains(name));
    let found = ROLES.iter().find(|(classes, _)| matches(classes));
    match found {
        Some((_, role)) => role,
        None if flags & flags::CLICKABLE != 0 => "item",
        None => "group",
    }
}

fn quoted(value: &str) -> String {
    let escaped = value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\t', " ");
    format!("\"{escaped}\"")
}

const NAMED_FLAGS: [(i64, &str); 6] = [
    (flags::LONG_CLICKABLE, "long-press"),
    (flags::SELECTED, "selected"),
    (flags::FOCUSED, "focused"),
    (flags::SCROLLABLE, "scrolls"),
    (flags::HEADING, "heading"),
    (flags::DISABLED, "disabled"),
];

/// A node's line; `None` for a node without a valid index.
fn line(snapshot: i64, node: &Map<String, Value>) -> Option<String> {
    let index = int(node, "i").filter(|index| *index >= 0)?.min(9999);
    let flags = int(node, "f").unwrap_or(0) & flags::ALL;
    let has = |flag: i64| flags & flag != 0;
    let depth = int(node, "d").unwrap_or(0).clamp(0, 16) as usize;
    let text = match has(flags::PASSWORD) {
        true => String::new(),
        false => json::text(node, "t", 1000),
    };
    let (desc, hint) = (json::text(node, "cd", 500), json::text(node, "h", 200));
    let role = role(&json::text(node, "c", 128), flags);
    let mut line = format!("{}[{snapshot}.{index}] {role}", " ".repeat(2 * depth));
    if has(flags::PASSWORD) {
        line.push_str(" password (hidden)");
    } else if !text.is_empty() {
        let _ = write!(line, " {}", quoted(&text));
    }
    if !desc.is_empty() && desc != text {
        let _ = write!(line, " ({})", validate::simplified(&desc));
    }
    if !hint.is_empty() && hint != text {
        let _ = write!(line, " hint {}", quoted(&hint));
    }
    let view_id = json::text(node, "v", 128);
    let view = view_id.rsplit(":id/").next().unwrap_or("");
    if !view.is_empty() {
        let _ = write!(line, " #{view}");
    }
    let mut state = Vec::new();
    if has(flags::CHECKABLE) {
        state.push(if has(flags::CHECKED) {
            "checked"
        } else {
            "unchecked"
        });
    }
    if has(flags::CLICKABLE) && !TAPPABLE.contains(&role) {
        state.push("tappable");
    }
    for (flag, name) in NAMED_FLAGS {
        if has(flag) {
            state.push(name);
        }
    }
    if !state.is_empty() {
        let _ = write!(line, " {{{}}}", state.join(", "));
    }
    Some(line)
}

/// One line per node, indented by depth, at most `max_units` UTF-16 units.
#[must_use]
pub fn render(screen: &Screen, max_units: usize) -> Rendered {
    let mut out = Rendered {
        text: String::new(),
        truncated: false,
    };
    let mut length = 0;
    for line in &screen.lines {
        length += units(line) + 1;
        if length > max_units {
            out.truncated = true;
            break;
        }
        let _ = writeln!(out.text, "{line}");
    }
    out
}

/// Parses an element id from `screen.read`.
#[must_use]
pub fn parse_node(id: &str) -> Option<NodeRef> {
    let (snapshot, index) = id.trim().split_once('.')?;
    let digits = |part: &str, max: usize| {
        (1..=max).contains(&part.len()) && part.bytes().all(|byte| byte.is_ascii_digit())
    };
    if !digits(snapshot, 10) || snapshot.starts_with('0') || !digits(index, 4) {
        return None;
    }
    let snapshot: i64 = snapshot.parse().ok()?;
    (snapshot <= 2_147_483_647).then_some(NodeRef {
        snapshot,
        index: index.parse().ok()?,
    })
}

/// A paired computer's keel-mcp (`--external`) gets no screen content.
#[must_use]
pub fn is_external_host(cmdline: &[u8]) -> bool {
    cmdline
        .split(|byte| *byte == 0)
        .any(|argument| argument == b"--external")
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "fixed protocol fixtures"
)]
mod tests {
    use super::*;

    fn object(value: Value) -> Map<String, Value> {
        value.as_object().cloned().expect("fixture is an object")
    }

    fn sample() -> Map<String, Value> {
        object(json!({
            "t": "app_info", "pkg": "org.example.chat", "label": "Chat\u{7} App",
            "links": [{"s": "https", "h": "chat.example.org"}, {"s": "chat", "h": ""},
                      {"s": "file", "h": ""}, {"s": "javascript", "h": ""}, {"s": "HTTPS", "h": "CHAT.example.org"}],
            "share": true, "search": "gms",
            "shortcuts": [{"id": "compose", "label": "New message", "long": "Write a new message"},
                          {"id": "compose!", "label": "Compose again"},
                          {"id": "wipe_all", "label": "Delete all chats"},
                          {"id": "bad\u{1}id", "label": "Bad"},
                          {"id": "nolabel", "label": ""}]
        }))
    }

    fn tool_of<'a>(manifest: &'a Value, action: &str) -> Option<&'a Value> {
        let tools = manifest["tools"].as_array()?;
        tools
            .iter()
            .find(|tool| tool["_meta"]["org.shipwright.keel/action"] == action)
    }

    #[test]
    fn apps_are_cleaned() {
        let app = app(&sample()).unwrap();
        assert_eq!(app.label, "Chat App");
        assert_eq!(app.links.len(), 2);
        assert_eq!(app.links[0].host, "chat.example.org");
        assert_eq!(app.links[1].scheme, "chat");
        assert_eq!(app.search, "gms");
        assert_eq!(app.shortcuts.len(), 3);
        assert_eq!(app.shortcuts[2].id, "wipe_all");

        let mut own = sample();
        own.insert("pkg".into(), json!("org.marathonos.deckard.bridge"));
        assert!(super::app(&own).is_none());
        own.insert("pkg".into(), json!("not a package"));
        assert!(super::app(&own).is_none());

        let saved = object(serde_json::to_value(&app).unwrap());
        assert_eq!(super::app(&saved).unwrap(), app);
    }

    #[test]
    fn manifests_are_keel_manifests() {
        let manifest = manifest(&app(&sample()).unwrap());
        let json = &manifest.json;
        assert_eq!(json["appId"], "deckard.org.example.chat");
        assert_eq!(json["version"], 1);
        assert_eq!(
            json["context"]["uri"],
            "keel://deckard.org.example.chat/context"
        );
        for tool in json["tools"].as_array().unwrap() {
            assert!(
                tool["name"]
                    .as_str()
                    .unwrap()
                    .starts_with("deckard.org.example.chat/")
            );
            assert!(tool["description"].as_str().unwrap().len() >= 20);
            assert_eq!(tool["inputSchema"]["additionalProperties"], false);
        }

        let show = tool_of(json, "app.show").unwrap();
        assert!(show["inputSchema"]["properties"].get("link").is_some());
        assert!(
            show["description"]
                .as_str()
                .unwrap()
                .contains("https://chat.example.org")
        );
        assert_eq!(
            tool_of(json, "share.text").unwrap()["annotations"]["openWorldHint"],
            true
        );
        assert!(tool_of(json, "app.search").is_some());

        let compose = tool_of(json, "shortcut.compose").unwrap();
        assert_eq!(compose["title"], "New message (Chat App)");
        assert_eq!(compose["_meta"]["org.shipwright.keel/confirm"], false);
        assert_eq!(manifest.shortcuts["shortcut.compose_2"], "compose!");
        let wipe = tool_of(json, "shortcut.wipe_all").unwrap();
        assert_eq!(wipe["_meta"]["org.shipwright.keel/confirm"], true);
        assert_eq!(wipe["annotations"]["destructiveHint"], true);

        let read = tool_of(json, "screen.read").unwrap();
        assert_eq!(read["annotations"]["readOnlyHint"], true);
        assert_eq!(read["_meta"]["org.shipwright.keel/untrusted"], true);
        assert!(read.get("outputSchema").is_some());
        assert_eq!(
            tool_of(json, "ui.tap").unwrap()["_meta"]["org.shipwright.keel/confirm"],
            true
        );
        assert_eq!(
            tool_of(json, "ui.type").unwrap()["_meta"]["org.shipwright.keel/confirm"],
            true
        );

        let bare = App {
            pkg: "org.example.bare".into(),
            label: "Bare".into(),
            links: Vec::new(),
            share: false,
            search: String::new(),
            shortcuts: Vec::new(),
        };
        let bare_manifest = super::manifest(&bare).json;
        assert!(tool_of(&bare_manifest, "share.text").is_none());
        assert_eq!(
            tool_of(&bare_manifest, "app.show").unwrap()["inputSchema"]["properties"],
            json!({})
        );

        let long_name = App {
            pkg: format!("org.{}", "x".repeat(110)),
            ..bare
        };
        let long_manifest = super::manifest(&long_name).json;
        for tool in long_manifest["tools"].as_array().unwrap() {
            assert!(tool["name"].as_str().unwrap().len() < 128);
        }
    }

    #[test]
    fn shortcut_names() {
        assert_eq!(shortcut_action("New-Chat", &[]), "shortcut.new_chat");
        assert_eq!(shortcut_action("123", &[]), "shortcut.s123");
        assert_eq!(shortcut_action("!!!", &[]), "shortcut.s");
        assert_eq!(shortcut_action("a", &["shortcut.a".into()]), "shortcut.a_2");
        assert!(RISKY.is_match("Call Mum"));
        assert!(!RISKY.is_match("Start recording"));
        assert!(RISKY.is_match("Record"));
        assert!(!RISKY.is_match("Navigate home"));
    }

    #[test]
    fn links() {
        assert!(link_allowed("https://chat.example.org/u/1").is_ok());
        assert!(link_allowed("chat://room/42").is_ok());
        assert!(link_allowed("https://user@[::1]:8080/x").is_ok());
        assert!(link_allowed("file:///data/data/x").is_err());
        assert!(link_allowed("content://media/1").is_err());
        assert!(link_allowed("intent://x#Intent;component=a/b;end").is_err());
        assert!(link_allowed("https:///nohost").is_err());
        assert!(link_allowed("no scheme").is_err());
        assert!(link_allowed("https://a.b/\u{202e}").is_err());
    }

    #[test]
    fn screens() {
        let message = object(json!({
            "t": "screen", "pkg": "org.example.chat", "snap": 3, "title": "Anna",
            "nodes": [
                {"i": 0, "d": 0, "c": "android.widget.TextView", "t": "Anna\nonline", "f": 1024},
                {"i": 1, "d": 1, "c": "androidx.recyclerview.widget.RecyclerView", "f": 256},
                {"i": 2, "d": 2, "c": "android.widget.TextView", "t": "See you at 5"},
                {"i": 3, "d": 1, "c": "android.widget.EditText", "t": "hello", "h": "Message", "v": "org.example.chat:id/input", "f": 132},
                {"i": 4, "d": 1, "c": "android.widget.EditText", "t": "hunter2", "f": 516},
                {"i": 5, "d": 1, "c": "android.widget.ImageButton", "cd": "Send", "v": "org.example.chat:id/send", "f": 65},
                {"i": 6, "d": 1, "c": "android.widget.Switch", "t": "Mute", "f": 25},
                {"i": -1, "d": 0, "c": "x", "t": "dropped"}
            ]
        }));
        let screen = screen(&message).unwrap();
        assert_eq!(screen.snapshot, 3);
        assert_eq!(screen.lines.len(), 7);

        let rendered = render(&screen, 16000);
        assert!(!rendered.truncated);
        let lines: Vec<&str> = rendered.text.lines().collect();
        assert_eq!(lines[0], r#"[3.0] text "Anna\nonline" {heading}"#);
        assert_eq!(lines[1], "  [3.1] list {scrolls}");
        assert_eq!(lines[2], r#"    [3.2] text "See you at 5""#);
        assert_eq!(
            lines[3],
            r#"  [3.3] field "hello" hint "Message" #input {focused}"#
        );
        assert_eq!(lines[4], "  [3.4] field password (hidden)");
        assert_eq!(lines[5], "  [3.5] button (Send) #send {disabled}");
        assert_eq!(lines[6], r#"  [3.6] switch "Mute" {checked}"#);
        assert!(!rendered.text.contains("hunter2"));
        assert!(render(&screen, 60).truncated);

        let mut bad = message;
        bad.insert("snap".into(), json!(0));
        assert!(super::screen(&bad).is_none());

        assert_eq!(
            parse_node("3.17"),
            Some(NodeRef {
                snapshot: 3,
                index: 17
            })
        );
        assert_eq!(parse_node("17"), None);
        assert_eq!(parse_node("0.1"), None);
        assert_eq!(parse_node("3.17; rm"), None);

        assert!(is_external_host(b"keel-mcp\0--socket\0--external\0"));
        assert!(!is_external_host(b"keel-mcp\0--stdio\0"));
    }
}
