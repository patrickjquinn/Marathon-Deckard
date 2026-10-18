# Marathon-Deckard

Deckard runs Android apps on Marathon OS as Marathon apps: their own cards,
Hub notifications, media controls, back gesture, keyboard and colours, with no
Android chrome. It builds on Waydroid.

| Directory | Licence | Contents |
|---|---|---|
| `image/` | GPL-3.0 | Android side: privileged apps, overlays and the files mounted over Android's system partition (`image/overlay`, the layout of Waydroid's `/var/lib/waydroid/overlay`) |
| `host/` | Apache-2.0 | Host daemons (`host/deckardd`) and packaging helpers |
| `protocol/` | | Wire protocols (`protocol/bridge.md`) |
| `docs/` | | Design notes and measured findings |

APKs are built on a developer host with `image/tools/build-apk.sh` (no
Gradle) and committed, because the Alpine build that packages Deckard has no
Android SDK. Marathon packages all of this as the `deckard` aport in
Marathon-Shell (`packaging/packages/deckard`).
