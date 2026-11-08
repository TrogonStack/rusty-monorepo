use crate::secrets::telemetry::testing::{assert_never_recorded, current_thread};
use opentelemetry::trace::SpanKind;
use opentelemetry::trace::Status;
use std::sync::{Arc, Mutex};

use super::super::kv_v2::{VersionMetadata, WritePayload};
use super::*;

/// One request the backend actually put on the wire.
#[derive(Debug, Clone)]
struct Seen {
    method: String,
    url: String,
    token: Option<String>,
    body: String,
}

/// A synthetic timestamp. Nothing reads these fields; they exist so a
/// fixture is the same shape as a real response.
const WHENEVER: &str = "1970-01-01T00:00:00Z";

fn version_metadata(version: u64, deleted: bool) -> VersionMetadata {
    VersionMetadata {
        version,
        created_time: WHENEVER.to_string(),
        deletion_time: if deleted { WHENEVER.to_string() } else { String::new() },
        destroyed: false,
        custom_metadata: None,
    }
}

/// One scripted answer, named for what OpenBao does rather than spelled
/// out as a status and a JSON literal.
///
/// The status each case carries is written down once, here, instead of at
/// every call site, which is what stops a fixture from claiming a shape
/// the server never sends.
struct Reply {
    status: u16,
    body: String,
}

impl Reply {
    fn json<T: serde::Serialize>(status: u16, payload: &T) -> Self {
        Self {
            status,
            body: serde_json::to_string(payload).expect("a fixture serializes"),
        }
    }

    /// `GET` on a live version.
    fn hit(pairs: &[(&str, &str)]) -> Self {
        let data = pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), Value::String((*v).to_string())))
            .collect();
        Self::json(
            200,
            &Envelope {
                data: ReadPayload {
                    data: Some(data),
                    metadata: Some(version_metadata(1, false)),
                },
            },
        )
    }

    /// `GET` on a path never written. The `errors` array is present and
    /// empty, which is the only thing separating this from a bad mount.
    fn miss() -> Self {
        Self::json(404, &ErrorBody::default())
    }

    /// `GET` on a soft-deleted version: `404`, the version metadata still
    /// there, and no `errors` key at all.
    fn soft_deleted() -> Self {
        Self::json(
            404,
            &Envelope {
                data: ReadPayload {
                    data: None,
                    metadata: Some(version_metadata(1, true)),
                },
            },
        )
    }

    /// A `200` carrying a null `data`, which no observed OpenBao sends.
    fn ok_with_no_data() -> Self {
        Self::json(
            200,
            &Envelope {
                data: ReadPayload {
                    data: None,
                    metadata: Some(version_metadata(4, false)),
                },
            },
        )
    }

    /// `POST` accepted.
    fn written(version: u64) -> Self {
        Self::json(
            200,
            &Envelope {
                data: WritePayload {
                    version,
                    created_time: WHENEVER.to_string(),
                    deletion_time: String::new(),
                    destroyed: false,
                    custom_metadata: None,
                },
            },
        )
    }

    /// `LIST` on a folder. A key naming a folder ends in `/`.
    fn listed(keys: &[&str]) -> Self {
        Self::json(
            200,
            &Envelope {
                data: ListPayload {
                    keys: keys.iter().map(|k| (*k).to_string()).collect(),
                },
            },
        )
    }

    /// `DELETE` accepted, with nothing to say.
    fn no_content() -> Self {
        Self {
            status: 204,
            body: String::new(),
        }
    }

    /// Any refusal that carries OpenBao's `errors` array.
    fn refused(status: u16, errors: &[&str]) -> Self {
        Self::json(
            status,
            &ErrorBody {
                errors: errors.iter().map(|e| (*e).to_string()).collect(),
            },
        )
    }

    /// Something that is not OpenBao answering, such as an intercepting
    /// proxy, or a status the client does not classify.
    fn verbatim(status: u16, body: &str) -> Self {
        Self {
            status,
            body: body.to_string(),
        }
    }
}

/// A real HTTP server answering scripted responses on a loopback port.
///
/// The backend under test is the production one, talking to it through the
/// production `reqwest` client, so the status-code table these tests pin
/// down is exercised through the same code path a live OpenBao would take.
struct StubBao {
    addr: String,
    seen: Arc<Mutex<Vec<Seen>>>,
    server: Arc<tiny_http::Server>,
}

/// A loopback listener and the address that reaches it.
fn bind_loopback() -> (Arc<tiny_http::Server>, String) {
    let server = Arc::new(tiny_http::Server::http("127.0.0.1:0").expect("bind"));
    let addr = format!(
        "http://{}",
        server.server_addr().to_ip().expect("a loopback tcp address")
    );
    (server, addr)
}

/// What the stub saw, so a test can assert on the request rather than only
/// on what the client made of the answer.
fn seen_from(request: &tiny_http::Request, body: String) -> Seen {
    Seen {
        method: request.method().as_str().to_string(),
        url: request.url().to_string(),
        token: request
            .headers()
            .iter()
            .find(|h| h.field.equiv("X-Vault-Token"))
            .map(|h| h.value.as_str().to_string()),
        body,
    }
}

fn respond_with(request: tiny_http::Request, reply: Reply) {
    let response = if reply.body.is_empty() {
        tiny_http::Response::empty(reply.status).boxed()
    } else {
        tiny_http::Response::from_string(reply.body)
            .with_status_code(reply.status)
            .with_header(
                tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).expect("static header"),
            )
            .boxed()
    };
    let _ = request.respond(response);
}

fn serve_in_order(server: Arc<tiny_http::Server>, seen: Arc<Mutex<Vec<Seen>>>, replies: Vec<Reply>) {
    for reply in replies {
        let Ok(mut request) = server.recv() else { return };

        let mut raw = String::new();
        let _ = request.as_reader().read_to_string(&mut raw);
        seen.lock().expect("stub lock").push(seen_from(&request, raw));

        respond_with(request, reply);
    }
}

fn serve_one_redirect_then_hits(
    server: Arc<tiny_http::Server>,
    seen: Arc<Mutex<Vec<Seen>>>,
    status: u16,
    location: String,
) {
    let mut first = true;
    while let Ok(request) = server.recv() {
        seen.lock().expect("stub lock").push(seen_from(&request, String::new()));

        if std::mem::take(&mut first) {
            let redirect = tiny_http::Response::empty(status).with_header(
                tiny_http::Header::from_bytes(&b"Location"[..], location.as_bytes()).expect("a location header"),
            );
            let _ = request.respond(redirect);
        } else {
            respond_with(request, Reply::hit(&[("k", "v")]));
        }
    }
}

impl StubBao {
    fn start(replies: Vec<Reply>) -> Self {
        let (server, addr) = bind_loopback();
        let seen = Arc::new(Mutex::new(Vec::new()));

        let worker = Arc::clone(&server);
        let recorder = Arc::clone(&seen);
        std::thread::spawn(move || serve_in_order(worker, recorder, replies));

        Self { addr, seen, server }
    }

    /// Answers the first request with `status` to `location` and every
    /// later one with a hit, recording each so a test can see whether the
    /// token travelled and where.
    fn start_redirecting_once_to(status: u16, location: String) -> Self {
        let (server, addr) = bind_loopback();
        let seen = Arc::new(Mutex::new(Vec::new()));

        let worker = Arc::clone(&server);
        let recorder = Arc::clone(&seen);
        std::thread::spawn(move || serve_one_redirect_then_hits(worker, recorder, status, location));

        Self { addr, seen, server }
    }

    fn backend(&self) -> OpenBaoBackend {
        let mut s = settings(&self.addr);
        s.timeout = Duration::from_secs(2);
        OpenBaoBackend::new(s).expect("build")
    }

    fn requests(&self) -> Vec<Seen> {
        self.seen.lock().expect("stub lock").clone()
    }

    fn only_request(&self) -> Seen {
        let seen = self.requests();
        assert_eq!(seen.len(), 1, "expected exactly one request, got {seen:?}");
        seen.into_iter().next().expect("checked above")
    }
}

impl Drop for StubBao {
    fn drop(&mut self) {
        self.server.unblock();
    }
}

fn path(raw: &str) -> SecretPath {
    SecretPath::parse(raw).expect("parse")
}

fn settings(addr: &str) -> OpenBaoSettings {
    OpenBaoSettings {
        addr: addr.to_string(),
        mount: "secret".to_string(),
        path_prefix: "trg".to_string(),
        owner: "yordis".to_string(),
        machine_id: Some("laptop".to_string()),
        token: TokenSource::Var(RawVarSource::Literal("t".to_string())),
        ca_cert_file: None,
        timeout: Duration::from_millis(DEFAULT_TIMEOUT_MS),
    }
}

fn backend(addr: &str) -> OpenBaoBackend {
    OpenBaoBackend::new(settings(addr)).expect("build")
}

#[test]
fn plain_http_is_refused_for_a_remote_backend() {
    for addr in [
        "http://bao.example.com:8200",
        "http://10.0.0.5:8200",
        "http://[2001:db8::1]:8200",
    ] {
        assert!(
            matches!(OpenBaoBackend::new(settings(addr)), Err(OpenBaoBuildError::Insecure(_))),
            "the token would go out in cleartext to {addr}"
        );
    }
}

#[test]
fn plain_http_is_allowed_for_a_loopback_backend() {
    for addr in ["http://127.0.0.1:8200", "http://localhost:8200", "http://[::1]:8200"] {
        assert!(
            OpenBaoBackend::new(settings(addr)).is_ok(),
            "{addr} never leaves the host"
        );
    }
}

/// reqwest strips `Authorization` across origins but not `X-Vault-Token`,
/// so nothing but the policy keeps the token off a third party. Both stubs
/// are `127.0.0.1` over http and differ only in port, so the scheme check
/// accepts both and only the origin check can refuse this one. Which is
/// also the case worth pinning: an origin is scheme, host and port, so a
/// port change alone is already somewhere else.
#[tokio::test]
async fn a_redirect_to_another_origin_never_carries_the_token() {
    let elsewhere = StubBao::start(vec![Reply::hit(&[("k", "v")])]);
    let bao = StubBao::start_redirecting_once_to(307, format!("{}/v1/secret/data/trg/mcp/laptop/x", elsewhere.addr));

    let err = bao
        .backend()
        .get(&path("mcp/laptop/x"))
        .await
        .expect_err("the redirect is refused");

    assert!(
        elsewhere.requests().is_empty(),
        "the token reached a third party: {:?}",
        elsewhere.requests()
    );
    let SecretsError::Transport(reason) = err else {
        panic!("expected a transport error, got {err:?}")
    };
    assert!(reason.contains("307"), "{reason}");
    assert!(reason.contains("not sent"), "{reason}");
}

/// The instance moving a request around inside itself is ordinary, and
/// the token is already its own.
#[tokio::test]
async fn a_redirect_within_the_configured_origin_is_followed() {
    let bao = StubBao::start_redirecting_once_to(307, "/v1/secret/data/trg/yordis/elsewhere".to_string());

    let hit = bao.backend().get(&path("mcp/laptop/x")).await.expect("followed");

    assert!(hit.is_some(), "the redirect target answered a hit");
    let seen = bao.requests();
    assert_eq!(seen.len(), 2, "{seen:?}");
    assert_eq!(seen[1].url, "/v1/secret/data/trg/yordis/elsewhere");
    assert_eq!(seen[1].token.as_deref(), Some("t"));
}

/// A `302` is a rewrite, not a detour. Following it would resend the `POST`
/// as a bodyless `GET`, which reads the path instead of writing it, and a
/// `200` from that read is indistinguishable from a stored credential.
#[tokio::test]
async fn a_write_is_never_redirected_into_a_read_that_stores_nothing() {
    let stub = StubBao::start_redirecting_once_to(302, "/v1/secret/data/trg/yordis/elsewhere".to_string());
    let mut map = SecretMap::new();
    map.insert(
        SecretKey::parse("credentials").unwrap(),
        SecretString::from("v".to_string()),
    );

    let err = stub
        .backend()
        .set(&path("mcp/laptop/github"), &map)
        .await
        .expect_err("a rewritten write did not store anything");

    assert!(err.to_string().contains("302"), "{err}");
    let seen = stub.requests();
    assert_eq!(seen.len(), 1, "the redirect must not be followed: {seen:?}");
    assert_eq!(seen[0].method, "POST");
}

/// The reason the three rewriting statuses can be refused outright: the one
/// OpenBao actually uses to point at the active node is not among them.
#[test]
fn only_the_redirects_that_keep_the_request_are_worth_following() {
    for status in [307u16, 308] {
        let status = reqwest::StatusCode::from_u16(status).expect("a redirect status");
        assert!(preserves_the_request(status), "{status} keeps the request");
    }
    for status in [301u16, 302, 303] {
        let status = reqwest::StatusCode::from_u16(status).expect("a redirect status");
        assert!(!preserves_the_request(status), "{status} rewrites the request");
    }
}

#[test]
fn a_redirect_may_not_downgrade_the_token_onto_cleartext() {
    let https = reqwest::Url::parse("https://bao.example.com/v1/x").expect("url");
    let remote_http = reqwest::Url::parse("http://bao.example.com/v1/x").expect("url");
    let loopback_http = reqwest::Url::parse("http://127.0.0.1:8200/v1/x").expect("url");

    assert!(carries_a_token_safely(&https));
    assert!(carries_a_token_safely(&loopback_http));
    assert!(!carries_a_token_safely(&remote_http));
}

/// `SecretPath` already refuses a dot segment, so the prefix is the only
/// way one can reach the joined path that becomes a URL. It never gets
/// that far: the backend refuses to exist with one.
#[test]
fn a_dot_segment_in_the_prefix_cannot_walk_out_of_it() {
    for prefix in ["trg/..", "..", "trg/.", "trg//mcp", "trg/ mcp"] {
        let mut s = settings("https://bao.example.com:8200");
        s.path_prefix = prefix.to_string();

        assert!(
            matches!(
                OpenBaoBackend::new(s),
                Err(OpenBaoBuildError::Segment {
                    field: "path_prefix",
                    ..
                })
            ),
            "should refuse the prefix {prefix:?}"
        );
    }
}

#[test]
fn a_mount_an_owner_and_a_machine_id_are_each_refused_before_the_backend_exists() {
    for field in ["mount", "owner", "machine_id"] {
        let mut s = settings("https://bao.example.com:8200");
        match field {
            "mount" => s.mount = "sec ret".to_string(),
            "owner" => s.owner = "a/b".to_string(),
            _ => s.machine_id = Some("..".to_string()),
        }

        match OpenBaoBackend::new(s).map(|_| ()) {
            Err(OpenBaoBuildError::Segment { field: got, .. }) => assert_eq!(got, field),
            other => panic!("{field} should be refused at construction, got {other:?}"),
        }
    }
}

/// The point of a typed source: the original error survives instead of
/// being flattened into a string, so a caller can ask what went wrong
/// rather than matching on prose.
#[test]
fn a_ca_cert_file_that_is_not_there_keeps_the_io_error() {
    let mut s = settings("https://bao.example.com:8200");
    s.ca_cert_file = Some(PathBuf::from("/definitely/not/here.pem"));

    let err = OpenBaoBackend::new(s).map(|_| ()).expect_err("should refuse");

    assert!(err.to_string().contains("/definitely/not/here.pem"), "{err}");
    let source = std::error::Error::source(&err).expect("the io error is kept");
    assert_eq!(
        source
            .downcast_ref::<std::io::Error>()
            .expect("an io error, not a string")
            .kind(),
        std::io::ErrorKind::NotFound
    );
}

/// `from_pem_bundle` reads anything without a PEM frame as a bundle of
/// nothing rather than as an error, so this is the only thing standing
/// between a mistyped path and a client that trusts no root at all.
#[test]
fn a_ca_cert_file_holding_no_certificate_is_refused_rather_than_pinned_to_nothing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pem = dir.path().join("not-a-bundle.pem");
    std::fs::write(&pem, b"this is not a certificate").expect("write");

    let mut s = settings("https://bao.example.com:8200");
    s.ca_cert_file = Some(pem);

    let err = OpenBaoBackend::new(s).map(|_| ()).expect_err("should refuse");

    assert!(matches!(err, OpenBaoBuildError::CaCertEmpty { .. }), "{err}");
}

/// A `\` continuation that loses its backslash still compiles, and the run
/// of indentation it was hiding then reaches the operator mid-sentence.
#[test]
fn no_build_error_reads_as_though_it_were_still_indented() {
    let path = PathBuf::from("/etc/bao/ca.pem");
    let messages = [
        OpenBaoBuildError::CaCertEmpty { path: path.clone() }.to_string(),
        OpenBaoBuildError::CaCertRead {
            path,
            cause: std::io::Error::from(std::io::ErrorKind::NotFound),
        }
        .to_string(),
        OpenBaoBuildError::Segment {
            field: "mount",
            value: "..".to_string(),
        }
        .to_string(),
    ];

    for message in messages {
        assert!(!message.contains("  "), "reads as indented: {message}");
    }
}

#[test]
fn a_malformed_pem_frame_keeps_the_parse_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pem = dir.path().join("broken.pem");
    std::fs::write(
        &pem,
        b"-----BEGIN CERTIFICATE-----\nnot!base64!\n-----END CERTIFICATE-----\n",
    )
    .expect("write");

    let mut s = settings("https://bao.example.com:8200");
    s.ca_cert_file = Some(pem);

    let err = OpenBaoBackend::new(s).map(|_| ()).expect_err("should refuse");

    assert!(err.to_string().contains("not a PEM certificate"), "{err}");
    assert!(
        std::error::Error::source(&err)
            .expect("the reqwest error is kept")
            .downcast_ref::<reqwest::Error>()
            .is_some(),
        "{err}"
    );
}

#[test]
fn tilde_expands_against_the_home_it_is_given() {
    let home = Some(PathBuf::from("/home/tester"));
    assert_eq!(
        expand_tilde_from("~/.vault-token", home.clone()),
        PathBuf::from("/home/tester/.vault-token")
    );
    assert_eq!(expand_tilde_from("~", home.clone()), PathBuf::from("/home/tester"));
    assert_eq!(
        expand_tilde_from("/etc/token", home.clone()),
        PathBuf::from("/etc/token")
    );
    assert_eq!(expand_tilde_from("~other/token", home), PathBuf::from("~other/token"));
}

#[test]
fn a_tilde_survives_unexpanded_when_there_is_no_home() {
    assert_eq!(
        expand_tilde_from("~/.vault-token", None),
        PathBuf::from("~/.vault-token")
    );
}

#[test]
fn rejects_an_addr_without_a_scheme() {
    let mut s = settings("bao.example.com:8200");
    s.addr = "bao.example.com:8200".to_string();
    assert!(matches!(OpenBaoBackend::new(s), Err(OpenBaoBuildError::Scheme(_))));
}

#[test]
fn trailing_slashes_do_not_double_up_in_urls() {
    let b = backend("https://bao.example.com:8200/");
    let path = SecretPath::parse("mcp/laptop/github").expect("parse");
    assert_eq!(
        b.data_url(&path).expect("url"),
        "https://bao.example.com:8200/v1/secret/data/trg/yordis/mcp/laptop/github"
    );
    assert_eq!(
        b.metadata_url(&path).expect("url"),
        "https://bao.example.com:8200/v1/secret/metadata/trg/yordis/mcp/laptop/github"
    );
}

#[test]
fn an_empty_path_prefix_does_not_leave_a_double_slash() {
    let mut s = settings("https://bao.example.com:8200");
    s.path_prefix = String::new();
    let b = OpenBaoBackend::new(s).expect("build");
    let path = SecretPath::parse("mcp/laptop/github").expect("parse");
    assert_eq!(
        b.data_url(&path).expect("url"),
        "https://bao.example.com:8200/v1/secret/data/yordis/mcp/laptop/github"
    );
}

#[test]
fn a_machine_id_scopes_the_credential_path_to_that_machine() {
    let b = backend("https://bao.example.com:8200");
    assert_eq!(b.credential_path("github"), "mcp/laptop/github");
}

/// The reason to leave the Keychain is that one credential is reachable
/// from everywhere, so sharing is what you get when you ask for nothing.
#[test]
fn without_a_machine_id_every_machine_shares_one_credential() {
    let mut s = settings("https://bao.example.com:8200");
    s.machine_id = None;
    let b = OpenBaoBackend::new(s).expect("build");

    assert_eq!(b.credential_path("github"), "mcp/github");
    assert_eq!(b.machine_id(), None);
}

/// With a `machine_id` configured, the pre-`machine_id` layout is still
/// reachable as a fallback, at the path it would have used before.
#[test]
fn a_shared_credential_path_is_offered_as_a_fallback_when_machine_id_is_set() {
    let b = backend("https://bao.example.com:8200");
    assert_eq!(b.shared_credential_path("github").as_deref(), Some("mcp/github"));
}

/// Without `machine_id`, the shared path already is the primary, so
/// there is nothing left to fall back onto.
#[test]
fn there_is_no_fallback_left_once_the_shared_path_is_already_the_primary() {
    let mut s = settings("https://bao.example.com:8200");
    s.machine_id = None;
    let b = OpenBaoBackend::new(s).expect("build");

    assert_eq!(b.shared_credential_path("github"), None);
}

/// A `SecretPath` is backend-relative, so the command a person is told to
/// paste has to carry the mount and storage prefix too: `mcp/github`
/// alone is not addressable against `bao` from outside this backend's own
/// config.
#[test]
fn the_removal_command_is_fully_qualified_with_the_mount_and_storage_prefix() {
    let b = backend("https://bao.example.com:8200");

    let command = b.removal_command(&path("mcp/github"));

    assert_eq!(command, "bao kv metadata delete secret/trg/yordis/mcp/github");
}

/// An empty `path_prefix` drops straight to `owner`, with no doubled or
/// missing separator.
#[test]
fn the_removal_command_has_no_stray_separator_when_path_prefix_is_empty() {
    let mut s = settings("https://bao.example.com:8200");
    s.path_prefix = String::new();
    let b = OpenBaoBackend::new(s).expect("build");

    let command = b.removal_command(&path("mcp/github"));

    assert_eq!(command, "bao kv metadata delete secret/yordis/mcp/github");
}

#[test]
fn an_unaddressable_server_name_is_blamed_on_the_path_it_would_take() {
    let mut shared = settings("https://bao.example.com:8200");
    shared.machine_id = None;
    let shared = OpenBaoBackend::new(shared).expect("build");
    let reason = shared.check_server_name("my server").expect_err("refused");
    assert!(reason.contains("secret/trg/yordis/mcp/"), "{reason}");
    assert!(!reason.contains("laptop"), "{reason}");

    let scoped = backend("https://bao.example.com:8200");
    let reason = scoped.check_server_name("my server").expect_err("refused");
    assert!(reason.contains("secret/trg/yordis/mcp/laptop/"), "{reason}");
}

#[test]
fn paths_a_keychain_accepts_but_a_url_should_not_are_refused() {
    let b = backend("https://bao.example.com:8200");
    for raw in ["my server", "Ünïcode", "a b/c", "we?rd"] {
        let path = SecretPath::parse(raw).expect("structurally valid");
        assert!(
            matches!(b.full_path(&path), Err(SecretsError::Unauthorized { .. })),
            "should refuse {raw:?}"
        );
    }
}

#[test]
fn ordinary_names_are_addressable() {
    let b = backend("https://bao.example.com:8200");
    for raw in ["github", "my-server", "a.b", "mcp/laptop/x_1"] {
        let path = SecretPath::parse(raw).expect("parse");
        assert!(b.full_path(&path).is_ok(), "should accept {raw:?}");
    }
}

#[test]
fn errors_of_extracts_only_the_errors_array() {
    assert_eq!(ErrorBody::of(r#"{"errors":[]}"#), Vec::<String>::new());
    assert_eq!(
        ErrorBody::of(r#"{"errors":["permission denied"]}"#),
        ["permission denied"]
    );
    assert_eq!(ErrorBody::of("not json"), Vec::<String>::new());
    assert_eq!(ErrorBody::of(r#"{"data":{"secret":"hunter2"}}"#), Vec::<String>::new());
}

#[test]
fn map_from_json_rejects_non_string_values() {
    let path = SecretPath::parse("x").expect("parse");
    let data: Map<String, Value> = serde_json::from_str(r#"{"a":1}"#).expect("json");
    assert!(matches!(
        map_from_json(&data, &path),
        Err(SecretsError::Malformed { .. })
    ));
}

#[test]
fn map_from_json_reads_string_values() {
    let path = SecretPath::parse("x").expect("parse");
    let data: Map<String, Value> = serde_json::from_str(r#"{"a":"1","b":"2"}"#).expect("json");
    let map = map_from_json(&data, &path).expect("map");
    assert_eq!(map.len(), 2);
    assert_eq!(
        map.get(&SecretKey::parse("a").unwrap()).map(|v| v.expose_secret()),
        Some("1")
    );
}

#[test]
fn a_group_readable_token_file_is_refused() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("token");
    std::fs::write(&path, "s.abc").expect("write");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).expect("chmod");

    let err = read_token_file(&path).expect_err("should refuse");
    assert!(matches!(err, TokenError::LooseMode { mode: 0o640, .. }), "{err:?}");
    assert!(err.remedy().starts_with("run `chmod 600 "), "{}", err.remedy());
}

#[test]
fn a_private_token_file_reads_and_trims() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("token");
    std::fs::write(&path, "s.abc\n").expect("write");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("chmod");

    assert_eq!(read_token_file(&path).expect("read").expose_secret(), "s.abc");
}

/// A token file that is missing or unreadable is not a denied read of a
/// secret, and a message shaped like one sends the reader after a path
/// that was never involved.
#[test]
fn a_token_that_could_not_be_read_names_no_secret_path() {
    let dir = tempfile::tempdir().expect("tempdir");
    let err = read_token_file(&dir.path().join("absent")).expect_err("should refuse");

    assert!(matches!(err, TokenError::Unreadable { .. }), "{err:?}");

    let message = SecretsError::from(err).to_string();
    assert!(!message.contains("not authorized to read"), "{message}");
    assert!(message.starts_with("could not read the token file"), "{message}");
    assert!(message.contains("bao login"), "{message}");
}

/// The cause is what separates a token file that is absent from one that
/// is there and unreadable, and only the source carries it.
#[test]
fn an_unreadable_token_keeps_what_the_filesystem_said() {
    let dir = tempfile::tempdir().expect("tempdir");
    let err = read_token_file(&dir.path().join("absent")).expect_err("should refuse");

    let source = std::error::Error::source(&err).expect("a source");
    assert!(source.to_string().contains("No such file"), "{source}");
    assert!(err.report().contains("No such file"), "{}", err.report());
}

#[test]
fn an_empty_token_file_names_the_recovery_command() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("token");
    std::fs::write(&path, "  \n").expect("write");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("chmod");

    let err = read_token_file(&path).expect_err("should refuse");
    assert!(matches!(err, TokenError::Empty { .. }), "{err:?}");
    assert!(err.to_string().starts_with("the token file at"), "{err}");
    assert!(err.remedy().contains("bao login"), "{}", err.remedy());
}

/// A caller with one line to print has nowhere to put the remedy, so the
/// composed error has to carry it.
#[test]
fn the_remedy_survives_being_wrapped_for_a_one_line_caller() {
    let dir = tempfile::tempdir().expect("tempdir");
    let err = SecretsError::from(read_token_file(&dir.path().join("absent")).expect_err("refuse"));
    let message = err.to_string();
    assert!(message.ends_with("run `bao login` to write one"), "{message}");
    assert!(message.contains("No such file"), "{message}");
}

#[tokio::test]
async fn a_hit_reads_the_nested_data_object() {
    let stub = StubBao::start(vec![Reply::hit(&[("credentials", r#"{"a":1}"#), ("other", "x")])]);
    let map = stub
        .backend()
        .get(&path("mcp/laptop/github"))
        .await
        .expect("get")
        .expect("some");

    assert_eq!(map.len(), 2);
    assert_eq!(
        map.get(&SecretKey::parse("credentials").unwrap())
            .map(|v| v.expose_secret()),
        Some(r#"{"a":1}"#)
    );

    let seen = stub.only_request();
    assert_eq!(seen.method, "GET");
    assert_eq!(seen.url, "/v1/secret/data/trg/yordis/mcp/laptop/github");
    assert_eq!(seen.token.as_deref(), Some("t"));
}

#[tokio::test]
async fn a_missing_secret_is_a_miss_not_an_error() {
    let stub = StubBao::start(vec![Reply::miss()]);
    assert!(stub.backend().get(&path("x")).await.expect("get").is_none());
}

#[tokio::test]
async fn a_missing_mount_is_a_configuration_error_not_a_miss() {
    let stub = StubBao::start(vec![Reply::refused(
        404,
        &[r#"no handler for route "nope/data/x". route entry not found."#],
    )]);
    let err = stub.backend().get(&path("x")).await.expect_err("should fail");

    assert!(matches!(err, SecretsError::Unavailable(_)), "{err:?}");
    assert!(err.to_string().contains("secret"), "{err}");
}

#[tokio::test]
async fn a_soft_deleted_version_reads_as_a_miss() {
    // A live OpenBao 2.5.5 answers `DELETE /v1/<mount>/data/<path>` and then
    // a read of that path with a `404` whose body carries `data` but no
    // `errors`, which is what [`Reply::soft_deleted`] reproduces.
    let stub = StubBao::start(vec![Reply::soft_deleted()]);
    assert!(stub.backend().get(&path("x")).await.expect("get").is_none());
}

/// The guard in `get`, which no observed OpenBao response reaches.
#[tokio::test]
async fn a_success_carrying_a_null_data_object_is_also_a_miss() {
    let stub = StubBao::start(vec![Reply::ok_with_no_data()]);
    assert!(stub.backend().get(&path("x")).await.expect("get").is_none());
}

#[tokio::test]
async fn a_rejected_token_names_the_recovery_command() {
    let stub = StubBao::start(vec![Reply::refused(403, &["permission denied"])]);
    let err = stub.backend().get(&path("x")).await.expect_err("should fail");

    assert!(matches!(err, SecretsError::Unauthorized { .. }), "{err:?}");
    assert!(err.to_string().contains("bao login"), "{err}");
}

#[tokio::test]
async fn a_sealed_backend_is_unavailable() {
    let stub = StubBao::start(vec![Reply::refused(503, &["Vault is sealed"])]);
    let err = stub.backend().get(&path("x")).await.expect_err("should fail");

    assert!(matches!(err, SecretsError::Unavailable(_)), "{err:?}");
    assert!(err.to_string().contains("sealed"), "{err}");
}

#[tokio::test]
async fn an_unclassified_status_never_repeats_the_response_body() {
    let stub = StubBao::start(vec![Reply::verbatim(500, r#"{"data":{"credentials":"hunter2"}}"#)]);
    let err = stub.backend().get(&path("x")).await.expect_err("should fail");

    assert!(matches!(err, SecretsError::Transport(_)), "{err:?}");
    assert!(!err.to_string().contains("hunter2"), "{err}");
}

#[tokio::test]
async fn a_success_that_is_not_json_is_malformed() {
    let stub = StubBao::start(vec![Reply::verbatim(200, "<html>proxy says no</html>")]);
    let err = stub.backend().get(&path("x")).await.expect_err("should fail");

    assert!(matches!(err, SecretsError::Malformed { .. }), "{err:?}");
}

#[tokio::test]
async fn set_wraps_the_map_in_the_kv_v2_data_envelope() {
    let stub = StubBao::start(vec![Reply::written(1)]);
    let mut map = SecretMap::new();
    map.insert(
        SecretKey::parse("credentials").unwrap(),
        SecretString::from("v".to_string()),
    );

    stub.backend().set(&path("mcp/laptop/github"), &map).await.expect("set");

    let seen = stub.only_request();
    assert_eq!(seen.method, "POST");
    assert_eq!(seen.url, "/v1/secret/data/trg/yordis/mcp/laptop/github");
    assert_eq!(seen.body, r#"{"data":{"credentials":"v"}}"#);
}

/// The same bare `404` that means "no such secret" to a read means the
/// write never happened, so the two cannot share an answer.
#[tokio::test]
async fn a_write_that_is_answered_with_a_bare_404_did_not_store_anything() {
    let stub = StubBao::start(vec![Reply::miss()]);
    let mut map = SecretMap::new();
    map.insert(
        SecretKey::parse("credentials").unwrap(),
        SecretString::from("v".to_string()),
    );

    let err = stub
        .backend()
        .set(&path("mcp/laptop/github"), &map)
        .await
        .expect_err("a 404 is not a completed write");

    assert!(matches!(err, SecretsError::Unavailable(_)), "{err}");
    assert!(err.to_string().contains("nothing was stored"), "{err}");
}

/// `metadata` exists to pin the wire shape and nothing reads it, so a
/// response that spells it differently must still hand back the credential
/// rather than being refused as malformed.
#[tokio::test]
async fn a_read_still_yields_the_credential_when_the_metadata_is_partial() {
    let stub = StubBao::start(vec![Reply {
        status: 200,
        body: r#"{"data":{"data":{"credentials":"v"},"metadata":{"version":3}}}"#.to_string(),
    }]);

    let got = stub
        .backend()
        .get(&path("mcp/laptop/github"))
        .await
        .expect("a partial metadata block is not a malformed read")
        .expect("a hit");

    assert_eq!(
        got.get(&SecretKey::parse("credentials").unwrap())
            .map(|v| v.expose_secret()),
        Some("v")
    );
}

/// Deleting what is already absent leaves the caller where it wanted to be.
#[tokio::test]
async fn a_delete_of_something_already_gone_is_not_an_error() {
    let stub = StubBao::start(vec![Reply::miss()]);

    stub.backend().delete(&path("mcp/laptop/github")).await.expect("delete");
}

#[tokio::test]
async fn delete_addresses_metadata_and_accepts_an_empty_204() {
    let stub = StubBao::start(vec![Reply::no_content()]);
    stub.backend().delete(&path("mcp/laptop/github")).await.expect("delete");

    let seen = stub.only_request();
    assert_eq!(seen.method, "DELETE");
    assert_eq!(seen.url, "/v1/secret/metadata/trg/yordis/mcp/laptop/github");
}

/// A prefix may still carry several segments of its own, independently
/// of `owner`, and they have to reach the wire intact.
#[tokio::test]
async fn a_multi_segment_path_prefix_keeps_its_segments() {
    let stub = StubBao::start(vec![Reply::hit(&[("k", "v")])]);
    let mut s = settings(&stub.addr);
    s.path_prefix = "trg/shared".to_string();
    s.machine_id = None;
    s.timeout = Duration::from_secs(2);
    let backend = OpenBaoBackend::new(s).expect("build");

    backend.get(&path("mcp/internal")).await.expect("get").expect("some");

    assert_eq!(
        stub.only_request().url,
        "/v1/secret/data/trg/shared/yordis/mcp/internal"
    );
}

/// `owner` is the segment a templated ACL path matches on, so it has to
/// land between the prefix and `mcp/`.
#[tokio::test]
async fn an_owner_scopes_the_path_to_that_person() {
    let stub = StubBao::start(vec![Reply::hit(&[("k", "v")])]);
    let mut s = settings(&stub.addr);
    s.machine_id = None;
    s.timeout = Duration::from_secs(2);
    let backend = OpenBaoBackend::new(s).expect("build");

    backend.get(&path("mcp/internal")).await.expect("get").expect("some");

    assert_eq!(stub.only_request().url, "/v1/secret/data/trg/yordis/mcp/internal");
}

/// One person on two machines is one owner and two holders, so the two
/// segments have to compose rather than stand in for each other.
///
/// `owner` scopes everything the backend addresses, while `machine_id`
/// scopes only the credential layout, so this goes through
/// [`OpenBaoBackend::credential_path`] the way a caller does rather than
/// naming the path directly.
#[tokio::test]
async fn an_owner_and_a_machine_id_are_independent_segments() {
    let stub = StubBao::start(vec![Reply::hit(&[("k", "v")])]);
    let mut s = settings(&stub.addr);
    s.machine_id = Some("desktop".to_string());
    s.timeout = Duration::from_secs(2);
    let backend = OpenBaoBackend::new(s).expect("build");

    let p = path(&backend.credential_path("internal"));
    backend.get(&p).await.expect("get").expect("some");

    assert_eq!(
        stub.only_request().url,
        "/v1/secret/data/trg/yordis/mcp/desktop/internal"
    );
}

#[test]
fn an_owner_survives_an_empty_path_prefix_without_a_double_slash() {
    let mut s = settings("https://bao.example.com:8200");
    s.path_prefix = String::new();
    let b = OpenBaoBackend::new(s).expect("build");

    assert_eq!(b.storage_prefix(), "yordis");
}

/// The subtree a caller is scoped to is the prefix and the owner together,
/// which is what a templated ACL matches and therefore what `list` must not
/// reach above.
#[test]
fn the_owner_is_part_of_the_subtree_and_not_only_of_the_keys_under_it() {
    let b = backend("https://bao.example.com:8200");
    assert_eq!(b.storage_prefix(), "trg/yordis");
}

#[tokio::test]
async fn list_sorts_the_keys_it_is_given() {
    let stub = StubBao::start(vec![Reply::listed(&["zeta", "alpha", "mid/"])]);
    let keys = stub.backend().list(None).await.expect("list");

    assert_eq!(keys, ["alpha", "mid/", "zeta"]);

    let seen = stub.only_request();
    assert_eq!(seen.method, "LIST");
    assert_eq!(seen.url, "/v1/secret/metadata/trg/yordis");
}

#[tokio::test]
async fn list_of_an_empty_folder_is_empty_not_an_error() {
    let stub = StubBao::start(vec![Reply::miss()]);
    assert!(stub.backend().list(None).await.expect("list").is_empty());
}

#[tokio::test]
async fn the_token_is_re_read_for_every_operation() {
    let dir = tempfile::tempdir().expect("tempdir");
    let token = dir.path().join("token");
    let initial_token = token.clone();
    tokio::task::spawn_blocking(move || {
        std::fs::write(&initial_token, "first").expect("write");
        std::fs::set_permissions(&initial_token, std::fs::Permissions::from_mode(0o600)).expect("chmod");
    })
    .await
    .expect("initialize token fixture");

    let stub = StubBao::start(vec![Reply::miss(), Reply::miss()]);
    let mut s = settings(&stub.addr);
    s.token = TokenSource::File(token.clone());
    let backend = OpenBaoBackend::new(s).expect("build");

    backend.get(&path("x")).await.expect("first get");
    tokio::task::spawn_blocking(move || std::fs::write(&token, "second").expect("rewrite"))
        .await
        .expect("refresh token fixture");
    backend.get(&path("x")).await.expect("second get");

    let tokens: Vec<Option<String>> = stub.requests().into_iter().map(|r| r.token).collect();
    assert_eq!(
        tokens,
        [Some("first".to_string()), Some("second".to_string())],
        "a token refreshed by `bao login` must be picked up without a restart"
    );
}

#[tokio::test]
async fn an_unreachable_backend_fails_instead_of_hanging() {
    // 203.0.113.0/24 is TEST-NET-3, reserved for documentation and
    // guaranteed not to be routed, so this connect can only time out.
    let mut s = settings("https://203.0.113.1:8200");
    s.timeout = Duration::from_millis(300);
    let b = OpenBaoBackend::new(s).expect("build");
    let path = SecretPath::parse("x").expect("parse");

    let started = std::time::Instant::now();
    let err = b.get(&path).await.expect_err("should not succeed");

    assert!(
        started.elapsed() < Duration::from_secs(5),
        "took {:?}",
        started.elapsed()
    );
    assert!(
        matches!(err, SecretsError::Unavailable(_)),
        "should be unavailable, got {err:?}"
    );
}

const SPAN_TOKEN: &str = "hvs.span-test-token-7f3a";
const SPAN_SECRET: &str = "span-test-secret-value-91c2";

fn traced_backend(stub: &StubBao) -> OpenBaoBackend {
    let mut s = settings(&stub.addr);
    s.timeout = Duration::from_secs(2);
    s.token = TokenSource::Var(RawVarSource::Literal(SPAN_TOKEN.to_string()));
    OpenBaoBackend::new(s).expect("build")
}

fn port_of(addr: &str) -> i64 {
    reqwest::Url::parse(addr)
        .expect("stub addr")
        .port()
        .map(i64::from)
        .expect("stub port")
}

#[test]
fn a_read_is_an_http_client_span_naming_only_its_route() {
    let stub = StubBao::start(vec![Reply::hit(&[("api", SPAN_SECRET)])]);
    let backend = traced_backend(&stub);
    let (read, trace) = crate::telemetry::testing::capture(|| current_thread(backend.get(&path("deploy-keys"))));
    assert!(read.expect("get").is_some());

    let name = "GET /v1/{mount}/data/{path}";
    let span = trace.span(name).expect("a span per request");
    assert_eq!(span.span_kind, SpanKind::Client);
    let attribute = |key: &str| trace.attribute(name, key).map(|v| v.to_string());
    assert_eq!(attribute("http.request.method").as_deref(), Some("GET"));
    assert_eq!(attribute("url.template").as_deref(), Some("/v1/{mount}/data/{path}"));
    assert_eq!(attribute("server.address").as_deref(), Some("127.0.0.1"));
    assert_eq!(
        trace.attribute(name, "server.port"),
        Some(opentelemetry::Value::I64(port_of(&stub.addr)))
    );
    assert_eq!(
        trace.attribute(name, "http.response.status_code"),
        Some(opentelemetry::Value::I64(200))
    );
    assert_eq!(attribute("trg.secrets.backend.kind").as_deref(), Some("openbao"));
    assert_eq!(attribute("error.type"), None);

    assert_never_recorded(&trace, &[SPAN_SECRET, SPAN_TOKEN, "deploy-keys", "yordis", &stub.addr]);
}

#[test]
fn a_list_reports_its_nonstandard_method_as_other() {
    let stub = StubBao::start(vec![Reply::listed(&["deploy-keys"])]);
    let backend = traced_backend(&stub);
    let (listed, trace) = crate::telemetry::testing::capture(|| current_thread(backend.list(None)));
    assert_eq!(listed.expect("list"), ["deploy-keys"]);

    let name = "HTTP /v1/{mount}/metadata/{path}";
    let attribute = |key: &str| trace.attribute(name, key).map(|v| v.to_string());
    assert_eq!(attribute("http.request.method").as_deref(), Some("_OTHER"));
    assert_eq!(attribute("http.request.method_original").as_deref(), Some("LIST"));

    assert_never_recorded(&trace, &[SPAN_TOKEN, "yordis"]);
}

#[test]
fn a_refused_request_is_a_failed_span_typed_by_its_status() {
    let stub = StubBao::start(vec![Reply::refused(403, &["permission denied"])]);
    let backend = traced_backend(&stub);
    let (read, trace) = crate::telemetry::testing::capture(|| current_thread(backend.get(&path("deploy-keys"))));
    assert!(matches!(read, Err(SecretsError::Unauthorized { .. })), "{read:?}");

    let name = "GET /v1/{mount}/data/{path}";
    let span = trace.span(name).expect("a span per request");
    assert!(matches!(span.status, Status::Error { .. }), "{:?}", span.status);
    assert_eq!(
        trace.attribute(name, "error.type").map(|v| v.to_string()).as_deref(),
        Some("403")
    );

    assert_never_recorded(&trace, &[SPAN_TOKEN, "deploy-keys"]);
}

/// A `200` whose body is not a KV v2 envelope is only caught once `get`
/// parses it, so the span must not finish until that parse has run.
#[test]
fn a_200_with_an_invalid_envelope_is_a_failed_span() {
    let stub = StubBao::start(vec![Reply::verbatim(200, r#"{"not":"a kv v2 envelope"}"#)]);
    let backend = traced_backend(&stub);
    let (read, trace) = crate::telemetry::testing::capture(|| current_thread(backend.get(&path("x"))));
    assert!(matches!(read, Err(SecretsError::Malformed { .. })), "{read:?}");

    let name = "GET /v1/{mount}/data/{path}";
    let span = trace.span(name).expect("a span per request");
    assert!(matches!(span.status, Status::Error { .. }), "{:?}", span.status);
    assert_eq!(
        trace.attribute(name, "error.type").map(|v| v.to_string()).as_deref(),
        Some("malformed")
    );
    assert_eq!(
        trace.attribute(name, "http.response.status_code"),
        Some(opentelemetry::Value::I64(200))
    );
}

/// Same shape, for a non-string value under an otherwise valid envelope:
/// `exchange` only hands back a parsed `Value`, so the span must still be
/// finished by what `map_from_json` makes of it.
#[test]
fn a_200_with_a_non_string_secret_value_is_a_failed_span() {
    let stub = StubBao::start(vec![Reply::verbatim(
        200,
        r#"{"data":{"data":{"credentials":1},"metadata":{"version":1}}}"#,
    )]);
    let backend = traced_backend(&stub);
    let (read, trace) = crate::telemetry::testing::capture(|| current_thread(backend.get(&path("x"))));
    assert!(matches!(read, Err(SecretsError::Malformed { .. })), "{read:?}");

    let name = "GET /v1/{mount}/data/{path}";
    let span = trace.span(name).expect("a span per request");
    assert!(matches!(span.status, Status::Error { .. }), "{:?}", span.status);
    assert_eq!(
        trace.attribute(name, "error.type").map(|v| v.to_string()).as_deref(),
        Some("malformed")
    );
}

#[test]
fn an_unreachable_backend_types_the_span_by_the_failure() {
    let (server, addr) = bind_loopback();
    drop(server);
    let mut s = settings(&addr);
    s.timeout = Duration::from_millis(500);
    let backend = OpenBaoBackend::new(s).expect("build");
    let (read, trace) = crate::telemetry::testing::capture(|| current_thread(backend.get(&path("x"))));
    assert!(read.is_err());

    let name = "GET /v1/{mount}/data/{path}";
    assert_eq!(trace.attribute(name, "http.response.status_code"), None);
    assert_eq!(
        trace.attribute(name, "error.type").map(|v| v.to_string()).as_deref(),
        Some("unavailable")
    );
}
