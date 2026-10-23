use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use deckard_core::palette::{Color, Theme};
use deckard_host_tools::files;
use deckard_image_tools::{fonts, hwcomposer, palette, permission_view};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

const USAGE: &str =
    "usage: deckard-image-tools palette|fonts|permission-view|hwcomposer [--option value]";

fn write_output(path: &Path, content: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent)?;
    let name = path.file_name().ok_or("output has no file name")?;
    Ok(files::replace(&parent.join(name), content, 0o644)?)
}

/// `--key value` and `--key=value` options, each given once.
fn options(mut arguments: impl Iterator<Item = OsString>) -> Result<BTreeMap<String, OsString>> {
    let mut options = BTreeMap::new();
    while let Some(key) = arguments.next() {
        let key = key.into_string().map_err(|_| "invalid option encoding")?;
        let (key, value) = match key.split_once('=') {
            Some((name, value)) => (name.to_owned(), OsString::from(value)),
            None => (key, arguments.next().ok_or("every option needs a value")?),
        };
        if !key.starts_with("--") || options.insert(key, value).is_some() {
            return Err("invalid or duplicate option".into());
        }
    }
    Ok(options)
}

fn run() -> Result<()> {
    let mut arguments = std::env::args_os().skip(1);
    let command = arguments.next().ok_or(USAGE)?;
    let command = command.to_str().unwrap_or_default().to_owned();
    let mut options = options(arguments)?;
    let mut take = |key: &str| options.remove(key).map(PathBuf::from);
    let output = take("--out").ok_or("missing --out")?;
    let (input, highlight, mcolors) = match command.as_str() {
        "palette" => (None, take("--highlight"), take("--mcolors")),
        "fonts" | "permission-view" | "hwcomposer" => {
            (Some(take("--input").ok_or("missing --input")?), None, None)
        }
        _ => {
            return Err(
                "unknown command; expected palette, fonts, permission-view or hwcomposer".into(),
            );
        }
    };
    if !options.is_empty() {
        return Err(format!("unknown {command} option").into());
    }
    let input = input.unwrap_or_default();
    match command.as_str() {
        "palette" => {
            let highlight = highlight.map_or(Ok("#7df0d4".to_owned()), |value| {
                value.into_os_string().into_string()
            });
            let highlight = Color::parse(&highlight.map_err(|_| "invalid highlight encoding")?)?;
            let theme = match mcolors {
                Some(path) => palette::theme(&fs::read_to_string(path)?)?,
                None => Theme::default(),
            };
            write_output(&output, palette::render(highlight, theme).as_bytes())
        }
        "fonts" => {
            let read = |name: &str| fs::read_to_string(input.join(name));
            let xml = read("fonts-lineage-20.xml")?;
            let generated = fonts::generate(
                &xml,
                &read("android-font-files.txt")?,
                &read("fonts_customization-lineage-20.xml")?,
            )?;
            let outputs = [
                ("fonts.xml", generated.xml),
                ("host-fonts.txt", generated.host),
                ("android-fonts.txt", generated.android),
                ("hidden-fonts.txt", generated.hidden),
            ];
            for (name, content) in outputs {
                write_output(&output.join(name), content.as_bytes())?;
            }
            Ok(())
        }
        "permission-view" => write_output(
            &output,
            permission_view::patch(&fs::read_to_string(input)?)?.as_bytes(),
        ),
        _ => write_output(&output, &hwcomposer::patch(&fs::read(input)?)?),
    }
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
