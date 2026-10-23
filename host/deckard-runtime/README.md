# deckard-runtime (session slice)

This crate is a native Rust candidate for the Deckard-owned user-session
launcher. It is **not selected by the package or service units** and is not a
complete replacement for upstream Waydroid's Python control plane.

`deckard-runtime session start` validates the graphical user and Wayland
socket, owns `id.waydro.Session` without queueing, exports `/SessionManager`
`Stop`, starts the existing system container unit, and sends the 14-field
`Start(a{ss})` payload expected by the installed container manager. Normal
signals and D-Bus Stop request `Stop(false)`; SIGUSR1 exits without stopping
the container, matching upstream. `session stop` requests the session method,
falling back to `Stop(true)` on the system bus.

`deckard-runtime prop get KEY` and `prop set KEY VALUE` are root-only native
operations. They inspect the session state through D-Bus, unfreeze/refreeze
when required, and run bounded `lxc-attach` calls to Android's getprop/setprop.
They deliberately fail for an unprivileged graphical user: this slice does not
yet contain an authenticated privileged property broker or Binder adapter.

After the container starts, the session starts a nonblocking native Android
catalog export. It waits for boot completion and user 0 credential-encrypted
storage unlock, reads `IPlatform.getAppsInfo` through libgbinder, and atomically
updates `waydroid.<package>.desktop` launchers under the graphical user's
`XDG_DATA_HOME/applications`. Existing localized/custom fields are retained;
the launcher identity, `waydroid app launch` and app-settings action, categories,
form factor, system-app visibility, and Android-written icon paths match the
installed Python exporter. An empty or malformed snapshot leaves existing
launchers untouched. `deckard-runtime session export` reruns this export for
the current non-root user; it is useful for a manual repair or isolated test.

The upstream `IUserMonitor` callback and package-change monitor, optional
clipboard bridge, auto-ADB, session-wide boot-completion wait, and host
timezone propagation are **not implemented**. The icon PNGs are created by
Android; this crate references them but does not generate them. The Python container service,
Android app launch/property Binder client, and image/network lifecycle also
remain. Replacing the installed wrapper with this binary now would regress
those functions. Do not activate it until those contracts are implemented and
verified on device.

Run the D-Bus contract tests on an isolated bus with:

```
dbus-run-session -- env DECKARD_RUNTIME_PRIVATE_BUS=1 cargo test -p deckard-runtime --frozen
```
