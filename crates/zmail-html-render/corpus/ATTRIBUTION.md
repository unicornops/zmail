# Spike corpus: sources and licences

HTML mail used by the issue #134 spike (`docs/spikes/blitz-html-renderer.md`).
No real person's mail is included. No image files are included: every remote
image the templates reference is refused by the renderer and was blocked in
the browser reference, so no third-party logo or artwork is ever loaded.

Files 01–26 are third-party email templates, copied at the revision listed and
modified as follows: template placeholders (`{{name}}`, `{% if %}` and similar)
were replaced with neutral sample values or removed, and the visible
"Sendwithus" footer text in the sendwithus templates was replaced with
"Example Ltd.". Licence texts are in `licenses/`.

| Files | Source | Revision | Licence |
| --- | --- | --- | --- |
| 01–03 `cerberus-*` | [TedGoas/Cerberus](https://github.com/TedGoas/Cerberus), Copyright (c) 2017 Ted Goas | `af3cec9` | MIT (`licenses/Cerberus-MIT.txt`) |
| 04–05 `leemunroe-*` | [leemunroe/responsive-html-email-template](https://github.com/leemunroe/responsive-html-email-template), Copyright (c) 2013 Lee Munroe | `60b5b6d` | MIT (`licenses/responsive-html-email-template-MIT.txt`) |
| 06–08 `mailgun-*` | [mailgun/transactional-email-templates](https://github.com/mailgun/transactional-email-templates) (inlined variants), Copyright (c) 2014 Mailgun | `e05e59e` | MIT (`licenses/mailgun-transactional-email-templates-MIT.txt`) |
| 09–15 `postmark-*` | [ActiveCampaign/postmark-templates](https://github.com/ActiveCampaign/postmark-templates) (`templates-inlined/basic-full`), Copyright (c) 2015 Wildbit | `fa73527` | MIT (`licenses/postmark-templates-MIT.txt`) |
| 16–26 `sendwithus-*` | [sendwithus/templates](https://github.com/sendwithus/templates) | `f555e49` | Apache-2.0 (`licenses/sendwithus-templates-Apache-2.0.txt`, `NOTICE` in `licenses/sendwithus-templates-NOTICE.txt`) |
| 27–33 `synthetic-*` | Written for this spike (zmail contributors) | — | GPL-3.0-only, like the rest of the repository |

The synthetic files cover what the templates do not: Word/Outlook-generated
markup (27), `prefers-color-scheme` dark mode (28), a quoted reply thread
(29), RTL and non-Latin scripts (30), floats (31), a `multipart/related`
message with `cid:` images (32) and a hostile probe of every load and
navigation path (33). A 1,500-row statement used for the stress numbers in
the report is generated, not committed.
