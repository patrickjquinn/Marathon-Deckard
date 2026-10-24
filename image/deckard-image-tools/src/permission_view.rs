use regex::Regex;

/// `GrantPermissionsActivity` smali with its handheld view handler replaced
/// by `MarathonGrantView`.
///
/// # Errors
/// The smali does not hold exactly one handler to replace.
pub fn patch(smali: &str) -> Result<String, Box<dyn std::error::Error>> {
    let implementation = regex::escape(
        "Lcom/android/permissioncontroller/permission/ui/handheld/GrantPermissionsViewHandlerImpl;",
    );
    let pattern = Regex::new(&format!(
        r"(?s)    new-instance v0, {implementation}\n(?P<target>\n    iget-object v1, p0, Lcom/android/permissioncontroller/permission/ui/GrantPermissionsActivity;->mTargetPackage:Ljava/lang/String;\n).*?    invoke-virtual \{{v0, p0\}}, {implementation}->setResultListener\([^)]*\){implementation}\n\n    move-result-object v0\n"
    ))?;
    let matches = pattern.find_iter(smali).count();
    if matches != 1 {
        return Err(format!(
            "GrantPermissionsActivity: expected one handheld view handler, found {matches}"
        )
        .into());
    }
    Ok(pattern.replace(smali, |capture: &regex::Captures<'_>| {
        let view = "Lorg/marathonos/deckard/permission/MarathonGrantView;";
        format!("    new-instance v0, {view}\n{}\n    invoke-direct {{v0, p0, v1, p0}}, {view}-><init>(Landroid/app/Activity;Ljava/lang/String;Lcom/android/permissioncontroller/permission/ui/GrantPermissionsViewHandler$ResultListener;)V\n", &capture["target"])
    }).into_owned())
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "tests inspect a successful guarded transformation"
)]
mod tests {
    use super::*;

    fn fixture() -> String {
        let implementation = "Lcom/android/permissioncontroller/permission/ui/handheld/GrantPermissionsViewHandlerImpl;";
        format!(
            "    new-instance v0, {implementation}\n\n    iget-object v1, p0, Lcom/android/permissioncontroller/permission/ui/GrantPermissionsActivity;->mTargetPackage:Ljava/lang/String;\n    arbitrary original instructions\n    invoke-virtual {{v0, p0}}, {implementation}->setResultListener(Lcom/android/permissioncontroller/permission/ui/GrantPermissionsViewHandler$ResultListener;){implementation}\n\n    move-result-object v0\n"
        )
    }

    #[test]
    fn replaces_exactly_one_handler_and_preserves_surrounding_code() {
        let output = patch(&format!("before\n{}after\n", fixture())).unwrap();
        assert!(output.starts_with("before\n"));
        assert!(output.ends_with("after\n"));
        assert!(output.contains("MarathonGrantView;-><init>"));
        assert!(!output.contains("arbitrary original instructions"));
        assert!(!output.contains("move-result-object"));
    }

    #[test]
    fn refuses_unknown_ambiguous_or_already_patched_inputs() {
        assert!(patch("").is_err());
        assert!(patch(&fixture().repeat(2)).is_err());
        assert!(patch(&patch(&fixture()).unwrap()).is_err());
    }
}
