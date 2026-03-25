# Zed Upstream Record

This document records the upstream inputs that Phase 0 was pinned against on March 20, 2026.

## Upstream Inputs

- Zed repository: `zed-industries/zed`
- Commit selected for theme and GPUI reference: `30ceb9176cb751b0f4f60516a3248f4ded99b082`
- Commit title: `Reduce wgpu memory usage during resize (#50030)`
- Commit date on the GitHub commits view: February 27, 2026
- GPUI crate version declared by upstream at that point: `0.2.2`
- Zed toolchain declaration at that point: Rust `1.93`
- Theme schema used by Zmail: `https://zed.dev/schema/themes/v0.2.0.json`

## Chosen Approach

- Use the published `gpui` crate for the application shell in Phase 0.
- Keep the Zed dependency surface narrow by implementing a local compatibility layer for unchanged Zed theme JSON files.
- Record the upstream commit above so later theme or UI parity work can be traced back to a concrete Zed snapshot.

## Follow-Up

If Phase 1 or later work requires upstream theme behavior not covered by the local compatibility layer, extend `crates/zmail-theme` in narrow slices and update this file with the new commit reference.
