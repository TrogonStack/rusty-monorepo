# trogon_dylint_lints

**The conventions your code review keeps asking for, enforced by the compiler.**
This is a [Dylint](https://github.com/trailofbits/dylint) library of Rust lints
that encode structural and operational policy, not style.

**Each rule inspects the crate through the same HIR and type information rustc
uses, so it judges what the code means rather than how it is spelled.** The
rules cover module layering, constructor fallibility, error handling,
observability, configuration access, and channel backpressure. Every rule ships
with its default level declared in the library, so a workspace adopts the policy
by naming the library rather than by curating flags.

**Conventions that live only in a review checklist are enforced unevenly and
decay silently.** A reviewer catches an inline module or an unbounded channel on
a good day and misses it on a busy one, the exception is never written down, and
by the time the pattern is widespread it is too expensive to reverse. Moving the
convention into a lint makes it fail the build the first time instead of the
hundredth, and makes every deliberate exception an `expect` attribute that
carries a reason and reports itself once it is no longer needed.

**It is useful to Rust teams that have already agreed on how their code should
be shaped and want that agreement mechanically enforced.** Clippy covers
correctness and idiom that apply to all Rust; these rules cover the decisions a
particular codebase has made, which no general-purpose linter can know about.
Teams that disagree with a given rule can adopt the rest, since every rule is
independently levelled.

## Rules

Each rule's default level and full documentation live with the rule itself, in
its `declare_lint!` block in `src/lib.rs`: what it detects, why it is bad, what
is out of scope, and an example. Policy lives in the lint crate rather than in
per-invocation flags.

| Rule | Level | Requires |
| --- | --- | --- |
| `acyclic_modules` | deny | module dependencies to flow in one direction, not in a cycle |
| `assertions_on_fixed_literals` | deny | compile-time assertions to express more than an obvious property of a fixed literal |
| `constant_outside_constants_module` | deny | module-level constants to be declared in a `constants` module |
| `debug_remnants` | deny | diagnostics to be recorded as `tracing` events, not writes to stdout or stderr |
| `error_string_comparison` | deny | error display strings not to drive behavior |
| `error_type_naming` | deny | types implementing `std::error::Error` to carry an `Error` suffix |
| `fallible_new` | deny | a constructor named `new` not to panic; return `Result` or rename it `try_new` |
| `function_local_macro_rules` | deny | `macro_rules!` definitions to live at module scope |
| `function_local_use` | deny | `use` imports to be declared at module level, not inside a function body |
| `inline_module_block` | deny | modules to be declared in their own file with `mod foo;` |
| `manual_error_impl` | deny | `std::error::Error` to be implemented with the thiserror derive, not by hand |
| `redundant_module_path` | deny | `#[path]` to be dropped when `mod foo;` already resolves to the same file |
| `serde_json_macro` | deny | JSON payloads to be built from a `Serialize` type, not an ad-hoc `json!` literal |
| `serde_json_macro_allow_without_reason` | deny | a technical reason to be stated when suppressing `serde_json_macro` |
| `std_env_access` | deny | environment variables to be read through an injected `trogon_std::env::ReadEnv` |
| `telemetry_attribute_literal` | deny | telemetry fields to be recorded with a generated `trogon_semconv` constant |
| `telemetry_key_value_literal` | deny | `KeyValue` keys to be built from a generated `trogon_semconv` constant |
| `telemetry_metric_construction` | deny | metric instruments to be constructed through a generated `trogon_semconv::metric::build_*` constructor |
| `telemetry_metric_name_literal` | deny | metric instruments to be named with a generated `trogon_semconv` constant |
| `telemetry_span_name_literal` | deny | spans to be named with a generated `trogon_semconv` constant |
| `test_module_naming` | deny | a module of tests to be named `tests` or `*_tests` |
| `unbounded_channel` | deny | a channel to be given an explicit capacity, so a slow consumer applies backpressure |
| `unstructured_log_fields` | deny | log values to be recorded as `tracing` fields, not format arguments in the message |
| `weakened_write_precondition` | deny | an unconditional `WritePrecondition::Any` append to name the invariant it depends on |

## Credits

`unstructured_log_fields`, `acyclic_modules`, `fallible_new`,
`debug_remnants`, and `unbounded_channel` are ported from the lints of the same
names in [li-kai/rust-lints](https://github.com/li-kai/rust-lints), documented at
[`docs/unstructured-log-fields.md`](https://github.com/li-kai/rust-lints/blob/main/docs/unstructured-log-fields.md),
[`docs/acyclic-modules.md`](https://github.com/li-kai/rust-lints/blob/main/docs/acyclic-modules.md),
[`docs/fallible-new.md`](https://github.com/li-kai/rust-lints/blob/main/docs/fallible-new.md),
[`docs/debug-remnants.md`](https://github.com/li-kai/rust-lints/blob/main/docs/debug-remnants.md),
and
[`docs/unbounded-channel.md`](https://github.com/li-kai/rust-lints/blob/main/docs/unbounded-channel.md).
The rules, their names, and the shape of their exceptions are theirs; full
credit for the ideas goes to li-kai. The implementations here were written
against this crate's own helpers rather than copied, because the upstream
repository publishes no license.

`unstructured_log_fields` departs from upstream in one respect. Upstream
documents that it "does not fire when at least one structured field is
present"; this port fires whenever the message interpolates a value, however
many fields sit beside it. Upstream's rule reads a callsite as either
structured or not, so `info!(user_id, "user performed {}", action)` counts as
structured and `action` goes unreported even though only `user_id` is
queryable. Treating the fields as a per-value question instead of a
per-callsite one is what lets the rule reach that case.

`acyclic_modules` departs from upstream in where the diagnostic is levelled.
Upstream documents `#[expect(acyclic_modules, reason = "...")]` as the opt-out
but reports the cycle after the crate is walked, where only a crate-level
attribute is in scope. This port attributes each cycle to the module that owns
both siblings, so the attribute goes on that module and covers the cycle
wherever the individual references sit.

`fallible_new` departs from upstream in two respects. Upstream exposes a
`check_new_variants` configuration flag; this port has no configuration and
always covers `new_*` variants, since a variant constructor makes the same
promise as `new` and a per-repository switch would only let one crate opt out of
the rule the rest follow. Upstream also reports only `Result` as the fallible
return type; this port treats `Option` the same way, because a constructor
returning `Option<Self>` has already told the caller construction can fail.

`debug_remnants` departs from upstream in two respects. Upstream exposes a
`suggested_strategy` configuration flag choosing between `tracing` and `log`;
this port has no configuration and always suggests `tracing`, which is the one
logging facade this repository uses, so the flag would only offer a way to
suggest something the codebase does not do. Upstream also reports only the
outermost expansion node's macro by name, which leaves `dbg!` unreported on a
toolchain where it delegates to an internal macro; this port walks the
expansion chain back out to the invocation a reader can see, so the diagnostic
names `dbg!` and points at the line that was typed.

`unbounded_channel` departs from upstream in two respects. Upstream exempts
channels created in `fn main()`; this port does not, because a queue wired up
during composition grows the same way as one wired up anywhere else, and `main`
is where a long-lived service's channels are usually built. Upstream also
exposes an `additional_paths` configuration flag for naming further
constructors; this port has no configuration and carries the full set it
recognises in the lint crate, extended beyond upstream's list with
`futures::channel::mpsc::unbounded` and `async_channel::unbounded`.

## Use

Dylint resolves a library from a `git` or `path` source, never from a registry,
so a consuming workspace names the published tag:

```toml
[workspace.metadata.dylint]
libraries = [
  { git = "https://github.com/TrogonStack/rusty-monorepo", tag = "trogon_dylint_lints@v0.0.1", pattern = "crates/trogon_dylint_lints" },
]
```

```bash
cargo dylint --all --workspace --no-deps -- --all-features
```

The crates.io release of the same code exists for provenance and discovery; the
dylint wiring above is what runs it.

## Run

In this repository (the `deny` rules are enforced by their declared default
level, no flags needed):

```bash
mise run lints:run
```

That builds the library from this directory first, which is what selects the
`dylint-link` linker in `.cargo/config.toml`, and then hands dylint the built
library. `cargo dylint --path crates/trogon_dylint_lints` from the repository root
skips that config and produces a library dylint cannot find. The repository's
own crates are not yet clean under these rules, so the run reports findings; CI
enforces the ui tests in this crate rather than the rules over `crates/trg`.

To also lint test targets such as `#[cfg(test)] mod tests { ... }`, which a late
(HIR) pass only sees when the test target is compiled, add `--all-targets` to the
dylint invocation:

```bash
cargo dylint --lib-path "$(ls crates/trogon_dylint_lints/target/release/libtrogon_dylint_lints@*)" \
  --workspace --no-deps -- --all-features --all-targets
```

## Develop

This crate is a Cargo workspace of its own, not a member of the repository
workspace, and pins its compiler in `rust-toolchain.toml`. The nightly toolchain
is only for building the rustc-integrated lint library; the rest of the
repository keeps using stable. Dylint also resolves a library from the library
package's own `target/release`, which a shared workspace target directory would
not produce.

```bash
mise run lints:test      # ui tests
mise run lints:package   # crates.io packaging dry run
```
