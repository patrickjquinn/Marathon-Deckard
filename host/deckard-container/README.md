# deckard-container prototype

This is an independent, root-side Rust **library prototype**. It is not in the
production Cargo workspace, no executable or systemd unit is installed, and it
must not be deployed until the parity gaps below are closed and tested on a
non-production device. It never executes the Waydroid Python CLI or network
script.

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

## Not parity-complete

The installed Python source at `/usr/lib/waydroid/tools/actions/container_manager.py`,
`tools/actions/initializer.py`, `tools/helpers/{lxc,images,mount}.py`,
`tools/interfaces/IHardware.py`, and `data/scripts/waydroid-net.sh` is the
reference. Specifically unsupported:

- **System D-Bus `id.waydro.Container`**: no service owner, no
  `/ContainerManager` `id.waydro.ContainerManager.Start(a{ss})`, `Stop(b)`,
  `Freeze()`, `Unfreeze()`, or `GetSession()`. `Start` has no sender UID/PID
  authorization yet. No `/Initializer` `id.waydro.Initializer.Init(a{ss})` or
  `Cancel()` and no `ProgressChanged(s)`, `Finished()`, or `Interrupted()` signals
  or Polkit check. No renamed `org.marathonos.Deckard.Container` API either.
- **Android Binder HAL `lineageos.waydroid.IHardware` / `waydroidhardware`**:
  transaction 1 `enableNFC`, 2 `enableBluetooth`, 3 `suspend`, 4 `reboot`,
  5 `upgrade` (32-bit timestamps), 6 `upgrade2` (64-bit timestamps), and
  7 `shutdownRequest` are all unimplemented. Binder presence/re-registration
  and replies are also absent.
- Binder/ashmem driver probing and permissions, LXC static/node/session config
  generation, session-owned Wayland/Pulse/data binds, host permissions,
  `waydroid_base.prop` generation, `waydroid.prop` bind, AIDL selection,
  optional host EGL/ODM binds, NFC and sensor process handling, session SIGUSR1,
  init/OTA/image replacement, and image hash verification are not implemented.
- Network supports the installed IPv4 iptables path only, not nftables,
  iptables implementations without `-w`, optional IPv6, bridge interface
  attachment policy, SELinux `restorecon`, or network coexistence. It uses a
  configurable bridge name but fixed subnet and DHCP parameters.
- Mount path validation is preflight rather than descriptor-pinned; a hostile
  concurrent root process could swap a path between validation and `mount`.
  No untrusted D-Bus caller reaches these paths today, but descriptor-pinned
  mounts and ownership/permission checks are required before deployment.

Run `cargo test --manifest-path host/deckard-container/Cargo.toml` and
`cargo clippy --manifest-path host/deckard-container/Cargo.toml --all-targets -- -D warnings`.
