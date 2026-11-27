use super::*;
use crate::secrets::mirror::{mirror, MirrorOutcome};
use crate::secrets::SecretKey;
use crate::secrets::{Backend, BackendError, SecretsSection};
use secrecy::ExposeSecret;

const PROJECT: &str = "my-project";
const ACCOUNT: &str = "deployer@my-project.iam.gserviceaccount.com";

fn sh_quote(raw: &str) -> String {
    format!("'{}'", raw.replace('\'', r"'\''"))
}

/// A stand-in for `gcloud` that keeps one secret in files next to itself, so a
/// write can be read back the way the real service would answer.
///
/// The body is the same checked-in shim every CLI stub here uses; per-test
/// behaviour travels in a script the shim reads rather than executes. The
/// stand-in answers `versions access` in the base64 form it was asked for, and
/// keeps the bytes it received on stdin untouched.
struct StubGcloud {
    dir: tempfile::TempDir,
}

const STATEFUL: &str = r#"
case "$1 $2 $3" in
  "secrets versions add")
    [ -f "$0.exists" ] || { echo "NOT_FOUND: Secret [projects/1/secrets/$4] not found." >&2; exit 1; }
    cat > "$0.state" || exit 1
    echo added >> "$0.adds"
    printf 'projects/1/secrets/%s/versions/%s' "$4" "$(wc -l < "$0.adds" | tr -d ' ')"
    exit 0;;
  "secrets versions access")
    [ -f "$0.state" ] || { echo "NOT_FOUND: Secret Version [projects/1/secrets/x/versions/latest] not found." >&2; exit 1; }
    base64 < "$0.state" | tr -d '\n' | tr '+/' '-_'
    exit 0;;
esac
case "$1 $2" in
  "secrets describe")
    [ -f "$0.exists" ] || { echo "NOT_FOUND: Secret [projects/1/secrets/$3] not found." >&2; exit 1; }
    printf 'projects/1/secrets/%s' "$3"
    exit 0;;
  "secrets list") exit 0;;
  "auth list") printf '%s' '[{"account":"a@my-project.iam.gserviceaccount.com","status":"ACTIVE"},{"account":"deployer@my-project.iam.gserviceaccount.com","status":""}]'; exit 0;;
esac
echo "unrouted: $*" >&2
exit 127
"#;

impl StubGcloud {
    fn scripted(body: &str) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let bin = dir.path().join("gcloud");
        std::fs::write(bin.with_extension("body"), body).expect("write body");
        let shim = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/op-stub.sh");
        std::os::unix::fs::symlink(shim, &bin).expect("link stub");
        Self { dir }
    }

    fn answering(exit: i32, stdout: &str, stderr: &str) -> Self {
        Self::scripted(&format!(
            "printf '%s' {}\nprintf '%s' {} >&2\nexit {exit}\n",
            sh_quote(stdout),
            sh_quote(stderr)
        ))
    }

    /// The stateful stand-in with the secret container already created.
    fn with_secret() -> Self {
        let stub = Self::scripted(STATEFUL);
        std::fs::write(stub.file("exists"), "").expect("create");
        stub
    }

    /// The stateful stand-in with no secret at all.
    fn without_secret() -> Self {
        Self::scripted(STATEFUL)
    }

    fn file(&self, ext: &str) -> PathBuf {
        self.dir.path().join(format!("gcloud.body.{ext}"))
    }

    fn put_bytes(&self, bytes: &[u8]) {
        std::fs::write(self.file("state"), bytes).expect("seed");
    }

    fn stored(&self) -> Option<Vec<u8>> {
        std::fs::read(self.file("state")).ok()
    }

    fn adds(&self) -> usize {
        std::fs::read_to_string(self.file("adds"))
            .map(|s| s.lines().count())
            .unwrap_or(0)
    }

    fn backend(&self) -> GcpBackend {
        GcpBackend::new(
            GcpProject::parse(PROJECT).expect("project"),
            GcpAccount::parse(ACCOUNT).expect("account"),
        )
        .with_bin(self.dir.path().join("gcloud"))
    }

    fn argv(&self) -> Vec<String> {
        std::fs::read_to_string(self.dir.path().join("gcloud.argv"))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }
}

fn id(raw: &str) -> GcpSecretId {
    GcpSecretId::parse(raw).expect("secret id")
}

fn secret(raw: &str) -> SecretString {
    SecretString::from(raw.to_string())
}

#[test]
fn a_project_is_an_id_and_not_a_number_or_a_display_name() {
    assert!(GcpProject::parse("my-project").is_ok());
    assert_eq!(GcpProject::parse(" "), Err(GcpProjectError::Empty));
    for bad in [
        "12345678",
        "My Project",
        "-flag-ish",
        "short",
        "ends-with-dash-",
        "UPPER-case-id",
    ] {
        assert!(
            matches!(GcpProject::parse(bad), Err(GcpProjectError::Shape(_))),
            "{bad}"
        );
    }
}

#[test]
fn an_account_cannot_be_read_as_a_flag() {
    assert!(GcpAccount::parse(ACCOUNT).is_ok());
    assert_eq!(GcpAccount::parse(""), Err(GcpAccountError::Empty));
    assert!(matches!(
        GcpAccount::parse("--impersonate"),
        Err(GcpAccountError::Dash(_))
    ));
}

#[test]
fn a_secret_id_is_the_short_name_and_cannot_be_read_as_a_flag() {
    assert!(GcpSecretId::parse("deploy_key-2").is_ok());
    assert_eq!(GcpSecretId::parse(""), Err(GcpSecretIdError::Empty));
    for bad in ["a/b", "a#b", "a b", "projects/p/secrets/s", "a.b"] {
        assert!(
            matches!(GcpSecretId::parse(bad), Err(GcpSecretIdError::Characters(_))),
            "{bad}"
        );
    }
    assert!(matches!(GcpSecretId::parse("-x"), Err(GcpSecretIdError::Dash(_))));
    assert!(matches!(
        GcpSecretId::parse(&"a".repeat(256)),
        Err(GcpSecretIdError::TooLong(_))
    ));
}

#[tokio::test]
async fn a_read_names_the_project_and_the_account_and_never_infers_either() {
    let stub = StubGcloud::with_secret();
    stub.put_bytes(b"v");
    stub.backend().get_latest(&id("tok")).await.expect("read");

    let argv = stub.argv();
    assert_eq!(&argv[..3], ["secrets", "versions", "access"], "{argv:?}");
    for wanted in [
        "latest",
        "--secret=tok",
        "--format=get(payload.data)",
        "--project=my-project",
        &format!("--account={ACCOUNT}"),
        "--quiet",
    ] {
        assert!(argv.iter().any(|a| a == wanted), "{wanted} missing from {argv:?}");
    }
}

/// Every subcommand this backend drives, not only the read: `versions add` sends
/// the value in its request, and the others are cheap to cover alongside.
#[tokio::test]
async fn no_call_lets_gcloud_log_http_traffic() {
    let id = id("tok");
    let calls: [(&str, StubGcloud); 5] = [
        ("read", StubGcloud::with_secret()),
        ("add", StubGcloud::with_secret()),
        ("describe", StubGcloud::with_secret()),
        ("probe", StubGcloud::with_secret()),
        ("accounts", StubGcloud::with_secret()),
    ];
    for (name, stub) in calls {
        let backend = stub.backend();
        match name {
            "read" => drop(backend.get_latest(&id).await),
            "add" => drop(backend.add_version(&id, &secret("v")).await),
            "describe" => drop(backend.secret_exists(&id).await),
            "probe" => drop(backend.probe_project().await),
            _ => drop(backend.accounts().await),
        }
        let argv = stub.argv();
        assert!(!argv.is_empty(), "{name} never reached gcloud");
        assert!(argv.iter().any(|a| a == "--no-log-http"), "{name}: {argv:?}");
        assert!(!argv.iter().any(|a| a == "--log-http"), "{name}: {argv:?}");
    }
}

#[tokio::test]
async fn a_read_returns_the_stored_bytes_including_a_trailing_newline() {
    let stub = StubGcloud::with_secret();
    let pem = "-----BEGIN PRIVATE KEY-----\nQUJD\n-----END PRIVATE KEY-----\n";
    stub.put_bytes(pem.as_bytes());
    let got = stub
        .backend()
        .get_latest(&id("tok"))
        .await
        .expect("read")
        .expect("a value");
    assert_eq!(got.expose_secret(), pem);
}

#[tokio::test]
async fn a_payload_that_is_not_base64_is_malformed_and_is_not_echoed() {
    let stub = StubGcloud::answering(0, "hunter2 not base64!!", "");
    let err = stub.backend().get_latest(&id("tok")).await.expect_err("refused");
    assert!(matches!(err, SecretsError::Malformed { .. }), "{err}");
    assert!(!err.to_string().contains("hunter2"), "{err}");
}

#[tokio::test]
async fn a_value_that_is_not_utf8_is_malformed() {
    // 0xff 0xfe in URL-safe base64.
    let stub = StubGcloud::answering(0, "__4=", "");
    let err = stub.backend().get_latest(&id("tok")).await.expect_err("refused");
    assert!(matches!(err, SecretsError::Malformed { .. }), "{err}");
}

#[tokio::test]
async fn an_unpadded_payload_decodes() {
    let stub = StubGcloud::answering(0, "YWJjZA", "");
    let got = stub
        .backend()
        .get_latest(&id("tok"))
        .await
        .expect("read")
        .expect("a value");
    assert_eq!(got.expose_secret(), "abcd");
}

#[tokio::test]
async fn nothing_to_read_is_none_and_a_refusal_is_an_error() {
    let missing = StubGcloud::answering(1, "", "ERROR: NOT_FOUND: Secret Version [x] not found.");
    assert!(missing.backend().get_latest(&id("tok")).await.expect("read").is_none());

    let denied = StubGcloud::answering(
        1,
        "",
        "ERROR: (gcloud) PERMISSION_DENIED: Permission 'secretmanager.versions.access' denied",
    );
    let err = denied.backend().get_latest(&id("tok")).await.expect_err("denied");
    assert!(matches!(err, SecretsError::PermissionDenied(_)), "{err}");

    let other = StubGcloud::answering(1, "", "ERROR: the network is down");
    let err = other.backend().get_latest(&id("tok")).await.expect_err("failed");
    assert!(matches!(err, SecretsError::Unavailable(_)), "{err}");
}

#[tokio::test]
async fn a_latest_version_that_is_disabled_or_destroyed_is_nothing_to_read() {
    for state in ["DISABLED", "DESTROYED"] {
        let stub = StubGcloud::answering(
            1,
            "",
            &format!(
                "ERROR: (gcloud.secrets.versions.access) FAILED_PRECONDITION: Secret Version [projects/1/secrets/tok/versions/3] is in {state} state."
            ),
        );
        assert!(
            stub.backend().get_latest(&id("tok")).await.expect("read").is_none(),
            "{state}"
        );
    }

    // Any other precondition failure is not a statement about the version.
    let other = StubGcloud::answering(1, "", "ERROR: FAILED_PRECONDITION: the project is suspended");
    let err = other.backend().get_latest(&id("tok")).await.expect_err("failed");
    assert!(matches!(err, SecretsError::Unavailable(_)), "{err}");
}

#[tokio::test]
async fn a_version_is_added_from_stdin_and_the_value_never_reaches_argv() {
    let stub = StubGcloud::with_secret();
    stub.backend()
        .add_version(&id("tok"), &secret("s3cr3t-value"))
        .await
        .expect("add");

    assert_eq!(stub.stored().as_deref(), Some(b"s3cr3t-value".as_slice()));
    let argv = stub.argv();
    assert_eq!(&argv[..3], ["secrets", "versions", "add"], "{argv:?}");
    assert!(argv.iter().any(|a| a == "--data-file=-"), "{argv:?}");
    assert!(argv.iter().any(|a| a == "--project=my-project"), "{argv:?}");
    assert!(!argv.iter().any(|a| a.contains("s3cr3t-value")), "{argv:?}");
}

#[tokio::test]
async fn a_value_ending_in_a_newline_is_written_byte_for_byte() {
    let stub = StubGcloud::with_secret();
    let pem = "-----BEGIN PRIVATE KEY-----\nQUJD\n-----END PRIVATE KEY-----\n";
    stub.backend().add_version(&id("tok"), &secret(pem)).await.expect("add");
    assert_eq!(stub.stored().as_deref(), Some(pem.as_bytes()));

    let back = stub
        .backend()
        .get_latest(&id("tok"))
        .await
        .expect("read")
        .expect("a value");
    assert_eq!(back.expose_secret(), pem);
}

/// `gcloud` refuses a missing secret without reading stdin. A value larger
/// than a pipe buffer makes the write outlast it every time, so the broken
/// pipe that follows is certain rather than a race, and the answer must still
/// be the missing secret.
#[tokio::test]
async fn a_gcloud_that_exits_without_reading_the_value_is_judged_by_what_it_said() {
    // The Rust runtime ignores SIGPIPE, but an `exec` that fails inside this
    // test process (the `commands::exec` tests) puts the default disposition
    // back for every thread, and the write below would then kill the whole
    // test binary with 141 instead of failing with `BrokenPipe`.
    // SAFETY: `signal` with `SIG_IGN` is async-signal-safe and only sets the
    // disposition the runtime installs at startup.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
    }
    let stub = StubGcloud::without_secret();
    let large = "v".repeat(1 << 20);
    let err = stub
        .backend()
        .add_version(&id("tok"), &secret(&large))
        .await
        .expect_err("missing");
    assert!(matches!(&err, SecretsError::MissingContainer { .. }), "{err}");
}

#[tokio::test]
async fn a_missing_secret_is_never_created_and_is_said_so() {
    let stub = StubGcloud::without_secret();
    let err = stub
        .backend()
        .add_version(&id("tok"), &secret("v"))
        .await
        .expect_err("missing");
    assert!(
        matches!(&err, SecretsError::MissingContainer { project, secret } if project == PROJECT && secret == "tok"),
        "{err}"
    );
    assert!(err.to_string().contains("never creates"), "{err}");
    assert!(
        !stub.argv().iter().any(|a| a == "create"),
        "no create call is made: {:?}",
        stub.argv()
    );
}

#[tokio::test]
async fn existence_is_told_apart_from_having_a_version() {
    let there = StubGcloud::with_secret();
    assert!(there.backend().secret_exists(&id("tok")).await.expect("describe"));
    assert!(there.backend().get_latest(&id("tok")).await.expect("read").is_none());

    let gone = StubGcloud::without_secret();
    assert!(!gone.backend().secret_exists(&id("tok")).await.expect("describe"));
}

#[tokio::test]
async fn accounts_are_read_from_gcloud() {
    let stub = StubGcloud::with_secret();
    let known = stub.backend().accounts().await.expect("accounts");
    assert_eq!(known.len(), 2);
    assert_eq!(known[1].account, ACCOUNT);
    assert_eq!(&stub.argv()[..2], ["auth", "list"]);
}

#[tokio::test]
async fn the_project_probe_lists_at_most_one_secret() {
    let stub = StubGcloud::with_secret();
    stub.backend().probe_project().await.expect("probe");
    assert!(stub.argv().iter().any(|a| a == "--limit=1"), "{:?}", stub.argv());

    let denied = StubGcloud::answering(1, "", "ERROR: PERMISSION_DENIED: nope");
    assert!(matches!(
        denied.backend().probe_project().await,
        Err(SecretsError::PermissionDenied(_))
    ));
    let down = StubGcloud::answering(1, "", "ERROR: project not usable");
    assert!(matches!(
        down.backend().probe_project().await,
        Err(SecretsError::Unavailable(_))
    ));
}

#[tokio::test]
async fn a_hung_gcloud_is_bounded_by_the_timeout() {
    // `exec` so the timeout kills `sleep` itself. A forked `sleep` outlives the
    // killed shell as an orphan, and tarpaulin aborts the run when it sees one.
    let stub = StubGcloud::scripted("exec sleep 300\n");
    let backend = stub.backend().with_timeout(Duration::from_millis(200));
    let err = backend.get_latest(&id("tok")).await.expect_err("timed out");
    assert!(matches!(err, SecretsError::Transport(_)), "{err}");
    assert!(err.to_string().contains("timed out"), "{err}");
}

/// A `gcloud` that never reads stdin leaves a value larger than the pipe
/// unwritten, so the write itself has to be inside the timeout. The outer
/// timeout turns a regression into a failure instead of a hung suite.
#[tokio::test]
async fn a_gcloud_that_never_reads_the_value_is_bounded_by_the_timeout() {
    let stub = StubGcloud::scripted("exec sleep 300\n");
    let backend = stub.backend().with_timeout(Duration::from_millis(200));
    let large = "v".repeat(1 << 22);
    let outcome = tokio::time::timeout(
        Duration::from_secs(20),
        backend.add_version(&id("tok"), &secret(&large)),
    )
    .await
    .expect("the invocation timeout must fire while the value is still being written");
    let err = outcome.expect_err("timed out");
    assert!(matches!(err, SecretsError::Transport(_)), "{err}");
    assert!(err.to_string().contains("timed out"), "{err}");
}

#[test]
fn a_backend_is_declared_with_a_project_and_an_account() {
    let section: SecretsSection = toml::from_str(&format!(
        "[backends.gsm]\nkind = \"gcp_secret_manager\"\nproject = \"{PROJECT}\"\naccount = \"{ACCOUNT}\"\n"
    ))
    .expect("section");
    assert_eq!(section.kind_of("gsm").map(|k| k.as_str()), Some("gcp_secret_manager"));
}

#[test]
fn a_backend_without_an_account_is_refused() {
    let err = toml::from_str::<SecretsSection>(&format!(
        "[backends.gsm]\nkind = \"gcp_secret_manager\"\nproject = \"{PROJECT}\"\n"
    ))
    .expect_err("refused");
    assert!(err.to_string().contains("account"), "{err}");
}

#[test]
fn a_backend_without_a_project_is_refused() {
    let err = toml::from_str::<SecretsSection>("[backends.gsm]\nkind = \"gcp_secret_manager\"\n").expect_err("refused");
    assert!(err.to_string().contains("project"), "{err}");
}

#[test]
fn an_unknown_field_on_a_backend_is_refused() {
    let err = toml::from_str::<SecretsSection>(&format!(
        "[backends.gsm]\nkind = \"gcp_secret_manager\"\nproject = \"{PROJECT}\"\nregion = \"x\"\n"
    ))
    .expect_err("refused");
    assert!(err.to_string().contains("region"), "{err}");
}

// Mirroring into Secret Manager, against the stateful stand-in.

const MIRROR_HEAD: &str = r#"
    [backends.src]
    kind = "keychain"

    [backends.gsm]
    kind = "gcp_secret_manager"
    project = "my-project"
    account = "deployer@my-project.iam.gserviceaccount.com"

    [mirrors.deploy]
    source = { backend = "src", path = "deploy", key = "token" }
"#;

fn mirror_to(target: &str) -> String {
    format!("{MIRROR_HEAD}    targets = [{target}]\n")
}

struct Mirrored {
    source: crate::secrets::fake::FakeBackend,
    stub: StubGcloud,
}

impl Mirrored {
    async fn new(value: &str, stub: StubGcloud) -> Self {
        let source = crate::secrets::fake::FakeBackend::new();
        let mut map = crate::secrets::SecretMap::new();
        map.insert(SecretKey::parse("token").expect("key"), secret(value));
        source
            .set(&SecretPath::parse("deploy").expect("path"), &map)
            .await
            .expect("seed");
        Self { source, stub }
    }

    async fn run(&self, apply: bool) -> Result<MirrorOutcome, String> {
        let section: SecretsSection =
            toml::from_str(&mirror_to(r#"{ backend = "gsm", secret = "deploy-token" }"#)).expect("section");
        let declared = section.resolve_mirrors().expect("mirrors").remove(0);
        let gcp = self.stub.backend();
        let resolve = |name: &str| -> Result<Backend, BackendError> {
            Ok(match name {
                "src" => Backend::Fake(self.source.clone()),
                _ => Backend::Gcp(gcp.clone()),
            })
        };
        let mut outcomes = mirror(&resolve, &declared, apply).await.map_err(|e| e.to_string())?;
        outcomes.remove(0).map_err(|e| e.to_string())
    }
}

#[tokio::test]
async fn a_mirror_with_nothing_stored_yet_adds_the_first_version() {
    let world = Mirrored::new("v1", StubGcloud::with_secret()).await;
    assert_eq!(world.run(false).await, Ok(MirrorOutcome::WouldCreate));
    assert_eq!(world.stub.adds(), 0, "a dry run writes nothing");
    assert_eq!(world.run(true).await, Ok(MirrorOutcome::Created));
    assert_eq!(world.stub.stored().as_deref(), Some(b"v1".as_slice()));
}

/// The secret's only version is disabled, so `latest` cannot be read. That is
/// the same as having nothing yet, and a new version is what fills it.
#[tokio::test]
async fn a_mirror_fills_a_secret_whose_only_version_is_disabled() {
    let stub = StubGcloud::scripted(
        r#"
case "$1 $2 $3" in
  "secrets versions add") cat > "$0.state"; printf 'projects/1/secrets/x/versions/2'; exit 0;;
  "secrets versions access")
    [ -f "$0.state" ] || { echo "ERROR: FAILED_PRECONDITION: Secret Version [projects/1/secrets/x/versions/1] is in DISABLED state." >&2; exit 1; }
    base64 < "$0.state" | tr -d '\n' | tr '+/' '-_'; exit 0;;
esac
case "$1 $2" in
  "secrets describe") printf x; exit 0;;
esac
exit 127
"#,
    );
    let world = Mirrored::new("v1", stub).await;
    assert_eq!(world.run(false).await, Ok(MirrorOutcome::WouldCreate));
    assert_eq!(world.run(true).await, Ok(MirrorOutcome::Created));
    assert_eq!(world.stub.stored().as_deref(), Some(b"v1".as_slice()));
}

#[tokio::test]
async fn a_mirror_that_agrees_writes_nothing() {
    let world = Mirrored::new("v1", StubGcloud::with_secret()).await;
    world.stub.put_bytes(b"v1");
    assert_eq!(world.run(true).await, Ok(MirrorOutcome::UpToDate));
    assert_eq!(world.stub.adds(), 0);
}

#[tokio::test]
async fn a_mirror_that_differs_adds_a_version_and_reads_it_back() {
    let world = Mirrored::new("v2", StubGcloud::with_secret()).await;
    world.stub.put_bytes(b"v1");
    assert_eq!(world.run(false).await, Ok(MirrorOutcome::WouldUpdate));
    assert_eq!(world.stub.adds(), 0);
    assert_eq!(world.run(true).await, Ok(MirrorOutcome::Updated));
    assert_eq!(world.stub.adds(), 1);
    assert_eq!(world.stub.stored().as_deref(), Some(b"v2".as_slice()));
}

#[tokio::test]
async fn a_trailing_newline_is_a_difference_for_a_store_that_keeps_bytes() {
    let pem = "-----BEGIN PRIVATE KEY-----\nQUJD\n-----END PRIVATE KEY-----\n";
    let world = Mirrored::new(pem, StubGcloud::with_secret()).await;
    world.stub.put_bytes(pem.trim_end_matches('\n').as_bytes());
    assert_eq!(world.run(true).await, Ok(MirrorOutcome::Updated));
    assert_eq!(world.stub.stored().as_deref(), Some(pem.as_bytes()));
    assert_eq!(world.run(true).await, Ok(MirrorOutcome::UpToDate));
    assert_eq!(world.stub.adds(), 1);
}

#[tokio::test]
async fn a_missing_secret_is_an_error_even_in_a_dry_run() {
    let world = Mirrored::new("v1", StubGcloud::without_secret()).await;
    for apply in [false, true] {
        let err = world.run(apply).await.expect_err("missing");
        assert!(err.contains("does not exist"), "{err}");
        assert!(err.contains("never creates"), "{err}");
    }
    assert_eq!(world.stub.adds(), 0);
}

#[tokio::test]
async fn a_write_that_reads_back_differently_fails() {
    // Accepts the add, and keeps serving an older value.
    let stub = StubGcloud::scripted(
        r#"
case "$1 $2 $3" in
  "secrets versions add") cat > /dev/null; printf 'projects/1/secrets/x/versions/2'; exit 0;;
  "secrets versions access") printf 'b2xk'; exit 0;;
esac
case "$1 $2" in
  "secrets describe") printf x; exit 0;;
esac
exit 127
"#,
    );
    let world = Mirrored::new("v1", stub).await;
    let err = world.run(true).await.expect_err("mismatch");
    assert!(err.contains("did not read back"), "{err}");
    assert!(!err.contains("v1"), "{err}");
}

/// `latest` is an alias and may keep answering with the previous value after
/// a version was added, which is not a failed write. Only the new version's own
/// number is promised to read back.
#[tokio::test]
async fn a_lagging_latest_alias_is_not_a_failed_write() {
    let stub = StubGcloud::scripted(
        r#"
case "$1 $2 $3" in
  "secrets versions add") cat > /dev/null; printf 'projects/1/secrets/x/versions/2'; exit 0;;
  "secrets versions access")
    case "$4" in
      latest) printf 'b2xk'; exit 0;;
      2) printf 'djE'; exit 0;;
    esac;;
esac
case "$1 $2" in
  "secrets describe") printf x; exit 0;;
esac
exit 127
"#,
    );
    let world = Mirrored::new("v1", stub).await;
    assert_eq!(world.run(true).await, Ok(MirrorOutcome::Updated));
}

#[tokio::test]
async fn a_version_gcloud_does_not_name_is_unknown_and_not_a_guess() {
    let stub = StubGcloud::scripted(
        r#"
case "$1 $2 $3" in
  "secrets versions add") cat > /dev/null; printf 'Created'; exit 0;;
  "secrets versions access") printf 'b2xk'; exit 0;;
esac
exit 127
"#,
    );
    let added = stub
        .backend()
        .add_version(&id("tok"), &secret("v"))
        .await
        .expect("added");
    assert_eq!(added, None);

    let world = Mirrored::new("v1", stub).await;
    let err = world.run(true).await.expect_err("unverifiable");
    assert!(err.contains("did not say which version"), "{err}");
}

#[test]
fn a_secret_target_is_addressed_by_secret_and_a_key_is_refused_at_parse() {
    for target in [
        r#"{ backend = "gsm", secret = "deploy-token", key = "token" }"#,
        r#"{ backend = "gsm", secret = "deploy-token", path = "deploy-token" }"#,
        r#"{ backend = "gsm", path = "deploy-token" }"#,
    ] {
        let err = toml::from_str::<SecretsSection>(&mirror_to(target))
            .expect_err("refused while parsing")
            .message()
            .to_string();
        assert!(err.contains("backend `gsm`"), "{err}");
    }
}

#[test]
fn path_and_key_are_refused_for_a_secret_target_at_resolve() {
    let section: SecretsSection = toml::from_str(&mirror_to(
        r#"{ backend = "gsm", path = "deploy-token", key = "token" }"#,
    ))
    .expect("section");
    let err = section.resolve_mirrors().expect_err("refused").to_string();
    assert!(err.contains("addressed with `secret`, not `path` and `key`"), "{err}");
}

#[test]
fn a_reference_is_refused_for_a_secret_target() {
    let with_ref = mirror_to(r#"{ backend = "gsm", ref = "op://v/i/f" }"#);
    let section: SecretsSection = toml::from_str(&with_ref).expect("section");
    assert!(section.resolve_mirrors().is_err());
}

#[test]
fn a_secret_cannot_be_a_target_of_two_mirrors() {
    let twice = format!(
        "{}\n[mirrors.again]\nsource = {{ backend = \"src\", path = \"deploy\", key = \"token\" }}\n\
         targets = [{{ backend = \"gsm\", secret = \"deploy-token\" }}]\n",
        mirror_to(r#"{ backend = "gsm", secret = "deploy-token" }"#)
    );
    let section: SecretsSection = toml::from_str(&twice).expect("section");
    let err = section.resolve_mirrors().expect_err("refused").to_string();
    assert!(err.contains("already a target"), "{err}");
}
