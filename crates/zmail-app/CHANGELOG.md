# Changelog

## [0.16.2](https://github.com/unicornops/zmail/compare/v0.16.1...v0.16.2) (2026-10-22)


### Bug Fixes

* **proton:** sign in with Bridge's app version, keeping a User-Agent naming Zmail ([#276](https://github.com/unicornops/zmail/issues/276)) ([c282c8f](https://github.com/unicornops/zmail/commit/c282c8fb15dad4b4d291e38ac1f9f713c36a9118))

## [0.16.1](https://github.com/unicornops/zmail/compare/v0.16.0...v0.16.1) (2026-10-22)


### Bug Fixes

* **proton:** send an app version Proton accepts, with a User-Agent naming Zmail ([#274](https://github.com/unicornops/zmail/issues/274)) ([5493207](https://github.com/unicornops/zmail/commit/54932075afb66c3c302d3dda4b356525850e8959))

## [0.16.0](https://github.com/unicornops/zmail/compare/v0.15.0...v0.16.0) (2026-10-21)


### Features

* **proton:** offer Proton Mail in release builds, marked experimental ([#273](https://github.com/unicornops/zmail/issues/273)) ([5ce3905](https://github.com/unicornops/zmail/commit/5ce390531f02d0d1a795c067736469533b4f86b5))
* **proton:** add Proton Mail to onboarding behind the proton feature ([#267](https://github.com/unicornops/zmail/issues/267)) ([fc0346f](https://github.com/unicornops/zmail/commit/fc0346f78ec1994d3bfe207a5a46b6679726b73e))
* **store:** rotate the store key, with a lock against concurrent rewrites ([#265](https://github.com/unicornops/zmail/issues/265)) ([245fba7](https://github.com/unicornops/zmail/commit/245fba7d6e66f13b8702be7d18052ca6c73f5abc))
* **ui:** unlock the encrypted store before the workspace opens ([#263](https://github.com/unicornops/zmail/issues/263)) ([fd1c63f](https://github.com/unicornops/zmail/commit/fd1c63fea47d64087753526155f08285e253764f))
* **store:** encrypt attachment files and short-lived opened copies ([#261](https://github.com/unicornops/zmail/issues/261)) ([37b1b7a](https://github.com/unicornops/zmail/commit/37b1b7abf239589fd7883f81e6727164acb64468))
* **proton:** send to outside recipients by contact settings ([#264](https://github.com/unicornops/zmail/issues/264)) ([4539317](https://github.com/unicornops/zmail/commit/4539317d062459b2160a9c22bc92018eac14f269))
* **proton:** send mail through Proton from the outbox ([#262](https://github.com/unicornops/zmail/issues/262)) ([ef183b7](https://github.com/unicornops/zmail/commit/ef183b78a3efcf5025ae3b34f8061f0beba27bc1))
* **proton:** write local mailbox changes back to Proton with Bridge's rules ([#259](https://github.com/unicornops/zmail/issues/259)) ([c61ed90](https://github.com/unicornops/zmail/commit/c61ed9020bae842962b23178aebf0c0b9ff23769))
* **onboarding:** guide Gmail users to their own Google OAuth client ([#272](https://github.com/unicornops/zmail/issues/272)) ([d2a5de5](https://github.com/unicornops/zmail/commit/d2a5de5e8c1bd8f90ae8a0cb9be4a4c39d3f3e7a))
* **proton:** follow the Proton event stream for incremental sync ([#258](https://github.com/unicornops/zmail/issues/258)) ([dee55c9](https://github.com/unicornops/zmail/commit/dee55c98ac518f0064cd1fe53f19ab2ec3e68987))
* **store:** encrypt the local store with SQLCipher, migrating plaintext stores ([#260](https://github.com/unicornops/zmail/issues/260)) ([52b02bd](https://github.com/unicornops/zmail/commit/52b02bd65adb58b01dcce13dd685bd97b4d61e09))
* **proton:** hydrate Proton messages by decrypting and rebuilding RFC 822 ([#257](https://github.com/unicornops/zmail/issues/257)) ([98f0bb4](https://github.com/unicornops/zmail/commit/98f0bb41c4250cbd8ef908be1410556de533c4f7))
* **proton:** read sync of labels as mailboxes and message metadata ([#253](https://github.com/unicornops/zmail/issues/253)) ([db8735a](https://github.com/unicornops/zmail/commit/db8735a8162647c0370a6f0449aa071cf42054d8))
* **security:** store encryption key management (ADR 0005) ([#256](https://github.com/unicornops/zmail/issues/256)) ([ed4c51d](https://github.com/unicornops/zmail/commit/ed4c51daa3c6e86636057b0f55a71b91fc4f00fa))
* **proton:** add the zmail-proton adapter behind zmail-sync's proton feature ([#252](https://github.com/unicornops/zmail/issues/252)) ([336d81b](https://github.com/unicornops/zmail/commit/336d81b472e3f3ca143d54fbc2ef0766ed38c84d))

### Bug Fixes

* **auth:** keep the client secret out of the browser, ask Microsoft 365 for SMTP.Send ([#271](https://github.com/unicornops/zmail/issues/271)) ([c5e0698](https://github.com/unicornops/zmail/commit/c5e06988eba868d8d1f5c1ae31ac1f7a9c594bca))

## [0.15.0](https://github.com/unicornops/zmail/compare/v0.14.0...v0.15.0) (2026-10-21)


### Features

* **proton:** key salts, user and address key unlocking, two-password mode ([#246](https://github.com/unicornops/zmail/issues/246)) ([17ac086](https://github.com/unicornops/zmail/commit/17ac08629d95bb5123f4b1ba06a812fed03367f8))
* **ui:** 24px minimum pointer targets, checked by walking each view ([#231](https://github.com/unicornops/zmail/issues/231)) ([aa8ace5](https://github.com/unicornops/zmail/commit/aa8ace51b3000264f755b3b9987284f56e7f9384))
* **ui:** read reduced motion on Windows and through AppKit on macOS ([#248](https://github.com/unicornops/zmail/issues/248)) ([d7671f0](https://github.com/unicornops/zmail/commit/d7671f0075c1de74bd636a15925e56f5401550b6))
* **ui:** text size setting that scales every window's rem size ([#228](https://github.com/unicornops/zmail/issues/228)) ([ee06d4b](https://github.com/unicornops/zmail/commit/ee06d4b961019692143a178bb24d53f6979b1c72))
* **proton:** SRP login, TOTP 2FA, logout and human verification ([#245](https://github.com/unicornops/zmail/issues/245)) ([7cf5d45](https://github.com/unicornops/zmail/commit/7cf5d45b7eb36ba6688ac12a9ace24005ae7ce35))
* **ui:** follow Windows contrast themes and macOS Increase Contrast ([#232](https://github.com/unicornops/zmail/issues/232)) ([c52f9c9](https://github.com/unicornops/zmail/commit/c52f9c9836f0ff587f85f9eb20dbc54b46bea570))
* **proton:** add the zmail-proton-api client core ([#244](https://github.com/unicornops/zmail/issues/244)) ([42d827f](https://github.com/unicornops/zmail/commit/42d827f87b2d75e4c1930329c0871a2732b0df52))
* **ui:** announce toasts, sync problems, search counts and errors ([#230](https://github.com/unicornops/zmail/issues/230)) ([3e16131](https://github.com/unicornops/zmail/commit/3e161319d19fff6e71bdeef4c67b561f202a61c2))
* **ui:** give every component and view a screen-reader role and name ([#229](https://github.com/unicornops/zmail/issues/229)) ([4a1e874](https://github.com/unicornops/zmail/commit/4a1e8743dd524e1d5ebe4cf8785bf77f26f3009d))
* **ui:** reduced motion from the system setting, with a Settings override ([#227](https://github.com/unicornops/zmail/issues/227)) ([fd76bf6](https://github.com/unicornops/zmail/commit/fd76bf66175b4b03bd63577c1eb1ce76a8808268))

## [0.14.0](https://github.com/unicornops/zmail/compare/v0.13.0...v0.14.0) (2026-10-20)


### Features

* **ui:** Settings → Keyboard with the keymap choice and a searchable shortcut list ([#222](https://github.com/unicornops/zmail/issues/222)) ([6a8eeb4](https://github.com/unicornops/zmail/commit/6a8eeb4d99cfc9c3edc24e67ded1ce305b135c2a))
* **ui:** Settings → Privacy with a sender-warning level ([#218](https://github.com/unicornops/zmail/issues/218)) ([1992f3b](https://github.com/unicornops/zmail/commit/1992f3b510c075f814e580b1a0867b2683e9f6c0))
* **ui:** Settings → Accounts with colour, cache budget, re-auth and remove ([#217](https://github.com/unicornops/zmail/issues/217)) ([f81c1e7](https://github.com/unicornops/zmail/commit/f81c1e79012da3be91f8087d775a25b833a5913d))
* **autodiscover:** look up SRV and MX records with hickory-resolver ([#221](https://github.com/unicornops/zmail/issues/221)) ([a480c0e](https://github.com/unicornops/zmail/commit/a480c0e387b6691cde20368de11da05e2c280576))
* **ui:** Settings window with Appearance, Sending and About & Diagnostics ([#211](https://github.com/unicornops/zmail/issues/211)) ([e3798c5](https://github.com/unicornops/zmail/commit/e3798c5193587b106de592e37e4098ee6a53a53e))
* **ui:** keyboard shortcut cheat sheet ([#215](https://github.com/unicornops/zmail/issues/215)) ([68a3166](https://github.com/unicornops/zmail/commit/68a316633b1963dfa7cd85c40cd767dcdd967b33))
* **keymap:** optional Gmail keymap ([#212](https://github.com/unicornops/zmail/issues/212)) ([a80ac70](https://github.com/unicornops/zmail/commit/a80ac70757db9cb50d4eaec099d84a84cffd9359))
* **ui:** command palette on Secondary+K ([#210](https://github.com/unicornops/zmail/issues/210)) ([74fa8d7](https://github.com/unicornops/zmail/commit/74fa8d7ecf4dd442f119698b6593717fc25544e3))
* **search:** filter chips, highlighted matches and paged results ([#213](https://github.com/unicornops/zmail/issues/213)) ([02504c7](https://github.com/unicornops/zmail/commit/02504c7f9d4f6bc1165a51dd1e8fc8ed922f907b))
* **onboarding:** first-run onboarding and the Add Account window ([#216](https://github.com/unicornops/zmail/issues/216)) ([10ed407](https://github.com/unicornops/zmail/commit/10ed40783ace019954030ef7d86e224553e56f95))
* **search:** live search as you type, with a clear button and keyboard flow ([#209](https://github.com/unicornops/zmail/issues/209)) ([bcf2dfd](https://github.com/unicornops/zmail/commit/bcf2dfd4e3840f0701c7913c316b8596b3e840a6))
* **store:** per-account colour override ([#207](https://github.com/unicornops/zmail/issues/207)) ([09113a2](https://github.com/unicornops/zmail/commit/09113a2cf641c856df52fb725f4333b49c154e46))
* **keymap:** bind the saved keymap and switch it at runtime ([#206](https://github.com/unicornops/zmail/issues/206)) ([7c2cd91](https://github.com/unicornops/zmail/commit/7c2cd913bc5986a7a051b7e495dff91f8c3fbe7f))
* **search:** queue unknown-hit syncs behind a running sync with request_sync ([#214](https://github.com/unicornops/zmail/issues/214)) ([a5863f8](https://github.com/unicornops/zmail/commit/a5863f8bea272c38dc57362d1e35599fa6604300))
* **autodiscover:** add the zmail-autodiscover crate ([#208](https://github.com/unicornops/zmail/issues/208)) ([d19c261](https://github.com/unicornops/zmail/commit/d19c2615b59f3a39ecb665ea9149858ed9950c5a))

## [0.13.0](https://github.com/unicornops/zmail/compare/v0.12.0...v0.13.0) (2026-10-19)


### Features

* **compose:** say when mail queued in the last session has been sent ([#204](https://github.com/unicornops/zmail/issues/204)) ([5d1300a](https://github.com/unicornops/zmail/commit/5d1300a854df9b3b6552745da455fa7f357c275e))
* **compose:** inline reply below the conversation, with a pop-out into its own window ([#202](https://github.com/unicornops/zmail/issues/202)) ([2946501](https://github.com/unicornops/zmail/commit/2946501e30fff1d302e86b6d35c4036c6c726a1a))
* **compose:** undo send, holding the outbox job for undo_send_delay ([#200](https://github.com/unicornops/zmail/issues/200)) ([dc6732d](https://github.com/unicornops/zmail/commit/dc6732d6270c4b413c7e08809e22feaabdcc8647))
* **ui:** attachment cards with Open, Save As and Save All ([#194](https://github.com/unicornops/zmail/issues/194)) ([16ee19f](https://github.com/unicornops/zmail/commit/16ee19fd957604ca339ea07c922a3c0232b4f504))
* **ui:** tiered sender-safety banners and a remote-content notice in the reader ([#191](https://github.com/unicornops/zmail/issues/191)) ([4514e72](https://github.com/unicornops/zmail/commit/4514e7273356db3c9f5285dc6c05f056854d2a23))
* **compose:** new mail in its own window, file-picker and drag-and-drop attachments ([#199](https://github.com/unicornops/zmail/issues/199)) ([534ac3c](https://github.com/unicornops/zmail/commit/534ac3c4ea29b5e795e7c916fd9090d3ee60bde8))
* **compose:** recipient chips with autocomplete, Cc/Bcc toggles and a From menu ([#195](https://github.com/unicornops/zmail/issues/195)) ([fee3021](https://github.com/unicornops/zmail/commit/fee30216e9a5fc9e6a41ed4731f7f4f701df78b4))
* **store:** frecency-ranked address history for recipient autocomplete ([#192](https://github.com/unicornops/zmail/issues/192)) ([ebf13de](https://github.com/unicornops/zmail/commit/ebf13deaae2b6a5180b062868d84627f344369d7))
* **ui:** readable, selectable message bodies with checked links ([#197](https://github.com/unicornops/zmail/issues/197)) ([1c905d2](https://github.com/unicornops/zmail/commit/1c905d2c9bf1ede130eb157c046c082172cd7d99))
* **ui:** message headers with sender menu, recipients, Reply and More ([#193](https://github.com/unicornops/zmail/issues/193)) ([52b1c0e](https://github.com/unicornops/zmail/commit/52b1c0e7c1f9a4e9c6e32b6a8eb435ca974a89ce))
* **ui:** render the conversation as a list with collapsed read messages ([#190](https://github.com/unicornops/zmail/issues/190)) ([75e8e7f](https://github.com/unicornops/zmail/commit/75e8e7f4c0d68cb3295c1ea7434a2faac30ca5b1))

### Bug Fixes

* **reader:** warn before opening web pages and SVGs, and prune opened copies at startup ([#203](https://github.com/unicornops/zmail/issues/203)) ([3d6f35e](https://github.com/unicornops/zmail/commit/3d6f35e1920f9410081b3441e412cfa643cef4f9))
* **compose:** autosave a second after typing pauses, never on every keystroke ([#189](https://github.com/unicornops/zmail/issues/189)) ([3f8b1cd](https://github.com/unicornops/zmail/commit/3f8b1cdcc27dc1ba4f08d8f45728058332cb25f5))

## [0.12.0](https://github.com/unicornops/zmail/compare/v0.11.0...v0.12.0) (2026-10-19)


### Features

* **search:** scope searches to the folder, the account or every account ([#184](https://github.com/unicornops/zmail/issues/184)) ([a2543b4](https://github.com/unicornops/zmail/commit/a2543b407733014adefdc45affc2d830c4c8b624))
* **ui:** account colours on list rows and density in the sidebar ([#182](https://github.com/unicornops/zmail/issues/182)) ([df9729e](https://github.com/unicornops/zmail/commit/df9729eff1f543694ac7e2e0ee07df429b82d3da))
* **ui:** skeleton rows, empty states and plain-word outbox rows ([#180](https://github.com/unicornops/zmail/issues/180)) ([4bc5a58](https://github.com/unicornops/zmail/commit/4bc5a5896ee7a064d5fb7f2cb3fee411119e98a7))
* **ui:** list header with filters and hover quick actions ([#177](https://github.com/unicornops/zmail/issues/177)) ([a7a0ce7](https://github.com/unicornops/zmail/commit/a7a0ce736fc1d561f0958d273eeefbd914a22fb4))
* **ui:** drag conversations from the list onto sidebar folders ([#183](https://github.com/unicornops/zmail/issues/183)) ([605814a](https://github.com/unicornops/zmail/commit/605814a30ffcef520afc89b1c78b7b656ff309b1))
* **ui:** right-click menu on message list rows ([#178](https://github.com/unicornops/zmail/issues/178)) ([224361a](https://github.com/unicornops/zmail/commit/224361a2fb4464b3d8cf11fcb436bc53aa0f97d6))
* **ui:** multi-select in the message list, with a bulk action bar and a Move to picker ([#176](https://github.com/unicornops/zmail/issues/176)) ([feeadc4](https://github.com/unicornops/zmail/commit/feeadc48f6668567f6d80cae0b81a91a56947baf))
* **ui:** sidebar with unified inbox, favourites and folder trees ([#179](https://github.com/unicornops/zmail/issues/179)) ([2e7a16d](https://github.com/unicornops/zmail/commit/2e7a16dbf8353cf088ba9e374ce0e42a4c5afbb8))
* **ui:** comfortable and compact message rows ([#175](https://github.com/unicornops/zmail/issues/175)) ([2d53336](https://github.com/unicornops/zmail/commit/2d533367ce3de3dff5d259baa9e63c96c3d21858))
* **store:** unified inbox and flagged thread lists across accounts ([#174](https://github.com/unicornops/zmail/issues/174)) ([b83e915](https://github.com/unicornops/zmail/commit/b83e91520b3aa440b711ae14d392897d8c48a003))
* **ui:** undo toast for mail changes, with exact inverses that fold into the write-back queue ([#173](https://github.com/unicornops/zmail/issues/173)) ([46fa375](https://github.com/unicornops/zmail/commit/46fa375503f623229877c02a296ccf19e49a0bd7))
* **store:** mailbox roles and hierarchy in the mailbox model ([#172](https://github.com/unicornops/zmail/issues/172)) ([dde26f3](https://github.com/unicornops/zmail/commit/dde26f3f3825156f163aef0ec6e5636e444df5f3))

## [0.11.0](https://github.com/unicornops/zmail/compare/v0.10.0...v0.11.0) (2026-10-18)


### Features

* **sync:** reconnect watchers at once after sleep or a network change ([#161](https://github.com/unicornops/zmail/issues/161)) ([110faa4](https://github.com/unicornops/zmail/commit/110faa48115ace05081d988e87a4b56abc83164d))
* **sync:** live IMAP updates for every folder with NOTIFY or an IDLE pool ([#158](https://github.com/unicornops/zmail/issues/158)) ([95479e3](https://github.com/unicornops/zmail/commit/95479e3bbc02bd52e082576058c5e6c1c13f07a8))
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
