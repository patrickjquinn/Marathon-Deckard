Deckard

Android runtime for Marathon OS. Technical design.

Draft, 18 October 2026. Status: proposal.

Repository: https://github.com/patrickjquinn/Marathon-Deckard

# 1. Summary

Deckard runs Android apps on Marathon OS as if they were Marathon apps. From the host's point of view Deckard is one ordinary Wayland client and one ordinary D-Bus client: it speaks the same protocols, portals and power interfaces as a native Marathon app and gets no special treatment. From the user's point of view each Android app is a first-class Marathon app with its own switcher card, Active Frame, Hub notifications, keyboard, permission prompts and colour palette, and Android's own chrome never appears.

Two rules drive the architecture. First, anything the host already owns (graphics drivers, audio, network, fonts, certificates, time, location, power policy) is supplied from the host and not duplicated inside Android. Second, anything in the Android image that exists only to be a phone operating system (launcher, keyguard, SystemUI chrome, keyboard, settings, stock apps) is removed in favour of Marathon's native components. What remains inside the container is the Android application runtime: zygote, ART, system_server, SurfaceFlinger with a Wayland hwcomposer, keystore, Codec2, WebView and microG.

Deckard builds on Waydroid for the container, binder plumbing and Wayland hwcomposer, and takes rootless operation, boot-state caching, binderfs in a user namespace, memfd in place of ashmem, and the host-packaged bionic graphics stack from Valve's Lepton. The host side is written fresh under Apache-2.0. The Android image is a GPL-3.0 fork of Waydroid's vendor and hardware trees.

# 2. Goals and non-goals

## Goals

Android apps appear in Marathon's app grid, switcher and Active Frames as first-class apps, one window per Android task.

All navigation is Marathon's: edge gestures, back, peek and minimise. Android's navigation bar, status bar and freeform caption never appear.

Android notifications land in the Hub with actions, inline reply and conversation threading; media sessions appear as Marathon media controls.

Android apps use Marathon's keyboard, permission prompts, share sheet and URL handlers, and render Marathon's exact colour tokens.

microG is built in. Push notifications work under Marathon's opportunistic suspend, with every Android wake lock accounted for by Marathon's power service.

Everything built for Deckard that is not Android-specific (surface protocol, keyboard over text-input-v3, inline reply, portals) also serves native Marathon and Linux apps.

## Non-goals

Play Integrity. Apps that require it (many banking apps) will not work under microG. This is a known, unfixable limit.

Per-app containers. One persistent Android system is required for microG, push and intents.

A full Android UI mode. Bluetooth passthrough in early milestones.

# 3. Design principle: Android is an ordinary Marathon app

The host sees one Wayland connection and one D-Bus connection. Those connections use standard protocols wherever one exists (xdg-shell, xdg-activation, idle-inhibit-v1, text-input-v3, wl_touch.cancel, org.freedesktop.Notifications, portals) and Marathon's own protocols where Marathon apps need them too. No Deckard-only daemon translates between worlds; the translation happens inside the container, in bionic, where Android's HALs and services speak Wayland and D-Bus directly.

What this buys:

One source of truth. Graphics, audio, network, fonts, certificates, time, location and power are configured once, on the host, and cannot drift inside Android.

Host updates reach Android for free. A Mesa, PipeWire, CA or tzdata update reaches Android apps without rebuilding the image.

A smaller image. Fewer AOSP components to fork, patch and carry through each rebase, which directly lowers the cost of the Android 13 to 16 move.

Less memory and power. No duplicate audio server, network stack, launcher, SystemUI or keyguard resident; fewer background processes, fewer wakeups.

One power model. Every Android wake lock lands in Marathon's accounting under the same policy as native apps.

Existing security machinery. Bus filtering, portals and Wayland are boundaries Marathon must get right for native apps anyway. Deckard adds one policy (the identity broker, section 6) rather than a parallel trust model.

Debuggable with host tools: busctl monitor, WAYLAND_DEBUG, pw-top, the journal.

Replaceable. Because the host contract is standard Wayland plus D-Bus, the Android base version, container runtime, or Android itself can be swapped without touching the shell.

The one hard boundary is the ABI. Android userspace is bionic; the host is glibc. Host shared libraries cannot be loaded inside the container. Devices, sockets and data cross the boundary directly; libraries the host should own (Mesa above all) are built for bionic and shipped as host packages versioned in lockstep with their glibc counterparts. This is exactly how Lepton ships its graphics stack.

The costs, stated once: a bionic Mesa package to maintain alongside host Mesa; stubs for ConnectivityService, TelephonyManager and a headless SystemUI that apps cannot tell from the real thing; a bionic D-Bus client, for which there is no shipping prior art; and the identity broker, which is new security design. All are bounded, one-off costs. Shipping a self-contained Android stack would cost on every update, indefinitely.

# 4. Verified constraints

The following facts were checked against primary sources (AOSP, developer.android.com, Waydroid and Lepton source trees, qtwayland, KDE and freedesktop specs, LineageOS Gerrit, USPTO TSDR) in October 2026 and fix several design decisions.

Topic

Finding

Consequence

Back on Android 13

targetSdk-gated changes apply only when the platform is at that version. On a LineageOS 20 base KEYCODE_BACK and onBackPressed reach every app regardless of targetSdk. BackEvent progress data is API 34+, and on 13 and 14 predictive-back animation is behind a developer option; Android 15+ enables it by default.

Ship complete-on-release back on Android 13 via KEYCODE_BACK. Finger-tracking back requires the Android 15+ base. Delivery runs through system_server's BackNavigationController, so a framework patch is required.

Edge-to-edge

Forced edge-to-edge for API 36 applies only on Android 16+. It does not apply on Android 13.

Correct inset reporting is still Milestone 1, because Marathon has no system bars for Android to measure.

Waydroid graphics

Waydroid does not mount host Mesa. It builds its own bionic Mesa (WayDroid fork on the lineage-18.1 branch) into the image. lxc.py mounts only device nodes, sockets, /vendor and data.

Deckard owns its GPU stack as a host package (section 14).

Lepton graphics

Lepton builds Zink and libgbm for bionic in-tree (BOARD_MESA3D_USES_MESON_BUILD) and ships the Vulkan ICD, Vulkan layers and Steam libraries as bionic builds in a host package (/usr/share/guestos/android/) bind-mounted file by file. No libhybris, no glibc bridge.

Model for marathon-mesa-android.

Lepton binder

Lepton uses the kernel binder driver through binderfs mounted by Android init inside a rootless podman user namespace (--userns=keep-id:uid=0, userns CAP_SYS_ADMIN; binderfs is FS_USERNS_MOUNT). ashmem is replaced by memfd (sys.use_memfd=true). Per-app UIDs are faked in bionic and carried in binder transaction flags.

Take binderfs-in-userns and memfd. Reject the fake-UID scheme: a persistent multi-app system needs real UIDs.

Palette

In LineageOS 20 ThemeOverlayController, system_palette is prefixed with # and parsed as a colour whenever Monet is enabled. The package-name path is a legacy fallback only used with Monet disabled.

Disable Monet and ship a static RRO over the 65 system_accent*/system_neutral* attributes (section 12). Seed colour is the fallback.

Cached-app freezer

Requires cgroup v2 freezer, per process, DeviceConfig-gated (activity_manager_native_boot use_freezer), not tied to screen-off. Inside Waydroid it is off: config_base mounts cgroup read-only, SetupCgroup fails and mUseFreezer=false. Waydroid's own container freeze is lxc-freeze writing the container's cgroup.freeze, opt-in via persist.waydroid.suspend.

Delegate a writable cgroup2 subtree with +freezer and confirm host-kernel BINDER_FREEZE support. Remove the whole-container freeze.

SystemSuspend

Current AOSP ships ISystemSuspend/IWakeLock as stable AIDL. Each client holds a unique IWakeLock; binder death cleans up. AlarmManager holds a CPU wake lock for the duration of onReceive(); FCM holds GOOGLE_C2DM (GCM_MESSAGE on 16+) until onMessageReceived() returns.

Deckard's suspend HAL implements the AIDL interface and forwards each wake lock to Marathon (section 11).

Binder on PREEMPT_RT

Binder's RT priority inheritance exists only in the Android Common Kernel; it was never mainlined. A near-mainline postmarketOS kernel has no binder PI at all.

The concern is the absence of PI and binder spinlock paths becoming sleeping locks. Measure in M0.

D-Bus in the container

No shipping stack (Waydroid, Anbox, Sailfish AppSupport) runs a D-Bus client inside the container; all keep D-Bus on the host and bridge over binder. Waydroid's AppArmor profile denies dbus. libdbus did build on bionic in AOSP external/dbus up to android-7.1.2.

A bionic libdbus port is new work (section 5). The host binder bridge remains the fallback.

Networking

Waydroid defaults to veth on bridge waydroid0 (192.168.240.1/24, dnsmasq, iptables MASQUERADE).

Share the host namespace: lxc.net.0.type=none, drop waydroid-net.sh.

microG on LineageOS 20

Google's November 2024 change moved signature checks to signingInfo. Upstream lineage-20.0 merged the fix on 2024-12-16 (Gerrit 411384, ComputerEngine.java). Upstream spoofing is restricted to the microG packages and only active on userdebug/eng builds.

Build from a lineage-20.0 checkout after 2024-12-16 as userdebug. A user build silently breaks microG.

Inline reply

KDE Plasma: capability and action id inline-reply; hints x-kde-reply-placeholder-text, x-kde-reply-submit-button-text, x-kde-reply-submit-button-icon-name; signal NotificationReplied(u id, s text) sent as a targeted signal to the connection that called Notify.

Reuse the KDE extension verbatim rather than inventing Marathon hints; Linux apps benefit too.

qtwayland

Custom extensions via QWaylandCompositorExtensionTemplate plus qt_generate_wayland_protocol_server_sources() are public API, unchanged through 6.10. text-input-v3 server support exists; the C++ class QWaylandTextInputManagerV3 is private, the QML type TextInputManagerV3 is the supported surface, selected with QT_WAYLAND_TEXT_INPUT_PROTOCOL.

Implement marathon_surface_v1 in C++; instantiate text-input-v3 from QML.

Waydroid regressions

Issue 2436 (30 Sep 2026): vendor image 20260927 breaks multi-window launches with a display hotplug loop. Issue 2426: an invisible GoogleDiscoverWindow with FLAG_NOT_FOCUSABLE is mapped as a focusable toplevel because Android window flags do not cross to Wayland.

Pin image builds and gate updates on CI. set_window_flags in the surface protocol fixes 2426 structurally.

Licensing

Waydroid host tool: GPL-3.0, dual-licensed with a commercial option. Lepton: GPL-3.0 image, MIT compat tool.

Android image GPL-3.0. Host side written fresh under Apache-2.0, not derived from Waydroid's Python.

# 5. Architecture

Integration is split into three planes by how tightly it is coupled to pixels. Anything that must be frame-accurate with a window (insets, focus, back progress, gesture exclusion, touch cancellation) travels over Wayland. Everything else (notifications, media, intents, power, permissions, settings, location, sensors) travels over D-Bus, spoken from inside the container through a filtering proxy. Container lifecycle sits in a separate privileged plane.

## Components

Component

Licence

Language

Responsibility

deckard-image

GPL-3.0

AOSP, C++, Java

Fork of android_vendor_waydroid and android_hardware_waydroid on LineageOS 20 (userdebug). Modified hwcomposer, Deckard IME, suspend HAL, bionic libdbus and D-Bus HAL shims, overlays (palette, no bars, no caption), headless SystemUI, stub Bluetooth, NFC, telephony and connectivity services, microG as privileged apps.

marathon-mesa-android

MIT

C, Meson

Bionic build of host Mesa (freedreno, turnip) plus libdrm and minigbm, packaged on the host and versioned with host Mesa. Bind-mounted into /vendor.

deckard-containerd

Apache-2.0

C++

Container lifecycle: rootless user namespace, binderfs, cgroup2 subtree delegation, host-namespace networking, mounts, cached boot state. Started by systemd at boot.

deckardd

Apache-2.0

C++, Qt6

Thin session broker: xdg-dbus-proxy policy, identity broker (section 6), app registry sync. No binder-to-D-Bus translation.

Marathon Shell integration

Apache-2.0

C++, QML

marathon_surface_v1 compositor extension next to the existing compositor sources; MarathonOS.Deckard QML module for app grid, switcher, Active Frames and settings.

## Repository

All Deckard code lives in github.com/patrickjquinn/Marathon-Deckard. The repository is a monorepo so that the surface protocol, the Android image patches and the host daemons change together and are released together; the GPL-3.0 image and the Apache-2.0 host code are kept in separate top-level directories with their own LICENSE files so the boundary is unambiguous. The Marathon Shell compositor extension stays in the Marathon-Shell repository because the protocol is general; this repository carries the protocol XML as the canonical copy and Marathon-Shell vendors it.

Marathon-Deckard/

  README.md

  docs/                       this design, ADRs, protocol notes

  protocol/                   marathon_surface_v1.xml (canonical)

  image/                      GPL-3.0

    LICENSE

    manifest/                 repo manifests pinning LineageOS 20 and WayDroid forks

    vendor_deckard/           fork of android_vendor_waydroid: overlays, stubs, microG, build config

    hardware_deckard/         fork of android_hardware_waydroid: hwcomposer, IME, suspend HAL, libdbus shims

    patches/                  frameworks/base, system/core, bionic patches by branch

    build.sh                  reproducible image build, pinned, CI-gated

  host/                       Apache-2.0

    LICENSE

    containerd/               deckard-containerd (C++)

    deckardd/                 session broker, dbus-proxy policy, identity broker (C++/Qt6)

    qml/MarathonOS.Deckard/   app grid, switcher, Active Frames, settings

    packaging/                postmarketOS APKBUILDs, systemd units, polkit rules

  mesa-android/               MIT. marathon-mesa-android: Meson cross files, APKBUILD, version pin to host Mesa

  ci/                         image build, protocol scanner check, on-device smoke tests

Branching: main tracks the Android 13 base through M2; the M3 rebase lands on an android-15 branch that becomes main when it passes the M1 and M2 exit criteria. Image builds are tagged image/YYYYMMDD and host releases v0.x; a host release records the image tag it was tested against.

## Mounted from the host

Devices: /dev/dri render node, binderfs, /dev/uhid, SDM845 video codec nodes (/dev/video*) for hardware codecs.

Graphics: marathon-mesa-android into /vendor/lib64 and /vendor/lib64/hw. The image ships no GPU userspace of its own.

Sockets: Wayland; PipeWire (audio, later camera); the D-Bus session socket through xdg-dbus-proxy with a Flatpak-style allowlist; GeoClue2 and iio-sensor-proxy over that same bus.

Network: the host network namespace. No veth, no NAT; host firewall and DNS apply. Android's ConnectivityService gets a stub network that mirrors host state.

Data: fonts (Slate plus a generated fonts.xml), tzdata, CA certificates converted to Android's hashed layout at mount time, locale, and scoped user directories.

## Stripped from the image

Removed: Launcher3, Settings UI, keyguard, wallpaper, LatinIME, DocumentsUI, PackageInstaller UI, print spooler, stock apps.

Headless: SystemUI stays as a stub because it hosts ThemeOverlayController and notification plumbing; all of its drawn surfaces are disabled.

Stubbed: Bluetooth, NFC and telephony stacks become fakes that report Marathon state, because apps still query TelephonyManager and BluetoothAdapter.

Hidden, kept: PermissionController service and the settings provider. Only their UIs go.

## Cannot be stripped

zygote, ART, system_server (activity, window and package managers), SurfaceFlinger with the Wayland hwcomposer, keystore, Codec2, WebView and microG. WebView cannot be replaced by the host browser engine because of the ABI boundary.

## D-Bus from inside the container

Android services and HALs that need the host (notifications, media, power, permissions, intents, location, sensors) link a bionic build of libdbus and connect to the proxied session socket. Authentication is EXTERNAL with the container UID mapped to the host user. deckardd writes the proxy policy: a fixed allowlist of well-known names and interfaces, --talk only, no --own except the identity broker. Treat every message from the container as untrusted; xdg-dbus-proxy rate-limits and filters, and deckardd validates argument shapes before anything reaches a Marathon service.

Fallback: if the bionic libdbus port proves fragile, keep the Waydroid pattern (gbinder service on the host, D-Bus spoken by the host) for the affected domain only. The host-side contract to Marathon does not change either way.

## What is reused from Waydroid

App listing calls getAppsInfo on IPlatform; launching sets waydroid.active_apps and calls launchApp. The hwcomposer's IWaydroidWindow HIDL 1.2 implements minimise via xdg-shell, pointer capture, and setIdleInhibit via idle-inhibit-unstable-v1 (per task, or * for all). /dev/uhid is mounted so Android can create virtual input devices. Upstream bug to carry a fix for: IPlatform.settingsGetInt sends the settingsGetString transaction code.

# 6. Identity broker

The host sees one client; users see many apps. Each Android package needs its own identity for the switcher (xdg_toplevel.set_app_id as org.marathonos.deckard.<package>), notifications (desktop-entry hint per package), and portal grants. Portals normally identify a caller by PID or cgroup, and everything in the container shares one. Deckard is therefore a trusted broker allowed to assert sub-identities, and this is the one place it does not behave like an ordinary app.

Rules:

Only deckardd may assert a sub-identity, and only for packages it has seen in getAppsInfo. The assertion is a signed token bound to the Android UID that made the request, taken from binder credentials inside the container.

Portal grants are keyed by org.marathonos.deckard.<package>, never by the container's shared identity. A compromised app can therefore claim only its own grants.

xdg-dbus-proxy is configured so the container can call portals only through deckardd, which stamps the identity; direct portal access from the container is denied.

The policy is documented as a single Marathon security rule and reviewed with the rest of the portal design.

# 7. Launch flow end to end

Boot. deckard-containerd starts the container from cached state in a user namespace; Android init mounts binderfs; zygote is warm.

App grid. deckardd calls getAppsInfo and registers every Android app in Marathon's app registry. PackageManager events keep it current.

Launch. The user taps an icon. Marathon draws a splash with the app's icon and issues an xdg-activation token while deckardd calls launchApp.

Window. Android starts the task. The hwcomposer creates an xdg_toplevel and a marathon_surface_v1 object carrying task ID, package and window flags, activates with the token; the shell swaps the splash for the live surface.

Gestures. A back swipe travels shell to protocol to hwcomposer to Android's back handling. Swiping a card away calls removeTask.

Screen off. Android enters Doze. microG's wake locks are Marathon wake locks, so push keeps working under opportunistic suspend.

# 8. Surface protocol: marathon_surface_v1

This protocol makes Android windows behave like Marathon windows. It is a general Marathon protocol: native Marathon apps also need insets, back progress, gesture exclusion and system-bar control, so Deckard is one client of it. It is implemented in Marathon Shell with the documented Qt Wayland Compositor extension pattern and on the Android side in the forked hwcomposer. Only the Deckard hwcomposer client and signed Marathon apps may bind the manager global; the compositor filters by client credentials.

<protocol name="marathon_surface_v1">

  <interface name="msf_manager_v1" version="1">

    <request name="destroy" type="destructor"/>

    <request name="get_surface">

      <arg name="id" type="new_id" interface="msf_surface_v1"/>

      <arg name="toplevel" type="object" interface="xdg_toplevel"/>

      <arg name="app_id" type="string"/>

      <arg name="task_id" type="int"/> <!-- 0 for native apps -->

    </request>

  </interface>

  <interface name="msf_surface_v1" version="1">

    <request name="destroy" type="destructor"/>

    <!-- client -> shell -->

    <request name="set_window_flags"><arg name="flags" type="uint"/></request>

    <request name="set_gesture_exclusion">

      <arg name="region" type="object" interface="wl_region" allow-null="true"/>

    </request>

    <request name="set_system_bars"><arg name="mode" type="uint"/></request>

    <request name="ack_insets"><arg name="serial" type="uint"/></request>

    <!-- shell -> client; insets precede xdg_surface.configure with the same serial -->

    <event name="insets">

      <arg name="serial" type="uint"/>

      <arg name="top" type="int"/><arg name="bottom" type="int"/>

      <arg name="left" type="int"/><arg name="right" type="int"/>

    </event>

    <event name="cutout">

      <arg name="serial" type="uint"/>

      <arg name="region" type="object" interface="wl_region"/>

    </event>

    <event name="ime_inset"><arg name="serial" type="uint"/><arg name="bottom" type="int"/></event>

    <event name="back_started"><arg name="edge" type="uint"/>

      <arg name="x" type="fixed"/><arg name="y" type="fixed"/></event>

    <event name="back_progress"><arg name="progress" type="fixed"/>

      <arg name="x" type="fixed"/><arg name="y" type="fixed"/></event>

    <event name="back_committed"/>

    <event name="back_cancelled"/>

    <event name="visibility"><arg name="state" type="uint"/><arg name="max_fps" type="uint"/></event>

  </interface>

</protocol>

set_window_flags carries focusable, touchable and secure bits. Not focusable and not touchable surfaces are mapped without activation, which closes Waydroid issue 2426. Secure surfaces are excluded from switcher thumbnails, Active Frames and screenshots by the shell, not merely hinted.

set_gesture_exclusion forwards the rectangles apps register for drawers and sliders so Marathon yields the edge. set_system_bars forwards immersive-mode requests so Marathon hides its own status area.

insets, cutout and ime_inset carry the configure serial they belong to and are sent before the matching xdg_surface.configure, so a client applies them in the same frame. The OnePlus 6 notch is reported as a cutout region, not an inset. The client acknowledges with ack_insets.

back_* events carry progress and finger position so, on an Android 15+ base, the app's predictive-back animation tracks the finger. On Android 13 the hwcomposer translates back_committed into KEYCODE_BACK.

visibility tells a task whether it is on screen, thumbnailed in an Active Frame, or hidden, with a frame-rate cap so an Active Frame does not render a full-resolution task at 60 fps on an Adreno 630.

Dialogs and popups owned by a task are mapped as xdg_popup children of the task toplevel. Picture-in-picture is a second msf_surface_v1 with the same task_id and a flag bit. Touch cancellation uses standard wl_touch.cancel; when Marathon claims a touch sequence mid-stream (a Hub peek), the hwcomposer must deliver ACTION_CANCEL to the app, which M0 verifies.

# 9. Keyboard

Marathon uses its own BB10-inspired keyboard with Hunspell prediction. The compositor exposes text-input-v3 through the TextInputManagerV3 QML type with QT_WAYLAND_TEXT_INPUT_PROTOCOL=text-input-v3. The Deckard IME is an Android InputMethodService that draws nothing: it sends editor state (input type, IME action, surrounding text, cursor rectangle) to the hwcomposer over binder, and the hwcomposer acts as a text-input-v3 client for the focused Android surface. Marathon's keyboard then types into Android apps exactly as it does into native ones, and the keyboard's height is reported back as ime_inset. If the keyboard does not yet serve Wayland clients over text-input-v3, wiring it up is a Marathon task that benefits Linux apps too.

# 10. System bridge

Status key: Exists means Waydroid already does it. Verified seam means the Android or host API to hook has been confirmed. Build means new work.

Domain

Android side

Marathon side

Status

App list and launch

IPlatform getAppsInfo / launchApp

Shell app registry, xdg-activation

Exists

Clipboard

Waydroid clipboard service

Wayland selection

Exists

Notifications

NotificationListener in headless SystemUI, bionic libdbus

Hub via org.freedesktop.Notifications; inline-reply action and x-kde-reply-* hints; NotificationReplied

Verified seam, build

Idle inhibit

IWaydroidWindow.setIdleInhibit

idle-inhibit-v1 (IdleInhibitManagerV1 in qtwayland)

Exists, wire up

Wake locks

SystemSuspend AIDL, Deckard suspend HAL

Marathon power service over D-Bus

Verified seam, build

Theme

Monet disabled, static RRO over 65 palette attributes

MColors tokens, dark mode, locale, font scale

Verified seam, build

Keyboard

InputMethodService (Deckard IME)

text-input-v3, Marathon keyboard

Verified seam, build

Back and insets

Android back handling, display policy

marathon_surface_v1

Build (Android 15+ for progress)

Permissions

PermissionController grant flow

Portal via identity broker

Build

Links and share

Resolver and chooser replaced, intent filters indexed

URL handlers, share sheet, OpenURI portal

Build

Files

DocumentsProvider plus scoped bind mounts

Host file chooser, storage permission

Build

Location and sensors

Location provider, sensor HAL

GeoClue2, iio-sensor-proxy

Build

Audio

Audio HAL

PipeWire socket

Build

Media

MediaSession

MPRIS2, lock-screen controls

Build

Network

Stub ConnectivityService

Host network namespace, NetworkManager state

Build

Telephony

Stub TelephonyManager, tel: and sms: intents

Marathon Phone, Messages, modem service

Build (limited)

The freedesktop notification spec allows vendor hints named x-vendor-name; KDE's inline-reply extension is reused as-is, and x-marathon-conversation carries Android MessagingStyle threads into the Hub's threaded view. Note that NotificationReplied is a targeted signal: the container-side client must receive it on the same connection it used to call Notify.

Treat the Android side as untrusted input everywhere. A compromised app can compromise the container, so every call is validated and rate-limited and no host directory is exposed by default.

# 11. Power and push

Marathon implements opportunistic suspend with kernel wake locks and RTC wake alarms, falling back to systemd-logind inhibitors. Android's seam is the SystemSuspend AIDL service: each client gets its own IWakeLock object with binder death cleanup, and ISuspendControlService enables autosuspend for system_server.

Deckard's suspend HAL never suspends anything itself. Each Android wake lock becomes a named Marathon wake lock over D-Bus (deckard:<tag>), so all suspend accounting lives in one place; when the host resumes, the HAL notifies Android that it woke. AlarmManager holds a wake lock for the whole of a receiver's onReceive(), and FCM holds one until onMessageReceived() returns, so with wake locks routed through Marathon, microG's heartbeat and push delivery behave as they do on a phone. Waydroid's whole-container freeze is removed. Android's cached-app freezer handles idle apps once a writable cgroup2 subtree with +freezer is delegated to the container and host-kernel BINDER_FREEZE support is confirmed.

Android's next wake-up deadline must reach Marathon's RTC scheduler, because the container cannot program the RTC. The suspend HAL forwards AlarmManager's next wake-up as a Marathon wake alarm request. Longer term, a Marathon-native UnifiedPush distributor (KUnifiedPush is prior art) could serve both native apps and UnifiedPush-capable Android apps.

# 12. Theming

Monet is disabled in the image. A static runtime resource overlay generated at build time from Marathon's MColors tokens covers the 65 system_accent1-3 and system_neutral1-2 attributes (five palettes, thirteen shades each) plus the Android 14+ Material 3 role tokens when the base moves to 15. Material 3 apps therefore render Marathon's exact colours. The fallback, when Monet must stay on for a specific base, is a seed colour in android.theme.customization.system_palette, which skips wallpaper extraction and expands the seed into the full palette. Font and icon-shape overlays complete the look, and Android system dialogs (permissions, chooser, toasts) are replaced by Marathon-drawn equivalents. Apps with fully custom UIs will not change; that is a hard limit.

# 13. microG

The image ships microG's GmsCore and com.android.vending builds as privileged system apps. The base is a lineage-20.0 frameworks/base checkout after 2024-12-16, which carries the signingInfo spoofing fix required since Google's November 2024 signature-check change. Upstream LineageOS spoofing is restricted to the microG packages signed with the microG release key and is only active on userdebug and eng builds, so deckard-image is always built as userdebug. Privapp permissions are pre-granted, device registration and cloud messaging are pre-configured, and microG's account and settings surface inside Marathon Settings. The microG Self-Check is the only reliable test; third-party spoofing checkers report false negatives against the upstream patch. Play Integrity remains unsupported.

# 14. Graphics stack

marathon-mesa-android is a bionic build of the same Mesa release the host ships, produced with Mesa's documented out-of-tree Meson path (host_machine system=android, NDK clang, -Dplatforms=android -Dplatform-sdk-version=33), with freedreno (GL) and turnip (Vulkan), plus libdrm and minigbm. It is a host package, updated in the same transaction as host Mesa, and bind-mounted into /vendor/lib64, /vendor/lib64/hw and /vendor/lib64/egl. The image sets ro.hardware.egl=mesa, ro.hardware.vulkan=freedreno, ro.hardware.gralloc=minigbm and ships no GPU userspace. This replaces Waydroid's lineage-18.1 Mesa fork, which lags host Mesa by years. linux-dmabuf is enabled in the compositor via QT_WAYLAND_HARDWARE_INTEGRATION so buffers pass zero-copy.

# 15. Container runtime

Rootless. deckard-containerd runs as the session user in a user namespace with the user mapped to UID 0 inside and userns CAP_SYS_ADMIN. Android init mounts binderfs itself (mount -t binder binder /dev/binderfs), which the kernel permits because binderfs is FS_USERNS_MOUNT. No host root helper after boot.

ashmem replaced by memfd (sys.use_memfd=true); no ashmem driver required on the host kernel.

cgroup2 subtree delegated writable with +freezer, +memory and +cpu, so Android's process groups and cached-app freezer work. A memory budget for the container (memory.max) is set so lmkd inside Android acts before the host OOM killer does.

Host network namespace, no bridge. Stub ConnectivityService mirrors NetworkManager state over D-Bus.

Cached boot state: the post-first-boot /data snapshot and warm zygote restored on start, as Lepton does.

Image builds pinned and gated on CI against the Waydroid vendor regressions seen in 2026.

# 16. Milestones

Milestone

Scope

Exit criteria

M0 Baseline

Stock Waydroid multi-window on the OnePlus 6 under Marathon Shell with linux-dmabuf. Measure binder latency on the PREEMPT_RT kernel and verify wl_touch.cancel becomes ACTION_CANCEL.

An Android app scrolls at 60 fps with no focus stealing; binder round trip within 2x of a non-RT kernel.

M1 Native illusion

Forked hwcomposer with marathon_surface_v1, insets and cutout, Android chrome removed, KEYCODE_BACK on release, notifications in the Hub with inline reply, Monet off with the palette RRO, cgroup2 delegation with the freezer active.

Someone unfamiliar cannot spot Android chrome; back, peek and minimise work in the top 20 apps; Material 3 apps show MColors exactly.

M2 Runtime

Rootless deckard-containerd with binderfs in userns and memfd; marathon-mesa-android replaces the image Mesa; host network namespace; warm boot from cached state; suspend HAL over SystemSuspend AIDL; bionic libdbus with xdg-dbus-proxy (host binder bridge as fallback); microG on a post-2024-12-16 lineage-20.0 userdebug image.

A Signal message arrives promptly after hours of suspend; idle drain close to a Deckard-off baseline; no root process after boot.

M3 Android 15 base

Rebase of hwcomposer, suspend HAL, IME, overlays and stubs onto an Android 15 or 16 base. Finger-tracking predictive back through a framework patch to BackNavigationController. Deckard IME over text-input-v3. Identity broker and permission portal. Intents, share and files.

Back animation tracks the finger in Material 3 apps; Marathon keyboard works in every text field; per-package portal grants hold under a hostile test app.

M4 Completion

Location, sensors, camera over PipeWire, media controls, contacts and calendar sync, F-Droid source in the Marathon Store.

Daily-driver use for a week without falling back to a separate Android phone.

The rebase in M3 is the largest single risk in the plan and is budgeted as its own milestone rather than as a feature line.

# 17. Open questions

Question

How to resolve

Does Marathon's keyboard serve Wayland clients over text-input-v3?

Check in tree; wire TextInputManagerV3 if not.

What D-Bus interface does the Hub expose, and what is the app registry API?

Check in tree; document for deckardd.

Does the hwcomposer turn wl_touch.cancel into ACTION_CANCEL?

Test with a peek gesture mid-scroll in M0.

Does a bionic libdbus authenticate EXTERNAL through xdg-dbus-proxy across the user namespace?

Prototype in M2 against AOSP's external/dbus as the starting point; fall back to the host binder bridge per domain.

Does the host kernel expose BINDER_FREEZE and a delegatable cgroup2 freezer?

Check cgroup.controllers and binder ioctls on the postmarketOS 6.12+ kernel.

Can the SDM845's Wi-Fi and modem wake the device for an incoming push?

Measure on device under suspend.

Do Material Components apps pick up the static palette with Monet disabled?

Test per app; AOSP asks vendors to register Build.MANUFACTURER for dynamic-colour detection.

Binder behaviour without priority inheritance on PREEMPT_RT.

Measure in M0; consider carrying the ACK binder PI patch if SurfaceFlinger latency suffers.

Is DECKARD clear in the EU?

Manual EUIPO TMview search, Classes 9 and 42. The US position is clear (section 18).

# 18. Name

Deckard references Blade Runner. The working name Replicant is taken by an FSF-backed Android distribution. Valve used Deckard as the internal codename for the Steam Frame headset, which shipped under that product name on 18 September 2026; Valve trademarked "Steam Frame", not "Deckard". US application 97071210 for DECKARD by Deckard Technologies (Class 42) was abandoned on 26 January 2023 and never registered; the only live US DECKARD mark is in Class 25 (clothing). No live US mark in Classes 9 or 42 was found. An EUIPO TMview search is still required before any public or commercial launch. This is not legal advice.

# 19. Sources

Marathon-Deckard repository: https://github.com/patrickjquinn/Marathon-Deckard

Marathon Shell: https://github.com/patrickjquinn/Marathon-Shell

Waydroid host tool, lxc.py and IPlatform.py: https://github.com/waydroid/waydroid

Waydroid lineage-20 manifest (bionic Mesa): https://raw.githubusercontent.com/waydroid/android_vendor_waydroid/lineage-20/manifest_scripts/manifests-33/02-waydroid.xml

Waydroid IWaydroidWindow HIDL: https://github.com/waydroid/android_hardware_waydroid

Waydroid issue 2436: https://github.com/waydroid/waydroid/issues/2436

Waydroid issue 2426: https://github.com/waydroid/waydroid/issues/2426

Waydroid licensing: https://waydro.id/licensing.html

Valve Lepton (binder.rc, liblepton.sh, mounting.sh, BoardConfig.mk): https://gitlab.steamos.cloud/frame-public/lepton

Linux binderfs FS_USERNS_MOUNT: https://github.com/torvalds/linux/blob/v6.12/drivers/android/binderfs.c

Android 16 behaviour changes: https://developer.android.com/about/versions/16/behavior-changes-16

Predictive back: https://developer.android.com/guide/navigation/predictive-back-gesture

BackEvent: https://developer.android.com/reference/android/window/BackEvent

AOSP Material You: https://source.android.com/docs/core/display/material

LineageOS 20 ThemeOverlayController: https://github.com/LineageOS/android_frameworks_base/blob/lineage-20.0/packages/SystemUI/src/com/android/systemui/theme/ThemeOverlayController.java

AOSP cached apps freezer: https://source.android.com/docs/core/perf/cached-apps-freezer

AOSP SystemSuspend: https://source.android.com/docs/core/power/systemsuspend

AlarmManager wake lock: https://developer.android.com/reference/android/app/AlarmManager

Wake lock use cases (FCM): https://developer.android.com/develop/background-work/background-tasks/awake/wakelock/identify-wls

Binder RT priority inheritance (never mainlined): https://lwn.net/Articles/732565/

Mesa Android build: https://docs.mesa3d.org/android.html

Qt Wayland Compositor custom shell: https://doc.qt.io/qt-6/qtwaylandcompositor-custom-shell-example.html

Qt text-input-unstable-v3: https://doc.qt.io/qt-6/qtwaylandcompositor-attribution-wayland-text-input-unstable-v3.html

Desktop Notifications Specification: https://specifications.freedesktop.org/notification/latest-single/

KDE inline reply (notification.cpp, server_p.cpp): https://invent.kde.org/plasma/plasma-workspace/-/tree/master/libnotificationmanager

AOSP external/dbus (libdbus on bionic): https://android.googlesource.com/platform/external/dbus/

LineageOS signingInfo spoofing fix, lineage-20.0: https://review.lineageos.org/c/LineageOS/android_frameworks_base/+/411384

microG issue 2680 (November 2024 change): https://github.com/microg/GmsCore/issues/2680

microG signature spoofing: https://github.com/microg/GmsCore/wiki/Signature-Spoofing

USPTO TSDR 97071210: https://tsdr.uspto.gov/statusview/sn97071210

UnifiedPush: https://f-droid.org/2026/01/08/unifiedpush-5-years.html
