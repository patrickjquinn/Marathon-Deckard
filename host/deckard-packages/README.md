# Deckard Packages adapter

Rust replacement for the removed C++ `deckardd` packages.{cpp,h}. It uses
`deckard-core` for package identities, bridge frame limits, text cleaning and
bounded base64 decoding. `deckard-bridge` serves it on the name it owns.

The live Pi exposes `org.marathonos.Deckard1.Packages` at
`/org/marathonos/Deckard`:

| Member | Input | Output |
| --- | --- | --- |
| `InspectPackage` | `s` | `a{sv}` |
| `InstallPackage` | `s` | `s` |
| `UninstallPackage` | `s` | `s` |
| `DeviceProfile` | none | `a{sv}` |
| `ListPackages` | none | `aa{sv}` |
| `Progress` signal | `si` | none |
| `Finished` signal | `sbss` | none |
| `PackageChanged` signal | `ss` | none |

An integrator creates `Packages` with its already-owned session bus, the
host-side install directory, a bridge sender, and a **fail-closed** mutation
policy. Register `packages.interface()` at the existing object path; the
interface holds a weak reference so it does not retain the bus connection.
Construct the service against the real install directory only after the old
daemon has stopped: construction removes stale `.apk` files, as C++ does.
Pass only authenticated Android bridge messages with their verified route to
`packages.handle(route, type, payload)`, and call `packages.bridge_lost(route)`
on that bridge's disconnect. A reply from the wrong bridge cannot complete a
request. Route `BridgeRoute::System`
to DeckardSystem's `system.sock`; all other requests go to `bridge.sock`.
The host's policy must resolve the D-Bus unique sender to a PID, accept the
verified `/usr/bin/marathon-store-handler` executable, or resolve the PID and
bounded ancestor chain through Shell `AppIdForPid` to `settings` or `store`.
Never trust a caller-supplied app ID or permit mutation based on bus name alone.
The policy is asynchronous and has a five-second fail-closed timeout.

The adapter caps APKs at 1 GiB, simultaneous staging reservations at 2 GiB,
outstanding requests at 64, bridge responses at the core's 1 MiB frame size,
package lists at 2,000 entries, and all returned strings/arrays/icons at the
C++ limits. One timeout worker serves all requests. Staged files are removed
on reply, timeout, disconnect, send failure and service drop.

`cargo test -p deckard-packages --locked` runs private-bus wire fixtures.
These are not production cutover tests. Remaining work: integrate the adapter
with the future Rust daemon's authenticated bridge listeners and caller
resolver; verify real Android PackageInstaller and DeckardSystem round trips
and Store/Settings behavior on device; compare signal ordering and failure
messages with the old daemon; switch ownership only after full daemon parity.
