#!/usr/bin/env python3
"""The documentation's screenshots: from docs/images to release assets and back.

    python3 -I scripts/docs-site/screenshots.py plan IMAGES_DIR [STATES_FILE [TUI_STATES_FILE]]
    python3 -I scripts/docs-site/screenshots.py apply IMAGES_DIR ASSETS_DIR

Every image in docs/images is a screenshot named after the state it shows.
An image whose name starts with tui- is the terminal UI, a state of
scripts/capture-tui-screenshots.sh --list (docs/images/tui-message-index.png
is the state tui-message-index); any other is the desktop app, a UI
screenshot matrix state of scripts/capture-release-screenshot.sh --list
(docs/images/reader-light.png is the state reader-light). The release
workflow captures those states and publishes each as a release asset named
zmail-docs-<image> (zmail-docs-reader-light.png), listed with its SHA-256 in
the release's SHA256SUMS-Screenshots. docs/site.md explains the flow.

plan prints "STATE ASSET" for each image in IMAGES_DIR, and fails on a file
that cannot be a screenshot: not a .png, or, with STATES_FILE (the matrix's
states, one per line) and TUI_STATES_FILE (the terminal UI's, likewise; no
terminal UI states when left out), named after no state of its kind.

apply puts the release's screenshots over the images in IMAGES_DIR (the
built site's docs/images, or the repository's when refreshing the committed
copies). ASSETS_DIR holds what was downloaded from the release, which is
untrusted: an asset is used only when SHA256SUMS-Screenshots in ASSETS_DIR
lists it with a matching SHA-256 and it is a well-formed PNG of a sane size
and dimensions. Any other image keeps the copy it has, and the log says
why; a missing or bad asset never fails the build and never removes an
image. ASSETS_DIR may be missing (no assets: every image keeps its copy).
Prints one summary line on stdout: "USED of TOTAL".
"""

import hashlib
import os
import re
import struct
import sys
import tempfile
import zlib

ASSET_PREFIX = "zmail-docs-"
# Images named tui-* are the terminal UI's (capture-tui-screenshots.sh).
TUI_PREFIX = "tui-"
SUMS = "SHA256SUMS-Screenshots"
STATE = re.compile(r"^[a-z0-9][a-z0-9-]*$")

PNG_SIGNATURE = b"\x89PNG\r\n\x1a\n"
# A screenshot is a few hundred kilobytes; the matrix's largest window is
# 1920x1080, and the terminal UI's widest image about 2200 pixels.
MAX_BYTES = 10 * 1024 * 1024
MIN_SIDE = 100
MAX_SIDE = 8192


def asset_name(image):
    """The release asset for docs/images/IMAGE."""
    return ASSET_PREFIX + image


def is_tui(state):
    """Whether state is a terminal UI screenshot's."""
    return state.startswith(TUI_PREFIX)


def plan(images_dir, states=None, tui_states=None):
    """[(state, asset)] for each image in images_dir; raises ValueError
    naming every file that cannot be a screenshot, or, when states is
    given, whose state is not one of its kind: a tui- image's must be in
    tui_states (none when not given), any other's in states."""
    entries, bad = [], []
    for name in sorted(os.listdir(images_dir)):
        state, ext = os.path.splitext(name)
        if states is None:
            known = True
        elif is_tui(state):
            known = state in (tui_states or ())
        else:
            known = state in states
        if (
            ext != ".png"
            or not STATE.match(state)
            or not os.path.isfile(os.path.join(images_dir, name))
            or not known
        ):
            bad.append(name)
            continue
        entries.append((state, asset_name(name)))
    if bad:
        raise ValueError(
            "each image here must be a screenshot named after a state the "
            "release can capture: a UI screenshot matrix state "
            "(scripts/capture-release-screenshot.sh --list), or for the "
            "terminal UI a tui- state (scripts/capture-tui-screenshots.sh "
            "--list); these are not: " + ", ".join(bad)
        )
    if not entries:
        raise ValueError("no screenshots")
    return entries


def read_sums(path):
    """{file name: sha256 hex} from a sha256sum listing."""
    sums = {}
    with open(path, encoding="utf-8") as handle:
        for line in handle:
            match = re.match(r"^([0-9a-f]{64}) [ *](\S+)\s*$", line)
            if match:
                sums[match.group(2)] = match.group(1)
    return sums


def png_problem(data):
    """Why data is not a well-formed PNG screenshot, or None. Walks every
    chunk and checks its CRC; does not decode the pixels."""
    if len(data) > MAX_BYTES:
        return f"{len(data)} bytes, more than {MAX_BYTES}"
    if not data.startswith(PNG_SIGNATURE):
        return "not a PNG (no PNG signature)"
    pos = len(PNG_SIGNATURE)
    first = True
    while True:
        if pos + 8 > len(data):
            return "truncated PNG (no IEND chunk)"
        length, kind = struct.unpack(">I4s", data[pos : pos + 8])
        end = pos + 8 + length + 4
        if end > len(data):
            return f"truncated PNG (chunk {kind!r} runs past the end)"
        body = data[pos + 8 : pos + 8 + length]
        (crc,) = struct.unpack(">I", data[pos + 8 + length : end])
        if zlib.crc32(kind + body) != crc:
            return f"corrupt PNG (bad CRC in chunk {kind!r})"
        if first:
            if kind != b"IHDR" or length != 13:
                return "corrupt PNG (does not start with IHDR)"
            width, height = struct.unpack(">II", body[:8])
            if not (MIN_SIDE <= width <= MAX_SIDE and MIN_SIDE <= height <= MAX_SIDE):
                return f"{width}x{height} is not a screenshot's size"
            first = False
        pos = end
        if kind == b"IEND":
            break
    if pos != len(data):
        return "data after the PNG's end"
    return None


def check_asset(assets_dir, asset, sums):
    """(bytes, None) for a usable asset, or (None, why not)."""
    path = os.path.join(assets_dir, asset)
    if not os.path.lexists(path):
        return None, "the release has no " + asset
    if os.path.islink(path) or not os.path.isfile(path):
        return None, asset + " is not a regular file"
    if asset not in sums:
        return None, f"{asset} is not listed in {SUMS}"
    with open(path, "rb") as handle:
        data = handle.read(MAX_BYTES + 1)
    if hashlib.sha256(data).hexdigest() != sums[asset]:
        return None, f"{asset} does not match its SHA-256 in {SUMS}"
    problem = png_problem(data)
    if problem:
        return None, f"{asset}: {problem}"
    return data, None


def replace(path, data):
    """Writes data to path through a temporary file in the same folder."""
    fd, tmp = tempfile.mkstemp(dir=os.path.dirname(path), prefix=".screenshot-")
    try:
        with os.fdopen(fd, "wb") as handle:
            handle.write(data)
        os.chmod(tmp, 0o644)
        os.replace(tmp, path)
    except BaseException:
        os.unlink(tmp)
        raise


def apply(images_dir, assets_dir, log=sys.stderr):
    """Puts each verified asset over its image; returns (used, total)."""
    images = [
        name for name in sorted(os.listdir(images_dir))
        if name.endswith(".png") and os.path.isfile(os.path.join(images_dir, name))
    ]
    sums_path = os.path.join(assets_dir, SUMS)
    sums = {}
    if not os.path.isdir(assets_dir):
        reason = "no release screenshots"
    elif os.path.islink(sums_path) or not os.path.isfile(sums_path):
        reason = f"no {SUMS}"
    else:
        reason = None
        sums = read_sums(sums_path)
    used = 0
    for image in images:
        if reason:
            why = reason
        else:
            data, why = check_asset(assets_dir, asset_name(image), sums)
            if data is not None:
                replace(os.path.join(images_dir, image), data)
                used += 1
                print(f"screenshots: {image}: the release's {asset_name(image)}", file=log)
                continue
        print(f"screenshots: {image}: kept the committed copy ({why})", file=log)
    return used, len(images)


def main(argv):
    if len(argv) in (2, 3, 4) and argv[0] == "plan":

        def read_states(path):
            with open(path, encoding="utf-8") as handle:
                return {line.strip() for line in handle if line.strip()}

        states = read_states(argv[2]) if len(argv) >= 3 else None
        tui_states = read_states(argv[3]) if len(argv) == 4 else None
        try:
            entries = plan(argv[1], states, tui_states)
        except ValueError as error:
            print(f"screenshots.py: {argv[1]}: {error}", file=sys.stderr)
            return 1
        for state, asset in entries:
            print(state, asset)
        return 0
    if len(argv) == 3 and argv[0] == "apply":
        used, total = apply(argv[1], argv[2])
        print(f"{used} of {total}")
        return 0
    print(__doc__.split("\n\n")[1], file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
