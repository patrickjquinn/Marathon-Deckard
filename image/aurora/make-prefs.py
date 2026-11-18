#!/usr/bin/env python3
"""make-prefs.py: marathon.properties as the two Aurora Store preferences
deckard-aurora-profile adds (../overlay/system/etc/deckard-aurora-device.xml).
Aurora keeps a spoofed profile as DEVICE_SPOOF_ENABLED and, in
DEVICE_SPOOF_PROPERTIES, the properties as a JSON object."""
import html
import json
import pathlib

here = pathlib.Path(__file__).parent
props = {}
for line in (here / "marathon.properties").read_text(encoding="utf-8").splitlines():
    if line and not line.startswith("#"):
        key, value = line.split("=", 1)
        props[key] = value.replace("\\:", ":")
value = html.escape(json.dumps(props, separators=(",", ":")), quote=True)
(here / "../overlay/system/etc/deckard-aurora-device.xml").write_text(
    '    <boolean name="DEVICE_SPOOF_ENABLED" value="true" />\n'
    f'    <string name="DEVICE_SPOOF_PROPERTIES">{value}</string>\n',
    encoding="utf-8")
