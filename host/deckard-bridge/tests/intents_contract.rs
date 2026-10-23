#![allow(clippy::unwrap_used, reason = "fixed contract fixtures")]

#[allow(
    dead_code,
    reason = "module is compiled standalone until daemon integration"
)]
#[path = "../src/intents.rs"]
mod intents;

use intents::{BusCall, Effect, SystemIntents, request_id, route, safe_name, settings_route};
use serde_json::{Value, json};

fn map(value: Value) -> serde_json::Map<String, Value> {
    value.as_object().unwrap().clone()
}

#[test]
fn routes_and_validation_match_cxx() {
    assert_eq!(
        route(&map(
            json!({"kind":"url","uri":"https://f-droid.org/packages/"})
        )),
        Some(BusCall::Keel {
            app: "browser",
            action: "browser.open",
            args: json!({"url":"https://f-droid.org/packages/","newTab":true}),
            timeout_ms: 30_000
        })
    );
    assert!(route(&map(json!({"kind":"url","uri":"file:///etc/passwd"}))).is_none());
    assert!(route(&map(json!({"kind":"tel","uri":"sms:1"}))).is_none());
    let sms = route(&map(
        json!({"kind":"sms","uri":"smsto:0871234567?body=hi","body":"draft"}),
    ))
    .unwrap();
    assert!(
        matches!(sms,BusCall::Navigation{app:"messages",route, args,..} if route=="conversation" && args["body"]=="draft")
    );
    let tel = route(&map(json!({"kind":"tel","uri":"tel:$(reboot)"}))).unwrap();
    assert!(matches!(
        tel,
        BusCall::Navigation {
            method: "LaunchApp",
            app: "phone",
            ..
        }
    ));
    assert_eq!(
        settings_route("android.settings.APP_NOTIFICATION_SETTINGS", "com.example"),
        "notifications/waydroid.com.example"
    );
    assert_eq!(
        settings_route(
            "android.settings.MANAGE_ALL_FILES_ACCESS_PERMISSION",
            "../../bad"
        ),
        "androidaccess/all_files"
    );
    assert!(request_id("0123456789abcdef"));
    assert!(!request_id("0123456789ABCDEF"));
    assert_eq!(safe_name("../../bad\0.png"), "bad.png");
}

#[test]
fn authenticated_generation_focus_and_bounded_picks() {
    let dir = tempfile::tempdir().unwrap();
    let mut bridge =
        SystemIntents::new(dir.path().join("xfer"), dir.path().join("camera")).unwrap();
    assert_eq!(
        bridge.connected(7),
        vec![Effect::Send(map(json!({"t":"focus","pkg":""})))]
    );
    assert_eq!(
        bridge.foreground_changed("waydroid.com.example"),
        Some(Effect::Send(map(json!({"t":"focus","pkg":"com.example"}))))
    );
    assert_eq!(bridge.foreground_changed("waydroid.com.example"), None);
    assert!(
        bridge
            .handle_authenticated(
                6,
                &map(json!({"t":"pick","id":"0123456789abcdef","mode":"open"}))
            )
            .is_empty()
    );
    for n in 0..4 {
        let id = format!("{n:016x}");
        assert!(matches!(
            &bridge.handle_authenticated(7, &map(json!({"t":"pick","id":id,"mode":"open"})))[0],
            Effect::Call(BusCall::Portal { .. })
        ));
    }
    let refusal = bridge.handle_authenticated(
        7,
        &map(json!({"t":"pick","id":"ffffffffffffffff","mode":"open"})),
    );
    assert!(matches!(&refusal[0],Effect::Send(message) if message["ok"]==false));
    assert_eq!(bridge.disconnected(6), Vec::new());
    assert_eq!(bridge.disconnected(7).len(), 4);
}

#[test]
fn portal_copy_rejects_symlinks_and_save_is_bounded() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("sample.txt");
    std::fs::write(&input, "hello").unwrap();
    let mut bridge =
        SystemIntents::new(dir.path().join("xfer"), dir.path().join("camera")).unwrap();
    bridge.connected(1);
    let id = "0123456789abcdef";
    bridge.handle_authenticated(1, &map(json!({"t":"pick","id":id,"mode":"open"})));
    let uri = format!("file://{}", input.display());
    let reply = bridge.portal_response(id, 0, &[uri]);
    assert!(matches!(&reply[0],Effect::Send(m) if m["ok"]==true && m["files"][0]["size"]==5));
    assert_eq!(
        std::fs::read(dir.path().join("xfer").join(id).join("sample.txt")).unwrap(),
        b"hello"
    );
    let link = dir.path().join("link.txt");
    std::os::unix::fs::symlink(&input, &link).unwrap();
    let id = "0123456789abcdee";
    bridge.handle_authenticated(1, &map(json!({"t":"pick","id":id,"mode":"open"})));
    let reply = bridge.portal_response(id, 0, &[format!("file://{}", link.display())]);
    assert!(
        matches!(&reply[0],Effect::Send(m) if m["ok"]==false),
        "{reply:?}"
    );
}
