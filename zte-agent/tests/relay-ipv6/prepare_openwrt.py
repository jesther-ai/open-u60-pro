#!/usr/bin/env python3
"""Fetch pinned official OpenWrt test artifacts; never contacts a router."""

import hashlib
import io
import json
from pathlib import Path
import sys
import tarfile
import urllib.request


def download(url, digest, path):
    data = path.read_bytes() if path.exists() else urllib.request.urlopen(url, timeout=60).read()
    if hashlib.sha256(data).hexdigest() != digest:
        raise RuntimeError(f"Checksum mismatch: {path.name}")
    path.write_bytes(data)
    return data


def prepare(cache):
    if sys.version_info < (3, 12):
        raise RuntimeError("Use Python 3.12+ for safe tar extraction (macOS: /opt/homebrew/bin/python3)")
    manifest = json.loads(Path(__file__).with_name("openwrt-assets.json").read_text())
    cache.mkdir(parents=True, exist_ok=True)
    rootfs = manifest["rootfs"]
    download(rootfs["url"], rootfs["sha256"], cache / "openwrt-rootfs.tar.gz")
    extra = cache / "ow-extra"
    extra.mkdir(exist_ok=True)
    for filename, digest in manifest["packages"].items():
        data = download(manifest["base_url"] + filename, digest, cache / filename)
        with tarfile.open(fileobj=io.BytesIO(data), mode="r:gz") as archive:
            member = next(m for m in archive if m.name.lstrip("./") == "data.tar.gz")
            with tarfile.open(fileobj=archive.extractfile(member), mode="r:gz") as payload:
                # Requires Python 3.12+; prevents package paths escaping cache.
                payload.extractall(extra, filter="data")
    print(f"Verified official OpenWrt rootfs and ip-full dependencies in {cache}")


if __name__ == "__main__":
    prepare(Path(sys.argv[1]).resolve())
