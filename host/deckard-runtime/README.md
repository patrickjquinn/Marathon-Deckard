# deckard-runtime

The native Deckard user session, replacing `waydroid session`. The Deckard
package installs `deckard-session.service`; `deckard-launch` starts it on
demand and `deckard-bridge` honors the start-at-login preference.

`deckard-runtime session start` validates the graphical user and Wayland
socket, owns `id.waydro.Session` without queueing, exports `/SessionManager`
`Stop`, starts `deckard-container.service` and sends the 14-field
`Start(a{ss})` the container manager expects. Signals and D-Bus `Stop` request
`Stop(false)`; SIGUSR1 exits without stopping the container, as upstream.
`session stop` calls the session's `Stop`, falling back to `Stop(true)` on the
system bus. If Shell replaces its compositor socket the session stops Android
and exits with failure, so systemd restarts it against the new compositor.

Once the container runs, the session:

- registers `waydroidusermonitor` (`lineageos.waydroid.IUserMonitor`), which
  Android's framework calls on user unlock and package changes. Unlock rewrites
  every `waydroid.<package>.desktop` launcher under
  `XDG_DATA_HOME/applications`; a package change rewrites or removes one
  (`IPlatform.getAppInfo`). Up to 64 events queue; beyond that the next idle
  tick rereads the whole catalog. The catalog is not polled.
- exports the launchers once after boot completion and user 0 unlock, in case
  Android unlocked before the service was registered. Launchers keep localized
  and custom fields; identity, `deckard-launch` commands, categories, form
  factor, system-app visibility and Android-written icon paths match the
  Python exporter. An empty or malformed snapshot changes nothing.
- sets `persist.sys.timezone` from `org.freedesktop.timedate1` `Timezone`
  through `IPlatform` setprop, then again on each change (what
  `marathon-waydroid-session` did with `waydroid prop set`).

Android properties are read and written with `deckard-launch prop`.

Not implemented, because Marathon does not use them: the clipboard bridge
(Shell keeps its own clipboard and the device has no wl-clipboard for
upstream's pyclip), auto-ADB (`auto_adb = False`) and upstream's
notification forwarder (`deckard-bridge` handles notifications).

```
dbus-run-session -- env DECKARD_RUNTIME_PRIVATE_BUS=1 cargo test -p deckard-runtime --frozen
```
