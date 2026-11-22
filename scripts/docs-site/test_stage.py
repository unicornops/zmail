#!/usr/bin/env python3
"""Self-test for stage.py's link rewriting, on a made-up repository.

    python3 -I scripts/docs-site/test_stage.py

With mdbook on PATH (or MDBOOK set) it also builds a copy at /dev/ with
build.sh and checks what mdBook does with the staged links. CI runs it in
the docs workflow.
"""

import importlib.util
import os
import shutil
import subprocess
import sys
import tempfile
import unittest

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


if __name__ == "__main__":
    unittest.main()
