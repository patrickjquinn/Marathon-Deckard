use std::collections::BTreeSet;
use std::fmt::Write as _;

use regex::Regex;

type Face<'a> = (u16, &'a str, &'a str);

const SANS: &[Face<'static>] = &[
    (100, "normal", "SourceSans3-ExtraLight.ttf"),
    (200, "normal", "SourceSans3-ExtraLight.ttf"),
    (300, "normal", "SourceSans3-Light.ttf"),
    (400, "normal", "SourceSans3-Regular.ttf"),
    (500, "normal", "SourceSans3-Semibold.ttf"),
    (600, "normal", "SourceSans3-Semibold.ttf"),
    (700, "normal", "SourceSans3-Bold.ttf"),
    (800, "normal", "SourceSans3-Bold.ttf"),
    (900, "normal", "SourceSans3-Bold.ttf"),
    (300, "italic", "SourceSans3-LightIt.ttf"),
    (400, "italic", "SourceSans3-It.ttf"),
];
const MONO: &[Face<'static>] = &[
    (400, "normal", "JetBrainsMono-Medium.ttf"),
    (500, "normal", "JetBrainsMono-Medium.ttf"),
];
const NOTO: &[Face<'static>] = &[
    (400, "normal", "NotoSans-Regular.ttf"),
    (700, "normal", "NotoSans-Bold.ttf"),
    (400, "italic", "NotoSans-Italic.ttf"),
    (700, "italic", "NotoSans-BoldItalic.ttf"),
];

const ALIASES: [(&str, &str, Option<u16>); 6] = [
    ("google-sans", "sans-serif", None),
    ("google-sans-text", "sans-serif", None),
    ("google-sans-medium", "sans-serif", Some(500)),
    ("google-sans-bold", "sans-serif", Some(700)),
    ("roboto", "sans-serif", None),
    ("roboto-medium", "sans-serif", Some(500)),
];
const HOST_EXTRA: [&str; 7] = [
    "NotoSerif-Regular.ttf",
    "NotoSerif-Bold.ttf",
    "NotoSerif-Italic.ttf",
    "NotoSerif-BoldItalic.ttf",
    "NotoSansSymbols-Regular.ttf",
    "NotoSansSymbols2-Regular.ttf",
    "NotoSansMath-Regular.ttf",
];

pub struct Outputs {
    pub xml: String,
    pub host: String,
    pub android: String,
    pub hidden: String,
}

fn family(attributes: &str, faces: &[Face<'_>]) -> String {
    let mut xml = format!("    <family{attributes}>\n");
    for (weight, style, file) in faces {
        let _ = writeln!(
            xml,
            "        <font weight=\"{weight}\" style=\"{style}\">{file}</font>"
        );
    }
    xml.push_str("    </family>");
    xml
}

fn file_names(pattern: &Regex, xml: &str) -> BTreeSet<String> {
    pattern
        .captures_iter(xml)
        .map(|capture| capture[1].to_owned())
        .collect()
}

fn list(names: impl IntoIterator<Item = String>) -> String {
    let names: Vec<_> = names.into_iter().collect();
    format!("{}\n", names.join("\n"))
}

/// Marathon's `fonts.xml` from Lineage's, with the font lists it needs.
///
/// # Errors
/// A pattern fails to compile or the XML names a font with no source.
pub fn generate(
    xml: &str,
    android_files: &str,
    customization: &str,
) -> Result<Outputs, Box<dyn std::error::Error>> {
    let families = Regex::new(r"(?s)    <family( [^>]*)?>.*?</family>")?;
    let names = Regex::new(r#"name="([^"]+)""#)?;
    let output = families.replace_all(xml, |capture: &regex::Captures<'_>| {
        let attributes = capture.get(1).map_or("", |matched| matched.as_str());
        let body = &capture[0];
        if let Some(name) = names.captures(attributes) {
            let faces = match &name[1] {
                "monospace" | "serif-monospace" => Some(MONO),
                "sans-serif"
                | "sans-serif-condensed"
                | "casual"
                | "cursive"
                | "sans-serif-smallcaps"
                | "source-sans-pro" => Some(SANS),
                _ => None,
            };
            if let Some(faces) = faces {
                return family(&format!(" name=\"{}\"", &name[1]), faces);
            }
        }
        if attributes.contains("ignore=\"true\"") && body.contains("NotoColorEmojiLegacy") {
            return String::new();
        }
        if body.contains("NotoSansSymbols-Regular-Subsetted.ttf") {
            return family("", NOTO);
        }
        if body.contains("NotoSansSymbols-Regular-Subsetted2.ttf") {
            return [
                family("", &[(400, "normal", "NotoSansSymbols-Regular.ttf")]),
                family("", &[(400, "normal", "NotoSansMath-Regular.ttf")]),
                family(
                    attributes,
                    &[(400, "normal", "NotoSansSymbols2-Regular.ttf")],
                ),
            ]
            .join("\n");
        }
        body.to_owned()
    });
    let mut aliases = String::new();
    for (name, target, weight) in ALIASES {
        let weight = weight.map_or_else(String::new, |value| format!(" weight=\"{value}\""));
        let _ = writeln!(
            aliases,
            "    <alias name=\"{name}\" to=\"{target}\"{weight} />"
        );
    }
    let thin = "    <alias name=\"sans-serif-thin\"";
    let output = output.replacen(thin, &format!("{aliases}{thin}"), 1);
    let files = Regex::new(r">\s*([\w.-]+\.(?:ttf|otf|ttc))")?;
    let named = file_names(&files, &output);
    let original = file_names(&files, xml);
    let mut custom = BTreeSet::new();
    for name in file_names(&files, customization) {
        custom.insert(format!("product/{name}"));
    }
    let mut host: BTreeSet<String> = HOST_EXTRA.iter().map(|file| (*file).to_owned()).collect();
    for (_, _, file) in [SANS, MONO, NOTO].concat() {
        host.insert(file.to_owned());
    }
    let mut android = BTreeSet::new();
    for line in android_files.lines() {
        if !line.trim().is_empty() {
            android.insert(line.trim().to_owned());
        }
    }
    let available: BTreeSet<_> = host.union(&android).cloned().collect();
    let missing: Vec<_> = named.difference(&available).collect();
    if !missing.is_empty() {
        return Err(format!("font references have no source: {missing:?}").into());
    }
    let (mut kept, mut hidden) = (Vec::new(), Vec::new());
    for name in &android {
        if named.contains(name) && !host.contains(name) {
            kept.push(name.clone());
        } else if !named.contains(name) && (original.contains(name) || custom.contains(name)) {
            hidden.push(name.clone());
        }
    }
    Ok(Outputs {
        xml: output,
        host: list(host.iter().cloned()),
        android: list(kept),
        hidden: list(hidden),
    })
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "tests compare all generated files with repository fixtures"
)]
mod tests {
    use super::*;

    #[test]
    fn all_generated_font_assets_match_the_existing_generator() {
        let output = generate(
            include_str!("../../vendor_deckard/fonts/fonts-lineage-20.xml"),
            include_str!("../../vendor_deckard/fonts/android-font-files.txt"),
            include_str!("../../vendor_deckard/fonts/fonts_customization-lineage-20.xml"),
        )
        .unwrap();
        assert_eq!(
            output.xml,
            include_str!("../../vendor_deckard/fonts/fonts.xml")
        );
        assert_eq!(
            output.host,
            include_str!("../../vendor_deckard/fonts/host-fonts.txt")
        );
        assert_eq!(
            output.android,
            include_str!("../../vendor_deckard/fonts/android-fonts.txt")
        );
        assert_eq!(
            output.hidden,
            include_str!("../../vendor_deckard/fonts/hidden-fonts.txt")
        );
    }

    #[test]
    fn rejects_unresolved_fonts_before_returning_outputs() {
        assert!(generate("    <family><font>missing.ttf</font></family>", "", "").is_err());
    }
}
