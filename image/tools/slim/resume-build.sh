#!/bin/sh
# Run only after the lead releases the bulk USB I/O slot.
set -eu
test "$#" = 2 || { echo 'usage: resume-build.sh ORIGINAL_IMAGE_DIR BUILD_DIR' >&2; exit 2; }
slim_repo=$(CDPATH='' cd -- "$(dirname -- "$0")/../../.." && pwd)
slim_original=$1
slim_build=$2
mkdir -p "$slim_build/input"
exec 9>"$slim_build/repack.lock"
flock -n 9 || { echo 'Another slimming resume owns the build lock' >&2; exit 1; }
# Check source and any existing copy before building. Never replace mismatches.
python3 - "$slim_original" "$slim_build/input" <<'PY'
import hashlib
import json
import os
from pathlib import Path
import shutil
import sys

source, destination = map(Path, sys.argv[1:])
def digest(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()

records = {}
for name in ('system.img', 'vendor.img'):
    original, target = source / name, destination / name
    if not original.is_file():
        raise SystemExit(f'Missing original image: {original}')
    if not target.exists():
        temporary = target.with_name(name + '.copying')
        with temporary.open('xb') as output, original.open('rb') as stream:
            shutil.copyfileobj(stream, output, length=1024 * 1024)
        # Refuse to replace an input that appeared while copying.
        os.link(temporary, target)
        temporary.unlink()
    original_sha, copied_sha = digest(original), digest(target)
    if original_sha != copied_sha:
        raise SystemExit(f'Input differs from original; refusing build: {target}')
    records[name] = dict(original=str(original), copied=str(target),
                         bytes=target.stat().st_size, sha256=copied_sha)
    print(f'Input verified: {name} {copied_sha}', flush=True)
(destination / 'verified-inputs.json').write_text(json.dumps(records, indent=2) + '\n')
PY
python3 -B "$slim_repo/image/tools/slim/repack.py" \
    "$slim_build/input/system.img" "$slim_build/system-slim-v2.img"
echo "Verified artifact: $slim_build/system-slim-v2.img"
echo "Evidence: $slim_build/system-slim-v2.json and system-slim-v2.payloads.json"
