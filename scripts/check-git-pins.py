#!/usr/bin/env python3
"""Report the upstream state of Zmail's git-pinned dependencies.

    scripts/check-git-pins.py [--dry-run]       print the report (the default)
    scripts/check-git-pins.py --update-issue    open or update the tracking issue
    scripts/check-git-pins.py --validate        only check Cargo.toml against
                                                docs/upstream/git-pins.toml

Some crates (GPUI, rPGP, proton-srp) come from git forks pinned by commit
in the root Cargo.toml. Dependabot skips them and RustSec cannot match a git
source, so nothing else says when upstream moves. For every git dependency
in [workspace.dependencies] this reads the fork and commit from Cargo.toml
and Cargo.lock, and what the fork follows from docs/upstream/git-pins.toml,
then reports:

  - the pinned commit and the crate versions it declares;
  - the upstream pull request the fork follows: open, merged or closed, its
    head and whether that is still the pinned commit, and once merged
    whether the latest release contains its merge commit;
  - the latest upstream release (GitHub releases/latest, or the newest
    non-yanked stable version in the crates.io index);
  - every RustSec advisory filed for the crate names, and whether the
    version the fork declares is in its affected range.

Advisories come from a shallow clone of github.com/rustsec/advisory-db, the
database cargo-audit and cargo-deny read, so no API quota is spent and the
version ranges are the ones RustSec publishes. Matching is by the version
number in the fork's Cargo.toml: a fork can carry a fix its version number
does not show, so an affected advisory is a prompt to check, not a verdict.

Each change worth acting on is a finding; the report lists them first. With
--update-issue the report becomes the body of one open issue that carries
the marker below, created when there is none and edited only when its title
or body changed, so a weekly run that finds nothing new is silent.

Environment:
  GIT_PINS_CARGO_TOML   workspace manifest (default: Cargo.toml)
  GIT_PINS_CARGO_LOCK   lockfile (default: Cargo.lock)
  GIT_PINS_MANIFEST     pin descriptions (default: docs/upstream/git-pins.toml)
  GIT_PINS_ADVISORY_DB  an existing advisory-db checkout to use instead of a
                        fresh clone (for example ~/.cargo/advisory-db)
  GIT_PINS_FIXTURES     answer GitHub, crates.io and advisory-db lookups from
                        this directory (api/, index/, advisory-db/) instead of
                        the network (see scripts/test-check-git-pins.sh)
  ISSUE_BODY_FILE       where to write the issue body (default: a temp file)
  GITHUB_OUTPUT         when set, receives findings, title and body_file

Requires python3 >= 3.11, gh (authenticated; read access to public
repositories is enough, plus issue write access for --update-issue) and git.
"""

import argparse
import glob
import json
import os
import re
import subprocess
import sys
import tempfile
import tomllib
import urllib.error
import urllib.request

REPO_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
MARKER = "<!-- git-pins-check -->"
ADVISORY_DB_URL = "https://github.com/rustsec/advisory-db.git"
GITHUB = "https://github.com"


class CheckError(Exception):
    pass


def log(message):
    print(message, file=sys.stderr)


def env_path(name, default):
    return os.environ.get(name) or os.path.join(REPO_ROOT, default)


def rel(path):
    """A path as the report names it: relative to the repository if inside it."""
    absolute = os.path.abspath(path)
    if absolute.startswith(REPO_ROOT + os.sep):
        return os.path.relpath(absolute, REPO_ROOT)
    return path


def short(sha):
    return sha[:12]


# --- Versions and RustSec requirements ------------------------------------

VERSION = re.compile(r"^(\d+)\.(\d+)\.(\d+)(?:-([0-9A-Za-z.-]+))?(?:\+[0-9A-Za-z.-]+)?$")
COMPARATOR = re.compile(
    r"^(=|>=|<=|>|<|~|\^)?\s*(\*|\d+)(?:\.(\*|\d+))?(?:\.(\*|\d+))?(?:-([0-9A-Za-z.-]+))?$"
)


def version_key(text):
    match = VERSION.match(text.strip())
    if not match:
        raise CheckError(f"'{text}' is not a semantic version")
    major, minor, patch, pre = match.groups()
    pre_key = ()
    if pre:
        pre_key = tuple((0, int(p), "") if p.isdigit() else (1, 0, p) for p in pre.split("."))
    # A pre-release sorts before its release.
    return (int(major), int(minor), int(patch), 0 if pre else 1, pre_key)


def is_prerelease(text):
    match = VERSION.match(text.strip())
    return bool(match and match.group(4))


def _bound(parts, pre=None):
    major, minor, patch = (parts + [0, 0, 0])[:3]
    return version_key(f"{major}.{minor}.{patch}" + (f"-{pre}" if pre else ""))


def _comparator(text):
    """One comparator as (low, low_inclusive, high, high_inclusive); None is open."""
    match = COMPARATOR.match(text.strip())
    if not match:
        raise CheckError(f"cannot read the version requirement '{text}'")
    op, *raw, pre = match.groups()
    parts = []
    for part in raw:
        if part is None or part == "*":
            break
        parts.append(int(part))
    if not parts:
        return (None, True, None, False)
    full = len(parts) == 3

    def bump(n):
        # The next version after the first n parts: 1.2 -> 1.3.0, 1 -> 2.0.0.
        bumped = parts[:n]
        bumped[-1] += 1
        return _bound(bumped)

    low = _bound(parts, pre)
    if op == "=":
        return (low, True, low, True) if full else (low, True, bump(len(parts)), False)
    if op == ">":
        return (low, False, None, False) if full else (bump(len(parts)), True, None, False)
    if op == ">=":
        return (low, True, None, False)
    if op == "<":
        return (None, True, low, False)
    if op == "<=":
        return (None, True, low, True) if full else (None, True, bump(len(parts)), False)
    if op == "~":
        return (low, True, bump(min(len(parts), 2)), False)
    # Caret, written or implied: the leftmost non-zero part may not change.
    first_nonzero = next((i for i, p in enumerate(parts) if p != 0), None)
    if first_nonzero is None:
        return (low, True, bump(len(parts)), False)
    return (low, True, bump(first_nonzero + 1), False)


def requirement_matches(requirement, version):
    """Whether VERSION meets every comparator of a RustSec range like '>= 1.2, < 2'."""
    key = version_key(version)
    for text in requirement.split(","):
        low, low_inclusive, high, high_inclusive = _comparator(text)
        if low is not None and (key < low or (key == low and not low_inclusive)):
            return False
        if high is not None and (key > high or (key == high and not high_inclusive)):
            return False
    return True


# --- Inputs: Cargo.toml, Cargo.lock and the pin descriptions ---------------


def github_repo(url):
    match = re.fullmatch(r"https://github\.com/([A-Za-z0-9._-]+/[A-Za-z0-9._-]+?)(?:\.git)?/?", url)
    if not match:
        raise CheckError(f"{url} is not a GitHub repository URL; the report only follows GitHub forks")
    return match.group(1)


def normalise_url(url):
    return re.sub(r"(\.git)?/?$", "", url.strip())


def load_git_dependencies(cargo_toml, cargo_lock):
    """Git dependencies in [workspace.dependencies]: name -> source details."""
    with open(cargo_toml, "rb") as f:
        manifest = tomllib.load(f)
    with open(cargo_lock, "rb") as f:
        lock = tomllib.load(f)
    dependencies = manifest.get("workspace", {}).get("dependencies", {})
    locked = {}
    for package in lock.get("package", []):
        source = package.get("source", "")
        if source.startswith("git+"):
            url, _, commit = source[len("git+"):].partition("#")
            url = normalise_url(url.split("?", 1)[0])
            locked.setdefault(package["name"], []).append((url, commit, package["version"]))
    pins = {}
    for name, spec in dependencies.items():
        if not isinstance(spec, dict) or "git" not in spec:
            continue
        package = spec.get("package", name)
        url = normalise_url(spec["git"])
        matches = [entry for entry in locked.get(package, []) if entry[0] == url]
        if len(matches) != 1:
            raise CheckError(
                f"{name} is a git dependency on {url} in {rel(cargo_toml)}, but {rel(cargo_lock)} "
                f"has {len(matches)} {package} packages from it; run cargo update -p {package}"
            )
        _, commit, version = matches[0]
        if not re.fullmatch(r"[0-9a-f]{40}", commit):
            raise CheckError(f"{rel(cargo_lock)} names no full commit for {package}: '{commit}'")
        rev = spec.get("rev")
        if rev is not None and not commit.startswith(rev):
            raise CheckError(
                f"{name} pins rev {rev} in {rel(cargo_toml)} but {rel(cargo_lock)} has {commit}; "
                f"run cargo update -p {package}"
            )
        pins[name] = {"url": url, "repo": github_repo(url), "commit": commit, "version": version}
    return pins


def load_pins(manifest_path, git_dependencies, cargo_toml):
    with open(manifest_path, "rb") as f:
        entries = tomllib.load(f).get("pin", [])
    where = rel(manifest_path)
    listed = {}
    pins = []
    for entry in entries:
        crates = entry.get("crates")
        if not isinstance(crates, list) or not crates or not all(isinstance(c, str) for c in crates):
            raise CheckError(f"every [[pin]] in {where} needs a non-empty list of crates: {entry}")
        for key in ("upstream", "releases", "docs", "drop_when"):
            if not isinstance(entry.get(key), str) or not entry[key]:
                raise CheckError(f"the [[pin]] for {', '.join(crates)} in {where} needs a string {key!r}")
        if not re.fullmatch(r"[A-Za-z0-9._-]+/[A-Za-z0-9._-]+", entry["upstream"]):
            raise CheckError(f"upstream must be owner/repo in {where}: {entry['upstream']}")
        if entry["releases"] not in ("github", "crates.io"):
            raise CheckError(f"releases must be \"github\" or \"crates.io\" in {where}: {entry['releases']}")
        if "pull_request" in entry and not isinstance(entry["pull_request"], int):
            raise CheckError(f"pull_request must be a number in {where}: {entry['pull_request']!r}")
        if "release_tag" in entry and "{version}" not in str(entry["release_tag"]):
            raise CheckError(f"release_tag must contain {{version}} in {where}: {entry['release_tag']!r}")
        for crate in crates:
            if crate in listed:
                raise CheckError(f"{crate} is listed twice in {where}")
            if crate not in git_dependencies:
                raise CheckError(
                    f"{where} lists {crate}, which is not a git dependency in "
                    f"{rel(cargo_toml)}'s [workspace.dependencies]; remove its [[pin]] once the fork is gone"
                )
            listed[crate] = entry
        sources = {(git_dependencies[c]["url"], git_dependencies[c]["commit"]) for c in crates}
        if len(sources) != 1:
            raise CheckError(f"{' and '.join(crates)} share one [[pin]] but not one git source and commit")
        pins.append(entry)
    for name in git_dependencies:
        if name not in listed:
            raise CheckError(
                f"{name} is a git dependency in {rel(cargo_toml)} but no [[pin]] in {where} lists it; "
                "describe what its fork follows there"
            )
    return pins


# --- Lookups: GitHub, the crates.io index and advisory-db -------------------


class Upstream:
    def __init__(self, fixtures):
        self.fixtures = fixtures
        self.advisory_db_path = None
        self.calls = 0

    def api(self, endpoint):
        if self.fixtures:
            path = os.path.join(self.fixtures, "api", endpoint.replace("/", "__") + ".json")
            if not os.path.isfile(path):
                raise CheckError(f"no fixture for {endpoint} ({path})")
            with open(path, encoding="utf-8") as f:
                return json.load(f)
        self.calls += 1
        result = subprocess.run(["gh", "api", endpoint], capture_output=True, text=True)
        if result.returncode != 0:
            raise CheckError(f"gh api {endpoint} failed: {result.stderr.strip()}")
        return json.loads(result.stdout)

    def crate_versions(self, name):
        """The crates.io index entries for NAME (one per version), or []."""
        if self.fixtures:
            path = os.path.join(self.fixtures, "index", name)
            if not os.path.isfile(path):
                return []
            with open(path, encoding="utf-8") as f:
                text = f.read()
        else:
            lower = name.lower()
            if len(lower) <= 2:
                prefix = f"{len(lower)}"
            elif len(lower) == 3:
                prefix = f"3/{lower[0]}"
            else:
                prefix = f"{lower[:2]}/{lower[2:4]}"
            request = urllib.request.Request(
                f"https://index.crates.io/{prefix}/{lower}", headers={"User-Agent": "git-pins-check"}
            )
            try:
                with urllib.request.urlopen(request, timeout=60) as response:
                    text = response.read().decode("utf-8")
            except urllib.error.HTTPError as error:
                if error.code == 404:
                    return []
                raise CheckError(f"the crates.io index answered {error.code} for {name}") from error
        return [json.loads(line) for line in text.splitlines() if line.strip()]

    def advisory_db(self):
        if self.advisory_db_path:
            return self.advisory_db_path
        path = os.environ.get("GIT_PINS_ADVISORY_DB")
        if not path and self.fixtures:
            path = os.path.join(self.fixtures, "advisory-db")
        if not path:
            path = os.path.join(tempfile.mkdtemp(prefix="git-pins-"), "advisory-db")
            result = subprocess.run(
                ["git", "clone", "--quiet", "--depth", "1", ADVISORY_DB_URL, path],
                capture_output=True,
                text=True,
            )
            if result.returncode != 0:
                raise CheckError(f"could not clone {ADVISORY_DB_URL}: {result.stderr.strip()}")
        if not os.path.isdir(os.path.join(path, "crates")):
            raise CheckError(f"{path} is not an advisory-db checkout (no crates/ folder)")
        if os.path.isdir(os.path.join(path, ".git")):
            head = subprocess.run(
                ["git", "-C", path, "log", "-1", "--format=%h %cs"], capture_output=True, text=True
            ).stdout.strip()
            log(f"RustSec advisory-db at {head}")
        self.advisory_db_path = path
        return path

    def advisories(self, crate):
        found = []
        for path in sorted(glob.glob(os.path.join(self.advisory_db(), "crates", crate, "*.md"))):
            with open(path, encoding="utf-8") as f:
                text = f.read()
            match = re.match(r"\s*```toml\n(.*?)\n```\n(.*)", text, flags=re.S)
            if not match:
                raise CheckError(f"cannot read the advisory {path}")
            data = tomllib.loads(match.group(1))
            title = re.search(r"^#\s+(.+)$", match.group(2), flags=re.M)
            advisory = data.get("advisory", {})
            versions = data.get("versions", {})
            found.append(
                {
                    "id": advisory.get("id", os.path.basename(path)[:-3]),
                    "title": title.group(1).strip() if title else "untitled",
                    "withdrawn": bool(advisory.get("withdrawn")),
                    "patched": versions.get("patched", []),
                    "unaffected": versions.get("unaffected", []),
                }
            )
        return found


def affects(advisory, version):
    if advisory["withdrawn"]:
        return False
    ranges = advisory["patched"] + advisory["unaffected"]
    return not any(requirement_matches(r, version) for r in ranges)


def latest_crate_release(entries):
    stable = [e for e in entries if not e.get("yanked") and not is_prerelease(e["vers"])]
    return max(stable, key=lambda e: version_key(e["vers"]), default=None)


def contains(upstream, repo, commit, ref):
    """Whether REF (a tag) contains COMMIT in REPO, by GitHub's compare."""
    status = upstream.api(f"repos/{repo}/compare/{commit}...{ref}").get("status")
    return status in ("ahead", "identical")


# --- The report -------------------------------------------------------------


def date_of(timestamp):
    return (timestamp or "")[:10] or "date unknown"


def report_pin(pin, git_dependencies, upstream):
    crates = pin["crates"]
    first = git_dependencies[crates[0]]
    fork, commit = first["repo"], first["commit"]
    up = pin["upstream"]
    docs = pin["docs"]
    pin_link = f"[`{short(commit)}`]({GITHUB}/{fork}/commit/{commit})"
    rows = [
        (
            "Pinned",
            f"{pin_link} on [`{fork}`]({GITHUB}/{fork}) ("
            + ", ".join(f"{c} {git_dependencies[c]['version']}" for c in crates)
            + ")",
        )
    ]
    findings = []

    # The latest upstream release, needed by the pull request's merge check.
    release_label = release_ref = None
    release_entry = None
    if pin["releases"] == "github":
        release = upstream.api(f"repos/{up}/releases/latest")
        release_ref = release["tag_name"]
        release_label = f"`{release_ref}`"
        release_cell = (
            f"[`{release_ref}`]({GITHUB}/{up}/releases/tag/{release_ref}) "
            f"({date_of(release.get('published_at'))})"
        )
    else:
        release_entry = latest_crate_release(upstream.crate_versions(crates[0]))
        if release_entry is None:
            release_cell = f"none on crates.io for `{crates[0]}`"
        else:
            version = release_entry["vers"]
            release_label = f"{crates[0]} {version}"
            if pin.get("release_tag"):
                release_ref = pin["release_tag"].format(version=version)
            release_cell = f"[{crates[0]} {version}](https://crates.io/crates/{crates[0]}/{version})"

    release_contains_pr = False
    if "pull_request" in pin:
        number = pin["pull_request"]
        pr = upstream.api(f"repos/{up}/pulls/{number}")
        pr_link = f"[{up}#{number}]({GITHUB}/{up}/pull/{number})"
        head = pr["head"]["sha"]
        head_repo = (pr["head"].get("repo") or {}).get("full_name") or up
        head_link = f"[`{short(head)}`]({GITHUB}/{head_repo}/commit/{head})"
        if pr.get("merged"):
            merge = pr["merge_commit_sha"]
            merge_link = f"[`{short(merge)}`]({GITHUB}/{up}/commit/{merge})"
            state = f"merged ({date_of(pr.get('merged_at'))}) as {merge_link}"
            if release_ref is None:
                finding = f"{pr_link} was {state}; check whether {release_label or 'a release'} contains it."
            elif contains(upstream, up, merge, release_ref):
                release_contains_pr = True
                state += f"; {release_label} contains it"
                finding = f"{pr_link} was {state.replace('; ', ', and ', 1)}: the fork can be dropped (`{docs}`)."
            else:
                state += f"; {release_label} does not contain it yet"
                finding = f"{pr_link} was {state.replace('; ', ', but ', 1)}, so the fork stays until a release does."
            findings.append(finding)
        elif pr.get("state") == "closed":
            state = f"closed without being merged ({date_of(pr.get('closed_at'))})"
            findings.append(f"{pr_link} was {state}. Decide where the pin goes next (`{docs}`).")
        else:
            state = "open"
        rows.append(("Tracks", f"{pr_link} \"{pr.get('title', '')}\": {state}"))
        moved = head != commit
        rows.append(
            (
                "Pull request head",
                f"{head_link} (`{head_repo}:{pr['head'].get('ref', '?')}`): "
                + ("moved since the pin" if moved else "the pinned commit"),
            )
        )
        if moved:
            findings.append(
                f"the head of {pr_link} moved to {head_link}; the pin is {pin_link} "
                f"([changes]({GITHUB}/{head_repo}/compare/{commit}...{head})). "
                f"Follow it by hand if the new commits are wanted (`{docs}`)."
            )
    else:
        rows.append(("Tracks", f"[`{up}`]({GITHUB}/{up}) releases; no pull request to follow"))

    rows.append(("Latest upstream release", release_cell))
    if release_entry is not None:
        pinned_version = first["version"]
        version = release_entry["vers"]
        dependency = pin.get("show_dependency")
        requirement = None
        if dependency:
            reqs = sorted(
                {
                    d["req"]
                    for d in release_entry.get("deps", [])
                    if d.get("package", d["name"]) == dependency and d.get("kind") != "dev"
                }
            )
            requirement = ", ".join(f"`{r}`" for r in reqs) if reqs else "not a dependency"
            rows.append((f"`{dependency}` in that release", requirement))
        if version_key(version) > version_key(pinned_version) and not release_contains_pr:
            finding = f"{crates[0]} {version} is out on crates.io (the fork declares {pinned_version})"
            if dependency:
                finding += f"; its `{dependency}` requirement is {requirement}"
            finding += ". Check it for fixes the fork lacks"
            if release_ref:
                finding += f" ([changes]({GITHUB}/{up}/compare/{commit}...{release_ref}))"
            finding += f", and whether the fork can be dropped (`{docs}`)."
            findings.append(finding)

    advisory_cells = []
    for crate in crates:
        version = git_dependencies[crate]["version"]
        prefix = f"`{crate}`: " if len(crates) > 1 else ""
        for advisory in upstream.advisories(crate):
            link = f"[{advisory['id']}](https://rustsec.org/advisories/{advisory['id']})"
            if advisory["withdrawn"]:
                advisory_cells.append(f"{prefix}{advisory['id']} ({advisory['title']}): withdrawn")
            elif affects(advisory, version):
                advisory_cells.append(f"{prefix}{link} ({advisory['title']}): **affects {version}**")
                patched = ", ".join(advisory["patched"]) or "none"
                findings.append(
                    f"{link} ({advisory['title']}) affects {crate} {version}, the version the fork declares; "
                    f"patched: `{patched}`. Check whether the fork has the fix (`{docs}`)."
                )
            else:
                advisory_cells.append(f"{prefix}{link} ({advisory['title']}): does not affect {version}")
    if not advisory_cells:
        advisory_cells.append("None for " + ", ".join(f"`{c}`" for c in crates) + ".")
    rows.append(("RustSec", "<br>".join(advisory_cells)))
    rows.append(("Drop the fork when", f"{pin['drop_when'].rstrip('.')} (`{docs}`)."))

    title = ", ".join(crates)
    section = "\n".join(
        [f"### {title}", "", "| | |", "| --- | --- |", *(f"| {k} | {v} |" for k, v in rows)]
    )
    return section, [f"**{title}**: {f}" for f in findings]


def build_report(pins, git_dependencies, upstream):
    sections, findings = [], []
    for pin in pins:
        section, pin_findings = report_pin(pin, git_dependencies, upstream)
        sections.append(section)
        findings += pin_findings
    count = len(findings)
    if not pins:
        title = "Git pins: none left"
    elif count == 0:
        title = "Git pins: nothing to act on"
    else:
        title = f"Git pins: {count} finding{'s' if count != 1 else ''}"
    crate_count = sum(len(p["crates"]) for p in pins)
    lines = [
        MARKER,
        f"Zmail takes {crate_count} crate(s) from git forks pinned by commit in `Cargo.toml`. "
        "Dependabot skips them and RustSec advisories cannot match a git source, so this report compares "
        "each pin with the upstream pull request it follows, the latest upstream release and the RustSec "
        "advisories for the upstream crate names. The weekly `git-pins` workflow "
        "(`scripts/check-git-pins.py`) updates it in place; `docs/upstream/git-pins.md` says what to do "
        "with each finding.",
        "",
        "## Findings",
        "",
    ]
    if not pins:
        lines.append("No git pins remain; this issue can be closed.")
    elif findings:
        lines += [f"- {f}" for f in findings]
    else:
        lines.append("Nothing to act on.")
    if sections:
        lines += ["", "## Pins", "", "\n\n".join(sections)]
    return title, "\n".join(lines) + "\n", count


# --- The tracking issue -----------------------------------------------------


def gh(*args):
    result = subprocess.run(["gh", *args], capture_output=True, text=True)
    if result.returncode != 0:
        raise CheckError(f"gh {' '.join(args[:2])} failed: {result.stderr.strip()}")
    return result.stdout


def normalise_body(text):
    return text.replace("\r\n", "\n").strip()


def update_issue(title, body, body_file, no_pins):
    issues = json.loads(gh("issue", "list", "--state", "open", "--limit", "200", "--json", "number,title,body") or "[]")
    existing = next((i for i in issues if MARKER in (i.get("body") or "")), None)
    if existing is None:
        if no_pins:
            log("No git pins and no open tracking issue; nothing to do.")
            return
        print(gh("issue", "create", "--title", title, "--body-file", body_file).strip(), file=sys.stderr)
        return
    number = str(existing["number"])
    if existing.get("title") == title and normalise_body(existing.get("body") or "") == normalise_body(body):
        log(f"Issue #{number} is already up to date.")
    else:
        gh("issue", "edit", number, "--title", title, "--body-file", body_file)
        log(f"Updated issue #{number}.")
    if no_pins:
        gh("issue", "close", number, "--comment", "No git-pinned dependencies remain.")
        log(f"Closed issue #{number}.")


def write_output(**values):
    path = os.environ.get("GITHUB_OUTPUT")
    if path:
        with open(path, "a", encoding="utf-8") as f:
            for key, value in values.items():
                f.write(f"{key}={value}\n")


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--dry-run", action="store_true", help="print the report (the default)")
    mode.add_argument("--update-issue", action="store_true", help="open or update the tracking issue")
    mode.add_argument("--validate", action="store_true", help="only check Cargo.toml against the pin descriptions")
    args = parser.parse_args()

    cargo_toml = env_path("GIT_PINS_CARGO_TOML", "Cargo.toml")
    cargo_lock = env_path("GIT_PINS_CARGO_LOCK", "Cargo.lock")
    manifest = env_path("GIT_PINS_MANIFEST", "docs/upstream/git-pins.toml")
    try:
        git_dependencies = load_git_dependencies(cargo_toml, cargo_lock)
        pins = load_pins(manifest, git_dependencies, cargo_toml)
        if args.validate:
            print(f"{len(git_dependencies)} git dependencies, all described in {rel(manifest)}.")
            return 0
        upstream = Upstream(os.environ.get("GIT_PINS_FIXTURES"))
        title, body, count = build_report(pins, git_dependencies, upstream)
        body_file = os.environ.get("ISSUE_BODY_FILE")
        if not body_file:
            fd, body_file = tempfile.mkstemp(prefix="git-pins-", suffix=".md")
            os.close(fd)
        with open(body_file, "w", encoding="utf-8") as f:
            f.write(body)
        if upstream.calls:
            log(f"{upstream.calls} GitHub API call(s).")
        write_output(findings=count, title=title, body_file=body_file)
        if args.update_issue:
            log(title)
            update_issue(title, body, body_file, no_pins=not pins)
        else:
            print(f"{title}\n\n{body}", end="")
    except CheckError as error:
        log(f"error: {error}")
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
