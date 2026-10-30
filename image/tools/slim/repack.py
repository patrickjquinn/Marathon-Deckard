#!/usr/bin/env python3
"""Remove packaged whiteout targets from an EROFS image, without root/extraction.

Stream retained payloads through dump.erofs into mkfs.erofs --tar=f. Image-merge
mode cannot import this compressed Android image in erofs-utils 1.9.4. Tar PAX
headers preserve original ownership, permissions, timestamps and xattrs.
No Android code, signatures, APEX contents or vendor image are changed.
"""

import argparse
import base64
import hashlib
import io
import json
import mmap
import os
from pathlib import Path, PurePosixPath
import stat
import struct
import subprocess
import tarfile


class Erofs:
    """Read metadata/flat directories in the Android 16 EROFS format.

    Fail closed on unsupported formats. File decompression remains the job of
    erofs-utils, not this metadata reader.
    """

    def __init__(self, path):
        self.path = path
        with open(path, "rb") as f:
            self.data = mmap.mmap(f.fileno(), 0, access=mmap.ACCESS_READ)
        if self.unpack("I", 1024)[0] != 0xE0F5E1E2:
            raise ValueError("input is not an EROFS image")
        self.block = 1 << self.data[1036]
        self.meta = self.unpack("I", 1064)[0] * self.block
        self.shared = self.unpack("I", 1068)[0] * self.block
        self.time = self.unpack("Q", 1048)[0]
        if self.data[1115]:
            raise ValueError("long xattr prefixes are unsupported")
        self.entries = {}
        self.walk("", self.unpack("H", 1038)[0])

    def unpack(self, fmt, offset):
        return struct.unpack_from("<" + fmt, self.data, offset)

    def xattr(self, offset):
        length, index, size = self.unpack("BBH", offset)
        prefixes = {1: "user.", 2: "system.posix_acl_access",
                    3: "system.posix_acl_default", 4: "trusted.",
                    5: "lustre.", 6: "security."}
        name = prefixes[index] + self.data[offset + 4:offset + 4 + length].decode()
        value = self.data[offset + 4 + length:offset + 4 + length + size]
        return name, value, (4 + length + size + 3) & ~3

    def inode(self, nid):
        off = self.meta + nid * 32
        form, count, mode = self.unpack("HHH", off)
        extended = form & 1
        header = 64 if extended else 32
        size = self.unpack("Q" if extended else "I", off + 8)[0]
        uid, gid = self.unpack("II" if extended else "HH", off + 24)
        mtime = self.unpack("Q", off + 32)[0] if extended else self.time
        nsec = self.unpack("I", off + 40)[0] if extended else self.unpack("I", 1056)[0]
        xsize = 12 + 4 * (count - 1) if count else 0
        attrs = {}
        if count:
            start = off + header
            shared_count = self.data[start + 4]
            for i in range(shared_count):
                index = self.unpack("I", start + 12 + i * 4)[0]
                key, value, _ = self.xattr(self.shared + index * 4)
                attrs[key] = value
            cursor = start + 12 + shared_count * 4
            while cursor < start + xsize:
                key, value, length = self.xattr(cursor)
                attrs[key] = value
                cursor += length
        return dict(nid=nid, mode=mode, uid=uid, gid=gid, mtime=mtime, nsec=nsec,
                    xattrs=attrs, size=size, layout=(form >> 1) & 7,
                    raw=self.unpack("I", off + 16)[0], inline=off + header + xsize)

    def flat(self, entry):
        size = entry["size"]
        if entry["layout"] not in (0, 2):
            raise ValueError("compressed directory/symlink unsupported")
        whole = size // self.block * self.block if entry["layout"] == 2 else size
        start = entry["raw"] * self.block
        content = self.data[start:start + whole]
        if whole < size:
            content += self.data[entry["inline"]:entry["inline"] + size - whole]
        return content

    def walk(self, path, nid):
        entry = self.inode(nid)
        self.entries[path] = entry
        if stat.S_ISLNK(entry["mode"]):
            entry["target"] = self.flat(entry)
        if not stat.S_ISDIR(entry["mode"]):
            return
        content = self.flat(entry)
        for start in range(0, len(content), self.block):
            block = content[start:start + self.block]
            end = struct.unpack_from("<H", block, 8)[0]
            for cursor in range(0, end, 12):
                child, nameoff = struct.unpack_from("<QH", block, cursor)
                nextoff = struct.unpack_from("<H", block, cursor + 20)[0] if cursor + 12 < end else len(block)
                name = block[nameoff:nextoff].rstrip(b"\0").decode()
                if name not in (".", ".."):
                    self.walk(path + "/" + name if path else name, child)


def run(*args):
    subprocess.run([str(a) for a in args], check=True)


def digest(path):
    with open(path, "rb") as f:
        return hashlib.file_digest(f, "sha256").hexdigest()


def payload_digest(image, path):
    with subprocess.Popen(["dump.erofs", "--cat", "--path=/" + path, str(image)],
                          stdout=subprocess.PIPE) as process:
        result = hashlib.file_digest(process.stdout, "sha256").hexdigest()
        if process.wait():
            raise RuntimeError(f"dump.erofs failed: {path}")
    return result


def tar_info(path, metadata):
    info = tarfile.TarInfo(path or ".")
    info.mode = stat.S_IMODE(metadata["mode"])
    info.uid, info.gid = metadata["uid"], metadata["gid"]
    info.mtime = metadata["mtime"]
    info.pax_headers = {"LIBARCHIVE.xattr." + key: base64.b64encode(value).decode()
                        for key, value in metadata["xattrs"].items()}
    info.pax_headers["mtime"] = f'{metadata["mtime"]}.{metadata["nsec"]:09d}'
    return info


def write_image(source, output, before, paths, override, payload):
    command = ["mkfs.erofs", "--quiet", "--tar=f", "-zlz4hc,9", "-b4096",
               "-C4096", "--workers=1", str(output)]
    with subprocess.Popen(command, stdin=subprocess.PIPE) as builder:
        try:
            hardlinks = {}
            streamed = 0
            with tarfile.open(fileobj=builder.stdin, mode="w|", format=tarfile.PAX_FORMAT) as archive:
                for path, metadata in sorted(before.entries.items()):
                    if any(path == target or path.startswith(target + "/") for target in paths):
                        continue
                    if path == override:
                        continue
                    info = tar_info(path, metadata)
                    if stat.S_ISDIR(metadata["mode"]):
                        info.type = tarfile.DIRTYPE
                        archive.addfile(info)
                    elif stat.S_ISLNK(metadata["mode"]):
                        info.type = tarfile.SYMTYPE
                        info.linkname = metadata["target"].decode()
                        archive.addfile(info)
                    elif stat.S_ISREG(metadata["mode"]):
                        if metadata["nid"] in hardlinks:
                            info.type = tarfile.LNKTYPE
                            info.linkname = hardlinks[metadata["nid"]]
                            archive.addfile(info)
                            continue
                        hardlinks[metadata["nid"]] = path
                        info.size = metadata["size"]
                        with subprocess.Popen(["dump.erofs", "--cat", "--path=/" + path, str(source)],
                                              stdout=subprocess.PIPE) as reader:
                            archive.addfile(info, reader.stdout)
                            if reader.wait():
                                raise RuntimeError(f"dump.erofs failed: {path}")
                        streamed += 1
                        if streamed % 250 == 0:
                            print(f"Streamed {streamed} retained file payloads", flush=True)
                    else:
                        raise ValueError(f"unsupported inode type: {path}")
                metadata = dict(before.entries["system/etc/sysconfig"], mode=stat.S_IFREG | 0o644,
                                uid=0, gid=0, xattrs={"security.selinux": b"u:object_r:system_file:s0\0"})
                info = tar_info(override, metadata)
                info.size = len(payload)
                archive.addfile(info, io.BytesIO(payload))
        finally:
            builder.stdin.close()
        if builder.wait():
            raise RuntimeError("mkfs.erofs failed")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=Path)
    parser.add_argument("output", type=Path, help="new, nonexistent output image")
    parser.add_argument("--plan", action="store_true", help="read metadata only; no build or image hashing")
    args = parser.parse_args()
    if not args.plan and args.output.exists():
        parser.error("output already exists; refusing to overwrite")
    image = Path(__file__).resolve().parents[2]
    before = Erofs(args.source)
    paths = [line.split("#", 1)[0].strip() for line in (image / "whiteouts").read_text().splitlines()]
    paths = [p for p in paths if p and p in before.entries]
    protected = ("DeckardHome", "DeckardSystem", "DeckardIme", "SystemUI", "PermissionController", "Settings")
    for path in paths:
        if any(part in protected for part in PurePosixPath(path).parts):
            parser.error(f"refusing to remove core runtime package: {path}")
    override = "system/etc/sysconfig/deckard-runtime-apps.xml"
    payload = (image / "overlay" / override).read_bytes()
    if args.plan:
        print(json.dumps(dict(source=str(args.source), output=str(args.output),
                              removed_paths=paths, retained_settings_backend=True,
                              source_entries=len(before.entries)), indent=2))
        return
    args.output.parent.mkdir(parents=True, exist_ok=True)
    partial = args.output.with_name(args.output.name + ".partial")
    # Exclusive reservation prevents accidentally replacing another interrupted build.
    with partial.open("xb"):
        pass
    write_image(args.source, partial, before, paths, override, payload)
    run("fsck.erofs", "--extract", partial)
    after = Erofs(partial)
    removed = lambda p: any(p == target or p.startswith(target + "/") for target in paths)
    expected = {p for p in before.entries if not removed(p)} | {override}
    if set(after.entries) != expected:
        raise ValueError("unexpected filesystem paths after rebuild")
    checked = 0
    hardlinks = {}
    for path, entry in before.entries.items():
        if removed(path):
            continue
        for key in ("mode", "uid", "gid", "mtime", "nsec", "xattrs", "target"):
            if entry.get(key) != after.entries[path].get(key):
                raise ValueError(f"metadata changed: {path}: {key}")
        if not stat.S_ISDIR(entry["mode"]) and entry["size"] != after.entries[path]["size"]:
            raise ValueError(f"file size changed: {path}")
        if stat.S_ISREG(entry["mode"]):
            actual = after.entries[path]["nid"]
            if hardlinks.setdefault(entry["nid"], actual) != actual:
                raise ValueError(f"hardlink changed: {path}")
        checked += 1
    def check_payload(path):
        original = payload_digest(args.source, path)
        if original != payload_digest(partial, path):
            raise ValueError(f"file contents changed: {path}")
        return path, original
    files = sorted(p for p, e in before.entries.items()
                   if not removed(p) and stat.S_ISREG(e["mode"]))
    print(f"Metadata verified for {checked} paths; comparing {len(files)} file payloads", flush=True)
    payloads = {}
    for index, path in enumerate(files, 1):
        checked_path, checksum = check_payload(path)
        payloads[checked_path] = checksum
        if index % 250 == 0:
            print(f"Verified {index}/{len(files)} retained payloads", flush=True)
    if payload_digest(partial, override) != hashlib.sha256(payload).hexdigest():
        raise ValueError("Settings component override contents changed")
    added = after.entries[override]
    if (added["mode"], added["uid"], added["gid"], added["xattrs"]) != (
            stat.S_IFREG | 0o644, 0, 0, {"security.selinux": b"u:object_r:system_file:s0\0"}):
        raise ValueError("Settings component override metadata changed")
    evidence = dict(source_sha256=digest(args.source), output_sha256=digest(partial),
                    source_bytes=args.source.stat().st_size, output_bytes=partial.stat().st_size,
                    compressed_saved_bytes=args.source.stat().st_size - partial.stat().st_size,
                    compressed_saved_percent=round(100 * (1 - partial.stat().st_size /
                                                           args.source.stat().st_size), 4),
                    metadata_verified_paths=checked, removed_paths=paths,
                    verified_payload_files=len(payloads),
                    removed_logical_bytes=sum(e["size"] for p, e in before.entries.items()
                                              if removed(p) and stat.S_ISREG(e["mode"])),
                    retained_settings_backend="system/system_ext/priv-app/Settings/Settings.apk",
                    whiteouts_sha256=digest(image / "whiteouts"),
                    override_sha256=hashlib.sha256(payload).hexdigest(),
                    added=[override])
    # Publish only after filesystem, path, metadata and full retained payload checks pass.
    os.link(partial, args.output)
    partial.unlink()
    args.output.with_suffix(".payloads.json").write_text(json.dumps(payloads, indent=2) + "\n")
    args.output.with_suffix(".json").write_text(json.dumps(evidence, indent=2) + "\n")
    print(json.dumps(evidence, indent=2))


if __name__ == "__main__":
    main()
