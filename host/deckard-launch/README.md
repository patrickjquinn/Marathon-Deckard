# deckard-launch

Native Rust replacement for the generated desktop-file commands
`waydroid app launch`, `waydroid app intent`, and the user-facing `waydroid prop`
operations. The Deckard package installs it at `/usr/libexec/deckard-launch`;
the native session exporter writes that path into Android desktop files.
It also installs a `/usr/bin/deckard-launch` symlink because Shell normalizes
absolute desktop `Exec` commands to their executable name. The prototype
container authorizer validates the `/usr/libexec` executable inode, not the
user-writable symlink path.

```text
deckard-launch app launch PACKAGE
deckard-launch app intent ACTION URI
deckard-launch prop get KEY
deckard-launch prop set KEY VALUE
```

The default config is `/var/lib/waydroid/waydroid.cfg`. The native session
unit is `deckard-session.service`. `DECKARD_CONFIG` and `DECKARD_SESSION_UNIT`
can override those values for integration. The upstream `id.waydro.*` D-Bus and
`lineageos.waydroid.IPlatform` Binder names remain unchanged while the Android
image still exposes those contracts; service *unit* renaming does not rename
these wire protocols.

## Observed device contract

Inspected on the Hackberry Pi from `/usr/lib/waydroid/tools/actions/app_manager.py`,
`/usr/lib/waydroid/tools/interfaces/IPlatform.py`,
`/usr/lib/waydroid/tools/actions/session_manager.py`, and
`/usr/lib/waydroid/tools/services/user_manager.py`:

- Launch first checks the session name on the user bus, starts a session if
  absent, reads the authenticated system-bus container state, and asks it to
  `Unfreeze` only when frozen. A denied or failed required unfreeze aborts
  before Binder. It then resolves `waydroidplatform` on the configured Binder
  node/protocol.
- `IPlatform` Binder transactions are `getprop=1`, `setprop=2`, `launchApp=7`,
  `settingsPutString=9`, and `launchIntent=13` under interface
  `lineageos.waydroid.IPlatform`. The arguments are UTF-16 Binder strings.
- App launch sets `waydroid.active_apps` **before** `launchApp`. Intent launch
  uses the package returned by `launchIntent`, maps `android` to `Waydroid`,
  and does not update active apps when the return is empty.
- Both launch paths set secure setting `policy_control` to
  `immersive.status=*` for single-window mode and `immersive.full=*` otherwise.
- Property get/set on an already-frozen container unfreeze, transact, then
  freeze it again. App launch leaves the container running.
- The session startup checks the Wayland socket. This implementation checks
  that the socket accepts a connection before requesting `StartUnit` through
  the user systemd D-Bus manager. Every app launch waits for
  `sys.boot_completed=1` and `sys.user.0.ce_available=true`, including when
  the session bus name was already acquired during Android boot.
- App launch calls Shell `ShowAndroidApp` before the Binder request so the
  Shell owns the future window even if the Binder call is slow. It does not
  reassert foreground afterward, which could steal focus if the user switched
  apps while Android was starting. Intent launch can only request the handoff
  after Binder returns the resolved package.

`src/binder.rs` is the one libgbinder binding of the host binaries: this
`IPlatform` client (also `getAppsInfo`/`getAppInfo` for the session's launcher
export) and the local services Android calls, `waydroidhardware` in
`deckard-containerd` and `waydroidusermonitor` in `deckard-runtime`. It loads
the device's `libgbinder.so.1` and `libglib-2.0.so.0` at run time and does not
invoke `waydroid`, Python, `am start`, or a shell. The ABI declarations were
checked against the [upstream libgbinder headers](https://github.com/mer-hybris/libgbinder/tree/master/include).

On Alpine, **do not ship Rust's default static-musl output**: static musl
cannot `dlopen` the Binder shared library. The crate deliberately rejects
that target configuration. Build an AArch64 dynamic-musl executable with:

```sh
CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER=musl-gcc \
RUSTFLAGS='-C target-feature=-crt-static' cargo build \
  -p deckard-launch \
  --target aarch64-unknown-linux-musl --release --frozen
```

Verify that `readelf -l` reports `/lib/ld-musl-aarch64.so.1` rather than the
glibc loader; otherwise the build is not deployable to Alpine even though the
Rust target name says musl. The package must keep `libgbinder.so.1` and
`libglib-2.0.so.0` installed.

## Tests

Run `cargo test -p deckard-launch --locked` and
`cargo clippy -p deckard-launch --all-targets --locked -- -D warnings`.
Unit tests use an injected backend to verify Binder operation order, intent
return behavior, immersive policy, frozen property behavior, config parsing,
and Wayland path validation. They do not prove device Binder round trips.

## Cutover gaps

- When the Python launcher started a stopped session, its callback waited for
  `IUserMonitor.userUnlocked`. This client waits for session D-Bus ownership,
  `waydroidplatform`, `sys.boot_completed=1` and
  `sys.user.0.ce_available=true` instead; the session now serves
  `waydroidusermonitor` but does not relay unlock to the launcher.
- An intent relayed through the nonvisual DeckardSystem bridge does not request
  a Shell window.
- App install/remove/list and the other Python `waydroid` subcommands are not
  provided, and the Android Binder and D-Bus contracts keep their Waydroid
  names. See `docs/rust-cutover-baseline.md` for the launch evidence.
