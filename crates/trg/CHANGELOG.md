# Changelog

## [0.10.0](https://github.com/TrogonStack/rusty-monorepo/compare/trg@v0.9.0...trg@v0.10.0) (2026-09-13)


### Features

* **trg:** A case needs a directory before it needs more fields ([#146](https://github.com/TrogonStack/rusty-monorepo/issues/146)) ([dd3d8d3](https://github.com/TrogonStack/rusty-monorepo/commit/dd3d8d3141c662ceafd683c7e1b4140994f2233e))
* **trg:** A harness must declare what it cannot do ([#145](https://github.com/TrogonStack/rusty-monorepo/issues/145)) ([ea5485a](https://github.com/TrogonStack/rusty-monorepo/commit/ea5485a2ec40c34db123dcebec53071a23582e34))
* **trg:** A judge asked once cannot say whether it is sure ([#138](https://github.com/TrogonStack/rusty-monorepo/issues/138)) ([6624297](https://github.com/TrogonStack/rusty-monorepo/commit/662429717c483ecf2eef3e2ba57977116788ca9e))
* **trg:** A pass that names no scenario gets a baseline to read against ([#137](https://github.com/TrogonStack/rusty-monorepo/issues/137)) ([e850e7b](https://github.com/TrogonStack/rusty-monorepo/commit/e850e7b1de99a6885bd6b96d127b085ceaf6ebe4))
* **trg:** Bound what one eval pass may spend ([#135](https://github.com/TrogonStack/rusty-monorepo/issues/135)) ([d8329d3](https://github.com/TrogonStack/rusty-monorepo/commit/d8329d3a5f9fc91801436c65d9fbf7c8cdc6963d))
* **trg:** Declare checks as data and report what a harness cannot answer ([#109](https://github.com/TrogonStack/rusty-monorepo/issues/109)) ([f4261dd](https://github.com/TrogonStack/rusty-monorepo/commit/f4261ddd7a9ea85594a818537053c51a62380558))
* **trg:** Draw every eval cell more than once by default ([#129](https://github.com/TrogonStack/rusty-monorepo/issues/129)) ([7202f91](https://github.com/TrogonStack/rusty-monorepo/commit/7202f9147fbedb3fd4afe2034f9d19a1fe6b78b8))
* **trg:** Execute more than one eval run at a time ([#128](https://github.com/TrogonStack/rusty-monorepo/issues/128)) ([8c2659c](https://github.com/TrogonStack/rusty-monorepo/commit/8c2659c12b598817574a8095bd7759f0390a71f3))
* **trg:** Let a case ask whether the skill gets reached for at all ([#123](https://github.com/TrogonStack/rusty-monorepo/issues/123)) ([6be74b2](https://github.com/TrogonStack/rusty-monorepo/commit/6be74b204eb45110ee462c1706187b5422379ba6))
* **trg:** Let a case say what state it is asking about ([#134](https://github.com/TrogonStack/rusty-monorepo/issues/134)) ([f0c7a5a](https://github.com/TrogonStack/rusty-monorepo/commit/f0c7a5a57d4d147fd25bddb35867febdeabaf213))
* **trg:** Let a case state that a tool must not be reached for ([#130](https://github.com/TrogonStack/rusty-monorepo/issues/130)) ([3656f18](https://github.com/TrogonStack/rusty-monorepo/commit/3656f18277330ec9ee5e42474666e76dd63ffa16))
* **trg:** Let the judge address any OpenAI-compatible endpoint ([#108](https://github.com/TrogonStack/rusty-monorepo/issues/108)) ([ef07863](https://github.com/TrogonStack/rusty-monorepo/commit/ef078630b0ca8b3568a5c4783578deec4614c454))
* **trg:** Observe every harness's tool calls and report what left the workspace ([#116](https://github.com/TrogonStack/rusty-monorepo/issues/116)) ([728d658](https://github.com/TrogonStack/rusty-monorepo/commit/728d6580978f2b485ada38699f16f5815d5deb04))
* **trg:** Read every harness transcript through one event vocabulary ([#107](https://github.com/TrogonStack/rusty-monorepo/issues/107)) ([a347293](https://github.com/TrogonStack/rusty-monorepo/commit/a3472936387881114383768acc0288a95b488c69))
* **trg:** Say which command a case expected, not just which tool ([#131](https://github.com/TrogonStack/rusty-monorepo/issues/131)) ([7cd19d2](https://github.com/TrogonStack/rusty-monorepo/commit/7cd19d27787096188094c93532a8395b0a5044b1))
* **trg:** Scope graders to the arm they can answer in ([#122](https://github.com/TrogonStack/rusty-monorepo/issues/122)) ([10caa5a](https://github.com/TrogonStack/rusty-monorepo/commit/10caa5a69f3b55d15f566a9df8e6f9a70ab0515e))
* **trg:** Select part of an eval suite per run ([#125](https://github.com/TrogonStack/rusty-monorepo/issues/125)) ([78a369c](https://github.com/TrogonStack/rusty-monorepo/commit/78a369c52e4adfabad7c5f46c6c2a57792f385fc))


### Bug Fixes

* **trg:** A cached run must answer for the draw that asked ([#127](https://github.com/TrogonStack/rusty-monorepo/issues/127)) ([0f7b5e5](https://github.com/TrogonStack/rusty-monorepo/commit/0f7b5e50933d429fc1b250b3e0ed21c1439e043e))
* **trg:** A cached run must not answer for an eval case it never ran ([#113](https://github.com/TrogonStack/rusty-monorepo/issues/113)) ([872a9f2](https://github.com/TrogonStack/rusty-monorepo/commit/872a9f2d8489443a59f5d43bb0e888c2ff7dad81))
* **trg:** A failed run must not read as a skill that failed its assertions ([#115](https://github.com/TrogonStack/rusty-monorepo/issues/115)) ([8f23d65](https://github.com/TrogonStack/rusty-monorepo/commit/8f23d6507200f3826b8dc63ac8d9ffdb6dfcc776))
* **trg:** A manifest that names no version should not get the narrowest one ([#142](https://github.com/TrogonStack/rusty-monorepo/issues/142)) ([082f368](https://github.com/TrogonStack/rusty-monorepo/commit/082f3681a852a5ba888a798873f2caa21acdf38c))
* **trg:** A pass nobody can price should not report a total of zero ([#139](https://github.com/TrogonStack/rusty-monorepo/issues/139)) ([20312d8](https://github.com/TrogonStack/rusty-monorepo/commit/20312d8aa5987971928024b29ed16a9802cde1a9))
* **trg:** A pass-rate gate nothing was measured against must not report green ([#132](https://github.com/TrogonStack/rusty-monorepo/issues/132)) ([66e7f68](https://github.com/TrogonStack/rusty-monorepo/commit/66e7f6860fbb070a8812b3323554c6420f24f0d2))
* **trg:** A reused run must answer for the arm that asked ([#126](https://github.com/TrogonStack/rusty-monorepo/issues/126)) ([b568c8e](https://github.com/TrogonStack/rusty-monorepo/commit/b568c8e2b4d6f3b5ee59d29d90271ec8de0aa358))
* **trg:** A run inherits nothing by accident ([#119](https://github.com/TrogonStack/rusty-monorepo/issues/119)) ([8d49745](https://github.com/TrogonStack/rusty-monorepo/commit/8d49745b8535b89f238204aca479e0bf27c26229))
* **trg:** A run must not be able to read its own answer key ([#118](https://github.com/TrogonStack/rusty-monorepo/issues/118)) ([aac9bcd](https://github.com/TrogonStack/rusty-monorepo/commit/aac9bcd6e446a57e2f1a581174efc5b470752dcb))
* **trg:** A run where nothing could be scored has no pass rate ([#114](https://github.com/TrogonStack/rusty-monorepo/issues/114)) ([39af3b4](https://github.com/TrogonStack/rusty-monorepo/commit/39af3b482b238aafca0d524c3364132e4a3bee3b))
* **trg:** A run's permissions must come from the eval, not the machine ([#144](https://github.com/TrogonStack/rusty-monorepo/issues/144)) ([7cc7526](https://github.com/TrogonStack/rusty-monorepo/commit/7cc7526c1bc933e173c2e7171214667cc08f1f87))
* **trg:** A score that could have been copied says so ([#124](https://github.com/TrogonStack/rusty-monorepo/issues/124)) ([0ca1c52](https://github.com/TrogonStack/rusty-monorepo/commit/0ca1c5269e3d582870f16a80c21d90ed8b07fd73))
* **trg:** A timed out run must not leave agents running ([#120](https://github.com/TrogonStack/rusty-monorepo/issues/120)) ([a8188e9](https://github.com/TrogonStack/rusty-monorepo/commit/a8188e97f895f88a98daea2e72072e44a5af9ac4))
* **trg:** A version nobody reads is not a contract ([#141](https://github.com/TrogonStack/rusty-monorepo/issues/141)) ([9cfaa4d](https://github.com/TrogonStack/rusty-monorepo/commit/9cfaa4d39cad8706b2a5851b402cae09835e3c12))
* **trg:** Docs must not tell consumers to gate on a version that is gone ([#143](https://github.com/TrogonStack/rusty-monorepo/issues/143)) ([0cb60b0](https://github.com/TrogonStack/rusty-monorepo/commit/0cb60b0061baf23d9ae828ae6e96a3380d3c7b7a))
* **trg:** Stop eval verify and run reuse from misreporting results ([#106](https://github.com/TrogonStack/rusty-monorepo/issues/106)) ([46304a5](https://github.com/TrogonStack/rusty-monorepo/commit/46304a5aef117b44578f89e136f67b60d17f8bad))
* **trg:** Whether a bundle conforms cannot depend on how the suite scored ([#133](https://github.com/TrogonStack/rusty-monorepo/issues/133)) ([c11036a](https://github.com/TrogonStack/rusty-monorepo/commit/c11036a6419eeee40deed2cc2d077c3c1af27abe))

## [0.9.0](https://github.com/TrogonStack/rusty-monorepo/compare/trg@v0.8.2...trg@v0.9.0) (2026-09-10)


### Features

* **trg:** Address secrets in the vocabulary of the backend that holds them ([#104](https://github.com/TrogonStack/rusty-monorepo/issues/104)) ([6ed1fbc](https://github.com/TrogonStack/rusty-monorepo/commit/6ed1fbc9887cfaf7975b7efdbcf7dab83f4527b3))

## [0.8.2](https://github.com/TrogonStack/rusty-monorepo/compare/trg@v0.8.1...trg@v0.8.2) (2026-09-09)


### Bug Fixes

* **trg:** Migrate legacy oauth credential payloads instead of failing to read them ([#101](https://github.com/TrogonStack/rusty-monorepo/issues/101)) ([6858492](https://github.com/TrogonStack/rusty-monorepo/commit/68584926ceeb553252d99ffc6ebbdadd976d6895))

## [0.8.1](https://github.com/TrogonStack/rusty-monorepo/compare/trg@v0.8.0...trg@v0.8.1) (2026-09-09)


### Bug Fixes

* **trg:** Require an explicit 1Password account ([#98](https://github.com/TrogonStack/rusty-monorepo/issues/98)) ([c716cdb](https://github.com/TrogonStack/rusty-monorepo/commit/c716cdbf032ca2d1d6ac2844139bd52ebe7c43d5))

## [0.8.0](https://github.com/TrogonStack/rusty-monorepo/compare/trg@v0.7.0...trg@v0.8.0) (2026-09-07)


### Features

* **trg:** Add trg exec list and move the launch verb to trg exec run ([#95](https://github.com/TrogonStack/rusty-monorepo/issues/95)) ([572337b](https://github.com/TrogonStack/rusty-monorepo/commit/572337b40b01940cbea7cc43061f5352355b7f27))

## [0.7.0](https://github.com/TrogonStack/rusty-monorepo/compare/trg@v0.6.0...trg@v0.7.0) (2026-09-07)


### Features

* **trg:** Add a 1Password secrets backend ([#92](https://github.com/TrogonStack/rusty-monorepo/issues/92)) ([9f17427](https://github.com/TrogonStack/rusty-monorepo/commit/9f17427b4d0409c474954403109f466cdc6924f7))
* **trg:** Add trg exec to hand off to any command with resolved secrets ([#90](https://github.com/TrogonStack/rusty-monorepo/issues/90)) ([299396e](https://github.com/TrogonStack/rusty-monorepo/commit/299396ee06f781d5fa0514955cbfac10ab7bfcbe))
* **trg:** Compose exec env values from several sources ([#93](https://github.com/TrogonStack/rusty-monorepo/issues/93)) ([6ac946e](https://github.com/TrogonStack/rusty-monorepo/commit/6ac946e486bedd59536bee1925de15edcec75460))

## [0.6.0](https://github.com/TrogonStack/rusty-monorepo/compare/trg@v0.5.0...trg@v0.6.0) (2026-09-06)


### Features

* **trg:** Give every command the same --output-format text|json ([#88](https://github.com/TrogonStack/rusty-monorepo/issues/88)) ([7c15dac](https://github.com/TrogonStack/rusty-monorepo/commit/7c15dacafa0961ce81f4dd98ffba89ab01ce3d47))
* **trg:** Read config vars straight from a secrets backend ([#85](https://github.com/TrogonStack/rusty-monorepo/issues/85)) ([c4b388d](https://github.com/TrogonStack/rusty-monorepo/commit/c4b388d436c1ab864a912f34788dbd261dd4a337))


### Bug Fixes

* **trg:** Repair the documented examples and report a version ([#87](https://github.com/TrogonStack/rusty-monorepo/issues/87)) ([df9c79a](https://github.com/TrogonStack/rusty-monorepo/commit/df9c79a9739d26f9abb3f40f79e0792c72672638))

## [0.5.0](https://github.com/TrogonStack/rusty-monorepo/compare/trg@v0.4.2...trg@v0.5.0) (2026-09-05)


### Features

* **trg:** Add a doctor for the configured secrets backends ([#79](https://github.com/TrogonStack/rusty-monorepo/issues/79)) ([64bc370](https://github.com/TrogonStack/rusty-monorepo/commit/64bc37090342857040a88fc1f714e928a016eda2))
* **trg:** Decouple credential storage from the macOS Keychain ([#73](https://github.com/TrogonStack/rusty-monorepo/issues/73)) ([3f1c015](https://github.com/TrogonStack/rusty-monorepo/commit/3f1c0158cb109f4140981256b6fd811af18091a3))
* **trg:** Give the OAuth callback a real landing page ([#84](https://github.com/TrogonStack/rusty-monorepo/issues/84)) ([be71788](https://github.com/TrogonStack/rusty-monorepo/commit/be71788be8f5361a3a6a3fdd166640ff8723a7fc))
* **trg:** Set the result of a login apart from the flow that produced it ([#80](https://github.com/TrogonStack/rusty-monorepo/issues/80)) ([f4a482a](https://github.com/TrogonStack/rusty-monorepo/commit/f4a482ac682e3ffef4848c47e842977b2abec5f4))
* **trg:** Store MCP OAuth credentials outside the macOS Keychain ([#77](https://github.com/TrogonStack/rusty-monorepo/issues/77)) ([884f8e8](https://github.com/TrogonStack/rusty-monorepo/commit/884f8e895d3e72958e2b9108a6393349cc776eec))


### Bug Fixes

* **trg:** Let the MCP host see why the proxy could not start ([#82](https://github.com/TrogonStack/rusty-monorepo/issues/82)) ([52e0da2](https://github.com/TrogonStack/rusty-monorepo/commit/52e0da22917eb42ef452858a39869a183d18089b))
* **trg:** Require an owner rather than defaulting everyone into one subtree ([#81](https://github.com/TrogonStack/rusty-monorepo/issues/81)) ([30d68f2](https://github.com/TrogonStack/rusty-monorepo/commit/30d68f25495cd9e1261af780319185702f9c2dd4))
* **trg:** Stop reporting an unreadable token as a denied secret read ([#83](https://github.com/TrogonStack/rusty-monorepo/issues/83)) ([f17e8a5](https://github.com/TrogonStack/rusty-monorepo/commit/f17e8a5461492ba24d04791e1199ce5ffca57de3))

## [0.4.2](https://github.com/TrogonStack/rusty-monorepo/compare/trg@v0.4.1...trg@v0.4.2) (2026-09-02)


### Bug Fixes

* **trg:** Keep the recovery command copy-pasteable for any server name ([#70](https://github.com/TrogonStack/rusty-monorepo/issues/70)) ([3d629ac](https://github.com/TrogonStack/rusty-monorepo/commit/3d629accd634ca6a87f32f968197098642554433))
* **trg:** Tell the user how to recover from the OAuth TTY error ([#67](https://github.com/TrogonStack/rusty-monorepo/issues/67)) ([13d7c6d](https://github.com/TrogonStack/rusty-monorepo/commit/13d7c6d143e78d92647d1d839d5700f38105589f))

## [0.4.1](https://github.com/TrogonStack/rusty-monorepo/compare/trg@v0.4.0...trg@v0.4.1) (2026-05-29)


### Bug Fixes

* Drop Windows support; recommend WSL ([#44](https://github.com/TrogonStack/rusty-monorepo/issues/44)) ([9c1960d](https://github.com/TrogonStack/rusty-monorepo/commit/9c1960dde1b71352819b785663475a6492ec8d5a))
* **trg:** Gate relative_path_from behind cfg(unix) ([#42](https://github.com/TrogonStack/rusty-monorepo/issues/42)) ([bf53675](https://github.com/TrogonStack/rusty-monorepo/commit/bf536757f866bf74d60ea8bd7fd4985040af643b))

## [0.4.0](https://github.com/TrogonStack/rusty-monorepo/compare/trg@v0.3.0...trg@v0.4.0) (2026-05-29)


### Features

* **trg:** Add skills eval subcommand for CI artifact bundles ([#36](https://github.com/TrogonStack/rusty-monorepo/issues/36)) ([94d933c](https://github.com/TrogonStack/rusty-monorepo/commit/94d933cc58124e64cedde38ac2be23bbb14bf0bd))
* **trg:** Expand skills eval into full evaluation pipeline ([#39](https://github.com/TrogonStack/rusty-monorepo/issues/39)) ([228eb51](https://github.com/TrogonStack/rusty-monorepo/commit/228eb5136a8b495c1219aef2f1ffbb97dcc776e9))

## [0.3.0](https://github.com/TrogonStack/rusty-monorepo/compare/trg@v0.2.1...trg@v0.3.0) (2026-05-24)


### Features

* **trg:** Add mcp subcommand ([#32](https://github.com/TrogonStack/rusty-monorepo/issues/32)) ([0c236d6](https://github.com/TrogonStack/rusty-monorepo/commit/0c236d6cb76383f6198700c1e5b451670e2ba942))
* **trg:** Oauth 2.1 support for mcp proxy ([#35](https://github.com/TrogonStack/rusty-monorepo/issues/35)) ([05d40f8](https://github.com/TrogonStack/rusty-monorepo/commit/05d40f8eaf6bdef9b7eb8716c2b4b7a4f9223b90))

## [0.2.1](https://github.com/TrogonStack/rusty-monorepo/compare/trg@v0.2.0...trg@v0.2.1) (2026-05-18)


### Bug Fixes

* **trg:** Accept yaml list form for allowed-tools ([#29](https://github.com/TrogonStack/rusty-monorepo/issues/29)) ([9a4480b](https://github.com/TrogonStack/rusty-monorepo/commit/9a4480b2f2fbf1cc149947c1ba5784a5022b851e))

## [0.2.0](https://github.com/TrogonStack/rusty-monorepo/compare/trg@v0.1.0...trg@v0.2.0) (2026-02-15)


### Features

* **trg:** Implement Agent Skills CLI with validation ([#6](https://github.com/TrogonStack/rusty-monorepo/issues/6)) ([180ee67](https://github.com/TrogonStack/rusty-monorepo/commit/180ee67b482d742134d4ba7688ee5d5a50715420))
