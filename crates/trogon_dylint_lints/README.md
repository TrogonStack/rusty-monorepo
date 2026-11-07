# trogon_dylint_lints

**The conventions your code review keeps asking for, enforced by the compiler.**
This is a [Dylint](https://github.com/trailofbits/dylint) library of Rust lints
that encode structural and operational policy, not style.

**Rules use rustc AST, HIR, and type information as appropriate to the policy.** The
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
| `blocking_call_in_async` | warn | known blocking standard-library calls to run outside async executor workers |
| `constant_outside_constants_module` | deny | module-level constants to be declared in a `constants` module |
| `debug_remnants` | deny | diagnostics to be recorded as `tracing` events, not writes to stdout or stderr |
| `discarded_error_source` | warn | a `Result`'s error to be preserved rather than discarded or reduced to display text |
| `discarded_task_handle` | warn | Tokio task handles to be retained, awaited, or deliberately detached with a reason |
| `doc_hidden_access` | deny | another crate's `#[doc(hidden)]` items not to be accessed, unless the crate is listed in `owned_crates` |
| `error_string_comparison` | deny | error display strings not to drive behavior |
| `error_type_naming` | deny | types implementing `std::error::Error` to carry an `Error` suffix |
| `fallible_new` | deny | a constructor named `new` not to panic; return `Result` or rename it `try_new` |
| `function_local_macro_rules` | deny | `macro_rules!` definitions to live at module scope |
| `function_local_use` | deny | `use` imports to be declared at module level, not inside a function body |
| `inline_module_block` | deny | modules to be declared in their own file with `mod foo;` |
| `iterator_chain_in_for_header` | deny | a `for` loop to iterate a named variable, not a method chain that takes closures |
| `literal_template_replace` | deny | a literal template to be filled in with `format!`, not `str::replace` |
| `manual_error_impl` | deny | `std::error::Error` to be implemented with the thiserror derive, not by hand |
| `multi_module_use` | deny | a `use` statement to import items from one module, with no `::` inside its braces |
| `new_instead_of_from` | warn | a single-field wrapper's `new` that only wraps its argument to be `From` instead |
| `options_struct_without_default` | warn | defaultable `...Options` to offer `Default`, with reasoned exceptions for required values |
| `policy_lint_suppression_without_reason` | deny | shared-policy `allow` and `expect` attributes to state a nonblank technical reason |
| `process_global_mutation_in_tests` | warn | tests to avoid mutating the shared environment and working directory |
| `redundant_module_path` | deny | `#[path]` to be dropped when `mod foo;` already resolves to the same file |
| `secret_exposure_in_diagnostics` | warn | direct `secrecy::ExposeSecret` values to stay out of diagnostic macros |
| `serde_json_macro` | deny | JSON payloads to be built from a `Serialize` type, not an ad-hoc `json!` literal |
| `serde_json_macro_allow_without_reason` | deny | a technical reason to be stated when suppressing `serde_json_macro` |
| `std_env_access` | deny | environment variables to be read through an injected `trogon_std::env::ReadEnv` |
| `telemetry_attribute_literal` | deny | telemetry fields to be recorded with a generated `trogon_semconv` constant |
| `telemetry_key_value_literal` | deny | `KeyValue` keys to be built from a generated `trogon_semconv` constant |
| `telemetry_metric_construction` | deny | metric instruments to be constructed through a generated `trogon_semconv::metric::build_*` constructor |
| `telemetry_metric_name_literal` | deny | metric instruments to be named with a generated `trogon_semconv` constant |
| `telemetry_span_name_literal` | deny | spans to be named with a generated `trogon_semconv` constant |
| `test_module_naming` | deny | unit tests to live in the owner's `tests` module, in a sibling file or under the owner's module directory |
| `unbounded_channel` | deny | a channel to be given an explicit capacity, so a slow consumer applies backpressure |
| `unstructured_log_fields` | deny | log values to be recorded as `tracing` fields, not format arguments in the message |
| `weakened_write_precondition` | deny | an unconditional `WritePrecondition::Any` append to name the invariant it depends on |

## Use

Dylint resolves a library from a `git` or `path` source, never from a registry,
so a consuming workspace names the published tag:

```toml
[workspace.metadata.dylint]
libraries = [
  { git = "https://github.com/TrogonStack/rusty-monorepo", tag = "trogon_dylint_lints@v0.3.1", pattern = "crates/trogon_dylint_lints" },
]
```

The tag above is the released baseline. The new safety rules, broader
suppression-reason checks, source-preservation improvements, advisory options
policy, and profiles described here are unreleased changes in this checkout.
Consumers need a containing Git revision or a future release to use them.

```bash
cargo dylint --all --workspace --no-deps -- --all-features --all-targets
```

Every rule carries its own default level. `--all-targets` includes test code,
which a late HIR pass only sees once the test target is compiled. A consumer
that denies warnings promotes advisory rules to errors as well.

Unit tests for `parser.rs` live beside it in `parser_tests.rs`, while remaining
logical children of the implementation module:

```rust
// parser.rs
#[cfg(test)]
#[path = "parser_tests.rs"]
mod tests;

// parser_tests.rs
use super::parse;
```

A suite that outgrows one file moves into the owner's module directory instead
of scattering more files beside the implementation. Its groups stay `*_tests`
children of `tests`, never siblings of it, so the owner still declares a single
test module:

```rust
// parser.rs
#[cfg(test)]
mod tests;

// parser/tests.rs
use super::parse;

mod parse_tests;

// parser/tests/parse_tests.rs
use super::super::parse;
```

Crate roots use `lib_tests.rs` and `main_tests.rs`, or `src/tests.rs` with
groups under `src/tests/`. Directory modules backed by `mod.rs` use `tests.rs`
beside it, with groups under `tests/`. The lint checks resolved source files
and module ownership; a source filename alone cannot exempt a reserved test
module from a normal build.

Cargo integration and benchmark targets, including custom target paths and
`harness = false`, are identified through Cargo's `CARGO_TARGET_TMPDIR`
compiler environment and are exempt from the unit-test layout rules. Generated
and macro-generated modules are exempt too. The checks cover modules loaded for
the selected configuration. Inactive feature/platform branches and orphan
files require separate source-tree validation. Existing application test files
are not migrated by adopting the library.

## Configuration

The default `trogon` profile preserves the Trogon environment, generated
telemetry, and write-precondition policies. Workspaces that use different
adapters can select `portable` in their workspace-root `dylint.toml`:

```toml
[trogon_dylint_lints]
profile = "portable"
```

The portable profile omits `std_env_access`, the `telemetry_*` rules, and
`weakened_write_precondition`. Structural rules and the safety checks remain
active. Unknown profile values, malformed values, and unknown fields in this
configuration table fail compilation instead of selecting a permissive profile.

`new_instead_of_from` skips externally reachable constructors by default to
avoid breaking public APIs. To check them too:

```toml
[new_instead_of_from]
avoid_breaking_exported_api = false
```

`doc_hidden_access` permits hidden items from explicitly owned crates:

```toml
[doc_hidden_access]
owned_crates = ["my_macros_runtime"]
```

## Safety checks and exceptions

Use a local `expect(rule, reason = "...")` for a deliberate exception so a
stale exemption is reported. The shared-policy reason check covers both direct
attributes and `cfg_attr(dylint_lib = "trogon_dylint_lints", ...)` attributes.
Ordinary Clippy does not see an attribute whose Dylint condition is false.
Compiler and Clippy suppression reasons can additionally be enforced with
`clippy::allow_attributes_without_reason`.

`options_struct_without_default` is advisory: implement `Default` only when
all fields have meaningful defaults. An options type with required endpoints,
identities, or credentials should retain its constructor and explain why a
default is invalid instead of inventing one.

`discarded_error_source` also reports direct standard `ToString` conversions
of an error inside `Result::map_err`, including a tuple or struct wrapper that
retains only that text. A wrapper that keeps the original typed cause remains
valid. Arbitrary dataflow and helper functions are outside this check.

`blocking_call_in_async` recognizes resolved `std::thread::{sleep, park,
park_timeout}` and `std::fs::{read, read_to_string, write}` calls in async
execution. Stored synchronous closures and blocking-worker callbacks are
outside that execution context. Function-pointer calls and arbitrary
synchronous callbacks require separate review.

`process_global_mutation_in_tests` reports resolved `std::env::{set_var,
remove_var, set_current_dir}` calls in test contexts. A legitimate isolated
subprocess case can state that isolation in a reasoned exception. A lock within
one test does not isolate unrelated tests that use the same process state.

`discarded_task_handle` reports bare Tokio task-handle expressions and explicit
`std::mem::drop` calls that detach them. Keeping, returning, awaiting, or
transferring the handle is permitted. The check does not prove that a retained
handle is supervised, and Clippy already covers `let _ = future`.

`secret_exposure_in_diagnostics` recognizes direct `secrecy::ExposeSecret`
values passed to standard printing/panic and `tracing`/`log` event macros.
Redacted `Debug`, safe metadata, and formatting for an authorized request are
permitted. The rule is not a general secret-taint analysis and does not follow
values through local variables or helper functions.

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
