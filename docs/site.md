# The documentation site

Zmail's documentation site, `https://docs.zmail.unicornops.ie/`, is built with [mdBook](https://rust-lang.github.io/mdBook/) from the markdown in this repository and served by Cloudflare Pages. The markdown stays the source and keeps reading as it does on GitHub: the build copies it into a staging tree and adapts the links there, never in the repository.

## What is on it

- The home page is [`README.md`](../README.md); the other pages are everything [`docs/SUMMARY.md`](./SUMMARY.md) lists, which is also the site's table of contents. A page keeps its repository path: `docs/install.md` is `docs/install.html` on the site.
- `/` is the latest release: the newest `vX.Y.Z` tag that has the site's scripts, built by that tag's own scripts, with links to repository files (scripts, config, licences) pointing at that tag on GitHub.
- `/dev/` is `main`, titled "Zmail (development)", with a note at the top of every page saying it describes unreleased code and linking to `/`. Its links to repository files point at `main`, and its pages ask search engines not to index them, so searches find the release's pages.
- Releases made before the site existed cannot be built (they have no `docs/SUMMARY.md` or scripts), so until a release includes the site, `/` is built from `main` too, with a note saying no release has the site yet. The first release after the site lands ends that.
- There are no copies per release: both copies are rebuilt from git on every deploy, which takes a second. Per-minor copies and a version menu can come later.
- Search is mdBook's own: a static index, loaded from the site on the first search.
- Each page has an edit button (it always edits `main`), a link to the repository, and a canonical link to its address under `https://docs.zmail.unicornops.ie/`, so the `pages.dev` address counts as a copy. The address is `SITE_ORIGIN` at the top of [`build-site.sh`](../scripts/docs-site/build-site.sh), the one place to change it.
- The site loads nothing from other origins: mdBook serves its own fonts and icons, the Rust playground is off (it would ask `play.rust-lang.org` for its crate list), and there are no analytics. The link check below fails the build if a page loads anything from elsewhere.
- Screenshots: `/` shows the release's own screenshots of the UI, captured and published by the release workflow; `/dev/` shows the committed files in [`docs/images`](./images), the same ones GitHub shows. [Screenshots](#screenshots) below has the details.

## Adding or changing a page

- Write the markdown as for GitHub, with relative links. A new page must also go in [`docs/SUMMARY.md`](./SUMMARY.md), or the site does not build it and the link check fails on every link to it.
- A link to a file that is not a page (a script, `Cargo.toml`, a licence) works on the site too: it points at the file on GitHub.
- Bare HTML tags in prose (`Remove <name>`) are dropped by GitHub and make mdBook warn, which fails the build: put them in backticks.

## Screenshots

Every image in [`docs/images`](./images) is a screenshot named after the UI screenshot matrix state it shows (`scripts/capture-release-screenshot.sh --list`): `reader-light.png` is the state `reader-light`. They are synthetic fixture data, never real mail.

1. **Release.** The release workflow's Release Screenshot job builds the release and runs [`capture-screenshots.sh`](../scripts/docs-site/capture-screenshots.sh), which captures one matrix state per image in the release's `docs/images` (no mail server, no network). Publish Release Assets uploads each as `zmail-docs-<image>` (`zmail-docs-reader-light.png` and so on), lists them, with `zmail-linux-screenshot.png`, in `SHA256SUMS-Screenshots`, and attests every one of them like the other assets.
2. **Site.** After the assets are uploaded, the release workflow calls the Docs Site workflow. `build-site.sh` builds `/` from the tag as before, then [`release-screenshots.sh`](../scripts/docs-site/release-screenshots.sh) downloads the tag's `zmail-docs-*.png` and `SHA256SUMS-Screenshots` with `gh` and the job's read-only token, and checks each file's build provenance attestation (made by `release-please.yml` in this repository on a GitHub-hosted runner). [`screenshots.py`](../scripts/docs-site/screenshots.py) then checks each against `SHA256SUMS-Screenshots`, and that it is a well-formed PNG (every chunk's checksum, nothing after the end, under 10 MB, 100 to 8192 pixels a side), before putting it over the committed copy in the built `/`. The build log and the job summary say how many screenshots came from the release.
3. **Fallback.** An image without a usable asset keeps the committed copy, with the reason in the log: releases up to 0.34.4 have no such assets, and a failed download, a failed check or no `gh` (a local build without it, for example) never fails the build or leaves a broken image. A bad attestation also leaves a warning on the run.
4. **`/dev/`** always uses the committed copies. `main`'s UI may be ahead of the last release, so neither set shows it exactly; the committed copies are what the working tree has, so a pull request that changes or adds an image sees it in its own site build.

Adding an image to the docs adds it to the next release's capture. Name it after a matrix state (add the state to the fixture and the script's `MATRIX` first if needed); the Docs Site workflow runs `capture-screenshots.sh --check` on every pull request and fails when an image in `docs/images` is not a state.

**Refreshing the committed copies.** Nothing commits screenshots automatically. Every so often, or after a release that changes what they show, replace them with the latest release's, checked the same way, and commit the result in a pull request:

```bash
scripts/docs-site/refresh-images.sh          # or: refresh-images.sh v0.35.0
git diff --stat docs/images
```

The README keeps pointing at the committed copies rather than at `releases/latest/download/` addresses, so it never shows an image that is missing from a release.

## Building it locally

On x86-64 Linux, fetch the pinned mdBook and lychee (checked against their SHA-256) and build the whole site as it is deployed, then open `target/docs-site/index.html` in a browser:

```bash
git fetch --tags
scripts/docs-site/install-tools.sh target/docs-tools
export PATH="$PWD/target/docs-tools:$PATH"
scripts/docs-site/build-site.sh target/docs-site
```

`build-site.sh` builds the latest release into `target/docs-site` and your working tree, uncommitted changes included, into `target/docs-site/dev`, then runs the offline link check. With `gh` signed in, `/` gets the release's screenshots as in CI; without it, or with `--screenshots none`, the committed ones. Elsewhere, install mdBook and lychee at the versions in [`install-tools.sh`](../scripts/docs-site/install-tools.sh) another way (`cargo install mdbook --version 0.5.4 --locked`, and lychee likewise).

| Script | What it does |
| --- | --- |
| [`build-site.sh OUT`](../scripts/docs-site/build-site.sh) | The deployable site: `/` and `/dev/`, then `check.sh`. `--dev-ref REF` (where `/dev/`'s source links point), `--release TAG` or `--release none`, `--screenshots download`, `none` or a folder of downloaded assets, `--no-check`. |
| [`build.sh OUT`](../scripts/docs-site/build.sh) | One copy from the working tree: `--ref REF`, `--site-url /dev/`, `--title`, `--banner`, `--canonical-url`, `--noindex`. Any mdBook warning fails it. |
| [`check.sh OUT`](../scripts/docs-site/check.sh) | Offline: links between pages, anchors and images, plus no resources from other origins. `--external` checks links to other sites online, and links to this repository's files against the local clone ([`repo_links.py`](../scripts/docs-site/repo_links.py)). |
| [`stage.py`](../scripts/docs-site/stage.py) | Copies the pages and their images into one tree and rewrites the links (see its header). |
| [`screenshots.py`](../scripts/docs-site/screenshots.py) | Maps `docs/images` to matrix states and release asset names, and checks downloaded screenshots before they replace an image (see its header). |
| [`release-screenshots.sh TAG DIR`](../scripts/docs-site/release-screenshots.sh) | Downloads a release's documentation screenshots and checks their attestations. |
| [`capture-screenshots.sh OUT`](../scripts/docs-site/capture-screenshots.sh) | Captures the documentation screenshots under an X server (the release workflow runs it); `--check` only checks every image is a matrix state. |
| [`refresh-images.sh [TAG]`](../scripts/docs-site/refresh-images.sh) | Replaces the committed screenshots with a release's. |
| [`head.py`](../scripts/docs-site/head.py) | Adds the canonical link and, for `/dev/`, the noindex tag to each built page. |
| [`test_stage.py`](../scripts/docs-site/test_stage.py) | Self-test for `stage.py`'s link rewriting, the screenshot checks and `release-screenshots.sh` (against a stand-in for `gh`) and, with mdBook installed, `build.sh` and `build-site.sh`: `python3 -I scripts/docs-site/test_stage.py`. |
| [`install-tools.sh DIR`](../scripts/docs-site/install-tools.sh) | Downloads the pinned mdBook and lychee and checks them. |
| [`book.toml`](../scripts/docs-site/book.toml) | mdBook's configuration. |
| [`deploy/`](../scripts/docs-site/deploy/package.json) | The pinned wrangler (`package.json` and lockfile) that uploads the site. |

## Checks

The [Docs Site workflow](../.github/workflows/docs.yml) builds the site on every pull request, not only docs changes, because the docs link into the code and moving a file there breaks them. It runs the self-test, `shellcheck`, `capture-screenshots.sh --check`, and the offline check over both copies: any broken link between pages, broken anchor, missing image or third-party resource fails it. It uploads the built site as the `docs-site` artefact, so a pull request's site can be downloaded and looked at.

Links to other sites are checked weekly (and when the workflow is run by hand), not on pull requests, since outside sites come and go. A failure shows as a failed scheduled run.

## Deploying

The Docs Site workflow deploys with `wrangler pages deploy`, uploading the folder CI built and checked (direct upload). Cloudflare's own Git integration is not used, so what is live is always what passed the check. Each deploy replaces the whole site, `/` and `/dev/` together, so each deploy rebuilds both.

It deploys:

- on a push to `main` that changes the docs or the site tooling;
- when release-please creates a release: `release-please.yml` calls the workflow (a release made with `GITHUB_TOKEN` starts no `release` workflows) once Publish Release Assets has finished, so `/` moves to the new tag with its screenshots. That waits for every platform's build, macOS notarisation included, so `/` changes up to an hour or so after the release appears; it runs even when a platform failed, with the committed images for any screenshot that is missing. The push that merges the release pull request also deploys (it changes `CHANGELOG.md`), possibly before the tag or its screenshots exist; the release's own run comes later and replaces it;
- when the workflow is run by hand on `main`.

Builds of `main` run one at a time in order and build `main` as it is when they start, so the newest deploy always wins. The deploy job runs only on `main` in this repository, never for pull requests (and forks get no secrets), in the `docs-site` environment that holds the Cloudflare credentials. Until the setup below is done it does not deploy: with the `CLOUDFLARE_PAGES_PROJECT` variable unset the job is skipped and the build job leaves a notice saying so, and with the variable set but a credential missing the job leaves a notice and stops without failing.

The build job holds no secrets, only the default token with `contents: read` and `attestations: read` (for the release's screenshots), and the deploy job runs none of the repository's scripts: it installs wrangler from its lockfile (`npm ci --ignore-scripts`, every package checked against its integrity hash) and uploads. Wrangler's usage metrics are turned off. Dependabot proposes wrangler updates monthly; mdBook and lychee are bumped by hand in `install-tools.sh` (version and checksum together, then build the site before merging).

## One-time setup

Cloudflare Pages and the `unicornops.ie` DNS zone are managed as code outside this repository (Pages with Terragrunt, DNS with octodns), and the zone's DNS is not served by Cloudflare. `www.unicornops.ie` is already served by Cloudflare Pages this way. Until these steps are done the workflow builds and checks the site but deploys nothing. The bare `zmail.unicornops.ie` is kept for a future project website, a separate Pages project.

1. **Pages project.** Create a Cloudflare Pages project named `zmail-docs` for direct upload: no Git source, production branch `main` (the workflow deploys with `--branch main`, so every deploy is a production deploy). In Terraform that is a `cloudflare_pages_project` with `production_branch = "main"` and no `source` block; by hand, `wrangler pages project create zmail-docs --production-branch main`.
2. **API token.** Create an API token with one permission, Account → Cloudflare Pages → Edit, limited to the account that holds the project, and no zone permissions.
3. **GitHub environment.** In the repository, create an environment named `docs-site`. Under deployment branches and tags, choose selected branches and allow only `main` (release deploys run on `main` too, through `release-please.yml`). Add two environment secrets: `CLOUDFLARE_API_TOKEN` (the token) and `CLOUDFLARE_ACCOUNT_ID` (the account's id).
4. **Turn deploys on.** Add the repository variable `CLOUDFLARE_PAGES_PROJECT` set to `zmail-docs`. This is the switch: set it last.
5. **First deploy.** Run the Docs Site workflow by hand on `main` (Actions → Docs Site → Run workflow) and check `https://zmail-docs.pages.dev/` and `/dev/`.
6. **Custom domain.** Add `docs.zmail.unicornops.ie` as a custom domain of the Pages project (`cloudflare_pages_domain` in Terraform), so Cloudflare verifies it and issues its certificate, and add a CNAME record from `docs.zmail.unicornops.ie` to `zmail-docs.pages.dev` in the `unicornops.ie` zone through octodns. The zone's CAA records allow Let's Encrypt and Amazon only; `www.unicornops.ie` got its Pages certificate under them, but if the certificate stays pending, look there first. The `pages.dev` address keeps working beside it.
7. **Afterwards.** Set the repository's website field to `https://docs.zmail.unicornops.ie/` and link the site from the README.

A different project name changes only steps 1, 4 and 6 (the CNAME target). A different hostname changes `SITE_ORIGIN` in `build-site.sh` and steps 6 and 7.
