#!/usr/bin/env python3
"""Stage Zmail's markdown into one tree for mdBook (the documentation site).

    python3 -I scripts/docs-site/stage.py REPO_DIR OUT_DIR [--ref REF] [--banner MARKDOWN]

The staged tree mirrors the repository: a page keeps its repository path
(docs/install.md is docs/install.html on the site, README.md the home page),
so mdBook's edit links point at the right file and most links need no
change. What is staged:

- every markdown page under docs/ (mdBook builds only those listed in
  docs/SUMMARY.md, so a link to a page left out of it fails the link check);
- the pages outside docs/ that docs/SUMMARY.md lists (README.md, SECURITY.md
  and so on);
- docs/SUMMARY.md itself, as SUMMARY.md at the top, its links rewritten from
  there;
- every image a staged page shows or links to.

Links are rewritten in the staged copies only, so the markdown in the
repository stays as it reads on GitHub:

- a link to a page stays relative; a link to a folder's README.md, or to the
  folder, points at its index.md, the name mdBook gives that page;
- a link to an image stays relative (the image is copied);
- a link to any other file or folder in the repository (source files,
  scripts, licences, PLAN.md, docker/...) points at it on GitHub at REF,
  where it reads as it does from the markdown on GitHub;
- a link to something that does not exist is left as it is, so the link
  check fails on it;
- links in code (fenced blocks and `code spans`) are left alone.

--banner prepends MARKDOWN (for example a GitHub alert saying the pages
describe unreleased code) to every page.
"""

import argparse
import os
import posixpath
import re
import shutil
import sys
from urllib.parse import quote, unquote

GITHUB = "https://github.com/unicornops/zmail"
SUMMARY = "docs/SUMMARY.md"
IMAGE_EXTENSIONS = {".png", ".jpg", ".jpeg", ".gif", ".svg", ".webp", ".avif"}

# [text](target) and ![alt](target); a title after the target is kept.
MD_LINK = re.compile(r"(\]\(\s*)([^)\s]+)")
# [label]: target
REFERENCE = re.compile(r"^( {0,3}\[[^\]]+\]:[ \t]*)(\S+)")
# src="..." and href="..." in inline HTML.
HTML_ATTR = re.compile(r"""(\b(?:src|href)=)("[^"]*"|'[^']*')""")
FENCE_OPEN = re.compile(r"^ {0,3}(`{3,}|~{3,})")
SCHEME = re.compile(r"^[A-Za-z][A-Za-z0-9+.-]*:")


def split_code_spans(line):
    """Yields (is_code, text) pieces of a line; code spans are `...`."""
    i = start = 0
    while i < len(line):
        if line[i] != "`":
            i += 1
            continue
        run = len(line[i:]) - len(line[i:].lstrip("`"))
        ticks = line[i : i + run]
        # A code span ends at the next run of exactly as many backticks.
        close = re.compile(r"(?<!`)" + ticks + r"(?!`)")
        end = close.search(line, i + run)
        if end is None:
            i += run
            continue
        if i > start:
            yield False, line[start:i]
        yield True, line[i : end.end()]
        i = start = end.end()
    if start < len(line):
        yield False, line[start:]


class Stager:
    def __init__(self, repo, ref):
        self.repo = repo
        self.ref = ref
        self.pages = set()
        self.images = set()

    def exists(self, repo_path):
        return os.path.exists(os.path.join(self.repo, repo_path))

    def is_dir(self, repo_path):
        return os.path.isdir(os.path.join(self.repo, repo_path))

    def resolve(self, target, src_repo_path):
        """The repository path a relative link from src_repo_path points
        at, or None when it is not a relative link inside the repository."""
        if SCHEME.match(target) or target.startswith(("#", "/")):
            return None
        path = target.partition("#")[0]
        if not path:
            return None
        joined = posixpath.join(posixpath.dirname(src_repo_path), unquote(path))
        repo_target = posixpath.normpath(joined)
        if repo_target == ".." or repo_target.startswith("../"):
            return None
        return repo_target

    def rewrite_target(self, target, src_repo_path, staged_dir, index_names=True):
        repo_target = self.resolve(target, src_repo_path)
        if repo_target is None or not self.exists(repo_target):
            # External, an anchor on the same page, or missing: as it is.
            return target
        _, sep, frag = target.partition("#")
        frag = sep + frag
        if self.is_dir(repo_target):
            readme = posixpath.join(repo_target, "README.md")
            if repo_target != "." and readme in self.pages:
                staged = readme
            else:
                return f"{GITHUB}/tree/{self.ref}/{quote(repo_target)}" + frag
        elif repo_target in self.pages:
            staged = repo_target
        elif posixpath.splitext(repo_target)[1].lower() in IMAGE_EXTENSIONS:
            self.images.add(repo_target)
            staged = repo_target
        else:
            return f"{GITHUB}/blob/{self.ref}/{quote(repo_target)}" + frag
        if index_names and posixpath.basename(staged) == "README.md":
            # mdBook turns a README.md page into index.html, but leaves
            # links to it pointing at README.html.
            staged = posixpath.join(posixpath.dirname(staged), "index.md")
        return quote(posixpath.relpath(staged, staged_dir or ".")) + frag

    def rewrite(self, text, src_repo_path, staged_dir, index_names=True):
        def fix(match):
            new = self.rewrite_target(match.group(2), src_repo_path, staged_dir, index_names)
            return match.group(1) + new

        def fix_attr(match):
            value = match.group(2)
            new = self.rewrite_target(value[1:-1], src_repo_path, staged_dir, index_names)
            return match.group(1) + value[0] + new + value[0]

        out = []
        fence = None
        for line in text.splitlines(keepends=True):
            if fence is not None:
                if re.match(r"^ {0,3}" + re.escape(fence) + r"+\s*$", line):
                    fence = None
                out.append(line)
                continue
            opening = FENCE_OPEN.match(line)
            if opening:
                fence = opening.group(1)
                out.append(line)
                continue
            line = REFERENCE.sub(fix, line)
            pieces = []
            for is_code, piece in split_code_spans(line):
                if not is_code:
                    piece = MD_LINK.sub(fix, piece)
                    piece = HTML_ATTR.sub(fix_attr, piece)
                pieces.append(piece)
            out.append("".join(pieces))
        return "".join(out)

    def summary_pages(self, text):
        pages = set()
        for match in MD_LINK.finditer(text):
            repo_target = self.resolve(match.group(2), SUMMARY)
            if repo_target and repo_target.endswith(".md") and self.exists(repo_target):
                pages.add(repo_target)
        return pages


def read(path):
    with open(path, encoding="utf-8") as f:
        return f.read()


def write(path, text):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w", encoding="utf-8") as f:
        f.write(text)


def stage(repo, out, ref="main", banner=None):
    stager = Stager(repo, ref)
    for root, _dirs, files in os.walk(os.path.join(repo, "docs")):
        for name in files:
            if name.endswith(".md"):
                rel = os.path.relpath(os.path.join(root, name), repo)
                stager.pages.add(rel.replace(os.sep, "/"))
    stager.pages.discard(SUMMARY)
    summary = read(os.path.join(repo, SUMMARY))
    stager.pages |= stager.summary_pages(summary)

    if os.path.exists(out):
        shutil.rmtree(out)
    # SUMMARY.md names each page's file, so its links keep README.md.
    write(os.path.join(out, "SUMMARY.md"), stager.rewrite(summary, SUMMARY, "", index_names=False))
    for page in sorted(stager.pages):
        text = stager.rewrite(read(os.path.join(repo, page)), page, posixpath.dirname(page))
        if banner:
            text = banner.rstrip("\n") + "\n\n" + text
        write(os.path.join(out, page), text)
    for image in sorted(stager.images):
        dest = os.path.join(out, image)
        os.makedirs(os.path.dirname(dest), exist_ok=True)
        shutil.copy2(os.path.join(repo, image), dest)
    return stager


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("repo", help="the repository checkout to stage")
    parser.add_argument("out", help="the folder to write (replaced)")
    parser.add_argument("--ref", default="main", help="git ref for links to repository files (default: main)")
    parser.add_argument("--banner", help="markdown to put at the top of every page")
    args = parser.parse_args()
    stager = stage(os.path.abspath(args.repo), os.path.abspath(args.out), args.ref, args.banner)
    print(f"staged {len(stager.pages)} pages and {len(stager.images)} images into {args.out}", file=sys.stderr)


if __name__ == "__main__":
    main()
