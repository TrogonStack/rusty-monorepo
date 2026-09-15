# Changelog

## [0.10.0](https://github.com/TrogonStack/rusty-monorepo/compare/trg@v0.9.0...trg@v0.10.0) (2026-09-15)


### Features

* **trg:** A case can be weighted and gated on its own score ([#159](https://github.com/TrogonStack/rusty-monorepo/issues/159)) ([6b5c40a](https://github.com/TrogonStack/rusty-monorepo/commit/6b5c40aa211db6be53a6008565611ebd2bc87d5b))
* **trg:** A case needs a directory before it needs more fields ([#146](https://github.com/TrogonStack/rusty-monorepo/issues/146)) ([dd3d8d3](https://github.com/TrogonStack/rusty-monorepo/commit/dd3d8d3141c662ceafd683c7e1b4140994f2233e))
* **trg:** A case that fails everything is invisible in a healthy average ([#157](https://github.com/TrogonStack/rusty-monorepo/issues/157)) ([6a52cb9](https://github.com/TrogonStack/rusty-monorepo/commit/6a52cb9f4db05424a4cb01ec7ecae351a3b0f6bc))
* **trg:** A harness must declare what it cannot do ([#145](https://github.com/TrogonStack/rusty-monorepo/issues/145)) ([ea5485a](https://github.com/TrogonStack/rusty-monorepo/commit/ea5485a2ec40c34db123dcebec53071a23582e34))
* **trg:** A judge asked once cannot say whether it is sure ([#138](https://github.com/TrogonStack/rusty-monorepo/issues/138)) ([6624297](https://github.com/TrogonStack/rusty-monorepo/commit/662429717c483ecf2eef3e2ba57977116788ca9e))
* **trg:** A pass that names no scenario gets a baseline to read against ([#137](https://github.com/TrogonStack/rusty-monorepo/issues/137)) ([e850e7b](https://github.com/TrogonStack/rusty-monorepo/commit/e850e7b1de99a6885bd6b96d127b085ceaf6ebe4))
* **trg:** A run that can reach any tool cannot measure whether a skill needed one ([#178](https://github.com/TrogonStack/rusty-monorepo/issues/178)) ([a991e13](https://github.com/TrogonStack/rusty-monorepo/commit/a991e13bdd9260867bdd1d2100deb0ee8af82b52))
* **trg:** A suite that does not sit where trg insists is a suite trg cannot run ([#180](https://github.com/TrogonStack/rusty-monorepo/issues/180)) ([97b9566](https://github.com/TrogonStack/rusty-monorepo/commit/97b9566381377dba90e1f87b84b58cea2d39fcf4))
* **trg:** A tool_order case can ask which call preceded which ([#161](https://github.com/TrogonStack/rusty-monorepo/issues/161)) ([6c84788](https://github.com/TrogonStack/rusty-monorepo/commit/6c84788dde55528cecc935b9c3c056dd7e83b7b8))
* **trg:** An eval bundle deserves a report, not a directory of json ([#148](https://github.com/TrogonStack/rusty-monorepo/issues/148)) ([db5028d](https://github.com/TrogonStack/rusty-monorepo/commit/db5028da9ac9f2c1d51c86cc6a0fa5f9d526624a))
* **trg:** Ask before a pass runs a skill directory nobody here wrote ([#190](https://github.com/TrogonStack/rusty-monorepo/issues/190)) ([fed2f93](https://github.com/TrogonStack/rusty-monorepo/commit/fed2f93d624b887ffb3126695f4b7d5f90e5e4ff))
* **trg:** Bound what one eval pass may spend ([#135](https://github.com/TrogonStack/rusty-monorepo/issues/135)) ([d8329d3](https://github.com/TrogonStack/rusty-monorepo/commit/d8329d3a5f9fc91801436c65d9fbf7c8cdc6963d))
* **trg:** Carry a case's priority into the eval-case dimension ([#155](https://github.com/TrogonStack/rusty-monorepo/issues/155)) ([f86b8f8](https://github.com/TrogonStack/rusty-monorepo/commit/f86b8f82217873f88a4778ef7de87c36c36507eb))
* **trg:** Carry each arm's draw count and spread on every benchmark delta ([#174](https://github.com/TrogonStack/rusty-monorepo/issues/174)) ([af8e8dd](https://github.com/TrogonStack/rusty-monorepo/commit/af8e8dd6ffb4e7e3893c9c0e5d74c8c0f13fc901))
* **trg:** Declare checks as data and report what a harness cannot answer ([#109](https://github.com/TrogonStack/rusty-monorepo/issues/109)) ([f4261dd](https://github.com/TrogonStack/rusty-monorepo/commit/f4261ddd7a9ea85594a818537053c51a62380558))
* **trg:** Draw every eval cell more than once by default ([#129](https://github.com/TrogonStack/rusty-monorepo/issues/129)) ([7202f91](https://github.com/TrogonStack/rusty-monorepo/commit/7202f9147fbedb3fd4afe2034f9d19a1fe6b78b8))
* **trg:** Execute more than one eval run at a time ([#128](https://github.com/TrogonStack/rusty-monorepo/issues/128)) ([8c2659c](https://github.com/TrogonStack/rusty-monorepo/commit/8c2659c12b598817574a8095bd7759f0390a71f3))
* **trg:** Let a case ask how its agent behaves when instructed a particular way ([#187](https://github.com/TrogonStack/rusty-monorepo/issues/187)) ([24c9c63](https://github.com/TrogonStack/rusty-monorepo/commit/24c9c6340ebe6ad58c6aadf5b5708653dfd3e566))
* **trg:** Let a case ask its question about the model it is about ([#183](https://github.com/TrogonStack/rusty-monorepo/issues/183)) ([edb031d](https://github.com/TrogonStack/rusty-monorepo/commit/edb031d82e31e58fb49938892c728b502bb1ca23))
* **trg:** Let a case ask to seed a conversation before its prompt ([#158](https://github.com/TrogonStack/rusty-monorepo/issues/158)) ([ebbc334](https://github.com/TrogonStack/rusty-monorepo/commit/ebbc334904df8e1943383eb0e1d7fb2f8685375e))
* **trg:** Let a case ask whether a run is at least as good as one we accept ([#165](https://github.com/TrogonStack/rusty-monorepo/issues/165)) ([a06c6f1](https://github.com/TrogonStack/rusty-monorepo/commit/a06c6f19e66b2347d17582751969dad839b5b335))
* **trg:** Let a case ask whether the skill gets reached for at all ([#123](https://github.com/TrogonStack/rusty-monorepo/issues/123)) ([6be74b2](https://github.com/TrogonStack/rusty-monorepo/commit/6be74b204eb45110ee462c1706187b5422379ba6))
* **trg:** Let a case declare the variables its own question needs ([#186](https://github.com/TrogonStack/rusty-monorepo/issues/186)) ([e27cae9](https://github.com/TrogonStack/rusty-monorepo/commit/e27cae91c527fd779d0dbdcbce19da96b972b750))
* **trg:** Let a case grade what the agent asked a mock for ([#166](https://github.com/TrogonStack/rusty-monorepo/issues/166)) ([684c80e](https://github.com/TrogonStack/rusty-monorepo/commit/684c80e8df0faad89845c4e7c71b8725e44b2461))
* **trg:** Let a case pin how many times it is drawn ([#184](https://github.com/TrogonStack/rusty-monorepo/issues/184)) ([1fa9ad3](https://github.com/TrogonStack/rusty-monorepo/commit/1fa9ad32989bca7d90564e1beed3e978df190729))
* **trg:** Let a case point a grader at the outputs a pattern names ([#163](https://github.com/TrogonStack/rusty-monorepo/issues/163)) ([0aa029c](https://github.com/TrogonStack/rusty-monorepo/commit/0aa029ce5fe44a4a37a8ec30b119b871fbb77a97))
* **trg:** Let a case say what state it is asking about ([#134](https://github.com/TrogonStack/rusty-monorepo/issues/134)) ([f0c7a5a](https://github.com/TrogonStack/rusty-monorepo/commit/f0c7a5a57d4d147fd25bddb35867febdeabaf213))
* **trg:** Let a case state that a tool must not be reached for ([#130](https://github.com/TrogonStack/rusty-monorepo/issues/130)) ([3656f18](https://github.com/TrogonStack/rusty-monorepo/commit/3656f18277330ec9ee5e42474666e76dd63ffa16))
* **trg:** Let a consumer tell which shape an emitted artifact was written to ([#182](https://github.com/TrogonStack/rusty-monorepo/issues/182)) ([376a99a](https://github.com/TrogonStack/rusty-monorepo/commit/376a99a9ce5fb7662b9702ae2e1829521fd6962f))
* **trg:** Let a mocked case run on codex where the run owns the config home ([#194](https://github.com/TrogonStack/rusty-monorepo/issues/194)) ([de2d017](https://github.com/TrogonStack/rusty-monorepo/commit/de2d0175ae629305b6b3130e8d5806e09fa71c36))
* **trg:** Let an eval fixture opt into read-only enforcement ([#149](https://github.com/TrogonStack/rusty-monorepo/issues/149)) ([f77ae12](https://github.com/TrogonStack/rusty-monorepo/commit/f77ae1221112e123a5fc3e8b73607e4fa4a8f920))
* **trg:** Let file_exists and regex graders match more than one shape ([#160](https://github.com/TrogonStack/rusty-monorepo/issues/160)) ([0ec321a](https://github.com/TrogonStack/rusty-monorepo/commit/0ec321a71628e4f6917d66e3c8f227867c34b01f))
* **trg:** Let the judge address any OpenAI-compatible endpoint ([#108](https://github.com/TrogonStack/rusty-monorepo/issues/108)) ([ef07863](https://github.com/TrogonStack/rusty-monorepo/commit/ef078630b0ca8b3568a5c4783578deec4614c454))
* **trg:** Let the llm judge target transcripts, files, and created-files lists ([#154](https://github.com/TrogonStack/rusty-monorepo/issues/154)) ([cf968fc](https://github.com/TrogonStack/rusty-monorepo/commit/cf968fce33eca1168b267741559f08c65f1551b8))
* **trg:** Observe every harness's tool calls and report what left the workspace ([#116](https://github.com/TrogonStack/rusty-monorepo/issues/116)) ([728d658](https://github.com/TrogonStack/rusty-monorepo/commit/728d6580978f2b485ada38699f16f5815d5deb04))
* **trg:** Read every harness transcript through one event vocabulary ([#107](https://github.com/TrogonStack/rusty-monorepo/issues/107)) ([a347293](https://github.com/TrogonStack/rusty-monorepo/commit/a3472936387881114383768acc0288a95b488c69))
* **trg:** Say where a run's harness looked for its configuration ([#170](https://github.com/TrogonStack/rusty-monorepo/issues/170)) ([7b057dd](https://github.com/TrogonStack/rusty-monorepo/commit/7b057dda09f540359ac8d71d53669a0d88bcc80b))
* **trg:** Say which command a case expected, not just which tool ([#131](https://github.com/TrogonStack/rusty-monorepo/issues/131)) ([7cd19d2](https://github.com/TrogonStack/rusty-monorepo/commit/7cd19d27787096188094c93532a8395b0a5044b1))
* **trg:** Scope graders to the arm they can answer in ([#122](https://github.com/TrogonStack/rusty-monorepo/issues/122)) ([10caa5a](https://github.com/TrogonStack/rusty-monorepo/commit/10caa5a69f3b55d15f566a9df8e6f9a70ab0515e))
* **trg:** Select part of an eval suite per run ([#125](https://github.com/TrogonStack/rusty-monorepo/issues/125)) ([78a369c](https://github.com/TrogonStack/rusty-monorepo/commit/78a369c52e4adfabad7c5f46c6c2a57792f385fc))
* **trg:** Separate an assertion nothing graded from one that failed ([#150](https://github.com/TrogonStack/rusty-monorepo/issues/150)) ([59de291](https://github.com/TrogonStack/rusty-monorepo/commit/59de29160f0414aa086da11ccf5aaca42f341ccd))
* **trg:** Stop a delta drawn from a handful of runs from reading as a finding ([#191](https://github.com/TrogonStack/rusty-monorepo/issues/191)) ([d732011](https://github.com/TrogonStack/rusty-monorepo/commit/d7320112b345c9a09c829b84cc704d37e21765f3))
* **trg:** Stop a hand-written mock from being the only thing a suite can be graded against ([#202](https://github.com/TrogonStack/rusty-monorepo/issues/202)) ([1aaa888](https://github.com/TrogonStack/rusty-monorepo/commit/1aaa8884ece29fe7de81faa719b6ecfbf96faae6))
* **trg:** Stop a judged pass from needing a model named on every invocation ([#192](https://github.com/TrogonStack/rusty-monorepo/issues/192)) ([fd0276e](https://github.com/TrogonStack/rusty-monorepo/commit/fd0276e9b21e7b6020e4e72fc65fdd3f43360069))
* **trg:** Stop a triggering case from scoring a skill that was the only thing to reach for ([#200](https://github.com/TrogonStack/rusty-monorepo/issues/200)) ([d37723d](https://github.com/TrogonStack/rusty-monorepo/commit/d37723dfacafcde58f9ea84d62dfdda44c942b43))
* **trg:** Stop an eval scaffold from being mistaken for a measurement ([#195](https://github.com/TrogonStack/rusty-monorepo/issues/195)) ([e7cb19d](https://github.com/TrogonStack/rusty-monorepo/commit/e7cb19dbcb2b584d0941d52121991a87dc16bd0b))
* **trg:** Stop two ways of checking a case from coexisting with no way to migrate between them ([#203](https://github.com/TrogonStack/rusty-monorepo/issues/203)) ([118b232](https://github.com/TrogonStack/rusty-monorepo/commit/118b23202b67a4399c236dcb9a4eb8f77330af3c))
* **trg:** Support fixed mcp mocks with expectation guards ([#153](https://github.com/TrogonStack/rusty-monorepo/issues/153)) ([deba02c](https://github.com/TrogonStack/rusty-monorepo/commit/deba02caa51af0baff91525d1db148597a617b5e))
* **trg:** Tell a failed eval gate apart from a broken eval run ([#169](https://github.com/TrogonStack/rusty-monorepo/issues/169)) ([9e96197](https://github.com/TrogonStack/rusty-monorepo/commit/9e9619781a163d8a24fc54eec821b6fa3b6e05e4))


### Bug Fixes

* **trg:** A cached run must answer for the draw that asked ([#127](https://github.com/TrogonStack/rusty-monorepo/issues/127)) ([0f7b5e5](https://github.com/TrogonStack/rusty-monorepo/commit/0f7b5e50933d429fc1b250b3e0ed21c1439e043e))
* **trg:** A cached run must not answer for an eval case it never ran ([#113](https://github.com/TrogonStack/rusty-monorepo/issues/113)) ([872a9f2](https://github.com/TrogonStack/rusty-monorepo/commit/872a9f2d8489443a59f5d43bb0e888c2ff7dad81))
* **trg:** A failed run must not read as a skill that failed its assertions ([#115](https://github.com/TrogonStack/rusty-monorepo/issues/115)) ([8f23d65](https://github.com/TrogonStack/rusty-monorepo/commit/8f23d6507200f3826b8dc63ac8d9ffdb6dfcc776))
* **trg:** A field the schema cannot describe is a field nobody can read ([#171](https://github.com/TrogonStack/rusty-monorepo/issues/171)) ([3f8d5b3](https://github.com/TrogonStack/rusty-monorepo/commit/3f8d5b3b52cfee49782a5ebca7bd5c9eb4256d98))
* **trg:** A manifest that names no version should not get the narrowest one ([#142](https://github.com/TrogonStack/rusty-monorepo/issues/142)) ([082f368](https://github.com/TrogonStack/rusty-monorepo/commit/082f3681a852a5ba888a798873f2caa21acdf38c))
* **trg:** A pass nobody can price should not report a total of zero ([#139](https://github.com/TrogonStack/rusty-monorepo/issues/139)) ([20312d8](https://github.com/TrogonStack/rusty-monorepo/commit/20312d8aa5987971928024b29ed16a9802cde1a9))
* **trg:** A pass-rate gate nothing was measured against must not report green ([#132](https://github.com/TrogonStack/rusty-monorepo/issues/132)) ([66e7f68](https://github.com/TrogonStack/rusty-monorepo/commit/66e7f6860fbb070a8812b3323554c6420f24f0d2))
* **trg:** A previous report can vanish between being found and used ([#151](https://github.com/TrogonStack/rusty-monorepo/issues/151)) ([34c5e7b](https://github.com/TrogonStack/rusty-monorepo/commit/34c5e7babc7c693b5c4e70601a353807886aeec0))
* **trg:** A reused run must answer for the arm that asked ([#126](https://github.com/TrogonStack/rusty-monorepo/issues/126)) ([b568c8e](https://github.com/TrogonStack/rusty-monorepo/commit/b568c8e2b4d6f3b5ee59d29d90271ec8de0aa358))
* **trg:** A run inherits nothing by accident ([#119](https://github.com/TrogonStack/rusty-monorepo/issues/119)) ([8d49745](https://github.com/TrogonStack/rusty-monorepo/commit/8d49745b8535b89f238204aca479e0bf27c26229))
* **trg:** A run must not be able to read its own answer key ([#118](https://github.com/TrogonStack/rusty-monorepo/issues/118)) ([aac9bcd](https://github.com/TrogonStack/rusty-monorepo/commit/aac9bcd6e446a57e2f1a581174efc5b470752dcb))
* **trg:** A run where nothing could be scored has no pass rate ([#114](https://github.com/TrogonStack/rusty-monorepo/issues/114)) ([39af3b4](https://github.com/TrogonStack/rusty-monorepo/commit/39af3b482b238aafca0d524c3364132e4a3bee3b))
* **trg:** A run's permissions must come from the eval, not the machine ([#144](https://github.com/TrogonStack/rusty-monorepo/issues/144)) ([7cc7526](https://github.com/TrogonStack/rusty-monorepo/commit/7cc7526c1bc933e173c2e7171214667cc08f1f87))
* **trg:** A schema looser than its parser certifies reports nothing can read ([#179](https://github.com/TrogonStack/rusty-monorepo/issues/179)) ([54849f1](https://github.com/TrogonStack/rusty-monorepo/commit/54849f115e6d930b8bf1413f0233182a4e052e1e))
* **trg:** A score that could have been copied says so ([#124](https://github.com/TrogonStack/rusty-monorepo/issues/124)) ([0ca1c52](https://github.com/TrogonStack/rusty-monorepo/commit/0ca1c5269e3d582870f16a80c21d90ed8b07fd73))
* **trg:** A struct field added beside a new caller must reach both ([#164](https://github.com/TrogonStack/rusty-monorepo/issues/164)) ([4050107](https://github.com/TrogonStack/rusty-monorepo/commit/405010780546c06d9a75dcbc43134bd4de05ae88))
* **trg:** A timed out run must not leave agents running ([#120](https://github.com/TrogonStack/rusty-monorepo/issues/120)) ([a8188e9](https://github.com/TrogonStack/rusty-monorepo/commit/a8188e97f895f88a98daea2e72072e44a5af9ac4))
* **trg:** A version nobody reads is not a contract ([#141](https://github.com/TrogonStack/rusty-monorepo/issues/141)) ([9cfaa4d](https://github.com/TrogonStack/rusty-monorepo/commit/9cfaa4d39cad8706b2a5851b402cae09835e3c12))
* **trg:** A weighted grader must count for what it declared ([#204](https://github.com/TrogonStack/rusty-monorepo/issues/204)) ([2aac762](https://github.com/TrogonStack/rusty-monorepo/commit/2aac7629f576b4ac83fb672278e2d12b9bd8a628))
* **trg:** Carry a named grader's fields through the published report ([#152](https://github.com/TrogonStack/rusty-monorepo/issues/152)) ([b7bfcce](https://github.com/TrogonStack/rusty-monorepo/commit/b7bfcce936aa644555080771d9a9a6dab43ff8f6))
* **trg:** Docs must not tell consumers to gate on a version that is gone ([#143](https://github.com/TrogonStack/rusty-monorepo/issues/143)) ([0cb60b0](https://github.com/TrogonStack/rusty-monorepo/commit/0cb60b0061baf23d9ae828ae6e96a3380d3c7b7a))
* **trg:** Pin benchmark and iteration-summary agreement on one pass ([#175](https://github.com/TrogonStack/rusty-monorepo/issues/175)) ([afd66cd](https://github.com/TrogonStack/rusty-monorepo/commit/afd66cdf12363f3d8d0d783917c2bf3c5a954c35))
* **trg:** Restore a main that builds and passes its own security gate ([#173](https://github.com/TrogonStack/rusty-monorepo/issues/173)) ([81c2ca1](https://github.com/TrogonStack/rusty-monorepo/commit/81c2ca1b96e9be42b0eec0f73eccaebb132cf7c3))
* **trg:** Split capability offered from capability driven ([#147](https://github.com/TrogonStack/rusty-monorepo/issues/147)) ([dc96e79](https://github.com/TrogonStack/rusty-monorepo/commit/dc96e7910bc11b6e9038f5f03c1d43657f88cc65))
* **trg:** Stop a baseline from being measured against whatever the host happened to have installed ([#201](https://github.com/TrogonStack/rusty-monorepo/issues/201)) ([ff2883c](https://github.com/TrogonStack/rusty-monorepo/commit/ff2883c3ae320a862b03a57c37e5e9edeb75ef09))
* **trg:** Stop a config edit from making every stored credential look like it was never there ([#209](https://github.com/TrogonStack/rusty-monorepo/issues/209)) ([82d655d](https://github.com/TrogonStack/rusty-monorepo/commit/82d655d06a9d7a4be67b7f59046b71f9962732cf))
* **trg:** Stop a garbled harness usage field from reading as a run nobody priced ([#197](https://github.com/TrogonStack/rusty-monorepo/issues/197)) ([946dd45](https://github.com/TrogonStack/rusty-monorepo/commit/946dd45ecace622af69b8effc19714ca2cd2f371))
* **trg:** Stop a harness that prices nothing from reading as a harness whose runs were free ([#188](https://github.com/TrogonStack/rusty-monorepo/issues/188)) ([a9714a9](https://github.com/TrogonStack/rusty-monorepo/commit/a9714a970d80130a53940145a5369e6e1cf3e327))
* **trg:** Stop a round trip from skipping the field it exists to check ([#176](https://github.com/TrogonStack/rusty-monorepo/issues/176)) ([1f68875](https://github.com/TrogonStack/rusty-monorepo/commit/1f6887547ea2d5e5673c3e01d993849191da95dc))
* **trg:** Stop a run's cache activity from being mistaken for billed tokens ([#172](https://github.com/TrogonStack/rusty-monorepo/issues/172)) ([f3c8d63](https://github.com/TrogonStack/rusty-monorepo/commit/f3c8d636ae11a1d3318a24a5d272800915d2c14a))
* **trg:** Stop a skill the host installed from passing as the skill under test ([#196](https://github.com/TrogonStack/rusty-monorepo/issues/196)) ([e01e424](https://github.com/TrogonStack/rusty-monorepo/commit/e01e424077726f942d76a6eeacc5cb950a94c052))
* **trg:** Stop a stub op binary from failing its own test run as text file busy ([#193](https://github.com/TrogonStack/rusty-monorepo/issues/193)) ([7418114](https://github.com/TrogonStack/rusty-monorepo/commit/741811471934d0efd908a8f550110943c02a6e6c))
* **trg:** Stop an unreadable answer from being reported as an answer nobody wrote ([#206](https://github.com/TrogonStack/rusty-monorepo/issues/206)) ([d412def](https://github.com/TrogonStack/rusty-monorepo/commit/d412def849575bbec26b607bb355c92d4d5e8a09))
* **trg:** Stop eval verify and run reuse from misreporting results ([#106](https://github.com/TrogonStack/rusty-monorepo/issues/106)) ([46304a5](https://github.com/TrogonStack/rusty-monorepo/commit/46304a5aef117b44578f89e136f67b60d17f8bad))
* **trg:** Stop reports from claiming a run was bounded when its harness ran unrestricted ([#181](https://github.com/TrogonStack/rusty-monorepo/issues/181)) ([a94d701](https://github.com/TrogonStack/rusty-monorepo/commit/a94d701fbbf348f48f02e69ef7be2524756fc1d2))
* **trg:** Stop the one artifact a grader reads from being the one nothing holds to a schema ([#199](https://github.com/TrogonStack/rusty-monorepo/issues/199)) ([b9fa34b](https://github.com/TrogonStack/rusty-monorepo/commit/b9fa34ba291f3987c7c0d73ac4355f7f78bc1648))
* **trg:** Stop two summaries of one pass from disagreeing about the same run ([#168](https://github.com/TrogonStack/rusty-monorepo/issues/168)) ([90ceeb9](https://github.com/TrogonStack/rusty-monorepo/commit/90ceeb9ee9a3686eb8f654007e8629bf9f4e2fec))
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
