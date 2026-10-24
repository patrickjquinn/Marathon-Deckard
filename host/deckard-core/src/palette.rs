//! Android's 65 Material You system colours from the Shell's highlight.

use std::fmt::Write;

use palette::convert::IntoColorUnclamped;
use palette::white_point::D65;
use palette::{Lch, LinSrgb, Srgb};

use crate::Error;

const SHADES: [u16; 13] = [0, 10, 50, 100, 200, 300, 400, 500, 600, 700, 800, 900, 1000];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Color(pub [u8; 3]);

impl Color {
    /// Parses `#rrggbb` or `#aarrggbb`, dropping the alpha.
    ///
    /// # Errors
    /// `value` is neither.
    pub fn parse(value: &str) -> Result<Self, Error> {
        let bytes = value
            .strip_prefix('#')
            .and_then(|hex| hex::decode(hex).ok());
        match bytes.as_deref() {
            Some(&[red, green, blue] | &[_, red, green, blue]) => Ok(Self([red, green, blue])),
            _ => Err(Error::InvalidColor),
        }
    }

    /// `#rrggbb`.
    #[must_use]
    pub fn hex(self) -> String {
        format!("#{}", hex::encode(self.0))
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Theme {
    pub elevation1: Color,
    pub elevation2: Color,
    pub elevation3: Color,
    pub elevation4: Color,
    pub elevation5: Color,
    pub background: Color,
    pub text_on_accent: Color,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            elevation1: Color([0x0a, 0x0a, 0x0b]),
            elevation2: Color([0x12, 0x12, 0x13]),
            elevation3: Color([0x1c, 0x1c, 0x1d]),
            elevation4: Color([0x28, 0x28, 0x29]),
            elevation5: Color([0x35, 0x35, 0x36]),
            background: Color([4, 4, 4]),
            text_on_accent: Color([0, 0, 0]),
        }
    }
}

fn to_lch(color: Color) -> (f64, f64) {
    let lch: Lch<D65, f64> = Srgb::from(color.0).into_format().into_color_unclamped();
    (lch.chroma, lch.hue.into_positive_degrees())
}

/// Gamma-encoded sRGB, with every channel at or below zero linear as -1.
fn to_rgb(lightness: f64, chroma: f64, hue: f64) -> [f64; 3] {
    let linear: LinSrgb<f64> = Lch::<D65, f64>::new(lightness, chroma, hue).into_color_unclamped();
    let encoded: [f64; 3] = Srgb::from_linear(linear).into();
    let linear: [f64; 3] = linear.into();
    [0, 1, 2].map(|index| {
        if linear[index] > 0.0 {
            encoded[index]
        } else {
            -1.0
        }
    })
}

fn in_gamut(channels: [f64; 3]) -> bool {
    channels
        .into_iter()
        .all(|channel| (-1e-4..=1.0 + 1e-4).contains(&channel))
}

fn tone(lightness: f64, chroma: f64, hue: f64) -> Color {
    let mut low = 0.0;
    let mut high = chroma;
    if in_gamut(to_rgb(lightness, high, hue)) {
        low = high;
    } else {
        for _ in 0..30 {
            let middle = (low + high) / 2.0;
            if in_gamut(to_rgb(lightness, middle, hue)) {
                low = middle;
            } else {
                high = middle;
            }
        }
    }
    Color(
        to_rgb(lightness, low, hue)
            .map(|channel| (channel.clamp(0.0, 1.0) * 255.0).round_ties_even() as u8),
    )
}

/// The `system_*` colours, named as Android's resources are.
#[must_use]
pub fn generate(highlight: Color, theme: Theme) -> Vec<(String, Color)> {
    let (chroma, hue) = to_lch(highlight);
    let (_, grey_hue) = to_lch(theme.elevation3);
    let ramp = |chroma, hue| SHADES.map(|shade| tone(100.0 - f64::from(shade) / 10.0, chroma, hue));
    let mut accent = ramp(chroma, hue);
    let secondary = ramp(16.0, hue);
    let mut neutral = ramp(1.5, grey_hue);
    let mut neutral_secondary = ramp(4.0, hue);
    accent[3] = highlight;
    accent[4] = highlight;
    accent[10] = theme.text_on_accent;
    neutral[3] = Color([255; 3]);
    neutral[9] = theme.elevation5;
    neutral[10] = theme.elevation3;
    neutral[11] = theme.background;
    neutral_secondary[4] = Color([0x9b; 3]);
    neutral_secondary[9] = theme.elevation4;
    neutral_secondary[10] = theme.elevation2;
    neutral_secondary[11] = theme.elevation1;
    let mut output = Vec::with_capacity(65);
    for (name, ramp) in [
        ("accent1", accent),
        ("accent2", secondary),
        ("accent3", accent),
        ("neutral1", neutral),
        ("neutral2", neutral_secondary),
    ] {
        let shades = SHADES.into_iter().zip(ramp);
        for (shade, color) in shades {
            output.push((format!("system_{name}_{shade}"), color));
        }
    }
    output
}

/// The colours as DeckardSystem's runtime palette file: `name #ffrrggbb`
/// per line.
#[must_use]
pub fn runtime_file(highlight: Color, theme: Theme) -> String {
    let mut file = String::new();
    for (name, color) in generate(highlight, theme) {
        let _ = writeln!(file, "{name} #ff{}", hex::encode(color.0));
    }
    file
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "tests compare the checked-in reference palette"
)]
mod tests {
    use super::*;

    #[test]
    fn all_65_colors_match_the_existing_boot_overlay() {
        let xml = include_str!(
            "../../../image/vendor_deckard/overlay/DeckardPalette/res/values/colors.xml"
        );
        let colors = generate(Color([0x7d, 0xf0, 0xd4]), Theme::default());
        assert_eq!(colors.len(), 65);
        for (name, color) in colors {
            assert!(
                xml.contains(&format!("<color name=\"{name}\">{}</color>", color.hex())),
                "{name}: {}",
                color.hex()
            );
        }
    }

    #[test]
    fn accent_and_neutral_roles_remain_pinned() {
        let highlight = Color([255, 0x6b, 0x9d]);
        let colors = generate(highlight, Theme::default());
        let value = |name| colors.iter().find(|(key, _)| key == name).unwrap().1;
        assert_eq!(value("system_accent1_100"), highlight);
        assert_eq!(value("system_accent3_200"), highlight);
        assert_eq!(value("system_neutral1_900"), Color([4; 3]));
        assert_eq!(value("system_accent1_800"), Color([0; 3]));
        assert_ne!(value("system_accent1_500"), highlight);
        assert_eq!(
            runtime_file(highlight, Theme::default()).lines().count(),
            65
        );
    }

    #[test]
    fn color_parser_handles_argb_without_accepting_malformed_input() {
        assert_eq!(Color::parse("#ff7DF0D4"), Ok(Color([0x7d, 0xf0, 0xd4])));
        for invalid in ["7df0d4", "#abc", "#zz0000", "#😀00"] {
            assert!(Color::parse(invalid).is_err());
        }
    }
}
