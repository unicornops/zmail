#!/usr/bin/env python3
"""Checks a built site's links to this repository's own files against git.

    python3 -I scripts/docs-site/repo_links.py SITE_DIR [REPO_DIR]

stage.py points links to files outside the site at GitHub, at the ref the
copy was built from (.../blob/REF/PATH and .../tree/REF/PATH). There are
hundreds of them, and asking github.com for each gets the check throttled
(503), so check.sh --external leaves them out and this script looks each
one up in the local clone instead: the ref must exist and hold the path.
A ref may contain slashes (a branch name), so the longest leading part
that git knows is taken as the ref.

Exits 1, listing the links, when any does not resolve.
"""

import os
import re
import subprocess
import sys
from urllib.parse import unquote

sys.dont_write_bytecode = True

GITHUB = "https://github.com/unicornops/zmail"
LINK = re.compile(r'href="' + re.escape(GITHUB) + r'/(blob|tree)/([^"#?]+)')


def links(site_dir):
    """Each distinct (kind, ref-and-path) the site links to, with one page."""
    found = {}
    for folder, _, names in os.walk(site_dir):
        for name in names:
            if not name.endswith(".html"):
                continue
            page = os.path.join(folder, name)
            with open(page, encoding="utf-8", errors="replace") as handle:
                for kind, rest in LINK.findall(handle.read()):
                    found.setdefault((kind, unquote(rest)), page)
    return found


def git(repo_dir, *args):
    return subprocess.run(
        ["git", "-C", repo_dir, *args],
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        text=True,
        check=False,
    )


def commit_for(repo_dir, ref, cache):
    """The commit a ref names: a tag, else the branch as GitHub has it (the
    links go there), else a local branch."""
    if ref not in cache:
        cache[ref] = None
        for name in (f"refs/tags/{ref}", f"refs/remotes/origin/{ref}", f"refs/heads/{ref}"):
            result = git(repo_dir, "rev-parse", "--verify", "--quiet", name + "^{commit}")
            if result.returncode == 0:
                cache[ref] = result.stdout.strip()
                break
    return cache[ref]


def resolves(repo_dir, kind, rest, cache):
    """Whether REF/PATH names a file (blob) or folder (tree) at a known ref."""
    parts = rest.strip("/").split("/")
    for length in range(len(parts), 0, -1):
        commit = commit_for(repo_dir, "/".join(parts[:length]), cache)
        if commit is None:
            continue
        path = "/".join(parts[length:])
        if not path:
            return kind == "tree"
        result = git(repo_dir, "cat-file", "-t", f"{commit}:{path}")
        return result.returncode == 0 and result.stdout.strip() == kind
    return False


def main(argv):
    if len(argv) not in (2, 3) or not os.path.isdir(argv[1]):
        print(__doc__.strip().splitlines()[2].strip(), file=sys.stderr)
        return 2
    site_dir = argv[1]
    repo_dir = argv[2] if len(argv) == 3 else os.path.dirname(os.path.abspath(__file__))
    found = links(site_dir)
    cache = {}
    broken = [
        (kind, rest, page)
        for (kind, rest), page in sorted(found.items())
        if not resolves(repo_dir, kind, rest, cache)
    ]
    for kind, rest, page in broken:
        print(f"repo_links.py: {GITHUB}/{kind}/{rest} (in {page}) is not in the repository", file=sys.stderr)
    print(f"repo_links.py: {len(found)} links to repository files, {len(broken)} broken")
    return 1 if broken else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
