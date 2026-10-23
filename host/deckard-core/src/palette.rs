use std::f64::consts::PI;

use crate::Error;

pub const SHADES: [u16; 13] = [0, 10, 50, 100, 200, 300, 400, 500, 600, 700, 800, 900, 1000];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Color(pub [u8; 3]);

impl Color {
    pub fn parse(value: &str) -> Result<Self, Error> {
        let hex = value.strip_prefix('#').ok_or(Error::InvalidColor)?;
        if !matches!(hex.len(), 6 | 8) || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(Error::InvalidColor);
        }
        let hex = &hex[hex.len() - 6..];
        let mut channels = [0; 3];
        for (channel, offset) in channels.iter_mut().zip([0, 2, 4]) {
            *channel = u8::from_str_radix(&hex[offset..offset + 2], 16)
                .map_err(|_| Error::InvalidColor)?;
        }
        Ok(Self(channels))
    }

    pub fn hex(self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.0[0], self.0[1], self.0[2])
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

fn to_linear(channel: f64) -> f64 {
    if channel <= 0.04045 {
        channel / 12.92
    } else {
        ((channel + 0.055) / 1.055).powf(2.4)
    }
}

fn from_linear(channel: f64) -> f64 {
    if channel <= 0.0031308 {
        12.92 * channel
    } else {
        1.055 * channel.powf(1.0 / 2.4) - 0.055
    }
}

fn lab(value: f64) -> f64 {
    let delta: f64 = 6.0 / 29.0;
    if value > delta.powi(3) {
        value.cbrt()
    } else {
        value / (3.0 * delta * delta) + 4.0 / 29.0
    }
}

fn inverse_lab(value: f64) -> f64 {
    let delta: f64 = 6.0 / 29.0;
    if value > delta {
        value.powi(3)
    } else {
        3.0 * delta * delta * (value - 4.0 / 29.0)
    }
}

fn to_lch(color: Color) -> (f64, f64) {
    let [red, green, blue] = color.0.map(|channel| to_linear(f64::from(channel) / 255.0));
    let x = 0.4124564 * red + 0.3575761 * green + 0.1804375 * blue;
    let y = 0.2126729 * red + 0.7151522 * green + 0.0721750 * blue;
    let z = 0.0193339 * red + 0.1191920 * green + 0.9503041 * blue;
    let axis_a = 500.0 * (lab(x / 0.95047) - lab(y));
    let axis_b = 200.0 * (lab(y) - lab(z / 1.08883));
    (
        axis_a.hypot(axis_b),
        axis_b.atan2(axis_a).to_degrees().rem_euclid(360.0),
    )
}

fn to_rgb(lightness: f64, chroma: f64, hue: f64) -> [f64; 3] {
    let axis_a = chroma * (hue * PI / 180.0).cos();
    let axis_b = chroma * (hue * PI / 180.0).sin();
    let axis_y = (lightness + 16.0) / 116.0;
    let x = 0.95047 * inverse_lab(axis_y + axis_a / 500.0);
    let y = inverse_lab(axis_y);
    let z = 1.08883 * inverse_lab(axis_y - axis_b / 200.0);
    [
        3.2404542 * x - 1.5371385 * y - 0.4985314 * z,
        -0.9692660 * x + 1.8760108 * y + 0.0415560 * z,
        0.0556434 * x - 0.2040259 * y + 1.0572252 * z,
    ]
    .map(|channel| {
        if channel > 0.0 {
            from_linear(channel)
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
        for (shade, color) in SHADES.into_iter().zip(ramp) {
            output.push((format!("system_{name}_{shade}"), color));
        }
    }
    output
}

pub fn runtime_file(highlight: Color, theme: Theme) -> String {
    generate(highlight, theme)
        .into_iter()
        .map(|(name, color)| format!("{name} #ff{}\n", &color.hex()[1..]))
        .collect()
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
