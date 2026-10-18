# Changelog

## [0.11.0](https://github.com/unicornops/zmail/compare/v0.10.0...v0.11.0) (2026-10-18)


### Features

* **search:** fall back to server search for mail not indexed yet ([#159](https://github.com/unicornops/zmail/issues/159)) ([68ea246](https://github.com/unicornops/zmail/commit/68ea24675f1dab98ec7d0e9a86f5adacac386760))
* **imap:** resync mailboxes with QRESYNC and VANISHED ([#157](https://github.com/unicornops/zmail/issues/157)) ([b267db2](https://github.com/unicornops/zmail/commit/b267db2fcb6406aa38ad3905fac06c7733ee6c1e))
* **ui:** add resizable panes and a responsive layout ([#155](https://github.com/unicornops/zmail/issues/155)) ([f6b100b](https://github.com/unicornops/zmail/commit/f6b100b87426b933c73524ed1b49189e82a34ab2))
* **jmap:** hold a push connection and sync changes through Email/changes ([#153](https://github.com/unicornops/zmail/issues/153)) ([ab314e2](https://github.com/unicornops/zmail/commit/ab314e21294a76bafac449b36b9fa643b7763ab7))
* **ui:** redesign the toolbar and move message actions into the reader ([#151](https://github.com/unicornops/zmail/issues/151)) ([277fa29](https://github.com/unicornops/zmail/commit/277fa29703019812a2f99fa03c37a972b8dae7ac))
* **search:** index message bodies in the background ([#156](https://github.com/unicornops/zmail/issues/156)) ([59bb725](https://github.com/unicornops/zmail/commit/59bb725ca8967ab98472639f0e9f19648e05fd0c))
* **search:** query operators compiled to parameterised SQL and FTS5 ([#152](https://github.com/unicornops/zmail/issues/152)) ([08c3da3](https://github.com/unicornops/zmail/commit/08c3da3e2408b445413a4091c691c2375e66e34e))
* **ui:** platform title bar with the toolbar inside and a Linux CSD frame ([#160](https://github.com/unicornops/zmail/issues/160)) ([660557d](https://github.com/unicornops/zmail/commit/660557d43952bb6b18db1f5068b538af6843269e))
* **ui:** menu model, native macOS menus, app-menu button and bundled Inter ([#154](https://github.com/unicornops/zmail/issues/154)) ([d160894](https://github.com/unicornops/zmail/commit/d1608945dddd0cf3220314399c5c54b75143de86))
* **ui:** real text input with IME, selection, undo and safe Unicode editing ([#144](https://github.com/unicornops/zmail/issues/144)) ([68682d5](https://github.com/unicornops/zmail/commit/68682d5e3b79697e25682131c66f05289b18da96))
* **ui:** add the core component set ([#145](https://github.com/unicornops/zmail/issues/145)) ([27dfd13](https://github.com/unicornops/zmail/commit/27dfd137348388e755bdc394d2e4c10b4d607a4a))
* **ui:** tokens 2.0, platform style, contrast checks and UI preferences ([#143](https://github.com/unicornops/zmail/issues/143)) ([ca2a4b7](https://github.com/unicornops/zmail/commit/ca2a4b78b788cdc9e22b6362ef62418797870824))

### Bug Fixes

* **store:** begin write transactions immediately so writes wait out a sync ([#165](https://github.com/unicornops/zmail/issues/165)) ([69e414f](https://github.com/unicornops/zmail/commit/69e414f8b858ef39d0d173776d88a21c25555e70))
* **security:** block remote URLs only inside tags, keep them in visible text ([#166](https://github.com/unicornops/zmail/issues/166)) ([13b6733](https://github.com/unicornops/zmail/commit/13b6733614d3b489e243f5cc76762e044c361fef))
* **store:** give each ephemeral store its own directory ([#149](https://github.com/unicornops/zmail/issues/149)) ([3a9005d](https://github.com/unicornops/zmail/commit/3a9005dbddb95601c5ffa661b8efa21a13e6c095))
* **jmap:** render HTML-only bodies through the sanitizer and detect remote content ([#141](https://github.com/unicornops/zmail/issues/141)) ([281c4ec](https://github.com/unicornops/zmail/commit/281c4ec5a6340bf4e91d3fbdde47c034b6dd15ef))

## [0.10.0](https://github.com/unicornops/zmail/compare/v0.9.0...v0.10.0) (2026-10-16)


### Features

* move GPUI to the Zed v1.24.2 stable release ([#125](https://github.com/unicornops/zmail/issues/125)) ([a943d26](https://github.com/unicornops/zmail/commit/a943d26c167a6c353aed0ae714b7adc2f0622cd2))
* **ui:** read real Zed theme colours and follow the system appearance ([#129](https://github.com/unicornops/zmail/issues/129)) ([a1ce599](https://github.com/unicornops/zmail/commit/a1ce599527ba32822d1c13d13a91c437076c2152))
* **ui:** relative dates, sender names and structured participants ([#127](https://github.com/unicornops/zmail/issues/127)) ([916508a](https://github.com/unicornops/zmail/commit/916508a56e2a1c751f53908c0b6ba72be61190ea))

### Bug Fixes

* **ui:** fix banners, hover, mark-read and account setup behaviour ([#128](https://github.com/unicornops/zmail/issues/128)) ([5108dc4](https://github.com/unicornops/zmail/commit/5108dc4228ebe5c4da978e117f9eda8afe795662))
* **ui:** stop rows, panes and forms from overflowing ([#126](https://github.com/unicornops/zmail/issues/126)) ([bd26cfc](https://github.com/unicornops/zmail/commit/bd26cfc8cfe942a684339377930428ed7a3e229c))
* **ui:** remove debug text from the shell and fix plurals ([#124](https://github.com/unicornops/zmail/issues/124)) ([02ce9fa](https://github.com/unicornops/zmail/commit/02ce9faeb52c513f659444bbe962746050d8063d))
* **imap:** render HTML-only bodies through the sanitizer and detect remote content ([#123](https://github.com/unicornops/zmail/issues/123)) ([0e56fc3](https://github.com/unicornops/zmail/commit/0e56fc35d1a2889fb917077089a738772ed3ace1))

## [0.9.0](https://github.com/unicornops/zmail/compare/v0.8.1...v0.9.0) (2026-10-15)


### Features

* warn about lookalike sender domains and Reply-To mismatches ([#108](https://github.com/unicornops/zmail/issues/108)) ([6967868](https://github.com/unicornops/zmail/commit/69678681624b04fb09cf62bb93d1ba7964d7bfef))
* keep a crash log for GUI launches ([#105](https://github.com/unicornops/zmail/issues/105)) ([f5f38d1](https://github.com/unicornops/zmail/commit/f5f38d1b0512c8dd9b3bb545b91bbf82b3534fc2))

### Bug Fixes

* **release:** configure git identity before complete-mode commits ([#110](https://github.com/unicornops/zmail/issues/110)) ([1228b66](https://github.com/unicornops/zmail/commit/1228b6626193d223eecb83a5f3b9c2df90d9b57d))
* **deps:** bump the mail and TLS stacks past the 2026 advisories ([#104](https://github.com/unicornops/zmail/issues/104)) ([636555a](https://github.com/unicornops/zmail/commit/636555a368b98804859a88f1447a7185ce9fb994))

## [0.8.1](https://github.com/unicornops/zmail/compare/v0.8.0...v0.8.1) (2026-10-14)


### Bug Fixes

* **release:** cover workspace commits outside zmail-app in release PRs ([#102](https://github.com/unicornops/zmail/issues/102)) ([9a3c043](https://github.com/unicornops/zmail/commit/9a3c043f761ccab6f9db59e63912af38826aa7ed))

## [0.8.0](https://github.com/unicornops/zmail/compare/v0.7.1...v0.8.0) (2026-10-12)


### Features

* **release:** app icon and Shuffleboard-style DMG with versioned filename ([#100](https://github.com/unicornops/zmail/issues/100)) ([50c1739](https://github.com/unicornops/zmail/commit/50c1739a9ec086d561ed081965da3a37c82735fc))

## [0.7.1](https://github.com/unicornops/zmail/compare/v0.7.0...v0.7.1) (2026-10-11)


### Bug Fixes

* run IMAP body hydration on the background Tokio runtime ([#98](https://github.com/unicornops/zmail/issues/98)) ([afc4493](https://github.com/unicornops/zmail/commit/afc4493c358bd2bb98f08848f74781134199bd48))

## [0.7.0](https://github.com/unicornops/zmail/compare/v0.6.0...v0.7.0) (2026-10-10)


### Features

* cache eviction with configurable per-account disk budgets ([#96](https://github.com/unicornops/zmail/issues/96)) ([ce2464d](https://github.com/unicornops/zmail/commit/ce2464de62ded724a265e649fc486ea145148b67))
* signed, notarized macOS DMG for releases ([#94](https://github.com/unicornops/zmail/issues/94)) ([766b4f0](https://github.com/unicornops/zmail/commit/766b4f0a2fb4c9cf422e507b62a5bd4654351b18))

### Bug Fixes

* **ci:** retry harness image pulls and support Docker Hub credentials ([#95](https://github.com/unicornops/zmail/issues/95)) ([0850318](https://github.com/unicornops/zmail/commit/0850318feaa945d72aeb1b0a2c3802ea1329e839))

## [0.6.0](https://github.com/unicornops/zmail/compare/v0.5.0...v0.6.0) (2026-10-09)


### Features

* **jmap:** authenticate with OAuth2 bearer tokens ([#86](https://github.com/unicornops/zmail/issues/86)) ([2720f52](https://github.com/unicornops/zmail/commit/2720f52b60256dca16d7f358c31a432152ce5c74))

## [0.5.0](https://github.com/unicornops/zmail/compare/v0.4.0...v0.5.0) (2026-10-09)


### Features

* JMAP attachment blobs in the reader, and a required release changelog audit ([#85](https://github.com/unicornops/zmail/issues/85)) ([8bdf250](https://github.com/unicornops/zmail/commit/8bdf250f3ea1a5fb7753a9cbf31522adb659808d))
* OAuth2 token lifecycle, settings-dialog setup, and searchable hydrated bodies ([#83](https://github.com/unicornops/zmail/issues/83)) ([fa265c4](https://github.com/unicornops/zmail/commit/fa265c42af7643cd6cda0362c7f947c58ea4ccfd))

### Bug Fixes

* **release:** match cargo-wix's hyphenated MSI output name ([#82](https://github.com/unicornops/zmail/issues/82)) ([60c77bf](https://github.com/unicornops/zmail/commit/60c77bf74e7c7ad8e6aaba689344f921dde973f6))

## [0.4.0](https://github.com/unicornops/zmail/compare/v0.3.0...v0.4.0) (2026-10-08)


### Features

* **auth:** run the OAuth2 browser flow with PKCE and provider presets ([#73](https://github.com/unicornops/zmail/issues/73)) ([fb46145c84cacf7adb41c0ff167124522337863c](https://github.com/unicornops/zmail/commit/fb46145c84cacf7adb41c0ff167124522337863c))
* hydrate message bodies lazily on open ([#66](https://github.com/unicornops/zmail/issues/66)) ([1fb3b4a55d83a9881ad49fb74cc753c49bf44b05](https://github.com/unicornops/zmail/commit/1fb3b4a55d83a9881ad49fb74cc753c49bf44b05)), closes [#46](https://github.com/unicornops/zmail/issues/46)
* persist and save received attachments ([#64](https://github.com/unicornops/zmail/issues/64)) ([33fac5b5530ef034f50e38a6abe27a2a411872d8](https://github.com/unicornops/zmail/commit/33fac5b5530ef034f50e38a6abe27a2a411872d8)), closes [#43](https://github.com/unicornops/zmail/issues/43)
* raise OS notifications for new mail ([#65](https://github.com/unicornops/zmail/issues/65)) ([5680334d43424a9be45a18992af3d9b6c753637a](https://github.com/unicornops/zmail/commit/5680334d43424a9be45a18992af3d9b6c753637a)), closes [#44](https://github.com/unicornops/zmail/issues/44)
* **release:** build a Windows MSI installer and scaffold opt-in signing ([#74](https://github.com/unicornops/zmail/issues/74)) ([a16b00ac62da5ae252e22c694e16f1deb08799ba](https://github.com/unicornops/zmail/commit/a16b00ac62da5ae252e22c694e16f1deb08799ba))
* sync flags through CONDSTORE CHANGEDSINCE ([#72](https://github.com/unicornops/zmail/issues/72)) ([7cf67d863adc93080476cb1bf3563b84275a47d2](https://github.com/unicornops/zmail/commit/7cf67d863adc93080476cb1bf3563b84275a47d2)), closes [#45](https://github.com/unicornops/zmail/issues/45)

### Bug Fixes

* **deps:** bump rand to the RUSTSEC-2026-0097/0186 patched releases ([#75](https://github.com/unicornops/zmail/issues/75)) ([f0b407411633eb530c1b4bd72c1a238ad1409df5](https://github.com/unicornops/zmail/commit/f0b407411633eb530c1b4bd72c1a238ad1409df5))
* **release:** pin zmail-notify to a concrete version for release-please ([#76](https://github.com/unicornops/zmail/issues/76)) ([e94d0293e7f8ab4c697d9fd38160f18b6ecdc22a](https://github.com/unicornops/zmail/commit/e94d0293e7f8ab4c697d9fd38160f18b6ecdc22a))
* **release:** repair the Linux deb asset path and screenshot GPU stack ([#70](https://github.com/unicornops/zmail/issues/70)) ([4bac30f0086adf4ef5f806eff9b7492844bb0d0a](https://github.com/unicornops/zmail/commit/4bac30f0086adf4ef5f806eff9b7492844bb0d0a))
* **release:** repair the WiX binary path and rewind the 0.4.0 cut ([#77](https://github.com/unicornops/zmail/issues/77)) ([ec793f6d01bfa07ec82c5dcccdcdb9b6606781fc](https://github.com/unicornops/zmail/commit/ec793f6d01bfa07ec82c5dcccdcdb9b6606781fc))
* **release:** use legal WiX identifiers and split the shortcut group ([#79](https://github.com/unicornops/zmail/issues/79)) ([749489b43f1962ae4a1fb64fe8f8cfd8091a3f13](https://github.com/unicornops/zmail/commit/749489b43f1962ae4a1fb64fe8f8cfd8091a3f13))
## [0.3.0](https://github.com/unicornops/zmail/compare/v0.2.0...v0.3.0) (2026-10-07)


### Features

* add a store benchmark harness ([#59](https://github.com/unicornops/zmail/issues/59)) ([017d3ba8f9332dc20187fc9025a3d45df59001b5](https://github.com/unicornops/zmail/commit/017d3ba8f9332dc20187fc9025a3d45df59001b5)), closes [#51](https://github.com/unicornops/zmail/issues/51)
* **auth:** support IMAP XOAUTH2 bearer-token authentication ([#63](https://github.com/unicornops/zmail/issues/63)) ([5bb0f8615fe84593eac416290582dd3e846efa27](https://github.com/unicornops/zmail/commit/5bb0f8615fe84593eac416290582dd3e846efa27))
* dead-letter write-back operations after five failures ([#56](https://github.com/unicornops/zmail/issues/56)) ([e0912cc4bbb879b73d6166cb4a413d63028818e0](https://github.com/unicornops/zmail/commit/e0912cc4bbb879b73d6166cb4a413d63028818e0)), closes [#48](https://github.com/unicornops/zmail/issues/48)
* define the deletion policy with an explicit empty-trash command ([#58](https://github.com/unicornops/zmail/issues/58)) ([5189d0fc0f6d8c9abf394255cee6928fe594fffb](https://github.com/unicornops/zmail/commit/5189d0fc0f6d8c9abf394255cee6928fe594fffb)), closes [#49](https://github.com/unicornops/zmail/issues/49)
* group IMAP messages into conversations ([#54](https://github.com/unicornops/zmail/issues/54)) ([75d969ce1f8ab3a0f4d4b11b66bebccc72961317](https://github.com/unicornops/zmail/commit/75d969ce1f8ab3a0f4d4b11b66bebccc72961317)), closes [#41](https://github.com/unicornops/zmail/issues/41)
* handle mailto URLs on launch ([#55](https://github.com/unicornops/zmail/issues/55)) ([3438ef2063f8daf697ccdbfd87e0b2b922f9e7eb](https://github.com/unicornops/zmail/commit/3438ef2063f8daf697ccdbfd87e0b2b922f9e7eb)), closes [#42](https://github.com/unicornops/zmail/issues/42)
* push new mail for JMAP accounts through the event source ([#62](https://github.com/unicornops/zmail/issues/62)) ([84fac604c74cc3f43acb46ff71c3f44e572dd9dc](https://github.com/unicornops/zmail/commit/84fac604c74cc3f43acb46ff71c3f44e572dd9dc)), closes [#47](https://github.com/unicornops/zmail/issues/47)
* skip unchanged threads during snapshot merges ([#61](https://github.com/unicornops/zmail/issues/61)) ([9d2511aa001351164a9715a88207af513fb1e6df](https://github.com/unicornops/zmail/commit/9d2511aa001351164a9715a88207af513fb1e6df)), closes [#60](https://github.com/unicornops/zmail/issues/60)
* watch accounts added after launch ([#52](https://github.com/unicornops/zmail/issues/52)) ([eb072ffdf63672f09d8bfe4ab45eb96331def7ce](https://github.com/unicornops/zmail/commit/eb072ffdf63672f09d8bfe4ab45eb96331def7ce)), closes [#38](https://github.com/unicornops/zmail/issues/38)

### Bug Fixes

* build the Linux package with cargo-deb's release defaults ([#37](https://github.com/unicornops/zmail/issues/37)) ([afb7b65f03b31d3b85fcb315778ac4c60c3e0042](https://github.com/unicornops/zmail/commit/afb7b65f03b31d3b85fcb315778ac4c60c3e0042))
* capture the release screenshot from the release workflow chain ([#53](https://github.com/unicornops/zmail/issues/53)) ([f51d0701195748a3d6f47afe627f3a4dafd181a6](https://github.com/unicornops/zmail/commit/f51d0701195748a3d6f47afe627f3a4dafd181a6)), closes [#39](https://github.com/unicornops/zmail/issues/39)

## [0.2.0](https://github.com/unicornops/zmail/compare/v0.1.0...v0.2.0) (2026-10-07)


### Features

* add release workflow to capture Linux screenshot ([#10](https://github.com/unicornops/zmail/issues/10)) ([eb523ed](https://github.com/unicornops/zmail/commit/eb523edaa6daa019492d0b613caf2f7681910007))
* add store backup and mbox export ([#34](https://github.com/unicornops/zmail/issues/34)) ([27a47fe](https://github.com/unicornops/zmail/commit/27a47fe23f20696714e29e849cb06d614b41b3a3))
* package installable Linux and macOS release artifacts ([#35](https://github.com/unicornops/zmail/issues/35)) ([a94562a](https://github.com/unicornops/zmail/commit/a94562a76b75a104bdf350c9826ec35d8126cf10)), closes [#18](https://github.com/unicornops/zmail/issues/18)
* persist the local store in the platform data directory ([#23](https://github.com/unicornops/zmail/issues/23)) ([5c21281](https://github.com/unicornops/zmail/commit/5c212811d9aa00fdf6cc2abfcdff9ab8811d6af0)), closes [#13](https://github.com/unicornops/zmail/issues/13)
* **ui:** overhaul desktop shell with icons, theming, and modular views ([#11](https://github.com/unicornops/zmail/issues/11)) ([713d8f5](https://github.com/unicornops/zmail/commit/713d8f5fbe2a93531ca5b305217c2d3799c2a506))
* watch for new mail in the background ([#32](https://github.com/unicornops/zmail/issues/32)) ([2408d01](https://github.com/unicornops/zmail/commit/2408d01979b337054ee3adb8de68f6a46d7163bd)), closes [#17](https://github.com/unicornops/zmail/issues/17)
* **zmail-app:** add launch-imap-fixture command for harness ([eb523ed](https://github.com/unicornops/zmail/commit/eb523edaa6daa019492d0b613caf2f7681910007))
* **zmail-ui:** set window title for release screenshot ([eb523ed](https://github.com/unicornops/zmail/commit/eb523edaa6daa019492d0b613caf2f7681910007))

## 0.1.0 (2026-03-26)


### Features

* bootstrap zmail desktop email client ([f5f9a2c](https://github.com/unicornops/zmail/commit/f5f9a2c74b9ca37e187bfa1764009f65e72c8a59))


### Bug Fixes

* set explicit version 0.0.1 for all crates ([#2](https://github.com/unicornops/zmail/issues/2)) ([be6bbc7](https://github.com/unicornops/zmail/commit/be6bbc7a39f61c70f2a216fb78daeaf503cbff82))
