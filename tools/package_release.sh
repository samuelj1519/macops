#!/bin/sh
set -eu

if [ "$#" -ne 3 ]; then
  echo 'Usage: package_release.sh TAG BINARY OUTPUT_DIR' >&2
  exit 2
fi

tag=$1
binary=$2
output=$3
root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
printf '%s\n' "$tag" | grep -Exq 'v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)'
file -b "$binary" | grep -Fq 'Mach-O 64-bit executable arm64'
[ "$("$binary" --version)" = "macops ${tag#v}" ]

mkdir -p "$output"
name="macops-$tag-darwin-arm64.tar.gz"
python3 -B - "$binary" "$root/LICENSE" "$output/$name" <<'PY_PACKAGE'
import gzip
import io
from pathlib import Path
import sys
import tarfile

binary, license_file, archive = map(Path, sys.argv[1:])
with archive.open("xb") as raw:
    with gzip.GzipFile(filename="", fileobj=raw, mode="wb", mtime=0) as compressed:
        with tarfile.open(fileobj=compressed, mode="w", format=tarfile.USTAR_FORMAT) as tar:
            for name, path, mode in (("macops", binary, 0o755), ("LICENSE", license_file, 0o644)):
                data = path.read_bytes()
                info = tarfile.TarInfo(name)
                info.mode = mode
                info.size = len(data)
                tar.addfile(info, io.BytesIO(data))
PY_PACKAGE
(
  cd "$output"
  shasum -a 256 "$name" > SHA256SUMS
  shasum -a 256 -c SHA256SUMS
)
