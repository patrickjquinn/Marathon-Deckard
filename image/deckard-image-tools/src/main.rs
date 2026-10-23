use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use deckard_core::palette::{Color, Theme};
use deckard_image_tools::{fonts, hwcomposer, palette, permission_view};

fn write_output(path: &Path, content: &[u8]) -> Result<(), Box<dyn std::error::Error>> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent)?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.write_all(content)?;
    temporary.as_file().sync_all()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        temporary
            .as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o644))?;
    }
    temporary.persist(path)?;
    Ok(())
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args_os().skip(1);
    let command = arguments.next().ok_or(
        "usage: deckard-image-tools palette|fonts|permission-view|hwcomposer [--option value]",
    )?;
    let mut options = BTreeMap::new();
    while let Some(key) = arguments.next() {
        let key = key.into_string().map_err(|_| "invalid option encoding")?;
        let (key, value) = if let Some((name, value)) = key.split_once('=') {
            (name.to_owned(), std::ffi::OsString::from(value))
        } else {
            (key, arguments.next().ok_or("every option needs a value")?)
        };
        if !key.starts_with("--") || options.insert(key, value).is_some() {
            return Err("invalid or duplicate option".into());
        }
    }
    let mut required = |key: &str| {
        options
            .remove(key)
            .map(PathBuf::from)
            .ok_or_else(|| format!("missing {key}"))
    };
    let output = required("--out")?;
    match command.to_str() {
        Some("palette") => {
            let highlight = options.remove("--highlight").map_or_else(
                || Ok("#7df0d4".to_owned()),
                |value| {
                    value
                        .into_string()
                        .map_err(|_| "invalid highlight encoding")
                },
            )?;
            let theme = match options.remove("--mcolors") {
                Some(path) => palette::theme(&std::fs::read_to_string(path)?)?,
                None => Theme::default(),
            };
            if !options.is_empty() {
                return Err("unknown palette option".into());
            }
            write_output(
                &output,
                palette::render(Color::parse(&highlight)?, theme).as_bytes(),
            )?;
        }
        Some("fonts") => {
            let input = options.remove("--input").ok_or("missing --input")?;
            if !options.is_empty() {
                return Err("unknown fonts option".into());
            }
            let input = PathBuf::from(input);
            let generated = fonts::generate(
                &std::fs::read_to_string(input.join("fonts-lineage-20.xml"))?,
                &std::fs::read_to_string(input.join("android-font-files.txt"))?,
                &std::fs::read_to_string(input.join("fonts_customization-lineage-20.xml"))?,
            )?;
            for (name, content) in [
                ("fonts.xml", generated.xml),
                ("host-fonts.txt", generated.host),
                ("android-fonts.txt", generated.android),
                ("hidden-fonts.txt", generated.hidden),
            ] {
                write_output(&output.join(name), content.as_bytes())?;
            }
        }
        Some("permission-view") => {
            let input = options.remove("--input").ok_or("missing --input")?;
            if !options.is_empty() {
                return Err("unknown permission-view option".into());
            }
            let patched = permission_view::patch(&std::fs::read_to_string(input)?)?;
            write_output(&output, patched.as_bytes())?;
        }
        Some("hwcomposer") => {
            let input = options.remove("--input").ok_or("missing --input")?;
            if !options.is_empty() {
                return Err("unknown hwcomposer option".into());
            }
            let patched = hwcomposer::patch(&std::fs::read(input)?)?;
            write_output(&output, &patched)?;
        }
        _ => {
            return Err(
                "unknown command; expected palette, fonts, permission-view or hwcomposer".into(),
            );
        }
    }
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("deckard-image-tools: {error}");
            ExitCode::FAILURE
        }
    }
}
