# Composer HAL: the boot aborts and the boot that never completes

Waydroid's composer HAL (`android.hardware.graphics.composer@2.1-service`
loading `vendor/lib64/hw/hwcomposer.waydroid.so` from the lineage-20 vendor
image of 2026-09-27) on the HackberryPi CM5, Waydroid 1.6.3, under the shells
that ran from 18 Oct 17:39 to 19 Oct 03:55. Read from the device on 2026-10-19 between 03:38 and 03:55, read-only:
53 composer cores in systemd-coredump, 7 tombstones, 10 ANR traces, the
`deckard.boot` journal entries, the host journal (Android init logs to it) and
`/var/lib/waydroid/waydroid.log`. Source: `waydroid/android_hardware_waydroid`
lineage-20 (HEAD a3e4239); symbols from the binaries' own `.gnu_debugdata`.

## Conclusions

1. **The aborts at boot are a Waydroid bug, not something Marathon sends.**
   46 of the 53 cores (every one from 01:54 to 03:34) are one abort:
   `Binder threadpool cannot be shrunk after starting`. The HAL's own binder
   thread calls `configureRpcThreadpool(1, true)` after the service's main
   thread has configured the hwbinder pool for 4 threads and started it.
   It is a race between two threads of the composer, decided by scheduling.
2. **The aborts are not what causes the hang.** Neither hang (01:42:45 and
   03:06:43 boots) has a composer core, and the abort can only happen after
   the main thread has left `hwc_open`; in the hangs it never did.
3. **The hang is the composer's main thread blocked inside `hwc_open`, before
   the HAL's binder thread exists.** That code is the Wayland handshake with
   Marathon (connect, registry, the calibration window) plus one bounded HIDL
   lookup. Which wait it was is not recorded: the stall happened before the
   boot watchdog existed, and Android's watchdog never dumps an unregistered
   HAL. Nothing captured implicates the shell (it was alive and drawing during
   the 01:42 hang), but nothing clears it either.
4. **The other 7 cores** are the HAL's Wayland thread aborting when its
   connection to the shell ends: a shell restart (4), a session stop (1), the
   shell dropping the client during the M0 deadlock (1), and one unexplained
   M0-era case. None is a protocol error.

Fix: patch one instruction of `hwcomposer.waydroid.so` through the Deckard
overlay (below), and capture the next hang with the two extra lines described
in *Next hang*. No shell change is proposed: none is supported by the evidence.

## 1. The boot abort

### Evidence

All 46 cores (18 Oct 18:02 to 19 Oct 03:34) crash at the same return address,
`hwcomposer.waydroid.so + 0x3eeec`, and all 7 tombstones (03:13 to 03:34) carry
the same abort message. Core 325150 / tombstone_06, `debuggerd` symbols:

```
#00 libc.so            abort+164
#01 liblog.so          __android_log_default_aborter+12
#02 liblog.so          __android_log_assert+308
#03 libhidlbase.so     android::hardware::ProcessState::setThreadPoolConfiguration(unsigned long, bool)+300
#04 libhidlbase.so     android::hardware::configureBinderRpcThreadpool(unsigned long, bool)+64
#05 hwcomposer.waydroid.so  hwc_binder_thread(void*)+68        (pc 0x3eee8)
#06 libc.so            __pthread_start(void*)+208
Abort message: 'Binder threadpool cannot be shrunk after starting'
```

`hwc_binder_thread+0x44` is the call in `hwcomposer/hwcomposer.cpp`:

```
static void* hwc_binder_thread(void* data) {
    ...
    setpriority(PRIO_PROCESS, 0, HAL_PRIORITY_URGENT_DISPLAY);
    configureRpcThreadpool(1, true /*callerWillJoin*/);      // 0x3eee0 mov w0,#1 ; 0x3eee8 bl
```

and the check that fires, disassembled from the vndk `libhidlbase.so`
(`ProcessState` fields: +176 `mThreadPoolStarted`, +72 `mMaxThreads`):

```
b2790  ldrb w8, [x0,#176]        // mThreadPoolStarted
b2794  cbz  w8, b27a4
b2798  ldr  x8, [x20,#72]        // mMaxThreads
b279c  cmp  x8, x21              // x21 = requested maxThreads (1)
b27a0  b.hi b2878                // -> __android_log_assert("...cannot be shrunk after starting")
```

The service's `main` (disassembled from
`vendor/bin/hw/android.hardware.graphics.composer@2.1-service`):

```
7110  sched_setscheduler(0, SCHED_FIFO|SCHED_RESET_ON_FORK, prio 2)
7140  configureRpcThreadpool(4, true)
7144  load the HWC1 module -> hwc_open()     // creates hwc_binder_thread at its end
71a8  IComposer::registerAsService("default")
71d0  joinRpcThreadpool()
```

In all three cores walked by frame pointer (325150, 214352, 26881) the main
thread is already in `joinRpcThreadpool` (return address `main+0x71d4`) while
the binder thread is still at its first instruction after `setpriority`.

### Mechanism

`mThreadPoolStarted` is set by `ProcessState::startThreadPool()`, which the
generated HIDL proxies call before any transaction that passes an interface.
Two such calls race the new binder thread:

- `IComposer::registerAsService` → `addWithChain(name, service, chain)`.
  Main gets there within 3 ms of `create_display` returning (03:44:21 boot,
  a clean one: `wayland display 0x…` at .177, `Registered
  ...IComposer/default` at .180, the binder thread's `Waydroid hwcomposer
  services are ready` at .182). Main is SCHED_FIFO; the binder thread is a
  fresh CFS thread (`SCHED_RESET_ON_FORK`). On a busy core it starts too late.
- `IWaydroidTask::getService()` at the end of `create_display`, when it runs
  before `task-hal-1-0` has registered: the getService `Waiter` calls
  `registerForNotifications(callback)`, which starts the pool before the binder
  thread even exists. init starts `vendor.hwcomposer-2-1` a few ms before
  `task-hal-1-0`. The journal shows it as `service 'task-hal-1-0' requested
  start, but it is already running` (hwservicemanager's lazy start) at 02:17:00,
  02:31:43, 02:36:13 and 03:13:13; each of those boots aborted.

If the binder thread reaches `configureRpcThreadpool(1)` first, it silently
lowers the pool to 1 and nothing aborts; that is a clean boot.

Of the 22 container boots between 01:42 and 03:44, 12 aborted at least once
(3 times in a row at 02:36), and so did the restart after the 02:03 SIGPIPE
(below). Each abort costs about 5 s: init restarts
the composer, and its `onrestart` restarts SurfaceFlinger, which restarts
zygote.

### Why Marathon is not involved

The abort is between two threads of the composer and the hwbinder driver.
No Wayland call is on either stack, and the outcome doesn't depend on any
event from the compositor.

### Fix

The service has already configured the pool with 4 threads (0x7140, always
before `hwc_open`), so the binder thread asking for 4 is a no-op and can't
shrink it. One instruction:

| | |
|---|---|
| File | `vendor/lib64/hw/hwcomposer.waydroid.so` (vendor image 2026-09-27) |
| sha256 before | `017cefa35bccc2d8ac7bec92dabeb65422c777229a787ed395bdb9c1b9015247` |
| File offset (= vaddr) | `0x3eee0` |
| Bytes | `20 00 80 52` (`mov w0,#1`) → `80 00 80 52` (`mov w0,#4`) |
| sha256 after | `42bef881c0fd2024d5438942eb3a070d8b486eebce28c5f4c9835b95eabf7ce0` |

Built and disassembled on the host (not installed):

```
3eedc  bl   setpriority
3eee0  mov  w0, #0x4
3eee4  mov  w1, #0x1
3eee8  bl   configureRpcThreadpool
```

Shipped (da3d580) as `image/overlay/vendor/lib64/hw/hwcomposer.waydroid.so`;
the vendor mount's lowerdir is `/var/lib/waydroid/overlay/vendor` first.
`image/tools/patch-hwcomposer.sh` rebuilds it from the stock file and checks
both hashes. So that a Waydroid vendor update is never shadowed by a stale
binary, `host/packaging/deckard-overlay-sync` (fa83c24) keeps it only while
`/var/lib/waydroid/waydroid.cfg` says `vendor_datetime = 1790486210` and
removes it otherwise, including when `waydroid.cfg` is missing (it runs
before the image is mounted, so it can't hash the file in place). Host test:
`host/packaging/tests/test-deckard-overlay-sync.sh` (ctest `overlay_sync`).
**On the device (2026-10-19, 04:27–04:38, under the session lock):** the
repo's deckard-overlay-sync, run against a copy of the live overlay state
plus the patched file, changed exactly one file in
`/var/lib/waydroid/overlay` (the composer; whiteouts, props and the other
overlay files compared equal before and after). Then 20 restarts of
`marathon-waydroid.service`:

| | Before (01:42–03:44, 22 boots) | Patched (20 starts) |
|---|---|---|
| Boots with a composer abort | 12 | 0 |
| Composer cores | 19 | 0 |
| `cannot be shrunk` tombstones | (7 kept) | 0 |
| Any coredump, any process | | 0 |
| `Waydroid hwcomposer services are ready` | | 20 of 20 |
| `/vendor/lib64/hw/hwcomposer.waydroid.so` in Android | stock | `42bef881…` every start |
| Restart to `sys.boot_completed=1` | | 11–13 s (12 s in 16 of 20) |
| Boot-watchdog stalls, composer/SurfaceFlinger/zygote restarts | | 0 |

The "before" boots were not timed the same way, so the table makes no boot
time comparison; each abort cost about 5 s (init restarts the composer,
SurfaceFlinger and zygote). The patched file stays installed. The deployed
`/usr/libexec/deckard-overlay-sync` predates fa83c24 and will remove it at
the next `waydroid-container.service` start, until the aport is updated.

Upstream patch for `hwcomposer/hwcomposer.cpp`: drop the
`configureRpcThreadpool(1, true)` line from `hwc_binder_thread` (the composer
service configures the pool before loading the HAL, which also satisfies
`joinRpcThreadpool`'s check), or pass the service's thread count.

## 2. The Wayland-thread aborts

7 cores abort at `hwc_wayland_thread+0x14c` (`abort()` after
`wl_display_dispatch` returned -1):

| Time | What ended the connection |
|---|---|
| 18 Oct 19:20:47 | not found (M0 testing; the logging relay was in use) |
| 18 Oct 19:31:54 | the shell: `error in client communication (pid 26881)`, during the M0 setFocusedTask deadlock (the HAL's Wayland thread had stopped reading); system_server's watchdog fired 5 s later |
| 18 Oct 22:52:21, 23:40:22, 23:59:31; 19 Oct 00:53:47 | a shell restart (new shell pids 144613, 164584, 175125, 217128 within 3 s) |
| 19 Oct 03:54:20 | a session stop (`Stopping Android apps`); the HAL wrote `socket error: Invalid argument` to `/data/waydroid_hwc_wayland_error.txt` |

The HAL records protocol errors in that file (`protocol error: interface
...`); there is none.

## 3. The boot that never completes

### What was seen

| Boot | Composer | system_server | Ended by |
|---|---|---|---|
| 01:42:45 | started 01:42:45, no core | killed by its watchdog every 72–73 s (zygote signal 9 at 01:43:58 … 01:50:01) | session restart (01:50:05 stop, 01:54:21 start) |
| 03:06:43 (first start after the data reset) | started 03:06:43, no core | same, 03:07:56 … 03:12:46 | session restart (03:13:08) |

In each of the 10 ANR traces (Android clock 02:07:15–02:12:41 = 03:07–03:12
IST):

- SurfaceFlinger's main thread is in `SurfaceFlinger::init` →
  `HWComposer::HWComposer` → `HidlComposer::HidlComposer` →
  `getServiceInternal<IComposer>` → `Waiter::wait`: IComposer was never
  registered.
- system_server's main thread is in `DisplayManagerService.<init>` →
  `SurfaceControl.getCompositionColorSpaces` → `ComposerService::connectLocked`
  → `waitForService("SurfaceFlinger")`: it waits for SurfaceFlinger, which
  publishes itself only after `init()`.
- The composer is not in the native dumps (the watchdog dumps registered HALs
  only), and no system_server thread is in `setFocusedTask`.

### Where the composer was

- The abort needs `hwc_binder_thread`, which `hwc_open` creates as one of its
  last statements. In the hangs there was no abort.
- After `hwc_open` returns, main only runs `HWC2On1Adapter` set-up
  (`hwc_query`, `hwc_get_display_configs`, `hwc_get_display_attributes`,
  `hwc_register_procs`: plain field reads, no locks) and then registers
  IComposer. If it had got that far, SurfaceFlinger would have booted.

So main was blocked inside `hwc_open`, before
`pthread_create(&pdev->binder_thread, ...)`. What can block there:

| Step (`wayland-hwc.cpp` / `hwcomposer.cpp`) | Waits on |
|---|---|
| `create_display`: `wl_display_connect` | the shell accepting |
| `wl_display_roundtrip` after `wl_display_get_registry`; inside it, the `wl_output` handler's own `wl_display_roundtrip` | the shell answering `wl_display.sync` |
| `IWaydroidTask::getService()` | task-hal registering; retried every second, so it ends once task-hal is up |
| `window::create("Waydroid")`: `do { wl_display_roundtrip } while (!window->configured)` | an `xdg_surface.configure` (Qt sends one when the toplevel is created) |
| calibration: attach a 1×1 shm buffer, commit, `wl_display_roundtrip` | the shell |
| `finished_calibrating`: `property_set("waydroid.display_scale")`, `ro.sf.lcd_density` | init |

From `create_display` on, the HAL's Wayland thread also dispatches the default
queue, so the roundtrips in `window::create` run with two threads dispatching
the same queue. libwayland's reader protocol makes that safe except for the
gap between `wl_display_sync` and `wl_callback_add_listener` inside
`wl_display_roundtrip_queue`; main is SCHED_FIFO there, so that gap is very
unlikely to be the cause.

The settings agent saw load near 0 for the 6 minutes, so main was blocked, not
spinning (a roundtrip loop that fails returns at once and would spin a core at
FIFO priority). The shell was alive in the 01:42 hang: the slim agent's
screenshot at 01:48 (`slim/results/now.png`) is a normal Home screen, drawn by
the shell's own SIGUSR1 handler (`slim/shot.sh`). A live shell answers `wl_display.sync` from
libwayland-server immediately.

### What Marathon sends in that window

From the shell (feat/keel-framework-1a) and the HAL source:

```
HAL  connect; get_registry; sync
SH   globals: wl_compositor, wl_subcompositor, xdg_wm_base, wl_shm, wl_seat,
     wl_output (HAL binds at most v3), zwp_linux_dmabuf_v1 (HAL binds v3),
     wp_viewporter, wp_fractional_scale_manager_v1 (NativeClientScale hides it
     from first-party clients only), wl_data_device_manager, ...;
     no wp_presentation; marathon_surface_v1 per MarathonSurfaceManagerV1::allowsGlobal
     callback.done
HAL  (wl_output handler) bind; sync
SH   wl_output geometry, mode, scale 1, done; callback.done;
     on the next idle: wl_output scale 2, done  (NativeClientScale::sendOutputScale)
     wl_seat capabilities, keymap ...
HAL  [Wayland thread starts]  IWaydroidTask::getService()
HAL  wl_surface, wp_viewport, xdg_surface, xdg_toplevel
SH   xdg_toplevel.configure(0,0,[]) + xdg_surface.configure   (QWaylandXdgToplevel ctor)
HAL  set_maximized, set_title "Waydroid", set_app_id "Waydroid", commit; sync
SH   at that commit (or 300 ms): configure(0,0,[maximized(,activated)])  (mapToplevel, eefb85ef)
HAL  ack; get_fractional_scale; attach 1x1; commit; sync
SH   preferred_scale 180
HAL  set_window_geometry(w,h), background buffer + viewport, commit
```

The 0×0 configure (eefb85ef) is handled: `xdg_toplevel_handle_configure`
ignores sizes ≤ 1, and calibration falls back to the output mode divided by
the scale. After a data reset `persist.waydroid.width/height` are gone with
`/data`; the size then comes from that fallback. Neither path waits.

### Next hang

The boot watchdog (deckardd `BootWatchdog`, 546da15) already takes
`debuggerd -b` of the composer, so the next real stall will show main's frames.
`deckard-boot-evidence` now also prints two things that place it even without
symbols:

- `logcat -d -v threadtime -s hwcomposer HidlServiceManagement ServiceManagement`
  in the container (last 200 lines). `WAYLAND_DISPLAY: …` comes right after
  `wl_display_connect`, `wayland display 0x…` right after `create_display`,
  `Waited one second for vendor.waydroid.task…` from the getService wait,
  `Registered …IComposer/default` from main, and `Waydroid hwcomposer services
  are ready` from the binder thread.
- Both ends of the composer's Wayland socket, with Recv-Q/Send-Q. The shell's
  end is not in the host's namespace: a Unix connection's accepted socket is
  created in the connecting socket's network namespace, so it shows in the
  container's `ss -xp` as the socket on `/run/user/1000/marathon-wayland-0`
  owned by marathon-shell. ss shows no peer inode on this kernel, so the pair is
  matched by that path (the composer is the container's only Wayland client).
  Bytes queued at the composer mean it stopped reading; nothing queued at
  either end while main waits means no reply was sent.

Until then the watchdog's single restart after 60 s turns a 6-minute hang into
about 70 s; both observed hangs cleared on one restart.

## Other findings

- **SIGPIPE.** At 02:03:21 the composer died of signal 13 (no core), during a
  zygote crash loop that restarted system_server every 5 s. libwayland sends
  with `MSG_NOSIGNAL`, so it came from a plain `write`: the input FIFOs
  `/dev/input/wl_*_events` (whose reader is system_server) or a clipboard
  pipe. The HAL doesn't ignore SIGPIPE. Not checked further.
- **Clipboard read on the Wayland thread.** `data_device_handle_selection` →
  `read_selection` does a blocking `read()` on the offer's pipe on the HAL's
  Wayland thread. If the selection owner never writes (for example a Marathon
  app frozen by the lifecycle freezer), that thread stops for good and the
  shell eventually drops the client, as at 19:31:54. Seen in source only, not
  on the device.
- The shell logged `Toplevel received an unexpected ack_configure!` at 02:17:00,
  02:31:43 and 02:46:08, the first two at composer starts. Harmless (Qt drops
  the ack), noted for the shell agent.
