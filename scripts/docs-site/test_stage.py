#!/usr/bin/env python3
"""Self-test for the site scripts, on made-up repositories and releases.

    python3 -I scripts/docs-site/test_stage.py

Covers stage.py's link rewriting, head.py, repo_links.py, and the release
screenshots: screenshots.py's naming and checks, and release-screenshots.sh
against a stand-in for gh. With mdbook on PATH (or MDBOOK set) it also
builds a copy at /dev/ with build.sh and checks what mdBook does with the
staged links, and builds the whole site from HEAD with build-site.sh, with
and without a release's screenshots. CI runs it in the docs workflow.
"""

import hashlib
import importlib.util
import os
import shutil
import struct
import subprocess
import sys
import tempfile
import unittest
import zlib

# No __pycache__ next to the scripts.
sys.dont_write_bytecode = True

HERE = os.path.dirname(os.path.abspath(__file__))


def load(name):
    spec = importlib.util.spec_from_file_location(name, os.path.join(HERE, name + ".py"))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


stage = load("stage")
head = load("head")
repo_links = load("repo_links")
screenshots = load("screenshots")
REPO = os.path.dirname(os.path.dirname(HERE))

BLOB = "https://github.com/unicornops/zmail/blob/v1.2.3/"
TREE = "https://github.com/unicornops/zmail/tree/v1.2.3/"

FILES = {
    "README.md": "# Zmail\n\n"
    "[install](./docs/install.md#first-run) [decisions](docs/adr/) "
    "[licence](LICENSE) [workspace](./Cargo.toml#L3)\n\n"
    '<p><img src="docs/images/shot.png" alt="a" width="49%"></p>\n',
    "SECURITY.md": "# Security\n\n[home](README.md)\n",
    "PLAN.md": "# Plan\n",
    "LICENSE": "GPL\n",
    "Cargo.toml": "[workspace]\n",
    "scripts/tool.sh": "#!/bin/sh\n",
    "docker/compose.yml": "x: 1\n",
    "assets/logo.svg": "<svg/>\n",
    "docs/SUMMARY.md": "# Summary\n\n[Overview](../README.md)\n\n"
    "- [Install](./install.md)\n"
    "- [Decisions](./adr/README.md)\n"
    "  - [ADR 1](./adr/0001-one.md)\n"
    "- [Security](../SECURITY.md)\n",
    "docs/install.md": "# Install\n\n"
    "## First run\n\n"
    "[home](../README.md) [decision](./adr/README.md#status) "
    "[same page](#first-run) [web](https://example.com/a.md)\n\n"
    "[script](../scripts/tool.sh) [harness](../docker/) [plan](../PLAN.md) "
    "[ports](./upstream/ports.toml) [unlisted](./unlisted.md) [gone](./gone.md)\n\n"
    "![shot](./images/shot.png) ![logo](../assets/logo.svg)\n\n"
    "Inline `[code](../LICENSE)` stays; [`code text`](../LICENSE) does not.\n\n"
    "```markdown\n[fenced](../LICENSE)\n```\n\n"
    "````text\n```\n[still fenced](../LICENSE)\n````\n\n"
    "[ref]: ../LICENSE\n",
    "docs/unlisted.md": "# Not in SUMMARY.md\n",
    "docs/upstream/ports.toml": "a = 1\n",
    "docs/images/shot.png": "png",
    "docs/images/unused.png": "png",
    "docs/adr/README.md": "# Decisions\n\n## Status\n\n[one](./0001-one.md) [home](../../README.md)\n",
    "docs/adr/0001-one.md": "# ADR 1\n\n[index](./README.md) [folder](./) [install](../install.md#first-run)\n",
}


class StageTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.mkdtemp()
        self.repo = os.path.join(self.tmp, "repo")
        self.out = os.path.join(self.tmp, "out")
        for path, text in FILES.items():
            full = os.path.join(self.repo, path)
            os.makedirs(os.path.dirname(full), exist_ok=True)
            with open(full, "w", encoding="utf-8") as f:
                f.write(text)

    def tearDown(self):
        shutil.rmtree(self.tmp)

    def staged(self, path):
        with open(os.path.join(self.out, path), encoding="utf-8") as f:
            return f.read()

    def run_stage(self, **kwargs):
        return stage.stage(self.repo, self.out, ref="v1.2.3", **kwargs)

    def test_layout_mirrors_the_repository(self):
        self.run_stage()
        for path in [
            "SUMMARY.md",
            "README.md",
            "SECURITY.md",
            "docs/install.md",
            "docs/unlisted.md",
            "docs/adr/README.md",
            "docs/adr/0001-one.md",
            "docs/images/shot.png",
            "assets/logo.svg",
        ]:
            self.assertTrue(os.path.isfile(os.path.join(self.out, path)), path)
        # Only pages and the images they show are staged.
        for path in ["PLAN.md", "LICENSE", "docs/SUMMARY.md", "docs/upstream/ports.toml", "docs/images/unused.png"]:
            self.assertFalse(os.path.exists(os.path.join(self.out, path)), path)

    def test_summary_links_are_written_from_the_top(self):
        self.run_stage()
        summary = self.staged("SUMMARY.md")
        self.assertIn("[Overview](README.md)", summary)
        self.assertIn("- [Install](docs/install.md)", summary)
        # mdBook needs the real file name here, not index.md.
        self.assertIn("- [Decisions](docs/adr/README.md)", summary)
        self.assertIn("  - [ADR 1](docs/adr/0001-one.md)", summary)
        self.assertIn("- [Security](SECURITY.md)", summary)

    def test_links_between_pages(self):
        self.run_stage()
        readme = self.staged("README.md")
        self.assertIn("[install](docs/install.md#first-run)", readme)
        # A folder with a README.md links to the page mdBook makes of it.
        self.assertIn("[decisions](docs/adr/index.md)", readme)
        install = self.staged("docs/install.md")
        self.assertIn("[home](../index.md)", install)
        self.assertIn("[decision](adr/index.md#status)", install)
        self.assertIn("[same page](#first-run)", install)
        self.assertIn("[web](https://example.com/a.md)", install)
        adr = self.staged("docs/adr/0001-one.md")
        self.assertIn("[index](index.md)", adr)
        self.assertIn("[folder](index.md)", adr)
        self.assertIn("[install](../install.md#first-run)", adr)
        self.assertIn("[home](../../index.md)", self.staged("docs/adr/README.md"))
        self.assertIn("[home](index.md)", self.staged("SECURITY.md"))

    def test_links_to_other_repository_files_go_to_github_at_the_ref(self):
        self.run_stage()
        readme = self.staged("README.md")
        self.assertIn(f"[licence]({BLOB}LICENSE)", readme)
        self.assertIn(f"[workspace]({BLOB}Cargo.toml#L3)", readme)
        install = self.staged("docs/install.md")
        self.assertIn(f"[script]({BLOB}scripts/tool.sh)", install)
        self.assertIn(f"[harness]({TREE}docker)", install)
        # A markdown file outside docs/ that SUMMARY.md does not list.
        self.assertIn(f"[plan]({BLOB}PLAN.md)", install)
        self.assertIn(f"[ports]({BLOB}docs/upstream/ports.toml)", install)
        self.assertIn(f"[ref]: {BLOB}LICENSE", install)
        self.assertIn(f"[`code text`]({BLOB}LICENSE)", install)

    def test_other_refs(self):
        stage.stage(self.repo, self.out, ref="main")
        self.assertIn("[licence](https://github.com/unicornops/zmail/blob/main/LICENSE)", self.staged("README.md"))

    def test_broken_and_unlisted_links_stay_broken(self):
        self.run_stage()
        install = self.staged("docs/install.md")
        # Left for the link check to fail on: mdBook builds no page for
        # either.
        self.assertIn("[unlisted](unlisted.md)", install)
        self.assertIn("[gone](./gone.md)", install)

    def test_images(self):
        self.run_stage()
        self.assertIn('<img src="docs/images/shot.png" alt="a" width="49%">', self.staged("README.md"))
        install = self.staged("docs/install.md")
        self.assertIn("![shot](images/shot.png)", install)
        self.assertIn("![logo](../assets/logo.svg)", install)

    def test_code_is_left_alone(self):
        self.run_stage()
        install = self.staged("docs/install.md")
        self.assertIn("Inline `[code](../LICENSE)` stays", install)
        self.assertIn("[fenced](../LICENSE)", install)
        self.assertIn("[still fenced](../LICENSE)", install)

    def test_banner_goes_on_every_page_but_not_the_summary(self):
        banner = "> [!WARNING]\n> Unreleased. See [the release](/)."
        self.run_stage(banner=banner)
        for page in ["README.md", "docs/install.md", "docs/adr/0001-one.md"]:
            self.assertTrue(self.staged(page).startswith(banner + "\n\n"), page)
        self.assertNotIn("Unreleased", self.staged("SUMMARY.md"))

    def test_repository_markdown_is_not_changed(self):
        self.run_stage()
        for path, text in FILES.items():
            with open(os.path.join(self.repo, path), encoding="utf-8") as f:
                self.assertEqual(f.read(), text, path)


class HeadTest(unittest.TestCase):
    def test_canonical_links_use_the_served_paths(self):
        url = "https://docs.example/dev/"
        self.assertEqual(head.tags_for("index.html", url, False), ['<link rel="canonical" href="https://docs.example/dev/">'])
        self.assertEqual(
            head.tags_for("docs/adr/index.html", url, False),
            ['<link rel="canonical" href="https://docs.example/dev/docs/adr/">'],
        )
        self.assertEqual(
            head.tags_for("docs/install.html", url, True),
            ['<link rel="canonical" href="https://docs.example/dev/docs/install">', head.NOINDEX],
        )
        self.assertEqual(head.tags_for("docs/install.html", None, False), [])

    def test_helper_pages_are_never_indexed(self):
        for page in ["404.html", "print.html", "toc.html"]:
            self.assertEqual(head.tags_for(page, "https://docs.example/", False), [head.NOINDEX])


class RepoLinksTest(unittest.TestCase):
    """repo_links.py against a small git repository."""

    def setUp(self):
        self.tmp = tempfile.mkdtemp()
        self.repo = os.path.join(self.tmp, "repo")
        self.site = os.path.join(self.tmp, "site")
        os.makedirs(os.path.join(self.repo, "scripts", "with space"))
        os.makedirs(self.site)
        for path in ("Cargo.toml", "scripts/run.sh", "scripts/with space/a.txt"):
            with open(os.path.join(self.repo, path), "w", encoding="utf-8") as handle:
                handle.write("x\n")
        env = dict(os.environ, GIT_CONFIG_GLOBAL=os.devnull, GIT_CONFIG_SYSTEM=os.devnull)
        for args in (
            ["init", "-q", "-b", "main"],
            ["add", "-A"],
            ["-c", "user.name=t", "-c", "user.email=t@example.invalid", "commit", "-q", "-m", "one"],
            ["tag", "v1.2.3"],
            ["branch", "docs/a-branch"],
        ):
            subprocess.run(["git", "-C", self.repo, *args], check=True, env=env)

    def tearDown(self):
        shutil.rmtree(self.tmp)

    def check(self, *hrefs):
        with open(os.path.join(self.site, "index.html"), "w", encoding="utf-8") as handle:
            handle.write("".join(f'<a href="{href}">x</a>' for href in hrefs))
        found = repo_links.links(self.site)
        cache = {}
        return sorted(
            rest for (kind, rest) in found
            if not repo_links.resolves(self.repo, kind, rest, cache)
        )

    def test_files_and_folders_at_a_tag_a_branch_and_a_branch_with_a_slash(self):
        github = "https://github.com/unicornops/zmail"
        self.assertEqual(
            self.check(
                f"{github}/blob/v1.2.3/Cargo.toml",
                f"{github}/blob/main/scripts/run.sh#L3",
                f"{github}/tree/main/scripts",
                f"{github}/blob/docs/a-branch/scripts/with%20space/a.txt",
                f"{github}/tree/v1.2.3/",
                "https://github.com/someone/else/blob/main/missing.txt",
            ),
            [],
        )

    def test_a_missing_file_a_missing_ref_and_the_wrong_kind_are_broken(self):
        github = "https://github.com/unicornops/zmail"
        self.assertEqual(
            self.check(
                f"{github}/blob/main/gone.txt",
                f"{github}/blob/v9.9.9/Cargo.toml",
                f"{github}/tree/main/Cargo.toml",
                f"{github}/blob/main/scripts",
            ),
            ["main/Cargo.toml", "main/gone.txt", "main/scripts", "v9.9.9/Cargo.toml"],
        )


def png(width=200, height=120, colour=(10, 20, 30)):
    """A small, well-formed RGB PNG."""

    def chunk(kind, body):
        return struct.pack(">I", len(body)) + kind + body + struct.pack(">I", zlib.crc32(kind + body))

    rows = (b"\x00" + bytes(colour) * width) * height
    return (
        screenshots.PNG_SIGNATURE
        + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0))
        + chunk(b"IDAT", zlib.compress(rows))
        + chunk(b"IEND", b"")
    )


def write_bytes(path, data):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "wb") as handle:
        handle.write(data)


def read_bytes(path):
    with open(path, "rb") as handle:
        return handle.read()


def write_assets(folder, assets, listed=None):
    """Writes assets ({name: bytes}) and a SHA256SUMS-Screenshots listing
    listed ({name: bytes}, default: the assets themselves)."""
    os.makedirs(folder, exist_ok=True)
    for name, data in assets.items():
        write_bytes(os.path.join(folder, name), data)
    listed = assets if listed is None else listed
    with open(os.path.join(folder, screenshots.SUMS), "w", encoding="utf-8") as handle:
        for name, data in listed.items():
            handle.write(f"{hashlib.sha256(data).hexdigest()}  {name}\n")


class ScreenshotPlanTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.mkdtemp()

    def tearDown(self):
        shutil.rmtree(self.tmp)

    def images(self, *names):
        folder = os.path.join(self.tmp, "images")
        for name in names:
            write_bytes(os.path.join(folder, name), b"png")
        return folder

    def test_each_image_is_a_state_and_an_asset(self):
        folder = self.images("reader-light.png", "onboarding-provider-light.png")
        self.assertEqual(
            screenshots.plan(folder, {"reader-light", "onboarding-provider-light", "inbox-dark"}),
            [
                ("onboarding-provider-light", "zmail-docs-onboarding-provider-light.png"),
                ("reader-light", "zmail-docs-reader-light.png"),
            ],
        )

    def test_an_image_that_is_no_state_fails(self):
        folder = self.images("reader-light.png", "diagram.svg", "Reader.PNG", "no-such-state.png")
        with self.assertRaises(ValueError) as caught:
            screenshots.plan(folder, {"reader-light"})
        for name in ("diagram.svg", "Reader.PNG", "no-such-state.png"):
            self.assertIn(name, str(caught.exception))
        self.assertNotIn("reader-light.png", str(caught.exception))

    def test_terminal_ui_images_are_terminal_ui_states(self):
        folder = self.images("reader-light.png", "tui-message-index.png", "tui-gone.png", "tui-x.png")
        with self.assertRaises(ValueError) as caught:
            # tui-x is a matrix state here, but a tui- image is the
            # terminal UI's and must be one of its states.
            screenshots.plan(folder, {"reader-light", "tui-x"}, {"tui-message-index"})
        self.assertIn("tui-gone.png", str(caught.exception))
        self.assertIn("tui-x.png", str(caught.exception))
        self.assertNotIn("tui-message-index.png", str(caught.exception))
        folder = self.images("reader-light.png", "tui-message-index.png")
        os.remove(os.path.join(folder, "tui-gone.png"))
        os.remove(os.path.join(folder, "tui-x.png"))
        self.assertEqual(
            screenshots.plan(folder, {"reader-light"}, {"tui-message-index"}),
            [
                ("reader-light", "zmail-docs-reader-light.png"),
                ("tui-message-index", "zmail-docs-tui-message-index.png"),
            ],
        )
        with self.assertRaises(ValueError):
            screenshots.plan(folder, {"reader-light"})
        with self.assertRaises(ValueError):
            # A desktop image is never a terminal UI state.
            screenshots.plan(folder, {"tui-message-index"}, {"reader-light", "tui-message-index"})

    def test_this_repository_s_images_are_all_states(self):
        def listed(script):
            return set(subprocess.run(
                ["bash", os.path.join(REPO, "scripts", script), "--list"],
                check=True, capture_output=True, text=True,
            ).stdout.split())

        tui_states = listed("capture-tui-screenshots.sh")
        self.assertTrue(tui_states)
        self.assertTrue(all(screenshots.is_tui(state) for state in tui_states))
        matrix_states = listed("capture-release-screenshot.sh")
        self.assertFalse(any(screenshots.is_tui(state) for state in matrix_states))
        entries = screenshots.plan(os.path.join(REPO, "docs", "images"), matrix_states, tui_states)
        self.assertIn(("reader-light", "zmail-docs-reader-light.png"), entries)
        self.assertIn(("tui-message-index", "zmail-docs-tui-message-index.png"), entries)
        subprocess.run(
            [os.path.join(HERE, "capture-screenshots.sh"), "--check"],
            check=True, stderr=subprocess.DEVNULL,
        )


class ScreenshotApplyTest(unittest.TestCase):
    """screenshots.py apply: only verified, well-formed assets replace an
    image; anything else keeps the committed copy."""

    COMMITTED = b"committed"

    def setUp(self):
        self.tmp = tempfile.mkdtemp()
        self.images = os.path.join(self.tmp, "site", "docs", "images")
        self.assets = os.path.join(self.tmp, "assets")
        for name in ("reader-light.png", "reader-dark.png"):
            write_bytes(os.path.join(self.images, name), self.COMMITTED)

    def tearDown(self):
        shutil.rmtree(self.tmp)

    def apply(self):
        with open(os.devnull, "w", encoding="utf-8") as log:
            return screenshots.apply(self.images, self.assets, log=log)

    def image(self, name):
        return read_bytes(os.path.join(self.images, name))

    def test_verified_assets_replace_the_images(self):
        light, dark = png(colour=(255, 255, 255)), png(colour=(0, 0, 0))
        write_assets(self.assets, {
            "zmail-docs-reader-light.png": light,
            "zmail-docs-reader-dark.png": dark,
            # An asset for an image the site does not show is ignored.
            "zmail-docs-inbox-dark.png": png(),
        })
        self.assertEqual(self.apply(), (2, 2))
        self.assertEqual(self.image("reader-light.png"), light)
        self.assertEqual(self.image("reader-dark.png"), dark)
        self.assertFalse(os.path.exists(os.path.join(self.images, "inbox-dark.png")))

    def test_no_assets_keeps_every_image(self):
        self.assertEqual(self.apply(), (0, 2))
        os.makedirs(self.assets)
        write_bytes(os.path.join(self.assets, "zmail-docs-reader-light.png"), png())
        # No SHA256SUMS-Screenshots: nothing is trusted.
        self.assertEqual(self.apply(), (0, 2))
        self.assertEqual(self.image("reader-light.png"), self.COMMITTED)

    def test_a_missing_asset_keeps_its_image(self):
        write_assets(self.assets, {"zmail-docs-reader-light.png": png()})
        self.assertEqual(self.apply(), (1, 2))
        self.assertEqual(self.image("reader-dark.png"), self.COMMITTED)

    def test_a_checksum_mismatch_is_rejected(self):
        write_assets(
            self.assets,
            {"zmail-docs-reader-light.png": png(colour=(1, 2, 3))},
            listed={"zmail-docs-reader-light.png": png(colour=(3, 2, 1))},
        )
        self.assertEqual(self.apply(), (0, 2))
        self.assertEqual(self.image("reader-light.png"), self.COMMITTED)

    def test_an_unlisted_asset_is_rejected(self):
        write_assets(self.assets, {"zmail-docs-reader-light.png": png()}, listed={})
        self.assertEqual(self.apply(), (0, 2))

    def test_what_is_not_a_sane_png_is_rejected(self):
        good = png()
        bad_crc = bytearray(good)
        bad_crc[40] ^= 0xFF
        cases = {
            "text": b"<html>not a picture</html>",
            "truncated": good[:-12],
            "bad CRC": bytes(bad_crc),
            "trailing data": good + b"<script>",
            "too small": png(width=10, height=10),
            "too large": png(width=9000, height=120),
        }
        for label, data in cases.items():
            with self.subTest(label):
                shutil.rmtree(self.assets, ignore_errors=True)
                write_assets(self.assets, {"zmail-docs-reader-light.png": data})
                self.assertIsNotNone(screenshots.png_problem(data))
                self.assertEqual(self.apply(), (0, 2))
                self.assertEqual(self.image("reader-light.png"), self.COMMITTED)
        self.assertIsNone(screenshots.png_problem(good))

    def test_a_symlink_is_rejected(self):
        target = os.path.join(self.tmp, "elsewhere.png")
        write_bytes(target, png())
        write_assets(self.assets, {}, listed={"zmail-docs-reader-light.png": png()})
        os.symlink(target, os.path.join(self.assets, "zmail-docs-reader-light.png"))
        self.assertEqual(self.apply(), (0, 2))


# Stands in for the GitHub CLI: serves the files in FAKE_GH_RELEASE as the
# release's assets, and fails `attestation verify` for the names in
# FAKE_GH_UNATTESTED (comma-separated).
FAKE_GH = """#!/usr/bin/env python3
import fnmatch, os, shutil, sys
args = sys.argv[1:]
release = os.environ.get("FAKE_GH_RELEASE", "")
if args[:2] == ["release", "view"]:
    if not os.path.isdir(release):
        sys.exit("release not found")
    print("\\n".join(sorted(os.listdir(release))))
elif args[:2] == ["release", "download"]:
    dest = args[args.index("--dir") + 1]
    patterns = [args[i + 1] for i, a in enumerate(args) if a == "--pattern"]
    for name in os.listdir(release):
        if any(fnmatch.fnmatch(name, p) for p in patterns):
            shutil.copy(os.path.join(release, name), dest)
elif args[:2] == ["attestation", "verify"]:
    bad = os.environ.get("FAKE_GH_UNATTESTED", "").split(",")
    sys.exit(1 if os.path.basename(args[2]) in bad else 0)
else:
    sys.exit("unexpected: " + " ".join(args))
"""


class ReleaseScreenshotsTest(unittest.TestCase):
    """release-screenshots.sh against a stand-in for gh."""

    def setUp(self):
        self.tmp = tempfile.mkdtemp()
        self.gh = os.path.join(self.tmp, "gh")
        with open(self.gh, "w", encoding="utf-8") as handle:
            handle.write(FAKE_GH)
        os.chmod(self.gh, 0o755)
        self.release = os.path.join(self.tmp, "release")
        self.dest = os.path.join(self.tmp, "dest")

    def tearDown(self):
        shutil.rmtree(self.tmp)

    def run_script(self, gh=None, unattested=""):
        env = dict(os.environ, GH=gh or self.gh, FAKE_GH_RELEASE=self.release, FAKE_GH_UNATTESTED=unattested)
        return subprocess.run(
            [os.path.join(HERE, "release-screenshots.sh"), "v1.2.3", self.dest],
            env=env, capture_output=True, text=True,
        )

    def test_downloads_and_keeps_attested_files(self):
        write_assets(self.release, {"zmail-docs-reader-light.png": png(), "zmail-docs-reader-dark.png": png()})
        write_bytes(os.path.join(self.release, "zmail-linux-x86_64.deb"), b"deb")
        result = self.run_script(unattested="zmail-docs-reader-dark.png")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            sorted(os.listdir(self.dest)),
            [screenshots.SUMS, "zmail-docs-reader-light.png"],
        )
        self.assertIn("zmail-docs-reader-dark.png has no valid build provenance attestation", result.stdout)

    def test_an_older_release_has_none(self):
        os.makedirs(self.release)
        write_bytes(os.path.join(self.release, "zmail-linux-screenshot.png"), png())
        result = self.run_script()
        self.assertEqual(result.returncode, 1)
        self.assertIn("has no SHA256SUMS-Screenshots", result.stderr)
        self.assertFalse(os.path.exists(self.dest))

    def test_unattested_checksums_mean_nothing_is_used(self):
        write_assets(self.release, {"zmail-docs-reader-light.png": png()})
        result = self.run_script(unattested=screenshots.SUMS)
        self.assertEqual(result.returncode, 1)
        self.assertFalse(os.path.exists(self.dest))

    def test_no_gh_and_no_release(self):
        result = self.run_script(gh=os.path.join(self.tmp, "no-such-gh"))
        self.assertEqual(result.returncode, 1)
        self.assertIn("no GitHub CLI", result.stderr)
        result = self.run_script()
        self.assertEqual(result.returncode, 1)
        self.assertIn("could not read release v1.2.3", result.stderr)
        self.assertFalse(os.path.exists(self.dest))


class Page(str):
    """A page's HTML, shortened in failure messages."""

    def __repr__(self):
        return f"<{len(self)} characters of HTML>"


MDBOOK = os.environ.get("MDBOOK") or shutil.which("mdbook")


@unittest.skipUnless(MDBOOK, "mdbook is not installed")
class BuildTest(unittest.TestCase):
    """build.sh on this repository's docs, as the /dev/ copy."""

    def test_dev_copy(self):
        with tempfile.TemporaryDirectory() as out:
            subprocess.run(
                [
                    os.path.join(HERE, "build.sh"),
                    "--ref", "v1.2.3",
                    "--site-url", "/dev/",
                    "--title", "Zmail (development)",
                    "--banner", "> [!WARNING]\n> Unreleased.",
                    "--canonical-url", "https://docs.example/dev/",
                    "--noindex",
                    out,
                ],
                check=True,
                env=dict(os.environ, MDBOOK=MDBOOK),
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
            )

            def html(path):
                with open(os.path.join(out, path), encoding="utf-8") as f:
                    return Page(f.read())

            home = html("index.html")
            self.assertIn("<title>Overview - Zmail (development)</title>", home)
            self.assertIn('href="docs/install.html"', home)
            self.assertIn('href="docs/adr/index.html"', home)
            self.assertIn(f'href="{BLOB}deny.toml"', home)
            self.assertIn('class="blockquote-tag blockquote-tag-warning"', home)
            self.assertIn('<link rel="canonical" href="https://docs.example/dev/">', home)
            self.assertIn('<meta name="robots" content="noindex">', home)
            self.assertIn(
                '<link rel="canonical" href="https://docs.example/dev/docs/install">',
                html("docs/install.html"),
            )
            self.assertIn(
                'href="https://github.com/unicornops/zmail/edit/main/./docs/install.md"',
                html("docs/install.html"),
            )
            self.assertIn('href="adr/index.html"', html("docs/development.html"))
            # Everything links relatively, so the copy works under /dev/;
            # only the 404 page, served from any depth, needs the prefix.
            self.assertIn('<base href="/dev/">', html("404.html"))
            self.assertFalse(os.path.exists(os.path.join(out, "book.toml")))


def in_git_checkout():
    return subprocess.run(
        ["git", "-C", REPO, "cat-file", "-e", "HEAD:scripts/docs-site/build.sh"],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
    ).returncode == 0


@unittest.skipUnless(MDBOOK and in_git_checkout(), "needs mdbook and a git checkout")
class BuildSiteScreenshotsTest(unittest.TestCase):
    """build-site.sh with / built from HEAD: the release's screenshots
    replace the committed copies in /, never in /dev/."""

    def build(self, out, screenshots_mode):
        subprocess.run(
            [
                os.path.join(HERE, "build-site.sh"),
                "--release", "HEAD",
                "--screenshots", screenshots_mode,
                "--no-check",
                out,
            ],
            check=True,
            env=dict(os.environ, MDBOOK=MDBOOK),
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )

    def test_assets_are_used_in_the_release_copy_only(self):
        def released(name):
            return subprocess.run(
                ["git", "-C", REPO, "show", f"HEAD:docs/images/{name}"],
                check=True, capture_output=True,
            ).stdout

        with tempfile.TemporaryDirectory() as tmp:
            committed = os.path.join(REPO, "docs", "images")
            names = sorted(n for n in os.listdir(committed) if n.endswith(".png"))
            assets = {screenshots.asset_name(n): png(colour=(i, 0, 0)) for i, n in enumerate(names)}
            # One bad asset: its image keeps the committed copy.
            bad = screenshots.asset_name(names[0])
            listed = dict(assets, **{bad: b"something else"})
            write_assets(os.path.join(tmp, "assets"), assets, listed=listed)

            out = os.path.join(tmp, "site")
            self.build(out, os.path.join(tmp, "assets"))
            for name in names:
                site_copy = read_bytes(os.path.join(out, "docs", "images", name))
                if screenshots.asset_name(name) == bad:
                    self.assertEqual(site_copy, released(name), name)
                else:
                    self.assertEqual(site_copy, assets[screenshots.asset_name(name)], name)
                self.assertEqual(
                    read_bytes(os.path.join(out, "dev", "docs", "images", name)),
                    read_bytes(os.path.join(committed, name)),
                    name,
                )

            self.build(out, "none")
            for name in names:
                self.assertEqual(read_bytes(os.path.join(out, "docs", "images", name)), released(name), name)


if __name__ == "__main__":
    unittest.main()
