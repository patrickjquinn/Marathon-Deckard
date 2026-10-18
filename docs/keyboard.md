# Keyboard

How Marathon's keyboard (BB10-style, Hunspell prediction, drawn by the shell)
types into Android apps, and why LatinIME never shows. Spec §9; M3 exit
"Marathon keyboard works in every text field". Measured on the HackberryPi
CM5, Waydroid 1.6.3, LineageOS 20 VANILLA (20260927), 2026-10-18.

## Does Waydroid already speak text-input?

No. Neither `lineage-20` nor `dev/lineage-23.2` of
[android_hardware_waydroid](https://github.com/waydroid/android_hardware_waydroid)
binds `zwp_text_input_v3`, v1 or v2. `hwcomposer/wayland-hwc.cpp` includes
linux-dmabuf, viewporter, presentation-time, xdg-shell, tablet-v2,
pointer-constraints, relative-pointer, idle-inhibit and fractional-scale
client protocols, and nothing else for input; keys go in only through
`wl_keyboard` into the `/dev/input/wl_keyboard_events` pipe. Android's own IME
draws the keyboard. In multi-window mode its window becomes a separate
`InputMethod` toplevel (`modes/multi-window.cpp`, `LayerSplitType::RawName`).
23.2's "keyboard on the card it types into" is about placing that window.

So the spec's design is used, without rebuilding the image: an Android IME that
draws nothing, reporting to the host. The spec routes it through binder and a
forked hwcomposer acting as a text-input-v3 client. Here it goes through
deckardd and a shell D-Bus interface instead, because the hwcomposer can't be
changed without an image build. The shell end keeps text-input-v3's model
(content purpose and hints, surrounding text, commit, delete-surrounding), so
moving it into a forked hwcomposer later only changes the transport.

## Design

```
Marathon keyboard ──QInputMethodEvent / keys──▶ focused Android surface item
                                                 │ (RemoteTextInput event filter)
shell  org.marathonos.Shell.TextInput1 ◀─────────┘
   │  SetEditor / SetSurroundingText / ShowPanel / HidePanel / ClearEditor
   │  ▲ CommitString / Key / PanelChanged (targeted signals)
deckardd  Ime  (host/deckardd/src/ime.cpp)
   │  length-prefixed JSON, /data/deckard/ime.sock, SO_PEERCRED = DeckardIme's uid
DeckardIme  (image/apps/DeckardIme, InputMethodService, no visible window)
   │  InputConnection
Android app's editor
```

Wire format: [protocol/bridge.md, "IME channel"](../protocol/bridge.md#ime-channel).

**Android side (DeckardIme).**
- Reports `start` (package, `zwp_text_input_v3` purpose and hints mapped from
  `EditorInfo.inputType` in `ContentType.java`), `state` (256 characters either
  side of the cursor and the selection; nothing for password and PIN fields),
  `show` (`onShowInputRequested`), `hide` (`hideWindow`) and `finish`.
- Android opens a raw-key (`TYPE_NULL`) connection on every focused window
  that has no editor. These count only once the app asks for the keyboard,
  so a plain window never looks like a text field to the shell.
- Applies `commit` (a replacement selects the range and commits over it, one
  edit; Launcher's search reset itself when delete-then-insert left the field
  empty for a moment), and `key`. Enter runs the editor's IME action (Go,
  Search, Send, Next, Done), a newline in multi-line fields, or a plain Enter
  key. Backspace, arrows, Tab and Escape are key events through the
  InputConnection, so they stay in order with commits.
- Its window has `alpha = 0`. SurfaceFlinger skips invisible layers, so
  Waydroid's hwcomposer never gets an `InputMethod` layer and maps no window
  for it (checked: no InputMethod row in `dumpsys SurfaceFlinger` HWC layers
  while the IME was shown). `FLAG_NOT_TOUCHABLE` and an empty touchable
  region keep it out of touch dispatch.
- **IME inset.** `onComputeInsets` reports content and visible insets of
  exactly `covered` pixels from the window's bottom. `covered` is what the
  shell's keyboard hides of the app window (`panel` message, logical pixels,
  scaled by display height / window height). With `covered` 0 the IME frame
  is empty. Measured: before `panel` the IME source was `[0,628][720,628]`,
  after `panel covered=200 height=419` it was `[0,328][720,628]`, i.e. 300 px
  on the 720x628 Android display. The IME asks to be shown or hidden to match
  the keyboard (`requestShowSelf` and `requestHideSelf`), so the inset also
  follows the keyboard when the user opens it from the nav bar button.
- Defaults: `deckard-ime.rc` in the overlay runs `pm disable
  com.android.inputmethod.latin`, `ime enable` and `ime set` for DeckardIme at
  `sys.boot_completed`. DeckardIme lives in `/system/app` (it needs no
  privileged permission, so no privapp XML). `method.xml` sets `isDefault`.

**Host side.**
- deckardd's `Ime` validates everything (package rule, purpose 0–13, hints
  masked to `0x3ff`, text at most 4,096 characters, offsets clamped, key
  names allowlisted). It talks to the shell only once a valid `start` has
  arrived, and keeps the last `panel` per app so it can replay it when an
  editor starts after the keyboard is already up.
- The shell's `RemoteTextInput` (Marathon-Shell `shell/src/services/remotetextinput.*`)
  sits behind `TextInput1`. Only a declared session service with the
  `text-input` permission may `Register`, and it can only address app ids
  under the prefix it registered (`waydroid.`). Signals are targeted at it
  alone. For the focused surface item of the reported app, it answers
  Marathon's keyboard's input-method queries (enabled, hints, surrounding
  text, cursor, selection) from the report and turns its commits,
  replacements and keys into signals. Edits are applied to the cached text
  at once, so autocorrect and double-space-period see their own typing before
  Android reports back. On-screen keyboard keys carry a `QInputDevice` named
  `marathon-osk`; any other key event (a hardware keyboard) passes through to
  `wl_keyboard` untouched.
- `ShowPanel` opens the keyboard only when that app's window has focus and no
  hardware keyboard is present (the native rule from 58de90d3). `HidePanel`
  closes it.
- No double resize: resizing a Waydroid toplevel hot-plugs Android's whole
  display (and the hot-plug is what crashed system_server repeatedly in M0
  and today). So windows under the registered prefix are no longer shrunk by
  `virtualKeyboard.reservedHeight`. The shell reports what the keyboard covers
  (`PanelChanged`) and Android makes room through the IME inset, as on a
  phone.

## Hardware keyboard

Physical keys never go through DeckardIme. The shell passes them to the focused
Android window over `wl_keyboard`. Waydroid writes them to its virtual
`wayland_keyboard` evdev device, and Android maps them with its own keymap, so
modifiers, repeat and shortcuts behave as on a laptop. Because that virtual
keyboard always exists, Android's `Configuration.keyboard` is `qwerty` even on
a phone without one, so Android's "hide the soft keyboard when a hardware
keyboard is present" rule can't be trusted. DeckardIme therefore always reports
`show`, and the shell applies Marathon's rule (`Platform.hasHardwareKeyboard`,
from `/proc/bus/input/devices`). On the HackberryPi the keyboard is not forced
up. The nav bar's keyboard button still opens it, and then the inset follows.

## Status and evidence

See "Verified on the device" below. Unit tests: Marathon-Shell
`Shell_RemoteTextInput` (16 cases), Marathon-Deckard `host/deckardd` `ime`
(5 cases) and `ContentTypeTest` (17 input types, runs in
`image/apps/DeckardIme/build.sh`).

## Where the spec's approach is still needed

- The cursor rectangle isn't reported. Marathon's keyboard doesn't use one
  today. A floating candidate bar would need `CursorAnchorInfo` (DeckardIme:
  `requestCursorUpdates`).
- Preedit (composing text) isn't used: Marathon's keyboard commits directly.
- A forked hwcomposer speaking text-input-v3 per surface (spec §9) would remove
  the deckardd hop and the package-to-window mapping (`waydroid.<package>`),
  and would tie the editor to the exact surface rather than the package.
