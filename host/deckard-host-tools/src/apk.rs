use std::io;
use std::path::Path;

pub fn arguments(path: &Path) -> io::Result<Vec<String>> {
    let path = path.canonicalize()?;
    if !path.is_file()
        || !path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("apk"))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "expected a regular .apk file",
        ));
    }
    let path = path
        .to_str()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "APK path is not UTF-8"))?;
    let payload = serde_json::to_string(&serde_json::json!({"path": path}))?;
    Ok([
        "--user",
        "call",
        "org.marathonos.store",
        "/org/shipwright/Keel/Actions",
        "org.shipwright.Keel.Actions",
        "Invoke",
        "ssa{sv}",
        "apps.openFile",
        &payload,
        "0",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect())
}
