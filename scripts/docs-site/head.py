#!/usr/bin/env python3
"""Adds tags to the <head> of every page mdBook built (build.sh runs it).

    python3 -I scripts/docs-site/head.py SITE_DIR [--canonical-url URL] [--noindex]

--canonical-url URL: a <link rel="canonical"> on each page, URL followed by
the page's path in SITE_DIR, so search engines treat the site's other
addresses (the pages.dev one) as copies. The path is written the way
Cloudflare Pages serves it, which redirects to it: without .html, and a
folder for index.html.
--noindex: a robots noindex tag, for a copy that should not show up in
search results (/dev/).

mdBook's helper pages (the 404 page, served at any address; the print
page, every page in one; the sidebar for readers without JavaScript) get
only the noindex tag.
"""

import argparse
import html
import os

MARKER = '<meta charset="UTF-8">'
NOINDEX = '<meta name="robots" content="noindex">'
HELPER_PAGES = {"404.html", "print.html", "toc.html"}


def tags_for(rel_path, canonical_url, noindex):
    if rel_path in HELPER_PAGES:
        return [NOINDEX]
    tags = []
    if canonical_url:
        page = rel_path
        if page == "index.html" or page.endswith("/index.html"):
            page = page[: -len("index.html")]
        elif page.endswith(".html"):
            page = page[: -len(".html")]
        tags.append(f'<link rel="canonical" href="{html.escape(canonical_url + page)}">')
    if noindex:
        tags.append(NOINDEX)
    return tags


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("site")
    parser.add_argument("--canonical-url", help="where this copy is served, ending in /")
    parser.add_argument("--noindex", action="store_true")
    args = parser.parse_args()
    if args.canonical_url and not args.canonical_url.endswith("/"):
        parser.error("--canonical-url must end with /")
    for root, _dirs, files in os.walk(args.site):
        for name in files:
            if not name.endswith(".html"):
                continue
            path = os.path.join(root, name)
            rel = os.path.relpath(path, args.site).replace(os.sep, "/")
            tags = tags_for(rel, args.canonical_url, args.noindex)
            if not tags:
                continue
            with open(path, encoding="utf-8") as f:
                text = f.read()
            if text.count(MARKER) != 1:
                raise SystemExit(f"head.py: {rel} has no single {MARKER} to add tags after")
            text = text.replace(MARKER, MARKER + "".join("\n        " + t for t in tags))
            with open(path, "w", encoding="utf-8") as f:
                f.write(text)


if __name__ == "__main__":
    main()
