//! `waydroid.cfg`, the instance's INI file (upstream `tools/config`).

/// The name of a `[section]` header line.
#[must_use]
pub fn section(line: &str) -> Option<&str> {
    line.trim().strip_prefix('[')?.strip_suffix(']')
}

/// The value of `key` in `[waydroid]`; names match case-insensitively.
#[must_use]
pub fn get<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    let mut in_waydroid = false;
    for line in text.lines() {
        if let Some(name) = section(line) {
            in_waydroid = name.eq_ignore_ascii_case("waydroid");
        } else if in_waydroid
            && let Some((candidate, value)) = line.split_once('=')
            && candidate.trim().eq_ignore_ascii_case(key)
        {
            return Some(value.trim());
        }
    }
    None
}
