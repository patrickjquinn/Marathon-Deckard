# deckard-notifications

Standalone Rust adapter for Android notifications and Marathon's
`org.freedesktop.Notifications` service. It is deliberately not wired into the
production daemon. The caller supplies an existing D-Bus connection and an
`mpsc::Sender<BridgeEvent>`; the caller owns bridge framing, authentication,
and transport. `NotificationAdapter::listen` subscribes to Hub signals, while
`post`, `remove`, and `sync` accept already-validated `deckard-core` inputs.
Use a dedicated connection with a five-second method timeout to match the old
handler; close that connection on shutdown to stop the signal listener.
Delayed dismissals use one timer worker per adapter and a 512-entry deadline
queue; repeated timers for the same Hub ID are coalesced. A full queue applies
backpressure rather than dropping an Android dismissal.

The state path should be the existing
`~/.local/state/deckard/notifications.json`; the icon directory should be the
existing `~/.cache/deckard/icons`. Never run this adapter in parallel with the
C++ notification handler against the same Hub and state file.

The adapter is covered by the root Cargo workspace tests. Run
`cargo test -p deckard-notifications --locked` and
`cargo clippy -p deckard-notifications --all-targets --locked -- -D warnings`.

Not covered here: icon ingestion, transient toast banners, the bridge socket,
daemon lifecycle, service packaging, and the remaining C++ notification
handler's removal. The adapter keeps the legacy `waydroid.<pkg>` desktop-entry
hint until the shell, desktop files, and app IDs are migrated together.
