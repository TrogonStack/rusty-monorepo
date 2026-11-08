use super::*;
use crate::secrets::telemetry::testing::{assert_never_recorded, current_thread};
use opentelemetry::trace::SpanKind;
use opentelemetry::trace::Status;
use secrecy::ExposeSecret;

/// A stand-in for `op` that records its argv and answers with a fixed
/// exit code, stdout and stderr.
///
/// The backend under test is the production one driving a real child
/// process, so what these tests pin down is the real argv this backend
/// sends and the real stderr tokens it keys off, not a re-description of
/// them.
struct StubOp {
    dir: tempfile::TempDir,
}

fn sh_quote(raw: &str) -> String {
    format!("'{}'", raw.replace('\'', r"'\''"))
}

impl StubOp {
    fn answering(exit: i32, stdout: &str, stderr: &str) -> Self {
        Self::scripted(&format!(
            "printf '%s' {stdout}\nprintf '%s' {stderr} >&2\nexit {exit}\n",
            stdout = sh_quote(stdout),
            stderr = sh_quote(stderr),
        ))
    }

    fn ok(stdout: &str) -> Self {
        Self::answering(0, stdout, "")
    }

    /// A stand-in that answers per `op` subcommand, for the probes that
    /// make more than one call.
    fn routing(routes: &[(&str, i32, &str)]) -> Self {
        let mut body = String::from("case \"$1 $2\" in\n");
        for (subcommand, exit, stdout) in routes {
            body.push_str(&format!(
                "  {subcommand}) printf '%s' {stdout}; exit {exit};;\n",
                subcommand = sh_quote(subcommand),
                stdout = sh_quote(stdout),
            ));
        }
        body.push_str("  *) printf 'unrouted: %s\\n' \"$*\" >&2; exit 127;;\nesac\n");
        Self::scripted(&body)
    }

    /// The one executable these tests run, checked into the tree rather
    /// than written here.
    ///
    /// A file this process has just written can still be open for writing
    /// in a child it forked for some other test running alongside, and
    /// executing such a file fails with `ETXTBSY`. Linking to a file
    /// nothing writes removes that race; the per-test behaviour travels in
    /// a body script, which the stub reads rather than executes.
    fn shim() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/op-stub.sh")
    }

    fn scripted(body: &str) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let bin = dir.path().join("op");
        std::fs::write(bin.with_extension("body"), body).expect("write body");
        std::os::unix::fs::symlink(Self::shim(), &bin).expect("link stub");

        Self { dir }
    }

    /// A stand-in that never returns on its own, for exercising the
    /// timeout — as if `op` were blocked on an unlock prompt no one is
    /// watching for.
    fn hanging() -> Self {
        Self::scripted("sleep 300\n")
    }

    fn backend(&self) -> OnePasswordBackend {
        OnePasswordBackend::with_bin(ACCOUNT, self.dir.path().join("op"))
    }

    fn argv(&self) -> Vec<String> {
        std::fs::read_to_string(self.dir.path().join("op.argv"))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }
}

const ACCOUNT: &str = "my.1password.com";

const ITEM_JSON: &str = r#"{
        "title": "deploy-keys",
        "vault": { "id": "v1", "name": "Ops" },
        "category": "SECURE_NOTE",
        "fields": [
            { "label": "notesPlain", "type": "STRING", "purpose": "NOTES", "value": "some notes" },
            { "label": "TOKEN_A", "type": "CONCEALED", "value": "sk-ant-fake-value" },
            { "label": "TOKEN_B", "type": "CONCEALED", "value": "sk-ant-fake-value-2" }
        ]
    }"#;

const ACCOUNTS_JSON: &str = r#"[
        { "url": "my.1password.com", "email": "someone@example.com", "user_uuid": "U1", "account_uuid": "A1" },
        { "url": "team-acme.1password.com", "email": "someone@acme.test", "user_uuid": "U2", "account_uuid": "A2" }
    ]"#;

const CURRENT_JSON: &str = r#"{ "id": "A1", "name": "Someone's Family", "domain": "my", "state": "ACTIVE" }"#;

const NOT_AN_ITEM: &str =
    "[ERROR] \"missing\" isn't an item in the \"Ops\" vault. Specify the item with its UUID, name, or domain.";
const NOT_A_VAULT: &str = "[ERROR] \"Ops\" isn't a vault in this account. Specify the vault with its ID or name.";

/// Two sections of one item holding a field of the same name is ordinary
/// in 1Password, and `op item get --format json` reports both in the same
/// flat `fields` array, so nothing but the section tells them apart.
const SECTIONED_JSON: &str = r#"{
        "title": "deploy-keys",
        "vault": { "id": "v1", "name": "Ops" },
        "fields": [
            { "label": "TOKEN", "value": "loose" },
            { "section": { "id": "s1", "label": "Prod" }, "label": "TOKEN", "value": "prod" },
            { "section": { "id": "s2", "label": "Staging" }, "label": "TOKEN", "value": "staging" },
            { "section": { "id": "s3" }, "label": "TOKEN", "value": "unlabelled" }
        ]
    }"#;

fn reference(raw: &str) -> OnePasswordReference {
    OnePasswordReference::parse(raw).expect(raw)
}

fn item(vault: &str, title: &str) -> OnePasswordItem {
    let at = format!("op://{vault}/{title}/FIELD");
    OnePasswordItem::new(
        OpSegment::parse(vault, &at).expect(vault),
        OpSegment::parse(title, &at).expect(title),
    )
}

fn sectioned_item() -> OnePasswordItemFields {
    let item = OnePasswordItem::new(
        OpSegment::parse("Ops", "op://Ops/deploy-keys/TOKEN").unwrap(),
        OpSegment::parse("deploy-keys", "op://Ops/deploy-keys/TOKEN").unwrap(),
    );
    OnePasswordItemFields::parse(&item, SECTIONED_JSON).expect("parse")
}

fn fields_from(json: &str) -> OnePasswordItemFields {
    OnePasswordItemFields::parse(&item("Ops", "deploy-keys"), json).expect("parse")
}

/// Every test here is against a reference that is not expected to be
/// ambiguous, so the ambiguity error is unwrapped away and the assertion
/// reads like the `Option` this returned before it had to become a
/// `Result`.
fn resolved<'a>(fields: &'a OnePasswordItemFields, raw: &str) -> Option<&'a str> {
    fields
        .get(&reference(raw))
        .expect("not expected to be ambiguous")
        .map(ExposeSecret::expose_secret)
}

/// A field's own `id`, alongside a section carrying only an `id` and no
/// `label`, both of which an import can produce and which `op` itself
/// still resolves a reference against.
const ID_JSON: &str = r#"{
        "fields": [
            {
                "id": "hk4m2pq7wxrz9vnb3ldsy6tuce",
                "label": "TOKEN",
                "section": { "id": "extras" },
                "value": "id-and-label"
            },
            { "id": "idonly1", "value": "id-only" }
        ]
    }"#;

#[test]
fn a_three_segment_reference_names_a_field_with_no_section() {
    let parsed = reference("op://Ops/deploy-keys/TOKEN");

    assert_eq!(parsed.vault().as_str(), "Ops");
    assert_eq!(parsed.title().as_str(), "deploy-keys");
    assert_eq!(parsed.section(), None);
    assert_eq!(parsed.field().as_str(), "TOKEN");
}

#[test]
fn a_four_segment_reference_names_the_section_between_item_and_field() {
    let parsed = reference("op://Ops/deploy-keys/Prod/TOKEN");

    assert_eq!(parsed.item().to_string(), "Ops/deploy-keys");
    assert_eq!(parsed.section().map(OpSegment::as_str), Some("Prod"));
    assert_eq!(parsed.field().as_str(), "TOKEN");
}

/// The rendering is what config errors and `trg doctor` show, so a
/// reference has to read back exactly as it was pasted.
#[test]
fn a_reference_renders_back_as_it_was_written() {
    for raw in ["op://Ops/deploy-keys/TOKEN", "op://Ops/deploy-keys/Prod/TOKEN"] {
        assert_eq!(reference(raw).to_string(), raw);
    }
}

/// Keeping the scheme is what makes a var and its backend cross-check: a
/// bare `Ops/deploy-keys/TOKEN` could as easily be a path meant for
/// another backend.
#[test]
fn a_reference_without_the_scheme_is_refused() {
    assert!(matches!(
        OnePasswordReference::parse("Ops/deploy-keys/TOKEN"),
        Err(OpReferenceError::Scheme(_))
    ));
    assert!(matches!(
        OnePasswordReference::parse("https://Ops/deploy-keys/TOKEN"),
        Err(OpReferenceError::Scheme(_))
    ));
}

#[test]
fn a_documented_query_renders_back_exactly_as_it_was_written() {
    for raw in [
        "op://Ops/deploy-keys/one-time password?attribute=otp",
        "op://Ops/deploy-keys/one-time password?attr=otp",
        "op://Ops/deploy-keys/private key?ssh-format=openssh",
        "op://Ops/deploy-keys/ssh/private key?ssh-format=openssh",
        "op://Ops/deploy-keys/file.pem?attribute=content",
    ] {
        let parsed = reference(raw);
        assert!(parsed.query().is_some(), "{raw}");
        assert_eq!(parsed.to_string(), raw);
    }
    assert!(reference("op://Ops/deploy-keys/TOKEN").query().is_none());
}

#[test]
fn the_query_does_not_change_which_field_a_reference_names() {
    let parsed = reference("op://Ops/deploy-keys/private key?ssh-format=openssh");
    assert_eq!(parsed.field().as_str(), "private key");
    assert_eq!(parsed.item(), reference("op://Ops/deploy-keys/x").item());
}

/// Ignoring a query would hand back a plausible wrong secret, so what 1Password
/// does not document is refused by name rather than stripped.
#[test]
fn an_undocumented_query_is_refused_rather_than_stripped() {
    for raw in [
        "op://Ops/deploy-keys/key?ssh-format=pem",
        "op://Ops/deploy-keys/key?attribute=title",
        "op://Ops/deploy-keys/key?attribute=",
        "op://Ops/deploy-keys/key?ssh-format",
        "op://Ops/deploy-keys/key?shape=round",
        "op://Ops/deploy-keys/key?",
        "op://Ops/deploy-keys/key?attribute=otp&ssh-format=openssh",
    ] {
        let err = OnePasswordReference::parse(raw).expect_err(raw);
        assert!(matches!(err, OpReferenceError::Query { .. }), "{raw}: {err}");
        assert!(err.to_string().contains("ssh-format=openssh"), "{err}");
    }
}

#[test]
fn a_reference_of_the_wrong_shape_is_refused() {
    for raw in ["op://", "op://Ops", "op://Ops/deploy-keys", "op://a/b/c/d/e"] {
        assert!(
            matches!(OnePasswordReference::parse(raw), Err(OpReferenceError::Shape(_))),
            "{raw}"
        );
    }
}

#[test]
fn a_reference_with_an_empty_segment_is_refused() {
    for raw in [
        "op:///deploy-keys/TOKEN",
        "op://Ops//TOKEN",
        "op://Ops/deploy-keys/",
        "op://Ops/deploy-keys//TOKEN",
    ] {
        assert!(
            matches!(OnePasswordReference::parse(raw), Err(OpReferenceError::EmptySegment(_))),
            "{raw}"
        );
    }
}

/// `op item get "--help" --vault Ops` prints help text and exits success
/// rather than reporting on the item, so a dashed vault must be refused
/// before it ever reaches argv.
#[test]
fn a_reference_with_a_dashed_vault_is_refused() {
    let err = OnePasswordReference::parse("op://-Ops/deploy-keys/TOKEN").expect_err("refused");
    assert!(matches!(err, OpReferenceError::Dash { .. }), "{err:?}");
    assert!(err.to_string().contains("vault"), "{err}");
}

/// Same hazard as the vault, on the other argument `op item get` takes
/// straight from the reference.
#[test]
fn a_reference_with_a_dashed_item_is_refused() {
    let err = OnePasswordReference::parse("op://Ops/-deploy-keys/TOKEN").expect_err("refused");
    assert!(matches!(err, OpReferenceError::Dash { .. }), "{err:?}");
    assert!(err.to_string().contains("item"), "{err}");
}

/// The section is never passed to `op` as argv, only matched against the
/// JSON `op item get` returns, so a dash cannot be confused for a flag
/// here and must keep resolving.
#[tokio::test]
async fn a_dashed_section_is_accepted_and_resolves_normally() {
    let stub = StubOp::ok(
        r#"{
                "fields": [
                    { "section": { "id": "s1", "label": "-Prod" }, "label": "TOKEN", "value": "prod-secret" }
                ]
            }"#,
    );
    let fields = stub
        .backend()
        .get_item(&item("Ops", "deploy-keys"))
        .await
        .expect("get")
        .expect("some");

    assert_eq!(
        resolved(&fields, "op://Ops/deploy-keys/-Prod/TOKEN"),
        Some("prod-secret")
    );
}

/// Same as the section: the field is matched client-side, never handed to
/// `op` as argv, so a field legitimately named `-TOKEN` must keep working.
#[tokio::test]
async fn a_dashed_field_is_accepted_and_resolves_normally() {
    let stub = StubOp::ok(
        r#"{
                "fields": [
                    { "label": "-TOKEN", "value": "dashed-field-value" }
                ]
            }"#,
    );
    let fields = stub
        .backend()
        .get_item(&item("Ops", "deploy-keys"))
        .await
        .expect("get")
        .expect("some");

    assert_eq!(
        resolved(&fields, "op://Ops/deploy-keys/-TOKEN"),
        Some("dashed-field-value")
    );
}

/// The whole point of carrying the section: a map keyed on label alone
/// would keep whichever `TOKEN` came last and answer every reference with
/// it. This also covers a reference naming no section preferring the
/// unsectioned field over any sectioned one of the same name.
#[test]
fn fields_sharing_a_label_in_different_sections_do_not_collide() {
    let fields = sectioned_item();

    for (raw, want) in [
        ("op://Ops/deploy-keys/TOKEN", "loose"),
        ("op://Ops/deploy-keys/Prod/TOKEN", "prod"),
        ("op://Ops/deploy-keys/Staging/TOKEN", "staging"),
        ("op://Ops/deploy-keys/s3/TOKEN", "unlabelled"),
    ] {
        assert_eq!(resolved(&fields, raw), Some(want), "{raw}");
    }
}

/// The same case named directly, since it is the one place an
/// unqualified reference resolving at all depends on picking the right
/// field rather than merely on there being one candidate.
#[test]
fn an_unsectioned_field_is_preferred_over_sectioned_fields_of_the_same_name() {
    assert_eq!(resolved(&sectioned_item(), "op://Ops/deploy-keys/TOKEN"), Some("loose"));
}

/// A reference naming no section still reaches a field that lives in a
/// section, as long as it is the only field of that name in the item.
/// This is a deliberate divergence from a literal reading of 1Password's
/// own reference grammar, matched here because `op read` resolves this
/// shape of reference in practice.
#[test]
fn a_sectioned_field_resolves_without_the_section_segment_when_it_is_the_only_match() {
    let fields = OnePasswordItemFields::parse(
        &item("Ops", "deploy-keys"),
        r#"{ "fields": [{ "section": { "id": "s1", "label": "Prod" }, "label": "TOKEN", "value": "prod" }] }"#,
    )
    .expect("parse");

    assert_eq!(resolved(&fields, "op://Ops/deploy-keys/TOKEN"), Some("prod"));
    assert_eq!(fields.addresses(), vec!["Prod/TOKEN".to_string()]);
}

/// Two sectioned fields sharing a name is precisely the case a bare
/// reference cannot resolve safely: which one real `op` would pick was
/// never checked against a live vault, and guessing risks silently
/// handing back the wrong secret, so this is an error rather than a pick.
#[test]
fn two_sectioned_fields_sharing_a_name_are_an_ambiguity_error_not_a_guess() {
    let fields = fields_from(
        r#"{
                "fields": [
                    { "section": { "id": "s1", "label": "Prod" }, "label": "TOKEN", "value": "prod-secret" },
                    { "section": { "id": "s2", "label": "Staging" }, "label": "TOKEN", "value": "staging-secret" }
                ]
            }"#,
    );

    let err = fields
        .get(&reference("op://Ops/deploy-keys/TOKEN"))
        .expect_err("ambiguous");
    assert!(matches!(err, SecretsError::AmbiguousField { .. }), "{err:?}");

    let message = err.to_string();
    assert!(message.contains("Prod/TOKEN"), "{message}");
    assert!(message.contains("Staging/TOKEN"), "{message}");
    assert!(!message.contains("prod-secret"), "{message}");
    assert!(!message.contains("staging-secret"), "{message}");
}

/// A section can be named by id or by label, so a field reachable via one
/// section's id and a different field reachable via another section's
/// label, in a different case besides, are both plausible answers to the
/// same section-qualified reference, and neither should win silently.
#[test]
fn two_fields_reachable_via_different_section_identifiers_are_an_ambiguity_error() {
    let fields = fields_from(
        r#"{
                "fields": [
                    { "section": { "id": "extras" }, "label": "TOKEN", "value": "via-id" },
                    { "section": { "id": "s9", "label": "Extras" }, "label": "TOKEN", "value": "via-label" }
                ]
            }"#,
    );

    let err = fields
        .get(&reference("op://Ops/deploy-keys/extras/TOKEN"))
        .expect_err("ambiguous");
    assert!(matches!(err, SecretsError::AmbiguousField { .. }), "{err:?}");

    let message = err.to_string();
    assert!(message.contains("extras/TOKEN"), "{message}");
    assert!(message.contains("Extras/TOKEN"), "{message}");
    assert!(!message.contains("via-id"), "{message}");
    assert!(!message.contains("via-label"), "{message}");
}

/// Two fields of the same name in the very same section are just as
/// ambiguous as two in different sections; the section qualifying the
/// reference narrows which section, not which field within it.
#[test]
fn two_fields_of_the_same_name_in_the_same_section_are_an_ambiguity_error() {
    let fields = fields_from(
        r#"{
                "fields": [
                    { "section": { "id": "s1", "label": "Prod" }, "label": "TOKEN", "value": "prod-a" },
                    { "section": { "id": "s1", "label": "Prod" }, "label": "TOKEN", "value": "prod-b" }
                ]
            }"#,
    );

    let err = fields
        .get(&reference("op://Ops/deploy-keys/Prod/TOKEN"))
        .expect_err("ambiguous");
    assert!(matches!(err, SecretsError::AmbiguousField { .. }), "{err:?}");

    let message = err.to_string();
    assert!(message.contains("Prod/TOKEN"), "{message}");
    assert!(!message.contains("prod-a"), "{message}");
    assert!(!message.contains("prod-b"), "{message}");
}

/// The same guarantee at the top of the precedence: an item can hold two
/// unsectioned fields sharing a label, and a reference naming no section
/// must not pick between them either.
#[test]
fn two_unsectioned_fields_sharing_a_label_are_an_ambiguity_error() {
    let fields = fields_from(
        r#"{
                "fields": [
                    { "label": "TOKEN", "value": "loose-a" },
                    { "label": "TOKEN", "value": "loose-b" }
                ]
            }"#,
    );

    let err = fields
        .get(&reference("op://Ops/deploy-keys/TOKEN"))
        .expect_err("ambiguous");
    assert!(matches!(err, SecretsError::AmbiguousField { .. }), "{err:?}");

    let message = err.to_string();
    assert!(message.contains("TOKEN"), "{message}");
    assert!(!message.contains("loose-a"), "{message}");
    assert!(!message.contains("loose-b"), "{message}");
}

/// A section-qualified reference is refused if the section it names
/// simply is not one this item has.
#[test]
fn a_reference_naming_a_section_that_does_not_exist_misses() {
    assert_eq!(resolved(&sectioned_item(), "op://Ops/deploy-keys/Nope/TOKEN"), None);
}

/// A field addressed under the wrong section must not fall through to a
/// match in whichever section it does live in. The field existing
/// somewhere in the item is not enough; `Staging` in this fixture is a
/// real section (it holds `OTHER`), just not the one `SOLO` is in.
#[test]
fn a_reference_naming_a_section_the_field_is_not_in_still_misses() {
    let fields = fields_from(
        r#"{
                "fields": [
                    { "section": { "id": "s1", "label": "Prod" }, "label": "SOLO", "value": "prod-solo" },
                    { "section": { "id": "s2", "label": "Staging" }, "label": "OTHER", "value": "staging-other" }
                ]
            }"#,
    );

    assert_eq!(resolved(&fields, "op://Ops/deploy-keys/Staging/SOLO"), None);
}

/// `label` and `id` are both accepted, and matching either is
/// case-insensitive, because 1Password labels may be non-ASCII and `op`
/// itself resolves a reference this loosely.
#[test]
fn a_field_is_matched_by_its_label_case_insensitively() {
    let fields = fields_from(ID_JSON);
    for raw in [
        "op://Ops/deploy-keys/extras/TOKEN",
        "op://Ops/deploy-keys/extras/token",
        "op://Ops/deploy-keys/EXTRAS/TOKEN",
    ] {
        assert_eq!(resolved(&fields, raw), Some("id-and-label"), "{raw}");
    }
}

/// A section named by its label is matched case-insensitively too.
#[test]
fn a_section_label_match_is_case_insensitive() {
    let fields = sectioned_item();
    for raw in ["op://Ops/deploy-keys/PROD/TOKEN", "op://Ops/deploy-keys/prod/TOKEN"] {
        assert_eq!(resolved(&fields, raw), Some("prod"), "{raw}");
    }
}

/// A field is addressable by its `id` too, with or without the section.
/// This item's id-bearing field is the only one of that id, so it
/// resolves the same way an unqualified label would.
#[test]
fn a_field_is_matched_by_its_id_case_insensitively() {
    let fields = fields_from(ID_JSON);
    for raw in [
        "op://Ops/deploy-keys/extras/hk4m2pq7wxrz9vnb3ldsy6tuce",
        "op://Ops/deploy-keys/hk4m2pq7wxrz9vnb3ldsy6tuce",
        "op://Ops/deploy-keys/HK4M2PQ7WXRZ9VNB3LDSY6TUCE",
    ] {
        assert_eq!(resolved(&fields, raw), Some("id-and-label"), "{raw}");
    }
}

/// A field imported with an `id` and no `label` at all is not dropped by
/// `parse`, and is still addressable by that `id`.
#[test]
fn a_field_with_only_an_id_and_no_label_is_matched_by_that_id() {
    let fields = fields_from(ID_JSON);
    assert_eq!(resolved(&fields, "op://Ops/deploy-keys/idonly1"), Some("id-only"));
    assert_eq!(resolved(&fields, "op://Ops/deploy-keys/IDONLY1"), Some("id-only"));
}

/// What a miss shows, so it has to name sectioned fields the way a
/// reference would have to write them.
#[test]
fn the_addresses_of_an_item_are_the_way_its_fields_are_referenced() {
    assert_eq!(
        sectioned_item().addresses(),
        vec![
            "Prod/TOKEN".to_string(),
            "Staging/TOKEN".to_string(),
            "TOKEN".to_string(),
            "s3/TOKEN".to_string(),
        ]
    );
}

#[test]
fn an_account_is_rejected_when_it_is_empty_or_would_read_as_a_flag() {
    assert!(matches!(OpAccount::parse(""), Err(AccountError::Empty)));
    assert!(matches!(OpAccount::parse("   "), Err(AccountError::Empty)));
    assert!(matches!(OpAccount::parse("--account"), Err(AccountError::Dash(_))));
    assert_eq!(OpAccount::parse("  my.1password.com ").unwrap().as_str(), ACCOUNT);
}

#[test]
fn an_account_resolves_by_any_form_op_itself_accepts() {
    let known: Vec<SignedInAccount> = serde_json::from_str(ACCOUNTS_JSON).unwrap();

    for form in [
        "my.1password.com",
        "MY.1Password.com",
        "someone@example.com",
        "U1",
        "A1",
    ] {
        let found = resolve(&OpAccount::parse(form).unwrap(), &known);
        assert_eq!(found.map(|a| a.account_uuid.as_str()), Some("A1"), "{form}");
    }
    assert!(resolve(&OpAccount::parse("nope.1password.com").unwrap(), &known).is_none());
}

/// `op --account my` reads fine, so a config saying `my` is correct and
/// must not be reported as naming an account `op` has never heard of.
#[test]
fn an_account_resolves_by_the_bare_sign_in_subdomain() {
    let known: Vec<SignedInAccount> = serde_json::from_str(ACCOUNTS_JSON).unwrap();

    for (form, want) in [("my", "A1"), ("MY", "A1"), ("team-acme", "A2")] {
        let found = resolve(&OpAccount::parse(form).unwrap(), &known);
        assert_eq!(found.map(|a| a.account_uuid.as_str()), Some(want), "{form}");
    }
    assert!(resolve(&OpAccount::parse("team-other").unwrap(), &known).is_none());
}

/// An account row missing a field must not turn every lookup into a match
/// on the empty string.
#[test]
fn an_account_with_blank_fields_matches_only_what_it_does_carry() {
    let known = vec![SignedInAccount {
        url: String::new(),
        email: String::new(),
        user_uuid: String::new(),
        account_uuid: "A9".to_string(),
    }];

    assert!(resolve(&OpAccount::parse("my.1password.com").unwrap(), &known).is_none());
    assert_eq!(
        resolve(&OpAccount::parse("A9").unwrap(), &known).map(|a| a.account_uuid.as_str()),
        Some("A9")
    );
}

#[tokio::test]
async fn a_read_answers_with_every_field_but_the_note_body() {
    let stub = StubOp::ok(ITEM_JSON);
    let fields = stub
        .backend()
        .get_item(&item("Ops", "deploy-keys"))
        .await
        .expect("get")
        .expect("some");

    assert_eq!(
        resolved(&fields, "op://Ops/deploy-keys/TOKEN_A"),
        Some("sk-ant-fake-value")
    );
    assert!(!fields.addresses().contains(&"notesPlain".to_string()));
}

#[tokio::test]
async fn every_read_carries_the_configured_account() {
    let stub = StubOp::ok(ITEM_JSON);
    stub.backend().get_item(&item("Ops", "deploy-keys")).await.expect("get");

    assert_eq!(
        stub.argv(),
        [
            "item",
            "get",
            "--cache=false",
            "deploy-keys",
            "--vault",
            "Ops",
            "--format",
            "json",
            "--account",
            ACCOUNT
        ]
    );
}

#[tokio::test]
async fn a_query_reference_is_read_with_op_read_and_carries_the_account() {
    let stub = StubOp::ok("-----BEGIN OPENSSH PRIVATE KEY-----");
    let value = stub
        .backend()
        .read(&reference("op://Ops/deploy-keys/private key?ssh-format=openssh"))
        .await
        .expect("read");
    let OpRead::Value(value) = value else {
        panic!("a value expected: {value:?}")
    };

    assert_eq!(value.expose_secret(), "-----BEGIN OPENSSH PRIVATE KEY-----");
    assert_eq!(
        stub.argv(),
        [
            "read",
            "--cache=false",
            "op://Ops/deploy-keys/private key?ssh-format=openssh",
            "--no-newline",
            "--account",
            ACCOUNT
        ]
    );
}

#[tokio::test]
async fn an_op_read_of_a_missing_item_is_a_miss_not_an_error() {
    let stub = StubOp::answering(1, "", NOT_AN_ITEM);
    assert!(matches!(
        stub.backend()
            .read(&reference("op://Ops/missing/key?ssh-format=openssh"))
            .await
            .expect("read"),
        OpRead::MissingItem
    ));
}

#[tokio::test]
async fn an_op_read_of_a_missing_item_is_a_missing_item_with_the_message_op_gives() {
    let stub = StubOp::answering(
            1,
            "",
            "[ERROR] 2026/10/28 17:13:42 could not read secret 'op://Ops/missing/password': could not get item Ops/missing: \"missing\" isn't an item in the \"Ops\" vault. Specify the item with its UUID, name, or domain.",
        );
    assert!(matches!(
        stub.backend()
            .read(&reference("op://Ops/missing/password"))
            .await
            .expect("read"),
        OpRead::MissingItem
    ));
}

#[tokio::test]
async fn an_op_read_of_a_missing_field_is_a_missing_field_with_the_message_op_gives() {
    let stub = StubOp::answering(
            1,
            "",
            "[ERROR] 2026/10/28 17:13:43 could not read secret 'op://Ops/deploy-keys/nope?ssh-format=openssh': item 'Ops/deploy-keys' does not have a field 'nope'",
        );
    assert!(matches!(
        stub.backend()
            .read(&reference("op://Ops/deploy-keys/nope?ssh-format=openssh"))
            .await
            .expect("read"),
        OpRead::MissingField
    ));
}

#[tokio::test]
async fn a_failed_op_read_is_unavailable_and_names_the_command() {
    let stub = StubOp::answering(1, "", "[ERROR] session expired");
    let err = stub
        .backend()
        .read(&reference("op://Ops/deploy-keys/key?attribute=otp"))
        .await
        .expect_err("read");
    assert!(
        err.to_string().contains("op read failed: [ERROR] session expired"),
        "{err}"
    );
}

#[tokio::test]
async fn a_read_of_a_missing_item_is_a_miss_not_an_error() {
    let stub = StubOp::answering(1, "", NOT_AN_ITEM);
    assert!(stub
        .backend()
        .get_item(&item("Ops", "missing"))
        .await
        .expect("get")
        .is_none());
}

#[tokio::test]
async fn a_vault_missing_from_the_account_names_the_account_that_was_asked() {
    let stub = StubOp::answering(1, "", NOT_A_VAULT);
    let err = stub
        .backend()
        .get_item(&item("Ops", "deploy-keys"))
        .await
        .expect_err("should fail");

    assert!(matches!(err, SecretsError::Unavailable(_)), "{err:?}");
    assert!(err.to_string().contains(ACCOUNT), "{err}");
}

#[tokio::test]
async fn a_payload_that_is_not_valid_json_is_malformed() {
    let stub = StubOp::ok("not json");
    let err = stub
        .backend()
        .get_item(&item("Ops", "deploy-keys"))
        .await
        .expect_err("should fail");
    assert!(matches!(err, SecretsError::MalformedItem { .. }), "{err:?}");
}

#[tokio::test]
async fn set_delete_and_list_are_unsupported_and_spawn_nothing() {
    let stub = StubOp::ok(ITEM_JSON);
    let backend = stub.backend();
    let path = SecretPath::parse("Ops/deploy-keys").unwrap();

    assert!(matches!(
        backend.set(&path, &SecretMap::new()).await,
        Err(SecretsError::Unsupported { op: "put", .. })
    ));
    assert!(matches!(
        backend.delete(&path).await,
        Err(SecretsError::Unsupported { op: "delete", .. })
    ));
    assert!(matches!(
        backend.list(None).await,
        Err(SecretsError::Unsupported { op: "list", .. })
    ));
    assert!(stub.argv().is_empty(), "should not have spawned op");
}

#[tokio::test]
async fn accounts_lists_what_op_can_reach_without_naming_one() {
    let stub = StubOp::routing(&[("account list", 0, ACCOUNTS_JSON)]);
    let known = stub.backend().accounts().await.expect("accounts");

    assert_eq!(known.len(), 2);
    assert_eq!(known[0].email, "someone@example.com");
    assert_eq!(stub.argv(), ["account", "list", "--cache=false", "--format", "json"]);
}

#[tokio::test]
async fn the_session_probe_asks_op_about_the_configured_account() {
    let stub = StubOp::routing(&[("account get", 0, CURRENT_JSON)]);
    let current = stub.backend().current_account().await.expect("current");

    assert_eq!(current.id, "A1");
    assert_eq!(current.state, "ACTIVE");
    assert_eq!(
        stub.argv(),
        [
            "account",
            "get",
            "--cache=false",
            "--format",
            "json",
            "--account",
            ACCOUNT
        ]
    );
}

#[tokio::test]
async fn a_locked_or_signed_out_account_makes_the_session_probe_fail() {
    let stub = StubOp::answering(1, "", "[ERROR] account is not signed in");
    let err = stub.backend().current_account().await.expect_err("should fail");
    assert!(matches!(err, SecretsError::Unavailable(_)), "{err:?}");
}

#[tokio::test]
async fn a_hung_op_is_reported_as_timed_out_rather_than_waited_on_forever() {
    let stub = StubOp::hanging();
    let backend = stub.backend().with_timeout(Duration::from_millis(50));

    let started = std::time::Instant::now();
    let err = backend.current_account().await.expect_err("should time out");

    assert!(err.to_string().contains("timed out"), "{err}");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "should not wait anywhere near the stub's 300s sleep"
    );
}

#[tokio::test]
async fn an_unspawnable_op_binary_is_a_transport_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let backend = OnePasswordBackend::with_bin(ACCOUNT, dir.path().join("absent"));
    let err = backend
        .get_item(&item("Ops", "deploy-keys"))
        .await
        .expect_err("should fail");
    assert!(matches!(err, SecretsError::Transport(_)), "{err:?}");
}

#[test]
fn each_call_is_a_cli_span_that_the_child_continues() {
    let stub = StubOp::scripted(&format!(
        "printf '%s' \"${{TRACEPARENT:-}}\" > \"${{0%.body}}.traceparent\"\nprintf '%s' {}\n",
        sh_quote(ITEM_JSON)
    ));
    let backend = stub.backend();
    let (read, trace) =
        crate::telemetry::testing::capture(|| current_thread(backend.get_item(&item("Ops", "deploy-keys"))));
    assert!(read.expect("get").is_some());

    let span = trace.span("op").expect("a span per call");
    assert_eq!(span.span_kind, SpanKind::Client);
    let attribute = |key: &str| trace.attribute("op", key).map(|v| v.to_string());
    assert_eq!(attribute("process.executable.name").as_deref(), Some("op"));
    assert_eq!(attribute("process.exit.code").as_deref(), Some("0"));
    assert!(matches!(
        trace.attribute("op", "process.pid"),
        Some(opentelemetry::Value::I64(_))
    ));
    assert_eq!(attribute("trg.secrets.operation").as_deref(), Some("item get"));
    assert_eq!(attribute("trg.secrets.backend.kind").as_deref(), Some("onepassword"));

    let traceparent = std::fs::read_to_string(stub.dir.path().join("op.traceparent")).expect("traceparent");
    assert_eq!(
        traceparent,
        format!("00-{}-{}-01", span.span_context.trace_id(), span.span_context.span_id()),
        "the child continues the call's span"
    );
    assert_never_recorded(&trace, &["sk-ant-fake-value", "deploy-keys", "Ops", ACCOUNT]);
}

#[test]
fn a_call_that_outlives_its_budget_is_typed_as_a_timeout() {
    let stub = StubOp::hanging();
    let backend = stub.backend().with_timeout(Duration::from_millis(50));
    let (current, trace) = crate::telemetry::testing::capture(|| current_thread(backend.current_account()));
    assert!(current.is_err());

    let span = trace.span("op").expect("a span per call");
    assert!(matches!(span.status, Status::Error { .. }));
    assert_eq!(
        trace.attribute("op", "error.type").map(|v| v.to_string()).as_deref(),
        Some("timeout")
    );
    assert_eq!(
        trace
            .attribute("op", "trg.secrets.operation")
            .map(|v| v.to_string())
            .as_deref(),
        Some("account get")
    );
}
