#![allow(clippy::unwrap_used, reason = "fixed protocol test fixtures")]

#[allow(
    dead_code,
    reason = "module is compiled standalone until daemon integration"
)]
#[path = "../src/sharing.rs"]
mod sharing;

use sharing::{request_id, simple_name, target_id};

#[test]
fn target_and_transfer_ids_are_bounded() {
    assert!(request_id("0123456789abcdef"));
    assert!(!request_id("0123456789ABCDEF"));
    assert!(target_id("org.example.app/.ShareActivity"));
    assert!(!target_id("../../etc/.Share"));
    assert!(!target_id("org.example.app/../../evil"));
    assert!(simple_name("picture.jpg"));
    assert!(!simple_name("../picture.jpg"));
    assert!(!simple_name("link\\picture.jpg"));
}
