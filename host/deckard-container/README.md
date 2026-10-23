# deckard-container

The root-side Rust container manager that replaces Waydroid's Python one.
`deckard-container.service` runs `deckard-containerd --production-run`; the
package's `id.waydro.Container.service` D-Bus activation names that unit. It
runs the Android container on the HackberryPi. It never executes the Waydroid
Python CLI, its network script or anything under `/usr/lib/waydroid`.

Modes: `--production-run` serves the `/var/lib/waydroid` instance (and keeps
serving `Init` while it is uninitialized); `--experimental-run` a scratch
`/var/lib/deckard` instance without Binder setup or the hardware service. The
read-only `--audit-production` checks the session config, Android properties
and Binder nodes against the running manager; `--audit-hardware` probes the
Binder service manager without registering anything.

## Bus contract

`id.waydro.Container` at `/ContainerManager` (`id.waydro.ContainerManager`):
`Start(a{ss})`, `Stop(b)`, `Freeze()`, `Unfreeze()`, `GetSession() -> a{ss}`,
plus root-only `GetProperty(s) -> s` and `SetProperty(ss)` (bounded
`lxc-attach` getprop/setprop). Credentials come from the bus daemon for the
sender. Non-root `Start` needs matching `user_id` and `pid`; `Stop` and
`Freeze` need that session owner or root. `GetSession` and `Unfreeze` also
admit `/usr/libexec/deckard-launch` when its `/proc/PID/exe` inode matches the
root-owned binary (every directory from `/usr` root-owned, not group/world
writable) and it runs under the session owner's `user@UID.service`. Python
admits more: its `Stop` and freeze are unauthenticated.

`/Initializer` (`id.waydro.Initializer`): `Init(a{ss})`, `Cancel()`,
`ProgressChanged(s)`, `Finished()`, `Interrupted()`. See below.

The package ships `host/packaging/id.waydro.Container.conf` (root may own the
name) and `host/packaging/id.waydro.Container.policy` (the polkit action).

## Start and stop

Production `Start` does what upstream's `do_start` does on a mainline host:

- reads `waydroid.cfg` (`images_path`, `mount_overlays`, `suspend_action`, with
  upstream's defaults) and refuses an uninitialized instance;
- if no Binder node of a kind exists: `modprobe binder_linux` when the kernel
  lacks binderfs, mounts binderfs on `/dev/binderfs` and links its default
  devices into `/dev` (allocating others through `binder-control` is not
  supported); then chmods the configured nodes 0666 and the GPU, video,
  framebuffer, DMA-heap and sync nodes 0777, as upstream;
- pins the session's Wayland socket, Pulse socket and data directory (below),
  writes `config_session` and `waydroid.prop`;
- brings up the `waydroid0` bridge (`ip`, `iptables -w`, `dnsmasq`), mounts
  `system.img` and `vendor.img` read-only with the overlays and the property
  bind, starts LXC;
- records the image SDK's `binder_protocol`/`service_manager_protocol` in
  `waydroid.cfg` (upstream `set_aidl_version`) and starts `waydroidhardware`.

Stop waits for LXC to stop before unmounting and tearing the network down; if
LXC is still running after an error, mounts stay. `SIGTERM`/`SIGINT` stop a
session this daemon started, never an unowned container.

Not ported, as no Marathon device needs them: `waydroid-sensord`,
`cgroup-lite`, the schedtune probe, the nfcd stop/start hack, IPv6, nftables,
bridge attachment policy and the Halium (non-mainline) branches.

## Mount paths

Session paths belong to the user, who could swap one for a symlink between a
check and the bind and hand Android a host path. Each is resolved once, opened
`O_PATH|O_NOFOLLOW` with `openat2(RESOLVE_NO_SYMLINKS)`, checked on the
descriptor (owner, socket or directory), cloned with `open_tree` and attached
with `move_mount` onto `<runtime>/session/{wayland-0,pulse-native,data}`, a
root-owned 0700 stage. `config_session` names only the stage.

Image, overlay and property mounts go through mount(8) by path, but every
component must be owned by root and not group/world writable (sticky
directories excepted), so only root could race them.

## Initializer

`Init` takes upstream's `system_channel`, `vendor_channel` and `system_type`.
Images reach the device by hand (one `waydroid init -s VANILLA` download into
`/var/lib/waydroid/images`), and Marathon, not Android's updater, replaces
them, so Init never downloads: custom channels are `NotSupported`, other
request shapes `InvalidArgs`, and the default channels select nothing.

- Preinstalled directories (`/etc/waydroid-extra/images`,
  `/usr/share/waydroid-extra/images`) win, clear the OTA keys and disable the
  updater; otherwise `/var/lib/waydroid/images` is used and the configured OTA
  keys are kept. No images: an explicit error.
- The LXC `config` comes from the templates in `lxc/` (upstream's
  `config_base`/`config_3`/`config_4` with Marathon's `cgroup:rw` and its two
  hooks, and `waydroid.seccomp`), compiled in. `config_nodes`, `waydroid.cfg`
  and `waydroid_base.prop` follow upstream's `generate_nodes_lxc_config`,
  `setup_config` and `make_base_props`; `[properties]` overrides apply;
  `overlay_rw` and `overlay_work` are dropped as upstream does. Everything is
  published in one transaction that restores its predecessors on failure.
- Non-root callers need polkit `id.waydro.Initializer.Init` (asked for the
  sender's bus name, interactively, 300 s). Unlike upstream there is no
  exemption for the default channels, because Init restarts the session.
- A tracked session is stopped, the instance initialized and the session
  started again; if the restart fails the session is dropped and told to exit.
  An untracked running container is refused.
- One job at a time. `Cancel` (root or the caller of `Init`) is cooperative,
  checked before each step and a 10 s deadline before publication.

## Hardware service

`waydroidhardware` (`lineageos.waydroid.IHardware`), served through the shared
libgbinder binding in `deckard_launch::binder` while the container runs:

- `suspend` freezes (or stops, with `suspend_action = stop`), `reboot` restarts
  LXC, `shutdownRequest` stops, or restarts for a reason starting with `1`;
  these go to a queue of 16, beyond which Android gets `WOULD_BLOCK`;
- `enableNFC`/`enableBluetooth` are no-ops upstream ("not implemented") and
  answer `false` without touching the host;
- `upgrade`/`upgrade2` are refused with `INVALID_OPERATION`;
- unknown codes get `UNKNOWN_TRANSACTION`.

Run `cargo test -p deckard-container` (the private-bus tests need
`dbus-daemon`) and `cargo clippy -p deckard-container --all-targets -- -D warnings`.
