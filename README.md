# Marathon-Deckard

Deckard runs Android apps on Marathon OS as Marathon apps: their own cards,
Hub notifications, media controls, back gesture, keyboard and colours, with no
Android chrome. It builds on Waydroid.

| Directory | Licence | Contents |
|---|---|---|
| `image/` | GPL-3.0 | Android side: privileged apps, overlays and the files mounted over Android's system partition (`image/overlay`, the layout of Waydroid's `/var/lib/waydroid/overlay`) |
| `host/` | Apache-2.0 | Host daemons (Rust workspace in `host/`, the user bridge `host/deckard-bridge`) and packaging helpers |
| `protocol/` | | Wire protocols (`protocol/bridge.md`) |
| `docs/` | | Design notes and measured findings |

APKs are built on a developer host with `image/tools/build-apk.sh` (no
Gradle) and committed, because the Alpine build that packages Deckard has no
Android SDK. Marathon packages all of this as the `deckard` aport in
Marathon-Shell (`packaging/packages/deckard`).

## Rust migration

The Rust workspace contains tested core primitives, image tools and native
shared-folder/APK-opening helpers. The installed daemon and package still use
the existing implementation until the integration cutover is verified.

```sh
cargo test --workspace --frozen
cargo clippy --workspace --all-targets --frozen -- -D warnings
cargo build --workspace --release --frozen
```

Image generation now uses Rust:

```sh
cargo run --locked -p deckard-image-tools -- palette --highlight '#7df0d4' --out /tmp/colors.xml
cargo run --locked -p deckard-image-tools -- fonts --input image/vendor_deckard/fonts --out /tmp/deckard-fonts
```

`build-chrome.sh` and `patch-permissioncontroller.sh` invoke the Rust image
tools. They need Cargo in addition to the existing Android build tools.

The implementation status, measured progress and cross-repository tasks are
in [the Marathon-Shell handoff](docs/rust-upgrade-shell-handoff.md).
