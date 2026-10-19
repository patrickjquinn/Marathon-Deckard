#!/bin/sh
# deckard-reset against a fake home, passwd and systemctl.
# Usage: test-deckard-reset.sh <path to deckard-reset>
set -eu

helper=$1
W=$(mktemp -d)
trap 'chmod -R u+w "$W" 2> /dev/null; rm -rf "$W"' EXIT
uid=$(id -u)
# The helper refuses root's data: as root (an aport build), act for another uid.
[ "$uid" != 0 ] || uid=4242
home=$W/home
data=$home/.local/share/waydroid/data
mkdir -p "$W/bin" "$home/.local/share/applications" "$W/etc"
echo "user:x:$uid:$uid::$home:/bin/sh" > "$W/passwd"
cat > "$W/bin/systemctl" <<EOS
#!/bin/sh
echo "\$*" >> "$W/systemctl.log"
case "\$*" in
	"is-active --quiet waydroid-container.service") [ -f "$W/active" ] ;;
	*) [ "\${FAIL_ON:-@@}" != "\$1" ] ;;
esac
EOS
chmod +x "$W/bin/systemctl"
DECKARD_RESET_PASSWD=$W/passwd
DECKARD_RESET_SYSTEMCTL=$W/bin/systemctl
export DECKARD_RESET_PASSWD DECKARD_RESET_SYSTEMCTL

fail() {
	echo "FAIL: $*" >&2
	exit 1
}

seed() {
	rm -rf "$data"
	mkdir -p "$data/app/org.fdroid.fdroid" "$data/system" "$data/deckard/xfer/0123456789abcdef" \
		"$data/deckard/install" "$data/.hidden"
	echo secret > "$data/system/packages.list"
	echo palette > "$data/deckard/palette"
	echo x > "$data/deckard/xfer/0123456789abcdef/photo.jpg"
	echo x > "$data/deckard/install/app.apk"
	ln -s "$W/etc" "$data/etc-link"
	echo keep > "$W/etc/keep"
	touch "$home/.local/share/applications/waydroid.org.fdroid.fdroid.desktop" \
		"$home/.local/share/applications/org.marathonos.notes.desktop"
	: > "$W/systemctl.log"
	[ "$(id -u)" != 0 ] || chown -R "$uid" "$home"
}

run() {
	rc=0
	env "$@" sh "$helper" > "$W/out" 2> "$W/err" || rc=$?
}

# Through pkexec only, never for root.
seed
run DECKARD_RESET_UID=
[ "$rc" = 2 ] || fail "without a uid exited $rc"
run DECKARD_RESET_UID=0
[ "$rc" = 2 ] || fail "for root exited $rc"
rc=0
PKEXEC_UID=$uid sh "$helper" extra > "$W/out" 2> "$W/err" || rc=$?
[ "$rc" = 2 ] || fail "with an argument exited $rc"
[ -f "$data/system/packages.list" ] || fail "refusals removed data"

# Android's data goes; deckardd's sockets and files stay; the container
# is stopped first and started again only if it was running.
touch "$W/active"
run PKEXEC_UID="$uid"
[ "$rc" = 0 ] || fail "reset exited $rc: $(cat "$W/err")"
[ ! -e "$data/app" ] && [ ! -e "$data/system" ] && [ ! -e "$data/.hidden" ] || fail "Android's data is still there"
[ ! -e "$data/etc-link" ] && [ -f "$W/etc/keep" ] || fail "a symlink was followed"
[ -f "$data/deckard/palette" ] || fail "deckard/ was removed"
[ ! -e "$data/deckard/xfer" ] && [ ! -e "$data/deckard/install" ] || fail "transfers are still there"
[ ! -e "$home/.local/share/applications/waydroid.org.fdroid.fdroid.desktop" ] || fail "Android app entries stayed"
[ -e "$home/.local/share/applications/org.marathonos.notes.desktop" ] || fail "a Marathon app entry went"
[ "$(sed -n 2p "$W/systemctl.log")" = "stop waydroid-container.service" ] || fail "container not stopped first: $(cat "$W/systemctl.log")"
[ "$(tail -n 1 "$W/systemctl.log")" = "start waydroid-container.service" ] || fail "container not started again"

seed
rm -f "$W/active"
run PKEXEC_UID="$uid"
[ "$rc" = 0 ] || fail "reset with the container stopped exited $rc"
! grep -q "^start" "$W/systemctl.log" || fail "started a container that wasn't running"

# Nothing is removed when the container won't stop.
seed
run PKEXEC_UID="$uid" FAIL_ON=stop
[ "$rc" = 1 ] && [ -f "$data/system/packages.list" ] || fail "removed data with the container running ($rc)"

# The data directory must be the user's own, inside their home.
seed
rm -rf "$data"
mkdir -p "$W/elsewhere/system"
ln -s "$W/elsewhere" "$data"
run PKEXEC_UID="$uid"
[ "$rc" = 1 ] && [ -d "$W/elsewhere/system" ] || fail "followed data out of the home ($rc)"
rm "$data"

run PKEXEC_UID="$uid"
[ "$rc" = 3 ] || fail "with no data exited $rc"
echo "PASS"
