# deckard-container

The root-side Rust container manager that replaces Waydroid's Python one.
`deckard-container.service` runs `deckard-containerd --production-run`, and
the package's `id.waydro.Container.service` D-Bus activation names that unit
(replacing Waydroid's, which ran the Python manager). It runs the Android
container on the HackberryPi today; the gaps below remain before full parity.
`--production-run` targets the initialized `/var/lib/waydroid` instance and
refuses missing configuration; `--experimental-run` is for scratch instances. The read-only
`--audit-production` mode checks its generated session config and properties
against the running manager. `--audit-hardware` probes the configured Binder
service manager without registering a service. It never executes the Waydroid
Python CLI or network script.

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
- `Start` now validates session-owned Wayland/Pulse/data paths, writes the
  authenticated LXC session binds and Android properties atomically, and mounts
  the resulting `waydroid.prop` after images and overlays. A zero requested
  density retains the image default. The read-only audit matched both installed
  files on the Hackberry Pi on 2026-10-23. `SIGTERM`/`SIGINT` stop a session
  started by this daemon, but do not stop an unowned existing container.
- The zbus 5.19.0 system-bus adapter owns `id.waydro.Container` at
  `/ContainerManager`. It implements the installed
  `id.waydro.ContainerManager` `Start(a{ss})`, `Stop(b)`, `Freeze()`,
  `Unfreeze()`, and `GetSession() -> a{ss}` signatures. Credentials come from
  the D-Bus daemon for the unique sender name, never from caller-supplied
  fields. Non-root starts require exact `user_id` and `pid` matches; subsequent
  `Stop` and `Freeze` require that same session owner (or root). `GetSession`
  and `Unfreeze` also admit the separate `deckard-launch` CLI when its D-Bus
  daemon-provided UID matches the active session, its kernel `/proc/PID/exe`
  inode matches the root-owned, non-group/world-writable
  `/usr/libexec/deckard-launch`, and both processes reside under the same
  `user@UID.service` cgroup. Every directory from `/usr` to the binary must
  also be root-owned and non-group/world-writable, and the binary may not be
  a symlink. This is deliberately narrower than granting all
  same-UID processes control, and stricter than Python's unauthenticated
  `Stop`/freeze paths. If the launcher is installed elsewhere, this policy
  fails closed until its exact installed path is updated. It does not admit
  other CLI executables or a launcher from another user manager.
  The Pi initially had UID 1000-owned `/usr` and `/usr/libexec` directories;
  the user-session cutover repaired these parent owners and installs the
  launcher at the trusted path.
  `Stop(true)` signals the original session through a retained Linux pidfd.
  Blocking LXC/mount/network calls run outside the D-Bus executor.
- Private-bus tests exercise the actual wire methods and credential lookup
  with an inert backend, including a launcher-shaped process permitted to
  read/unfreeze while another same-UID D-Bus process is denied. No host
  network, mounts, or LXC state is changed.
- Root-only `GetProperty(s) -> s` and `SetProperty(ss)` methods use direct,
  three-second-bounded `lxc-attach --clear-env -- /system/bin/getprop|setprop`.
  Keys and values are validated before execution, stopped containers are
  rejected, and a frozen container is thawed then refrozen. The device's
  installed `lxc-attach ... getprop sys.boot_completed` returned `1` in a
  read-only check; `setprop` has **not** been exercised on the device.
- An offline local-image initializer now validates existing ext-format
  `system.img` and `vendor.img`, renders the installed LXC base/v3/v4
  templates for an explicit work directory, container name, bridge, arch,
  and binder drivers, discovers static host device nodes, copies the seccomp
  profile, and publishes `config`, `config_nodes`, and an empty
  `config_session` as one new LXC instance directory. It refuses to replace
  an existing instance. The only CLI entry is root-only
  `--experimental-prepare-local`; it does not start, stop, or mount anything.
- The same bus server exposes `/Initializer`
  with the installed `id.waydro.Initializer` wire contract:
  `Init(a{ss})`, `Cancel()`, `ProgressChanged(s)`, `Finished()`, and
  `Interrupted()`. Credentials are resolved from the unique D-Bus sender.
  Both methods require root; unlike upstream, the default-channel exemption
  and interactive Polkit authorization are deliberately unsupported.
  Upstream's three-field `Init` request runs the local-image initializer on
  the production instance: `system.img` and `vendor.img` must already be in
  one of the Waydroid image directories, and the container must be stopped.
  Its channel and type fields are validated but select nothing (no OTA). The
  only other accepted request is `mode=prepare-local-scratch` plus
  exactly `work`, `images`, `templates`, `name`, `bridge`, `arch`, `lxc_major`,
  `binder`, `vndbinder`, and `hwbinder`. `work` must be an existing direct child
  of a real `/tmp/deckard-init-scratch` directory, with `work/lxc` present.
  `images` and `templates` must each be direct children of `work`; all these
  directories must be daemon-owned and not group/world-writable.
  This runs the existing offline local-image preparation there only. It does
  not write `/var/lib/waydroid` or `/var/lib/deckard`.
  One worker is allowed at a time; a second `Init` fails. `Cancel` sets a
  cooperative flag and returns without joining or blocking the D-Bus loop.
  The worker checks cancellation and a 10-second deadline before each write
  and final publication, removes staging files on failure, and emits
  `Interrupted` rather than `Finished`. Individual kernel filesystem calls
  and D-Bus signal sends cannot be preempted, so this is not a hard wall-clock
  timeout. Cancellation after publication cannot undo the new scratch output.
  Private-bus tests exercise the wire signatures, credential check, unsupported
  legacy request, and scratch completion without claiming the production bus.

## Not parity-complete

The installed Python source at `/usr/lib/waydroid/tools/actions/container_manager.py`,
`tools/actions/initializer.py`, `tools/helpers/{lxc,images,mount}.py`,
`tools/interfaces/IHardware.py`, and `data/scripts/waydroid-net.sh` is the
reference. Specifically unsupported:

- **Production initialization and rename**: `Init` prepares local images only:
  no default or custom OTA channels, `system_type` selection, URL fetch/hash
  verification, image replacement or running-container coordination. There is no Polkit check or authorization path
  for non-root callers. The adapter retains the old bus/interface names for
  client compatibility; no renamed
  `org.marathonos.Deckard.Container` API or migration alias exists yet.
- **User-session property broker**: these property methods intentionally
  reject non-root D-Bus callers, including the current user session. There is
  no narrowly scoped authenticated broker or client migration; Python's
  `waydroid prop get` boot polling remains in production. The native methods
  also use Android `getprop`/`setprop` rather than the Python `IPlatform`
  Binder transaction path, so behavioral parity beyond the observed read is
  not established.
- **Android Binder HAL `lineageos.waydroid.IHardware` / `waydroidhardware`**:
  the `hardware` module decodes all seven transaction codes, registers with
  Binder, queues requests boundedly and is started with the container
  lifecycle. `Upgrade` is refused (Android image upgrade is not
  implemented). NFC and Bluetooth requests answer success without toggling
  anything on the host; their intended behavior is still to be defined. The
  service-manager probe passes; not every transaction has been exercised.
- Binder/ashmem driver probing and permissions, host permissions,
  `waydroid_base.prop` generation, AIDL selection,
  optional host EGL/ODM binds, NFC and sensor process handling,
  init/OTA/image replacement, and image hash verification are not implemented.
  The static generator only covers the local-image MAINLINE branch, uses
  existing trusted LXC templates, and requires a pre-existing `WORK/lxc`
  directory. It does not create rootfs/overlay directories, select or verify
  downloaded OTA images, update `waydroid.cfg`, or coordinate a running
  container. The scratch initializer publishes an empty `config_session`;
  authenticated `Start` builds it for production, but the scratch output is
  not boot-ready on its own.
- Network supports the installed IPv4 iptables path only, not nftables,
  iptables implementations without `-w`, optional IPv6, bridge interface
  attachment policy, SELinux `restorecon`, or network coexistence. It uses a
  configurable bridge name but fixed subnet and DHCP parameters.
- Mount path validation is preflight rather than descriptor-pinned; a hostile
  concurrent root process could swap a path between validation and `mount`.
  The D-Bus adapter validates caller identity and session path ownership, but
  descriptor-pinned mounts are still required before deployment.

Run `cargo test -p deckard-container --locked` and
`cargo clippy -p deckard-container --all-targets --locked -- -D warnings`.
The private-bus test requires `dbus-daemon` on the test host.
