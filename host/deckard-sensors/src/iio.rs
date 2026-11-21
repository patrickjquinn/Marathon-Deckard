//! The host's IIO sensors, found by their sysfs channels and read through
//! `in_*_raw` one sample at a time.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub const SYSFS: &str = "/sys/bus/iio/devices";

/// Android's distance for a far proximity reading; near is 0.
pub const FAR_CM: f32 = 5.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    Accelerometer,
    MagneticField,
    Gyroscope,
    Light,
    Proximity,
}

impl Kind {
    const ALL: [Self; 5] = [
        Self::Accelerometer,
        Self::MagneticField,
        Self::Gyroscope,
        Self::Light,
        Self::Proximity,
    ];

    /// Android's `SensorType`.
    #[must_use]
    pub fn android_type(self) -> u32 {
        match self {
            Self::Accelerometer => 1,
            Self::MagneticField => 2,
            Self::Gyroscope => 4,
            Self::Light => 5,
            Self::Proximity => 8,
        }
    }

    /// Reported when the value changes, not at a rate.
    #[must_use]
    pub fn on_change(self) -> bool {
        matches!(self, Self::Light | Self::Proximity)
    }

    #[must_use]
    pub fn wake_up(self) -> bool {
        self == Self::Proximity
    }

    fn channel(self) -> &'static str {
        match self {
            Self::Accelerometer => "accel",
            Self::MagneticField => "magn",
            Self::Gyroscope => "anglvel",
            Self::Light => "illuminance",
            Self::Proximity => "proximity",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Accelerometer => "Accelerometer",
            Self::MagneticField => "Magnetometer",
            Self::Gyroscope => "Gyroscope",
            Self::Light => "Light",
            Self::Proximity => "Proximity",
        }
    }

    /// IIO's unit to Android's: gauss to microtesla; m/s², rad/s and lux
    /// are the same in both.
    fn unit(self) -> f64 {
        if self == Self::MagneticField {
            100.0
        } else {
            1.0
        }
    }
}

const AXES: [&str; 3] = ["x", "y", "z"];

#[derive(Debug)]
enum Reading {
    /// Device frame = `matrix` × ((raw + offset) × scale), per axis.
    Vector {
        raw: [PathBuf; 3],
        offset: [f64; 3],
        scale: [f64; 3],
        matrix: [[f64; 3]; 3],
    },
    Scalar {
        raw: PathBuf,
        offset: f64,
        scale: f64,
    },
    /// Near when the raw value reaches `near`.
    Near { raw: PathBuf, near: i64 },
}

#[derive(Debug)]
pub struct Sensor {
    pub kind: Kind,
    pub name: String,
    pub max_range: f32,
    pub resolution: f32,
    reading: Reading,
}

fn read_text(path: &Path) -> io::Result<String> {
    Ok(fs::read_to_string(path)?.trim().to_owned())
}

fn invalid(path: &Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("{}: not a number", path.display()),
    )
}

fn read_i64(path: &Path) -> io::Result<i64> {
    read_text(path)?.parse().map_err(|_| invalid(path))
}

fn read_f64(path: &Path) -> Option<f64> {
    read_text(path).ok()?.parse().ok()
}

/// `in_<channel>_<axis>_<info>`, else the channel's shared `in_<channel>_<info>`.
fn info(dir: &Path, channel: &str, axis: Option<&str>, info: &str) -> Option<f64> {
    axis.and_then(|axis| read_f64(&dir.join(format!("in_{channel}_{axis}_{info}"))))
        .or_else(|| read_f64(&dir.join(format!("in_{channel}_{info}"))))
}

/// IIO's `mount_matrix`: "x0, y0, z0; x1, y1, z1; x2, y2, z2", row i giving
/// device axis i from the sensor's axes.
fn parse_matrix(text: &str) -> Option<[[f64; 3]; 3]> {
    let mut matrix = [[0.0; 3]; 3];
    let mut rows = text.split(';');
    for row in &mut matrix {
        let mut cells = rows.next()?.split(',');
        for cell in row.iter_mut() {
            *cell = cells.next()?.trim().parse().ok()?;
        }
        if cells.next().is_some() {
            return None;
        }
    }
    rows.next().is_none().then_some(matrix)
}

const IDENTITY: [[f64; 3]; 3] = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

fn matrix(dir: &Path, channel: &str) -> [[f64; 3]; 3] {
    [
        format!("in_{channel}_mount_matrix"),
        "mount_matrix".to_owned(),
    ]
    .iter()
    .find_map(|file| parse_matrix(&read_text(&dir.join(file)).ok()?))
    .unwrap_or(IDENTITY)
}

/// The sensor of `kind` that the IIO device at `dir` carries, if any.
fn probe(dir: &Path, device: &str, kind: Kind) -> Option<Sensor> {
    let channel = kind.channel();
    let unit = kind.unit();
    let (reading, max_range, resolution) = match kind {
        Kind::Accelerometer | Kind::MagneticField | Kind::Gyroscope => {
            let raw = AXES.map(|axis| dir.join(format!("in_{channel}_{axis}_raw")));
            if !raw.iter().all(|path| path.exists()) {
                return None;
            }
            let scale =
                AXES.map(|axis| info(dir, channel, Some(axis), "scale").unwrap_or(1.0) * unit);
            let offset = AXES.map(|axis| info(dir, channel, Some(axis), "offset").unwrap_or(0.0));
            let finest = scale.iter().copied().fold(f64::INFINITY, f64::min);
            // IIO does not give the sample width: IMUs are 16-bit, and for
            // magnetometers Android's usual 4900 µT is reported.
            let max_range = if kind == Kind::MagneticField {
                4900.0
            } else {
                scale.iter().copied().fold(0.0, f64::max) * 32768.0
            };
            let matrix = matrix(dir, channel);
            (
                Reading::Vector {
                    raw,
                    offset,
                    scale,
                    matrix,
                },
                max_range,
                finest,
            )
        }
        Kind::Light => {
            let raw = dir.join(format!("in_{channel}_raw"));
            if !raw.exists() {
                return None;
            }
            let scale = info(dir, channel, None, "scale").unwrap_or(1.0) * unit;
            let offset = info(dir, channel, None, "offset").unwrap_or(0.0);
            (
                Reading::Scalar { raw, offset, scale },
                // Direct sunlight; the sample width is not given here either.
                100_000.0,
                scale,
            )
        }
        Kind::Proximity => {
            let raw = dir.join(format!("in_{channel}_raw"));
            let near = read_i64(&dir.join(format!("in_{channel}_nearlevel"))).ok()?;
            if !raw.exists() {
                return None;
            }
            (
                Reading::Near { raw, near },
                f64::from(FAR_CM),
                f64::from(FAR_CM),
            )
        }
    };
    #[allow(
        clippy::cast_possible_truncation,
        reason = "Android's sensor list is in f32"
    )]
    Some(Sensor {
        kind,
        name: format!("{device} {}", kind.label()),
        max_range: max_range as f32,
        resolution: resolution as f32,
        reading,
    })
}

/// The first sensor of each kind under `root` (`SYSFS`), by device number,
/// in `Kind` order.
#[must_use]
pub fn discover(root: &Path) -> Vec<Sensor> {
    let mut devices: Vec<(u32, PathBuf)> = fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let number = entry
                .file_name()
                .to_str()?
                .strip_prefix("iio:device")?
                .parse()
                .ok()?;
            Some((number, entry.path()))
        })
        .collect();
    devices.sort();
    Kind::ALL
        .iter()
        .filter_map(|&kind| {
            devices.iter().find_map(|(_, dir)| {
                let device = read_text(&dir.join("name")).ok()?;
                probe(dir, &device, kind)
            })
        })
        .collect()
}

impl Sensor {
    /// One sample in Android's units and frame; scalars in `[0]`.
    ///
    /// # Errors
    /// A channel could not be read.
    pub fn read(&self) -> io::Result<[f32; 3]> {
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_precision_loss,
            reason = "samples are 20 bits at most; Android's events are f32"
        )]
        let sample = match &self.reading {
            Reading::Vector {
                raw,
                offset,
                scale,
                matrix,
            } => {
                let mut sensor = [0.0; 3];
                for (i, value) in sensor.iter_mut().enumerate() {
                    *value = (read_i64(&raw[i])? as f64 + offset[i]) * scale[i];
                }
                matrix.map(|row| {
                    (row[0] * sensor[0] + row[1] * sensor[1] + row[2] * sensor[2]) as f32
                })
            }
            Reading::Scalar { raw, offset, scale } => {
                [((read_i64(raw)? as f64 + offset) * scale) as f32, 0.0, 0.0]
            }
            Reading::Near { raw, near } => {
                let distance = if read_i64(raw)? >= *near { 0.0 } else { FAR_CM };
                [distance, 0.0, 0.0]
            }
        };
        Ok(sample)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "tests use local temporary fixtures")]
mod tests {
    use super::*;

    fn device(root: &Path, number: u32, name: &str, files: &[(&str, &str)]) {
        let dir = root.join(format!("iio:device{number}"));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("name"), format!("{name}\n")).unwrap();
        for (file, text) in files {
            fs::write(dir.join(file), format!("{text}\n")).unwrap();
        }
    }

    /// The Zinwa Q25's sensors as its sysfs shows them.
    fn q25(root: &Path) {
        device(root, 0, "MT6358", &[("in_voltage5_raw", "100")]);
        device(
            root,
            1,
            "hx32062se",
            &[
                ("in_illuminance_raw", "37080"),
                ("in_illuminance_scale", "0.026940"),
                ("in_proximity_raw", "0"),
                ("in_proximity_nearlevel", "600"),
            ],
        );
        device(
            root,
            2,
            "mmc5603",
            &[
                ("in_magn_x_raw", "16384"),
                ("in_magn_y_raw", "0"),
                ("in_magn_z_raw", "-8192"),
                ("in_magn_scale", "0.000061035"),
            ],
        );
        device(
            root,
            3,
            "icm42607p-accel",
            &[
                ("in_accel_x_raw", "-216"),
                ("in_accel_y_raw", "24"),
                ("in_accel_z_raw", "8259"),
                ("in_accel_scale", "0.001197101"),
                ("mount_matrix", "1, 0, 0; 0, 1, 0; 0, 0, 1"),
            ],
        );
        device(
            root,
            4,
            "icm42607p-gyro",
            &[
                ("in_anglvel_x_raw", "-2"),
                ("in_anglvel_y_raw", "20"),
                ("in_anglvel_z_raw", "-3"),
                ("in_anglvel_scale", "0.000532632"),
                ("mount_matrix", "1, 0, 0; 0, 1, 0; 0, 0, 1"),
            ],
        );
    }

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-3
    }

    #[test]
    fn the_q25s_sensors_are_found_in_kind_order() {
        let root = tempfile::tempdir().unwrap();
        q25(root.path());
        let sensors = discover(root.path());
        let found: Vec<_> = sensors.iter().map(|s| (s.kind, s.name.as_str())).collect();
        assert_eq!(
            found,
            [
                (Kind::Accelerometer, "icm42607p-accel Accelerometer"),
                (Kind::MagneticField, "mmc5603 Magnetometer"),
                (Kind::Gyroscope, "icm42607p-gyro Gyroscope"),
                (Kind::Light, "hx32062se Light"),
                (Kind::Proximity, "hx32062se Proximity"),
            ]
        );
    }

    #[test]
    fn samples_are_in_androids_units() {
        let root = tempfile::tempdir().unwrap();
        q25(root.path());
        let sensors = discover(root.path());
        let read = |kind| {
            sensors
                .iter()
                .find(|s| s.kind == kind)
                .map(|s| s.read().unwrap())
                .unwrap_or_default()
        };
        let accel = read(Kind::Accelerometer);
        assert!(close(accel[2], 9.886), "{accel:?}");
        let magn = read(Kind::MagneticField);
        assert!(close(magn[0], 100.0) && close(magn[2], -50.0), "{magn:?}");
        assert!(close(read(Kind::Light)[0], 998.935));
        assert!(close(read(Kind::Proximity)[0], FAR_CM));
        fs::write(root.path().join("iio:device1/in_proximity_raw"), "600").unwrap();
        assert!(close(read(Kind::Proximity)[0], 0.0));
    }

    #[test]
    fn the_mount_matrix_turns_samples_into_the_device_frame() {
        let root = tempfile::tempdir().unwrap();
        device(
            root.path(),
            0,
            "imu",
            &[
                ("in_accel_x_raw", "1"),
                ("in_accel_y_raw", "2"),
                ("in_accel_z_raw", "3"),
                ("in_accel_mount_matrix", "0, -1, 0; 1, 0, 0; 0, 0, -1"),
            ],
        );
        let sensors = discover(root.path());
        let sample = sensors[0].read().unwrap();
        assert_eq!(sample, [-2.0, 1.0, -3.0]);
    }

    #[test]
    fn proximity_needs_its_near_level() {
        let root = tempfile::tempdir().unwrap();
        device(root.path(), 0, "prox", &[("in_proximity_raw", "3")]);
        assert!(discover(root.path()).is_empty());
        assert_eq!(parse_matrix("1, 0; 0, 1"), None);
    }
}
