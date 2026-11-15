use crate::secrets::telemetry::testing::{assert_never_recorded, current_thread};
use opentelemetry::trace::Status;
use secrecy::{ExposeSecret, SecretString};

use super::*;
use crate::config::VarResolveError;
use crate::secrets::fake::{FakeBackend, FakeFailure};
use crate::secrets::openbao::OpenbaoReference;
use crate::secrets::SecretMap;

fn address(backend: &str, path: &str, key: &str) -> SecretVar {
    SecretVar::new(
        backend.to_string(),
        SecretAddress::Openbao(OpenbaoReference::new(
            SecretPath::parse(path).expect("path"),
            SecretKey::parse(key).expect("key"),
        )),
    )
}

fn var(backend: &str, path: &str, key: &str) -> SecretRead {
    SecretRead::single(address(backend, path, key))
}

/// A var addressed the way 1Password is addressed, over the same stand-in
/// backend: the fake answers an item read out of the entry at
/// `"<vault>/<item>"`, so the round-trip count is comparable.
fn op_address(backend: &str, reference: &str) -> SecretVar {
    SecretVar::new(
        backend.to_string(),
        SecretAddress::OnePassword(Box::new(OnePasswordReference::parse(reference).expect("reference"))),
    )
}

fn op_var(backend: &str, reference: &str) -> SecretRead {
    SecretRead::single(op_address(backend, reference))
}

async fn seeded(entries: &[(&str, &[(&str, &str)])]) -> FakeBackend {
    let backend = FakeBackend::new();
    for (path, pairs) in entries {
        let mut map = SecretMap::new();
        for (k, v) in *pairs {
            map.insert(SecretKey::parse(k).unwrap(), SecretString::from((*v).to_string()));
        }
        backend.set(&SecretPath::parse(path).unwrap(), &map).await.unwrap();
    }
    backend
}

/// The whole reason to address a secret as path plus key: an entry answers
/// with every key it holds, so vars that share a path share the read.
#[tokio::test]
async fn vars_sharing_a_path_cost_one_read_between_them() {
    let fake = seeded(&[("agentgateway", &[("token", "t"), ("principal", "p")])]).await;
    let backend = Backend::Fake(fake.clone());

    let wanted = vec![
        var("homelab", "agentgateway", "token"),
        var("homelab", "agentgateway", "principal"),
    ];
    let fetched = fetch_with(|_| Ok(backend.clone()), &wanted).await.unwrap();

    assert_eq!(fetched.len(), 2);
    assert_eq!(
        fetched.get(&wanted[0]).unwrap().expose_secret(),
        "t",
        "the first var reads its own key, not the other one's"
    );
    assert_eq!(fetched.get(&wanted[1]).unwrap().expose_secret(), "p");
    assert_eq!(fake.get_count(), 1, "two vars at one path is one round trip");
}

#[tokio::test]
async fn distinct_paths_are_read_separately() {
    let fake = seeded(&[("one", &[("k", "1")]), ("two", &[("k", "2")])]).await;
    let backend = Backend::Fake(fake.clone());

    let wanted = vec![var("homelab", "one", "k"), var("homelab", "two", "k")];
    fetch_with(|_| Ok(backend.clone()), &wanted).await.unwrap();

    assert_eq!(fake.get_count(), 2);
}

/// Two servers declaring the same secret is the point of giving it an
/// identity of its own, so it must not cost twice.
#[tokio::test]
async fn the_same_var_named_twice_is_read_once() {
    let fake = seeded(&[("agentgateway", &[("token", "t")])]).await;
    let backend = Backend::Fake(fake.clone());

    let wanted = vec![
        var("homelab", "agentgateway", "token"),
        var("homelab", "agentgateway", "token"),
    ];
    let fetched = fetch_with(|_| Ok(backend.clone()), &wanted).await.unwrap();

    assert_eq!(fetched.len(), 1);
    assert_eq!(fake.get_count(), 1);
}

#[tokio::test]
async fn nothing_wanted_reaches_nothing() {
    let fake = FakeBackend::new();
    let backend = Backend::Fake(fake.clone());

    let fetched = fetch_with(|_| Ok(backend.clone()), &[]).await.unwrap();

    assert!(fetched.is_empty());
    assert_eq!(
        fake.get_count(),
        0,
        "a server with no secret vars must not touch a backend"
    );
}

/// An entry that was never written and one that lacks the key are separate
/// mistakes, and the first is the one with a command to fix it.
#[tokio::test]
async fn an_unwritten_path_names_the_command_that_writes_it() {
    let fake = seeded(&[]).await;
    let backend = Backend::Fake(fake);

    let wanted = vec![var("homelab", "agentgateway", "token")];
    let err = fetch_with(|_| Ok(backend.clone()), &wanted).await.unwrap_err();

    let msg = err.to_string();
    assert!(matches!(err, VarFetchError::MissingPath { .. }), "{msg}");
    assert!(msg.contains("trg secret put"), "{msg}");
    assert!(msg.contains("--key token"), "{msg}");
}

#[tokio::test]
async fn a_missing_key_names_the_keys_that_are_there() {
    let fake = seeded(&[("agentgateway", &[("token", "t"), ("principal", "p")])]).await;
    let backend = Backend::Fake(fake);

    let wanted = vec![var("homelab", "agentgateway", "tokne")];
    let err = fetch_with(|_| Ok(backend.clone()), &wanted).await.unwrap_err();

    let msg = err.to_string();
    assert!(matches!(err, VarFetchError::MissingKey { .. }), "{msg}");
    assert!(msg.contains("principal") && msg.contains("token"), "{msg}");
}

/// The keys are named to catch a typo; the values behind them must not
/// travel along with that.
#[tokio::test]
async fn a_missing_key_does_not_name_any_value() {
    let fake = seeded(&[("agentgateway", &[("token", "super-secret-value")])]).await;
    let backend = Backend::Fake(fake);

    let wanted = vec![var("homelab", "agentgateway", "nope")];
    let err = fetch_with(|_| Ok(backend.clone()), &wanted).await.unwrap_err();

    assert!(!err.to_string().contains("super-secret-value"), "{err}");
}

/// The same batching property, in 1Password's own vocabulary: an
/// `op item get` answers with every field of the item, so four references
/// into one item must not become four subprocesses.
#[tokio::test]
async fn references_into_one_item_cost_one_read_between_them() {
    let fake = seeded(&[("Ops/deploy", &[("TOKEN", "t"), ("Prod/TOKEN", "p")])]).await;
    let backend = Backend::Fake(fake.clone());

    let wanted = vec![
        op_var("op", "op://Ops/deploy/TOKEN"),
        op_var("op", "op://Ops/deploy/Prod/TOKEN"),
    ];
    let fetched = fetch_with(|_| Ok(backend.clone()), &wanted).await.unwrap();

    assert_eq!(fetched.get(&wanted[0]).unwrap().expose_secret(), "t");
    assert_eq!(
        fetched.get(&wanted[1]).unwrap().expose_secret(),
        "p",
        "a sectioned reference must not pick up the unsectioned field of the same name"
    );
    assert_eq!(fake.get_count(), 1, "two references into one item is one round trip");
}

#[tokio::test]
async fn a_derived_reference_is_read_by_itself_beside_the_plain_ones() {
    let fake = seeded(&[("Ops/deploy", &[("TOKEN", "t"), ("private key", "k")])]).await;
    let backend = Backend::Fake(fake.clone());

    let wanted = vec![
        op_var("op", "op://Ops/deploy/TOKEN"),
        op_var("op", "op://Ops/deploy/private key?ssh-format=openssh"),
    ];
    let fetched = fetch_with(|_| Ok(backend.clone()), &wanted).await.unwrap();

    assert_eq!(fetched.get(&wanted[0]).unwrap().expose_secret(), "t");
    assert_eq!(fetched.get(&wanted[1]).unwrap().expose_secret(), "k");
    assert_eq!(fake.get_count(), 2, "one item read and one derived read");
}

/// 1Password items are managed in 1Password, so there is no
/// `trg secret put` to offer and the error must not invent one.
#[tokio::test]
async fn a_missing_item_offers_no_write_command() {
    let fake = seeded(&[]).await;
    let backend = Backend::Fake(fake);

    let wanted = vec![op_var("op", "op://Ops/deploy/TOKEN")];
    let err = fetch_with(|_| Ok(backend.clone()), &wanted).await.unwrap_err();

    let msg = err.to_string();
    assert!(matches!(err, VarFetchError::MissingItem { .. }), "{msg}");
    assert!(!msg.contains("trg secret put"), "{msg}");
}

#[tokio::test]
async fn a_reference_to_no_such_field_names_the_fields_that_are_there() {
    let fake = seeded(&[("Ops/deploy", &[("TOKEN", "t"), ("Prod/OTHER", "o")])]).await;
    let backend = Backend::Fake(fake);

    let wanted = vec![op_var("op", "op://Ops/deploy/TOEKN")];
    let err = fetch_with(|_| Ok(backend.clone()), &wanted).await.unwrap_err();

    let msg = err.to_string();
    assert!(matches!(err, VarFetchError::MissingKey { .. }), "{msg}");
    assert!(msg.contains("TOKEN") && msg.contains("Prod/OTHER"), "{msg}");
}

#[tokio::test]
async fn a_backend_that_cannot_be_named_reports_the_var_that_named_it() {
    let wanted = vec![var("nosuch", "agentgateway", "token")];
    let err = fetch_with(
        |name| {
            Err(BackendError::Unknown {
                name: name.to_string(),
                declared: "homelab".to_string(),
            })
        },
        &wanted,
    )
    .await
    .unwrap_err();

    let msg = err.to_string();
    assert!(matches!(err, VarFetchError::Backend { .. }), "{msg}");
    assert!(msg.contains("nosuch"), "{msg}");
}

#[test]
fn each_round_trip_is_a_span_under_the_fetch_naming_no_value() {
    const SECRET: &str = "vars-span-secret-4d1e";
    let (fetched, trace) = crate::telemetry::testing::capture(|| {
        current_thread(async {
            let fake = seeded(&[
                ("deploy-keys", &[("token", SECRET), ("principal", "p")]),
                ("other", &[("k", "v")]),
            ])
            .await;
            let backend = Backend::Fake(fake);
            let wanted = vec![
                var("homelab", "deploy-keys", "token"),
                var("homelab", "deploy-keys", "principal"),
                var("homelab", "other", "k"),
            ];
            fetch_with(|_| Ok(backend.clone()), &wanted).await
        })
    });
    assert_eq!(fetched.expect("fetch").len(), 3);

    let parent = trace.span("fetch secrets").expect("the fetch span");
    assert_eq!(
        trace.attribute("fetch secrets", "trg.secrets.var.count"),
        Some(opentelemetry::Value::I64(3))
    );
    let round_trips = trace.children_of(parent);
    assert_eq!(round_trips.len(), 2, "one span per distinct entry");
    let mut counts = Vec::new();
    for span in round_trips {
        assert_eq!(span.name, "fetch secret");
        let attribute = |key: &str| {
            span.attributes
                .iter()
                .find(|kv| kv.key.as_str() == key)
                .map(|kv| kv.value.to_string())
        };
        assert_eq!(attribute("trg.secrets.backend.kind").as_deref(), Some("fake"));
        assert_eq!(attribute("trg.secrets.backend.name").as_deref(), Some("homelab"));
        counts.push(attribute("trg.secrets.var.count").expect("count"));
    }
    counts.sort();
    assert_eq!(counts, ["1", "2"]);

    assert_never_recorded(&trace, &[SECRET, "deploy-keys", "principal"]);
}

#[test]
fn a_missing_entry_fails_its_round_trip_and_the_fetch() {
    let (fetched, trace) = crate::telemetry::testing::capture(|| {
        current_thread(async {
            let backend = Backend::Fake(FakeBackend::new());
            fetch_with(|_| Ok(backend.clone()), &[var("homelab", "absent", "k")]).await
        })
    });
    assert!(fetched.is_err());

    for name in ["fetch secrets", "fetch secret"] {
        let span = trace.span(name).expect(name);
        assert!(matches!(span.status, Status::Error { .. }), "{name}: {:?}", span.status);
        assert_eq!(
            trace.attribute(name, "error.type").map(|v| v.to_string()).as_deref(),
            Some("missing_path")
        );
    }
}

/// The entry round trip succeeds; it is picking the wanted key out of it
/// that fails. That selection runs inside the traced work, so the
/// round-trip span must carry the failure too, not only the parent fetch.
#[test]
fn a_missing_key_fails_its_round_trip_and_the_fetch() {
    let (fetched, trace) = crate::telemetry::testing::capture(|| {
        current_thread(async {
            let fake = seeded(&[("agentgateway", &[("token", "t")])]).await;
            let backend = Backend::Fake(fake);
            fetch_with(|_| Ok(backend.clone()), &[var("homelab", "agentgateway", "nope")]).await
        })
    });
    assert!(fetched.is_err());

    for name in ["fetch secrets", "fetch secret"] {
        let span = trace.span(name).expect(name);
        assert!(matches!(span.status, Status::Error { .. }), "{name}: {:?}", span.status);
        assert_eq!(
            trace.attribute(name, "error.type").map(|v| v.to_string()).as_deref(),
            Some("missing_key")
        );
    }
}

/// The same, for a 1Password reference whose field is not on the item:
/// the lookup happens after `get_item` succeeds, so it must still be
/// attributed to that round trip.
#[test]
fn a_missing_field_in_an_item_fails_its_round_trip_and_the_fetch() {
    let (fetched, trace) = crate::telemetry::testing::capture(|| {
        current_thread(async {
            let fake = seeded(&[("Ops/deploy", &[("TOKEN", "t")])]).await;
            let backend = Backend::Fake(fake);
            fetch_with(|_| Ok(backend.clone()), &[op_var("op", "op://Ops/deploy/NOPE")]).await
        })
    });
    assert!(fetched.is_err());

    for name in ["fetch secrets", "fetch secret"] {
        let span = trace.span(name).expect(name);
        assert!(matches!(span.status, Status::Error { .. }), "{name}: {:?}", span.status);
        assert_eq!(
            trace.attribute(name, "error.type").map(|v| v.to_string()).as_deref(),
            Some("missing_key")
        );
    }
}

/// A mistyped field on an item that exists must not be reported as an item
/// to create.
#[tokio::test]
async fn a_missing_field_on_a_derived_read_is_not_reported_as_a_missing_item() {
    let fake = seeded(&[("Ops/deploy", &[("TOKEN", "t")])]).await;
    let backend = Backend::Fake(fake);
    let wanted = vec![op_var("op", "op://Ops/deploy/nope?ssh-format=openssh")];

    let err = fetch_with(|_| Ok(backend.clone()), &wanted).await.unwrap_err();

    assert!(matches!(err, VarFetchError::MissingField { .. }), "{err}");
    assert_eq!(err.error_type(), "missing_field");
    let msg = err.to_string();
    assert!(msg.contains("no such field"), "{msg}");
    assert!(!msg.contains("create it"), "{msg}");
}

#[tokio::test]
async fn a_missing_item_on_a_derived_read_keeps_the_create_it_message() {
    let fake = seeded(&[]).await;
    let backend = Backend::Fake(fake);
    let wanted = vec![op_var("op", "op://Ops/deploy/key?ssh-format=openssh")];

    let err = fetch_with(|_| Ok(backend.clone()), &wanted).await.unwrap_err();

    assert!(matches!(err, VarFetchError::MissingItem { .. }), "{err}");
    assert!(err.to_string().contains("create it in 1Password first"), "{err}");
}

/// Two stores standing in for the two kinds of backend a fallback list
/// crosses: OpenBao-shaped addressing first, a 1Password reference second.
struct Stores {
    bao: FakeBackend,
    op: FakeBackend,
}

impl Stores {
    async fn new(bao: &[(&str, &[(&str, &str)])], op: &[(&str, &[(&str, &str)])]) -> Self {
        Self {
            bao: seeded(bao).await,
            op: seeded(op).await,
        }
    }

    fn resolve(&self, name: &str) -> Result<Backend, BackendError> {
        match name {
            "bao" => Ok(Backend::Fake(self.bao.clone())),
            "op" => Ok(Backend::Fake(self.op.clone())),
            other => Err(BackendError::Unknown {
                name: other.to_string(),
                declared: "bao, op".to_string(),
            }),
        }
    }
}

fn bao_then_op() -> SecretRead {
    SecretRead::from_addresses(vec![
        address("bao", "exec/github", "GH_TOKEN"),
        op_address("op", "op://Ops/github/GH_TOKEN"),
    ])
    .expect("two addresses")
}

async fn fetch_chain(stores: &Stores, read: &SecretRead) -> Result<FetchedSecrets, VarFetchError> {
    fetch_with(|name| stores.resolve(name), std::slice::from_ref(read)).await
}

#[tokio::test]
async fn the_first_address_answering_leaves_the_rest_unread() {
    let stores = Stores::new(
        &[("exec/github", &[("GH_TOKEN", "from-bao")])],
        &[("Ops/github", &[("GH_TOKEN", "from-op")])],
    )
    .await;
    let read = bao_then_op();

    let fetched = fetch_chain(&stores, &read).await.unwrap();

    assert_eq!(fetched.get(&read).unwrap().expose_secret(), "from-bao");
    assert_eq!(fetched.answered_by(&read), Some("bao"));
    assert_eq!(
        stores.op.get_count(),
        0,
        "a fallback is not consulted while the first answers"
    );
}

#[tokio::test]
async fn an_unreachable_first_backend_falls_back_to_the_next() {
    let stores = Stores::new(&[], &[("Ops/github", &[("GH_TOKEN", "from-op")])]).await;
    stores.bao.set_get_failure(Some(FakeFailure::Transport));
    let read = bao_then_op();

    let fetched = fetch_chain(&stores, &read).await.unwrap();

    assert_eq!(fetched.get(&read).unwrap().expose_secret(), "from-op");
    assert_eq!(fetched.answered_by(&read), Some("op"));
}

#[tokio::test]
async fn a_named_secret_whose_primary_fails_is_answered_by_its_fallback() {
    let section: crate::secrets::SecretsSection = toml::from_str(
        r#"
            [backends.bao]
            kind = "openbao"
            addr = "https://bao.example.com"
            mount = "kv"
            path_prefix = "trg"
            owner = "me"
            token_file = "~/.t"

            [backends.op]
            kind = "onepassword"
            account = "my.1password.com"

            [vars.github]
            backend = "bao"
            path = "exec/github"
            key = "GH_TOKEN"
            fallbacks = [ { backend = "op", ref = "op://Ops/github/GH_TOKEN" } ]
            "#,
    )
    .unwrap();
    let read = section
        .resolve_named(&crate::secrets::SecretName::parse("github").unwrap())
        .unwrap();
    let stores = Stores::new(&[], &[("Ops/github", &[("GH_TOKEN", "from-op")])]).await;
    stores.bao.set_get_failure(Some(FakeFailure::Transport));

    let fetched = fetch_chain(&stores, &read).await.unwrap();

    assert_eq!(fetched.get(&read).unwrap().expose_secret(), "from-op");
    assert_eq!(fetched.answered_by(&read), Some("op"));
}

/// A reference with a query is derived by `op` rather than listed on the
/// item, so it is read by its own call, and as a fallback it must still
/// answer after the first address cannot.
#[tokio::test]
async fn a_derived_reference_answers_as_a_fallback() {
    let stores = Stores::new(&[], &[("Ops/deploy-key", &[("private key", "key-material")])]).await;
    stores.bao.set_get_failure(Some(FakeFailure::Transport));
    let read = SecretRead::from_addresses(vec![
        address("bao", "exec/ssh", "KEY"),
        op_address("op", "op://Ops/deploy-key/private key?ssh-format=openssh"),
    ])
    .unwrap();

    let fetched = fetch_chain(&stores, &read).await.unwrap();

    assert_eq!(fetched.get(&read).unwrap().expose_secret(), "key-material");
    assert_eq!(fetched.answered_by(&read), Some("op"));
}

/// A field one item lacks may be on another, so a field miss on a derived
/// reference is a miss like any other and moves on.
#[tokio::test]
async fn a_derived_reference_whose_field_is_missing_falls_back() {
    let stores = Stores::new(
        &[],
        &[
            ("Ops/deploy-key", &[("other", "x")]),
            ("Ops/spare-key", &[("private key", "key-material")]),
        ],
    )
    .await;
    let read = SecretRead::from_addresses(vec![
        op_address("op", "op://Ops/deploy-key/private key?ssh-format=openssh"),
        op_address("op", "op://Ops/spare-key/private key?ssh-format=openssh"),
    ])
    .unwrap();

    let fetched = fetch_chain(&stores, &read).await.unwrap();

    assert_eq!(fetched.get(&read).unwrap().expose_secret(), "key-material");
}

#[tokio::test]
async fn the_exhausted_error_names_a_field_miss() {
    let stores = Stores::new(&[], &[("Ops/deploy-key", &[("other", "x")])]).await;
    let read = SecretRead::from_addresses(vec![
        op_address("op", "op://Ops/deploy-key/private key?ssh-format=openssh"),
        address("bao", "exec/ssh", "KEY"),
    ])
    .unwrap();

    let err = fetch_chain(&stores, &read).await.unwrap_err();

    let msg = err.to_string();
    assert!(matches!(err, VarFetchError::Exhausted { .. }), "{msg}");
    assert!(msg.contains("no such field"), "{msg}");
    assert!(msg.contains("found nothing at that path"), "{msg}");
}

/// A derived reference as the first address is declined like any other
/// when it cannot answer.
#[tokio::test]
async fn a_derived_reference_that_cannot_answer_first_falls_back() {
    let stores = Stores::new(&[("exec/ssh", &[("KEY", "from-bao")])], &[]).await;
    let read = SecretRead::from_addresses(vec![
        op_address("op", "op://Ops/deploy-key/private key?ssh-format=openssh"),
        address("bao", "exec/ssh", "KEY"),
    ])
    .unwrap();

    let fetched = fetch_chain(&stores, &read).await.unwrap();

    assert_eq!(fetched.get(&read).unwrap().expose_secret(), "from-bao");
}

/// A backend that cannot be built on this machine, as a backend whose
/// address comes from an unset environment variable cannot, is the case a
/// fallback exists for.
#[tokio::test]
async fn a_first_backend_that_cannot_be_built_here_falls_back() {
    let stores = Stores::new(&[], &[("Ops/github", &[("GH_TOKEN", "from-op")])]).await;
    let read = bao_then_op();

    let fetched = fetch_with(
        |name| match name {
            "bao" => Err(BackendError::Resolve {
                name: "bao".to_string(),
                field: "addr",
                cause: Box::new(VarResolveError::MissingEnv("BAO_ADDR".to_string())),
            }),
            other => stores.resolve(other),
        },
        std::slice::from_ref(&read),
    )
    .await
    .unwrap();

    assert_eq!(fetched.get(&read).unwrap().expose_secret(), "from-op");
}

/// A value that is not there yet is what the list is for while values are
/// being copied across, so it falls through rather than failing.
#[tokio::test]
async fn a_value_the_first_backend_does_not_hold_falls_back() {
    let read = bao_then_op();

    let no_entry = Stores::new(&[], &[("Ops/github", &[("GH_TOKEN", "from-op")])]).await;
    let fetched = fetch_chain(&no_entry, &read).await.unwrap();
    assert_eq!(fetched.get(&read).unwrap().expose_secret(), "from-op");

    let no_key = Stores::new(
        &[("exec/github", &[("OTHER", "x")])],
        &[("Ops/github", &[("GH_TOKEN", "from-op")])],
    )
    .await;
    let fetched = fetch_chain(&no_key, &read).await.unwrap();
    assert_eq!(fetched.get(&read).unwrap().expose_secret(), "from-op");
}

/// An entry that is there but will not decode is a store holding something
/// wrong, and the next address answering would paper over it.
#[tokio::test]
async fn a_first_backend_holding_a_malformed_entry_stops_the_read() {
    let stores = Stores::new(&[], &[("Ops/github", &[("GH_TOKEN", "from-op")])]).await;
    stores.bao.set_get_failure(Some(FakeFailure::Malformed));
    let read = bao_then_op();

    let err = fetch_chain(&stores, &read).await.unwrap_err();

    assert!(matches!(err, VarFetchError::Read { .. }), "{err}");
    assert_eq!(stores.op.get_count(), 0);
}

/// A configuration mistake is wrong on every machine, so a fallback that
/// answered would hide it for exactly as long as the fallback kept working.
#[tokio::test]
async fn a_first_backend_that_is_not_declared_stops_the_read() {
    let stores = Stores::new(&[], &[("Ops/github", &[("GH_TOKEN", "from-op")])]).await;
    let read = SecretRead::from_addresses(vec![
        address("typo", "exec/github", "GH_TOKEN"),
        op_address("op", "op://Ops/github/GH_TOKEN"),
    ])
    .unwrap();

    let err = fetch_chain(&stores, &read).await.unwrap_err();

    assert!(matches!(err, VarFetchError::Backend { .. }), "{err}");
    assert_eq!(stores.op.get_count(), 0);
}

#[tokio::test]
async fn a_malformed_fallback_stops_the_read_rather_than_being_skipped() {
    let stores = Stores::new(&[], &[]).await;
    stores.op.set_get_failure(Some(FakeFailure::Malformed));
    let read = SecretRead::from_addresses(vec![
        address("bao", "exec/github", "GH_TOKEN"),
        op_address("op", "op://Ops/github/GH_TOKEN"),
        address("bao", "exec/other", "GH_TOKEN"),
    ])
    .unwrap();

    let err = fetch_chain(&stores, &read).await.unwrap_err();

    assert!(matches!(err, VarFetchError::Read { .. }), "{err}");
}

#[tokio::test]
async fn when_every_address_fails_the_error_lists_each_one_and_why() {
    let stores = Stores::new(&[], &[]).await;
    stores.bao.set_get_failure(Some(FakeFailure::Transport));
    let read = bao_then_op();

    let err = fetch_chain(&stores, &read).await.unwrap_err();

    let msg = err.to_string();
    assert!(matches!(err, VarFetchError::Exhausted { .. }), "{msg}");
    assert!(msg.contains("`GH_TOKEN` at `exec/github` from backend `bao`"), "{msg}");
    assert!(msg.contains("transport: injected"), "{msg}");
    assert!(msg.contains("`op://Ops/github/GH_TOKEN` from backend `op`"), "{msg}");
    assert!(msg.contains("found no such item"), "{msg}");
}

/// Declining is per read. One that did not opt in to a fallback still
/// fails the fetch, and nothing is read on behalf of the other.
#[tokio::test]
async fn a_read_sharing_an_entry_with_one_that_has_no_fallback_still_fails() {
    let stores = Stores::new(&[], &[("Ops/github", &[("GH_TOKEN", "from-op")])]).await;
    let covered = bao_then_op();
    let bare = var("bao", "exec/github", "OTHER");

    let err = fetch_with(|name| stores.resolve(name), &[covered, bare])
        .await
        .unwrap_err();

    assert!(matches!(err, VarFetchError::MissingPath { .. }), "{err}");
}

#[tokio::test]
async fn reads_that_fall_back_to_the_same_entry_each_get_their_own_value() {
    let stores = Stores::new(&[], &[("Ops/github", &[("GH_TOKEN", "t"), ("GH_USER", "u")])]).await;
    let token = SecretRead::from_addresses(vec![
        address("bao", "exec/github", "GH_TOKEN"),
        op_address("op", "op://Ops/github/GH_TOKEN"),
    ])
    .unwrap();
    let user = SecretRead::from_addresses(vec![
        address("bao", "exec/github", "GH_USER"),
        op_address("op", "op://Ops/github/GH_USER"),
    ])
    .unwrap();

    let fetched = fetch_with(|name| stores.resolve(name), &[token.clone(), user.clone()])
        .await
        .unwrap();

    assert_eq!(fetched.get(&token).unwrap().expose_secret(), "t");
    assert_eq!(fetched.get(&user).unwrap().expose_secret(), "u");
    assert_eq!(stores.bao.get_count(), 1, "the first addresses still share one read");
}

#[test]
fn the_notice_names_the_backends_and_the_reasons_and_no_value() {
    let read = bao_then_op();
    let notice = fallback_notice(
        &read,
        &read.fallbacks()[0],
        &["var `GH_TOKEN` at `exec/github` from backend `bao` could not be read: transport: down".to_string()],
    );

    assert!(
        notice.starts_with("warning: backend `bao` could not answer"),
        "{notice}"
    );
    assert!(notice.contains("read from backend `op`"), "{notice}");
    assert!(notice.contains("transport: down"), "{notice}");
    assert!(!notice.contains("from-op"), "{notice}");
}

/// Which backend answered is recorded whether or not a fallback was
/// needed, so a trace reads the same on a healthy machine as on one that
/// has drifted.
#[test]
fn the_backend_that_answered_is_recorded_on_a_span() {
    const SECRET: &str = "chain-span-secret-7b2f";
    let (answers, trace) = crate::telemetry::testing::capture(|| {
        current_thread(async {
            let stores = Stores::new(
                &[("exec/github", &[("GH_TOKEN", SECRET)])],
                &[("Ops/github", &[("GH_TOKEN", SECRET)])],
            )
            .await;
            let read = bao_then_op();
            let healthy = fetch_chain(&stores, &read).await.is_ok();
            stores.bao.set_get_failure(Some(FakeFailure::Transport));
            let drifted = fetch_chain(&stores, &read).await.is_ok();
            (healthy, drifted)
        })
    });
    assert_eq!(answers, (true, true));

    let answered_by: Vec<(String, String)> = trace
        .spans_named("resolve secret")
        .iter()
        .map(|span| {
            let attribute = |key: &str| {
                span.attributes
                    .iter()
                    .find(|kv| kv.key.as_str() == key)
                    .map(|kv| kv.value.to_string())
                    .expect(key)
            };
            (
                attribute("trg.secrets.answered_by"),
                attribute("trg.secrets.fallback.used"),
            )
        })
        .collect();
    assert_eq!(
        answered_by,
        [
            ("bao".to_string(), "false".to_string()),
            ("op".to_string(), "true".to_string())
        ]
    );

    assert_never_recorded(&trace, &[SECRET, "exec/github", "GH_TOKEN"]);
}
