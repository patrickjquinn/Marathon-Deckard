# Sensors

Android apps in Deckard get the phone's accelerometer, magnetometer,
gyroscope, light and proximity sensors, and Android's own fusion sensors
built on them (rotation vector, game rotation vector, gravity, linear
acceleration, orientation). The sensors stay the host's: the kernel's IIO
drivers own them, Marathon reads them too, and Android never opens a sysfs
or IIO node.

## Shape

```
SensorManager app (Sky Map, compass, games)
  └▶ SensorService, fusion sensors                         (Android, unchanged)
       └▶ ISensors@1.0 "default"                            (HIDL, vendor, uid system)
            android.hardware.sensors@1.0-service.deckard
            └ list, events ◀── /data/deckard/sensors.sock ──▶ deckard-sensors   (host, user session)
                              SOCK_SEQPACKET                   └ /sys/bus/iio/devices/iio:device*/in_*_raw
```

## Options considered

| Option | Verdict |
|---|---|
| Waydroid's `waydroid-sensord` (host sensors HAL over `/dev/host_hwbinder`) | Rejected. It exists for Halium hosts, whose Android sensors HAL runs on the host; Deckard's hosts have IIO drivers and no Android HAL. |
| iio-sensor-proxy over D-Bus | Rejected. It gives an accelerometer orientation, light level and a compass heading, not the raw vectors Android's sensors and fusion need, and no gyroscope. |
| IIO nodes bound into the container, read by the HAL | Rejected. Android would need the host's sysfs and its device numbering, and a reader inside the container would see every IIO device, the PMIC's ADC included. |
| In-container HAL fed by a host service over a socket | **Chosen**, as for the camera ([camera.md](camera.md)). The host reads IIO; the HAL only speaks HIDL. |

## Host: `deckard-sensors`

A user service (`deckard-sensors.service`, `host/deckard-sensors`). It binds
`<Android data>/deckard/sensors.sock` (Android's `/data/deckard/sensors.sock`,
mode 0666), accepts only peers whose UID is Android's `system` (1000, the
HAL's user; the container shares the host's UIDs) and re-creates the socket
after an Android data reset. It starts with the graphical session, ordered
before `deckard-session.service`, because SensorService reads the sensor
list once.

At start it walks `/sys/bus/iio/devices` once, by device number, and keeps
the first device of each kind, found by its channels:

| Android sensor | IIO channels | Value |
|---|---|---|
| ACCELEROMETER (1) | `in_accel_{x,y,z}_raw` | (raw + offset) × scale, m/s² |
| MAGNETIC_FIELD (2) | `in_magn_{x,y,z}_raw` | (raw + offset) × scale × 100: gauss to µT |
| GYROSCOPE (4) | `in_anglvel_{x,y,z}_raw` | (raw + offset) × scale, rad/s |
| LIGHT (5) | `in_illuminance_raw` | (raw + offset) × scale, lux |
| PROXIMITY (8) | `in_proximity_raw`, `in_proximity_nearlevel` | 0 cm at or over the near level, else 5 cm |

Scale and offset are the axis's own (`in_accel_x_scale`) or the channel's
(`in_accel_scale`), defaulting to 1 and 0. A vector is turned into the device
frame by `in_<channel>_mount_matrix` or `mount_matrix` when the driver gives
one. IIO's device frame (x right, y up, z out of the screen) is Android's.

Accelerometer, magnetometer and gyroscope are continuous, 1–100 Hz. Light
and proximity are on-change; proximity is a wake-up sensor, as Android's
default proximity sensor is. IIO gives no sample width, so the maximum range
of the IMU's vectors is 32768 × scale, the magnetometer's Android's usual
4900 µT and the light sensor's 100000 lux.

Nothing is read until Android activates a sensor. Each active sensor then
has a thread that reads its raw channels at the batched period (10 ms to
1 s; on-change sensors every 200 ms, sending only changes, and the current
value on activation) and stamps the sample on CLOCK_BOOTTIME, Android's
sensor clock, at the middle of the read. Batching latency is ignored: there
is no FIFO, every sample is sent at once.

## Socket

One SOCK_SEQPACKET datagram per message: a little-endian `u32` type, then
its fields, unpadded. `host/deckard-sensors/src/wire.rs` and the HAL's
`Sensors.cpp` hold the two copies.

| HAL → host | Fields | Bytes |
|---|---|---|
| `HELLO` (1) | `u32 version` (1) | 8 |
| `ACTIVATE` (2) | `u32 handle, u32 enabled` | 12 |
| `BATCH` (3) | `u32 handle, i64 period_ns` | 16 |

| Host → HAL | Fields | Bytes |
|---|---|---|
| `SENSOR` (101) | `u32 handle, u32 type, u32 flags, f32 max_range, f32 resolution, f32 power_ma, i32 min_delay_us, i32 max_delay_us, name[48]` | 84 |
| `SENSORS_END` (102) | | 4 |
| `EVENT` (103) | `u32 handle, i64 timestamp_ns, f32 x, f32 y, f32 z` | 28 |

`HELLO` is answered with one `SENSOR` per sensor, then `SENSORS_END`.
Handles count from 1; `type` is Android's `SensorType`, `flags` its
`SensorFlagBits` (1 wake-up, 2 on-change). `name` is UTF-8, NUL-padded.
Scalar sensors send their value in `x`. A connection's sensors stop when it
closes.

## Android: the Deckard sensors HAL

`hardware/deckard/sensors` in the image tree (image patch
`hardware_deckard/0011`): `android.hardware.sensors@1.0-service.deckard`,
running as `system`. It still registers only when `deckard.stub_sensors_hal`
is set: `deckard-containerd` sets it whenever the host has no
`waydroid-sensord`, and frameworks_base reads the same property to keep
Android's display fixed to the compositor's rotation, so Android does not
turn its own display when the phone turns.

- **Start.** Connects and reads the list, retrying for up to 15 s, before
  registering, so a host service that starts late is still seen. With no
  host it registers with no sensors.
- **Events.** A reader thread queues `EVENT`s (at most 1024) for
  `poll()`. Accelerometer and gyroscope events report ACCURACY_HIGH; the
  magnetometer reports ACCURACY_MEDIUM, as nothing calibrates it for the
  phone's own magnetic parts.
- **Flush.** Nothing waits in a FIFO, so `flush()` of an active sensor
  queues FLUSH_COMPLETE at once.
- **Host restart.** The reader reconnects every second and sends `BATCH`
  and `ACTIVATE` again for the active sensors. The list stays the one read
  at start.
- **Threads.** Two binder threads, since `poll()` holds one.

The HAL is built outside Soong with `image/build/scripts/sensors-hal/build.sh`
and ships in the overlay (`image/overlay/vendor/bin/hw/`), over the stub in
the vendor image, until the image is rebuilt.

## Verified on the Zinwa Q25 (2026-11-21, kernel 7.0.0-q25)

`dumpsys sensorservice` lists `icm42607p-accel Accelerometer`,
`mmc5603 Magnetometer`, `icm42607p-gyro Gyroscope`, `hx32062se Light`
and `hx32062se Proximity` (wake-up), plus AOSP's fusion sensors. An NDK
client reading the default sensors (20 ms asked), phone flat on a desk:

| Sensor | Rate | Last sample |
|---|---|---|
| Accelerometer | 47.8/s | −0.26, 0.03, 9.91 m/s² (|g| 9.92) |
| Gyroscope | 95.2/s (fusion asks 100 Hz) | ≈0 rad/s |
| Magnetometer | 12.8/s | 109.6, 60.3, −165.6 µT (|B| 207.5) |
| Light | on change | 2273 lux |
| Proximity | on change | 5 cm (far) |
| Rotation vector (fusion) | 47.2/s | |

Reading all of them at once costs `deckard-sensors` about 13% of one core,
nothing while no sensor is active. Restarting `deckard-sensors` while an app
reads resumes its events; a container started before the host service
waited for it and listed all five.

Open:

- **Magnetometer magnitude.** Earth's field is 25–65 µT, and the Q25
  reports about 207 µT lying still. The z axis points the right way (down
  into the desk, negative); the driver cancels the bridge offset, but the
  phone's own hard iron (speaker, vibrator) is not calibrated out, and the
  scale has not been checked by turning the phone. Until it is, compass
  headings may be off; Android's fusion sensors and Sky Map use it as is.
- **Magnetometer axes.** The mmc5603 node has no mount matrix and identity
  is assumed. The accelerometer's frame checks out (+z up when flat); the
  magnetometer's x and y have not been checked against it.
- **Magnetometer rate.** The driver measures on each read (set and reset per
  axis), about 75 ms a sample, so it gives about 13 Hz whatever is asked.
- **Light level.** 2273 lux on an indoor desk is high; the value is the
  driver's raw × scale.
