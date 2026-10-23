# deckard-container prototype

This is an independent, root-side Rust **prototype**. It is not in the
production Cargo workspace, no systemd unit is installed, and it must not be
deployed until the parity gaps below are closed and tested on a non-production
device. The experimental `deckard-containerd` binary requires root and an
explicit `--experimental-run` argument; it is not selected by any service. It
never executes the Waydroid Python CLI or network script.

## Implemented

- Direct `lxc-info`, detached `lxc-start`, `lxc-stop`, `lxc-freeze`, and
  `lxc-unfreeze` commands with bounded state polling. Container name and LXC
  root are explicit; a future Deckard configuration can use `deckard` rather
  than silently mutating the existing `waydroid` instance.
- Read-only loop mounts of `system.img` and `vendor.img`; optional writable
  overlayfs layers in the installed Waydroid ordering; refusal to mount over
  existing rootfs mounts; path/symlink/overlay-option validation; reverse-order
  rollback and nested unmount ordering.
- Direct `ip`, `iptables`, and `dnsmasq` bridge/NAT/DHCP setup and teardown.
  The installed Hackberry image on 2026-10-23 has `iptables -w` but no `nft`.
  Rules target the configured bridge, and partial firewall setup is rolled
  back. The default subnet matches the installed script's `192.168.240.0/24`,
  so it must **not** run alongside the production Waydroid network.
- Startup ordering is network, mounts, then LXC. Shutdown waits for LXC to stop
  before unmounting. It intentionally does not guess that an LXC launch failed
  harmlessly: if LXC remains running after an error, mounts stay in place.
- An inactive zbus 5.19.0 system-bus adapter owns `id.waydro.Container` at
  `/ContainerManager` when explicitly run. It implements the installed
  `id.waydro.ContainerManager` `Start(a{ss})`, `Stop(b)`, `Freeze()`,
  `Unfreeze()`, and `GetSession() -> a{ss}` signatures. Credentials come from
  the D-Bus daemon for the unique sender name, never from caller-supplied
  fields. Non-root starts require exact `user_id` and `pid` matches; subsequent
  control and session reads require that same session owner (or root). This is
  deliberately stricter than Python's unauthenticated `Stop`/freeze paths.
  `Stop(true)` signals the original session through a retained Linux pidfd.
  Blocking LXC/mount/network calls run outside the D-Bus executor.
- A private-bus test exercises the actual wire methods and credential lookup
  with an inert backend, so no host network, mounts, or LXC state is changed.
- Root-only `GetProperty(s) -> s` and `SetProperty(ss)` methods use direct,
  three-second-bounded `lxc-attach --clear-env -- /system/bin/getprop|setprop`.
  Keys and values are validated before execution, stopped containers are
  rejected, and a frozen container is thawed then refrozen. The device's
  installed `lxc-attach ... getprop sys.boot_completed` returned `1` in a
  read-only check; `setprop` has **not** been exercised on the device.

## Not parity-complete

The installed Python source at `/usr/lib/waydroid/tools/actions/container_manager.py`,
`tools/actions/initializer.py`, `tools/helpers/{lxc,images,mount}.py`,
`tools/interfaces/IHardware.py`, and `data/scripts/waydroid-net.sh` is the
reference. Specifically unsupported:

- **System D-Bus initialization and rename**: no `/Initializer`
  `id.waydro.Initializer.Init(a{ss})` or `Cancel()` and no `ProgressChanged(s)`,
  `Finished()`, or `Interrupted()` signals or Polkit check. The adapter retains
  the old bus/interface names for client compatibility; no renamed
  `org.marathonos.Deckard.Container` API or migration alias exists yet.
- **User-session property broker**: these property methods intentionally
  reject non-root D-Bus callers, including the current user session. There is
  no narrowly scoped authenticated broker or client migration; Python's
  `waydroid prop get` boot polling remains in production. The native methods
  also use Android `getprop`/`setprop` rather than the Python `IPlatform`
  Binder transaction path, so behavioral parity beyond the observed read is
  not established.
- **Android Binder HAL `lineageos.waydroid.IHardware` / `waydroidhardware`**:
  transaction 1 `enableNFC`, 2 `enableBluetooth`, 3 `suspend`, 4 `reboot`,
  5 `upgrade` (32-bit timestamps), 6 `upgrade2` (64-bit timestamps), and
  7 `shutdownRequest` are all unimplemented. Binder presence/re-registration
  and replies are also absent.
- Binder/ashmem driver probing and permissions, LXC static/node/session config
  generation, session-owned Wayland/Pulse/data binds, host permissions,
  `waydroid_base.prop` generation, `waydroid.prop` bind, AIDL selection,
  optional host EGL/ODM binds, NFC and sensor process handling,
  init/OTA/image replacement, and image hash verification are not implemented.
- Network supports the installed IPv4 iptables path only, not nftables,
  iptables implementations without `-w`, optional IPv6, bridge interface
  attachment policy, SELinux `restorecon`, or network coexistence. It uses a
  configurable bridge name but fixed subnet and DHCP parameters.
- Mount path validation is preflight rather than descriptor-pinned; a hostile
  concurrent root process could swap a path between validation and `mount`.
  The D-Bus adapter validates caller identity but does not yet apply or verify
  session paths. Descriptor-pinned mounts and ownership/permission checks are
  required before deployment.

Run `cargo test --manifest-path host/deckard-container/Cargo.toml` and
`cargo clippy --manifest-path host/deckard-container/Cargo.toml --all-targets -- -D warnings`.
The private-bus test requires `dbus-daemon` on the test host.
