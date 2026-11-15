//! The host's cameras, as libcamera's GStreamer device provider sees them.

use gstreamer as gst;
use gstreamer::prelude::*;

use crate::wire::MAX_SIZES;

/// Larger frames stall SoftISP's viewfinder path today (docs/camera.md).
const MAX_WIDTH: u32 = 1920;
const MAX_HEIGHT: u32 = 1080;

/// The pixel format asked of libcamerasrc: libcamera's ABGR8888, bytes R, G, B, A.
pub const FORMAT: &str = "RGBA";

#[derive(Clone, Debug)]
pub struct Camera {
    /// libcamera's camera id, libcamerasrc's `camera-name`.
    pub id: String,
    /// libcamera `Location`, which has Android's `LENS_FACING` values:
    /// 0 front, 1 back, 2 external.
    pub facing: u32,
    /// libcamera `Rotation`, Android's `SENSOR_ORIENTATION`.
    pub rotation: u32,
    pub sizes: Vec<(u32, u32)>,
}

/// What a probe found: the usable cameras, and whether libcamera listed any
/// camera at all (one it lists without a usable size stays unusable; a
/// camera another process streams from is not listed).
pub struct Probe {
    pub cameras: Vec<Camera>,
    pub listed: bool,
}

/// The cameras, back first, then front, then external; none when libcamera's
/// GStreamer plugin is missing.
#[must_use]
pub fn cameras() -> Probe {
    let Some(provider) =
        gst::DeviceProviderFactory::find("libcameraprovider").and_then(|factory| factory.get())
    else {
        eprintln!("deckard-camera: no libcamera GStreamer provider");
        return Probe {
            cameras: Vec::new(),
            listed: false,
        };
    };
    let devices = provider.devices();
    let mut cameras: Vec<Camera> = devices
        .iter()
        .filter_map(|device| {
            let properties = device.properties()?;
            let int = |name: &str| {
                properties
                    .get::<i32>(name)
                    .ok()
                    .and_then(|value| u32::try_from(value).ok())
            };
            let camera = Camera {
                id: device.display_name().to_string(),
                facing: facing(&properties),
                rotation: int("api.libcamera.Rotation").unwrap_or(0) % 360,
                sizes: sizes(&device.caps()?),
            };
            eprintln!("deckard-camera: {camera:?}");
            (!camera.sizes.is_empty()).then_some(camera)
        })
        .collect();
    cameras.sort_by_key(|camera| match camera.facing {
        1 => 0,
        0 => 1,
        _ => 2,
    });
    Probe {
        cameras,
        listed: !devices.is_empty(),
    }
}

/// libcamera's `Location`, which libcamerasrc's provider gives by name.
fn facing(properties: &gst::StructureRef) -> u32 {
    match properties.get::<&str>("api.libcamera.Location") {
        Ok("Front") => 0,
        Ok("Back") => 1,
        _ => 2,
    }
}

/// The `FORMAT` sizes `caps` offers within the cap: each fixed size, and for a
/// range its largest 16:9 and 4:3 sizes.
fn sizes(caps: &gst::Caps) -> Vec<(u32, u32)> {
    let mut sizes = Vec::new();
    for structure in caps.iter() {
        if structure.get::<&str>("format").ok() != Some(FORMAT) {
            continue;
        }
        let fixed = |name: &str| {
            structure
                .get::<i32>(name)
                .ok()
                .and_then(|value| u32::try_from(value).ok())
        };
        let range = |name: &str| {
            let range = structure.get::<gst::IntRange<i32>>(name).ok()?;
            let step = u32::try_from(range.step()).ok()?.max(1);
            let min = u32::try_from(range.min()).ok()?;
            let max = u32::try_from(range.max()).ok()?;
            Some((min, max, step))
        };
        match (fixed("width"), fixed("height")) {
            (Some(width), Some(height)) => sizes.push((width, height)),
            _ => {
                let width = range("width").or_else(|| fixed("width").map(|w| (w, w, 1)));
                let height = range("height").or_else(|| fixed("height").map(|h| (h, h, 1)));
                if let (Some(width), Some(height)) = (width, height) {
                    sizes.extend(range_sizes(width, height));
                }
            }
        }
    }
    sizes.retain(|&(width, height)| width <= MAX_WIDTH && height <= MAX_HEIGHT);
    sizes.sort_unstable();
    sizes.dedup();
    if sizes.len() > MAX_SIZES {
        sizes.drain(..sizes.len() - MAX_SIZES);
    }
    sizes
}

/// The largest size in the range, if under the cap, else its largest 16:9 and
/// 4:3 sizes under the cap.
fn range_sizes(width: (u32, u32, u32), height: (u32, u32, u32)) -> Vec<(u32, u32)> {
    let fit = |value: u32, (min, max, step): (u32, u32, u32)| {
        let value = value.min(max);
        let value = value - (value - min.min(value)) % step;
        (value >= min).then_some(value)
    };
    let max = (width.1, height.1);
    if max.0 <= MAX_WIDTH && max.1 <= MAX_HEIGHT {
        return vec![max];
    }
    let mut out = Vec::new();
    for (num, den) in [(16, 9), (4, 3)] {
        let h = height.1.min(MAX_HEIGHT).min(MAX_WIDTH * den / num);
        if let (Some(w), Some(h)) = (fit(h * num / den, width), fit(h, height)) {
            out.push((w, h));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges_give_their_largest_sizes_under_the_cap() {
        assert_eq!(range_sizes((2, 1276, 2), (2, 720, 2)), vec![(1276, 720)]);
        assert_eq!(
            range_sizes((2, 4076, 2), (2, 3072, 2)),
            vec![(1920, 1080), (1440, 1080)]
        );
    }
}
