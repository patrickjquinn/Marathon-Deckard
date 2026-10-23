# deckard-overlay-sync

Copies Deckard's Android files into Waydroid's overlay before the container
starts (`ExecStartPre` of `deckard-container.service`), removes files an
earlier package shipped, leaves out files built for another image, and
reconciles the whiteouts and properties.

The binary defaults to `/usr/share/deckard` and `/var/lib/waydroid`.
`DECKARD_SHARE_DIR` and `DECKARD_WAYDROID_DIR` override these paths for staging
or tests. Real whiteout creation requires permission to create `c 0:0` device
nodes in the Waydroid overlay. The tests inject a fake whiteout backend and do
not require root.
