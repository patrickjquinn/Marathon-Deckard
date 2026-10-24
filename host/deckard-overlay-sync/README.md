# deckard-overlay-sync

Copies Deckard's Android files into Waydroid's overlay before the container
starts (`ExecStartPre` of `deckard-container.service`), removes files an
earlier package shipped, leaves out files built for another image, and
reconciles the whiteouts and properties.

The binary reads `/usr/share/deckard` and writes `/var/lib/waydroid`.
Whiteouts are `c 0:0` device nodes, which Linux 5.8 and later let any user
create, so the tests need no root.
