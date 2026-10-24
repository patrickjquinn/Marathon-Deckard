#!/usr/bin/env python3
"""Deckard timings, read from dbus-monitor's receive times. Run as root.

  bench.py startup LABEL system|session BUSNAME PEERS 'START COMMAND'
      Runs the command and times, from just before it: the bus name's
      owner, PEERS accepted bridge sockets (0 skips) and, when BUSNAME is
      id.waydro.Session, Android's sys.boot_completed.
  bench.py latency LABEL mpris|notify|ime COUNT
      mpris:  Seek on the Notify Test player until the bridge's Seeked.
      notify: Notify Test's post (logcat time) until the bridge's Notify.
      ime:    an OSK tap's CommitString until the bridge relays Android's
              new editor state (SetSurroundingText). Needs a focused
              Android editor with the OSK shown, and its 'a' key at KEY_A.
  bench.py launches LABEL COUNT
      Times deckard-launch app launches, cycling through three apps.
Prints one JSON line.
"""

import json
import os
import re
import shlex
import statistics
import subprocess
import sys
import threading
import time

BUS = "unix:path=/run/user/1000/bus"
LXC = ["lxc-attach", "-n", "waydroid", "-P", "/var/lib/waydroid/lxc", "--"]
TEST = "org.marathonos.deckard.notifytest"
SOCKETS = "/home/user/.local/share/waydroid/data/deckard/"
KEY_A = os.environ.get("KEY_A", "38 490").split()
KEY_BACKSPACE = os.environ.get("KEY_BACKSPACE", "665 563").split()
HEADER = re.compile(r"^(signal|method call|method return) time=([\d.]+) (.*)")


def user(*command):
    line = " ".join(shlex.quote(c) for c in command)
    return ["su", "user", "-c", f"env DBUS_SESSION_BUS_ADDRESS={BUS} {line}"]


class Monitor:
    """dbus-monitor's messages as [header, time, body]."""

    def __init__(self, bus, rules):
        command = ["dbus-monitor", f"--{bus}", *rules]
        self.proc = subprocess.Popen(
            user(*command) if bus == "session" else command,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            text=True,
        )
        self.messages = []
        self.lock = threading.Lock()
        threading.Thread(target=self.read, daemon=True).start()
        time.sleep(1)

    def read(self):
        for line in self.proc.stdout:
            match = HEADER.match(line)
            with self.lock:
                if match:
                    self.messages.append([match[3], float(match[2]), ""])
                elif self.messages:
                    self.messages[-1][2] += line

    def find(self, member, after=0.0, text=None):
        """The first matching message's time after `after`, or None."""
        with self.lock:
            for header, when, body in self.messages:
                if when > after and f"member={member}" in header:
                    if text is None or text in body:
                        return when
        return None

    def stop(self):
        self.proc.terminate()


def android(*command):
    env = os.environ | {"PATH": "/system/bin:/system/xbin:/usr/sbin:/usr/bin:/sbin:/bin"}
    return subprocess.run(LXC + list(command), capture_output=True, text=True, env=env).stdout


def broadcast(kind, *extra):
    android("am", "broadcast", "-n", f"{TEST}/.TestReceiver",
            "-a", f"{TEST}.POST", "--es", "kind", kind, *extra)


def summary(label, kind, samples, **extra):
    ms = [round(s * 1000, 2) for s in samples if s is not None]
    result = {"label": label, "kind": kind, "n": len(ms), "missing": len(samples) - len(ms)}
    if ms:
        result |= {"median_ms": statistics.median(ms), "min_ms": min(ms), "max_ms": max(ms)}
    print(json.dumps(result | extra | {"samples_ms": ms}))


def peers(pid):
    """Android's sockets accepted by `pid`; they live in the container's netns."""
    inodes = set()
    for fd in os.listdir(f"/proc/{pid}/fd"):
        target = os.readlink(f"/proc/{pid}/fd/{fd}")
        if target.startswith("socket:["):
            inodes.add(target[8:-1])
    table = android("cat", "/proc/net/unix").splitlines()
    return sum(1 for row in table
               if SOCKETS in row and row.split()[5] == "03" and row.split()[6] in inodes)


def startup(label, bus, name, want_peers, command):
    monitor = Monitor(bus, [f"type=signal,member=NameOwnerChanged,arg0={name}"])
    start = time.time()
    proc = subprocess.Popen(command, shell=True)
    owned = booted = peered = returned = None
    while time.time() - start < 120:
        if returned is None and proc.poll() is not None:
            returned = time.time()
        if owned is None:
            owned = monitor.find("NameOwnerChanged", start, 'string ""\n   string ":')
        if owned and want_peers and peered is None:
            pid = subprocess.run(["sh", "-c", "pidof deckard-bridge deckardd"],
                                 capture_output=True, text=True).stdout.split()
            if pid and peers(pid[0]) >= want_peers:
                peered = time.time()
        if name == "id.waydro.Session" and booted is None:
            if android("getprop", "sys.boot_completed").strip() == "1":
                booted = time.time()
        if owned and returned and (peered or not want_peers) and (
                booted or name != "id.waydro.Session"):
            break
        time.sleep(0.05)
    monitor.stop()
    since = lambda t: None if t is None else round((t - start) * 1000)
    print(json.dumps({"label": label, "kind": "startup", "name": name,
                      "command_ms": since(returned), "name_ms": since(owned),
                      "peers_ms": since(peered), "boot_ms": since(booted)}))


def mpris(label, count):
    broadcast("media")
    time.sleep(2)
    names = subprocess.run(user("busctl", "--user", "list", "--no-legend"),
                           capture_output=True, text=True).stdout
    player = next(n.split()[0] for n in names.splitlines()
                  if n.startswith("org.mpris.MediaPlayer2.waydroid_org_marathonos_deckard_notifytest"))
    monitor = Monitor("session", [
        "type=method_call,interface=org.mpris.MediaPlayer2.Player,member=Seek",
        "type=signal,interface=org.mpris.MediaPlayer2.Player,member=Seeked"])
    samples = []
    for i in range(count):
        offset = "20000000" if i % 2 == 0 else "-20000000"
        before = time.time()
        subprocess.run(user("busctl", "--user", "call", "--", player, "/org/mpris/MediaPlayer2",
                            "org.mpris.MediaPlayer2.Player", "Seek", "x", offset))
        time.sleep(1)
        sent = monitor.find("Seek", before)
        seeked = sent and monitor.find("Seeked", sent)
        samples.append(seeked - sent if seeked else None)
    monitor.stop()
    broadcast("media_stop")
    summary(label, "mpris", samples)


def notify(label, count):
    tag = f"bench{int(time.time())}"
    monitor = Monitor("session", [
        "type=method_call,interface=org.freedesktop.Notifications,member=Notify"])
    start = time.time()
    for i in range(count):
        broadcast("plain", "--es", "text", f"{tag}-{i}")
        time.sleep(1)
    time.sleep(2)
    log = android("logcat", "-d", "-v", "epoch,usec", "-s", "DeckardTest:I")
    posts = [float(line.split()[0]) for line in log.splitlines()
             if line.rstrip().endswith("post plain") and float(line.split()[0]) >= start - 1]
    samples = []
    for i, posted in enumerate(posts[-count:]):
        arrived = monitor.find("Notify", posted - 1, f'"{tag}-{i}"')
        samples.append(None if arrived is None else arrived - posted)
    monitor.stop()
    broadcast("cancel")
    summary(label, "notify", samples, posts=len(posts))


def ime(label, count):
    monitor = Monitor("session", [
        "type=signal,interface=org.marathonos.Shell.TextInput1,member=CommitString",
        "type=method_call,interface=org.marathonos.Shell.TextInput1,member=SetSurroundingText"])
    samples = []
    for i in range(count):
        before = time.time()
        subprocess.run(["marathon-touchctl", "tap", *KEY_A])
        time.sleep(0.7)
        committed = monitor.find("CommitString", before, '"a"')
        relayed = committed and monitor.find("SetSurroundingText", committed, "a")
        samples.append(relayed - committed if relayed else None)
    for _ in range(count):
        subprocess.run(["marathon-touchctl", "tap", *KEY_BACKSPACE])
        time.sleep(0.15)
    monitor.stop()
    summary(label, "ime", samples)


def launches(label, count):
    apps = [TEST, "com.android.settings", "net.gsantner.markor"]
    samples = []
    for i in range(count):
        start = time.time()
        subprocess.run(["systemd-run", "--user", "-M", "user@", "--wait", "--quiet", "--collect",
                        "/usr/libexec/deckard-launch", "app", "launch", apps[i % len(apps)]])
        samples.append(time.time() - start)
        time.sleep(3)
    summary(label, "launch", samples)


def main():
    command, label, args = sys.argv[1], sys.argv[2], sys.argv[3:]
    if command == "startup":
        startup(label, args[0], args[1], int(args[2]), args[3])
    elif command == "launches":
        launches(label, int(args[0]))
    else:
        {"mpris": mpris, "notify": notify, "ime": ime}[args[0]](label, int(args[1]))


if __name__ == "__main__":
    main()
