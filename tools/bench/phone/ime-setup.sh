#!/bin/sh
# Brings Markor forward with its search field focused and the OSK shown
# (720x720 HackberryPi coordinates); check with a screenshot.
su user -c "env DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus busctl --user call \
	org.marathonos.Shell /org/marathonos/Shell/Navigation org.marathonos.Shell.Navigation1 \
	LaunchApp s deckard.net.gsantner.markor"
sleep 4
marathon-touchctl tap 622 82
sleep 2
marathon-touchctl tap 358 242
sleep 2
marathon-touchctl tap 678 695
sleep 2
