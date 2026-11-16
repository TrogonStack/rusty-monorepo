//! `trg secret mirror` into an OpenBao Transit key, through the binary.
//!
//! The OpenBao here is an in-process HTTP server that behaves the way the real
//! one documents: it publishes a real RSA-4096 wrapping key, refuses to import
//! over an existing key, only versions a key that was itself imported, and
//! unwraps what it is sent (RSA-OAEP-SHA256 around an AES-256 key, AES-KWP
//! around the PKCS#8 document) so a test can check that the key it ends up
//! holding is the one that was mirrored. Every key is freshly generated.

#![expect(
    clippy::unwrap_used,
    reason = "an integration test is test code end to end, so a failed setup step should fail the test"
)]

use std::net::SocketAddr;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, Mutex};

use assert_cmd::prelude::*;
use aws_lc_rs::encoding::{AsDer, PublicKeyX509Der};
use aws_lc_rs::key_wrap::{AesKek, KeyWrapPadded, AES_256};
use aws_lc_rs::rand::SystemRandom;
use aws_lc_rs::rsa::{KeySize, OaepPrivateDecryptingKey, PrivateDecryptingKey, OAEP_SHA256_MGF1SHA256};
use aws_lc_rs::signature::{Ed25519KeyPair, KeyPair as _, RsaKeyPair};
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;

const TOKEN: &str = "test-token";
const TRANSIT_KEY: &str = "git-personal";
const WRAPPING_BYTES: usize = 512;

struct Held {
    key_type: String,
    pkcs8: Vec<u8>,
    imported: bool,
    versions: u32,
}

#[derive(Default)]
struct State {
    source: String,
    held: Option<Held>,
    imports: Vec<serde_json::Value>,
    import_versions: usize,
    wrapping_fetches: usize,
}

struct FakeOpenBao {
    addr: SocketAddr,
    state: Arc<Mutex<State>>,
    server: Arc<tiny_http::Server>,
}

fn der(tag: u8, body: &[u8]) -> Vec<u8> {
    let mut out = vec![tag];
    if body.len() < 0x80 {
        out.push(u8::try_from(body.len()).unwrap());
    } else {
        let bytes = body.len().to_be_bytes();
        let skip = bytes.iter().take_while(|&&b| b == 0).count();
        out.push(0x80 | u8::try_from(bytes.len() - skip).unwrap());
        out.extend_from_slice(&bytes[skip..]);
    }
    out.extend_from_slice(body);
    out
}

fn pem(label: &str, document: &[u8]) -> String {
    let encoded = BASE64.encode(document);
    let mut out = format!("-----BEGIN {label}-----\n");
    for line in encoded.as_bytes().chunks(64) {
        out.push_str(std::str::from_utf8(line).unwrap());
        out.push('\n');
    }
    out.push_str(&format!("-----END {label}-----\n"));
    out
}

/// What `GET keys/<name>` reports as the public key of a held PKCS#8 key.
fn transit_public_key(key_type: &str, pkcs8: &[u8]) -> String {
    if key_type == "ed25519" {
        let pair = Ed25519KeyPair::from_pkcs8_maybe_unchecked(pkcs8).unwrap();
        return BASE64.encode(pair.public_key().as_ref());
    }
    let pair = RsaKeyPair::from_pkcs8(pkcs8).unwrap();
    let mut bits = vec![0u8];
    bits.extend_from_slice(pair.public_key().as_ref());
    let algorithm = der(
        0x30,
        &[
            der(0x06, &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x01]),
            der(0x05, &[]),
        ]
        .concat(),
    );
    pem("PUBLIC KEY", &der(0x30, &[algorithm, der(0x03, &bits)].concat()))
}

/// The key material Transit would recover from an import's `ciphertext`.
fn unwrap(private: &OaepPrivateDecryptingKey, ciphertext: &str) -> Vec<u8> {
    let raw = BASE64.decode(ciphertext).unwrap();
    assert!(
        raw.len() > WRAPPING_BYTES,
        "the ciphertext is shorter than the RSA block"
    );
    let mut ephemeral = vec![0u8; WRAPPING_BYTES];
    let ephemeral = private
        .decrypt(&OAEP_SHA256_MGF1SHA256, &raw[..WRAPPING_BYTES], &mut ephemeral, None)
        .unwrap();
    assert_eq!(ephemeral.len(), 32, "the ephemeral key must be AES-256");
    let kek = AesKek::new(&AES_256, ephemeral).unwrap();
    let mut out = vec![0u8; raw.len() - WRAPPING_BYTES];
    kek.unwrap_with_padding(&raw[WRAPPING_BYTES..], &mut out)
        .unwrap()
        .to_vec()
}

fn json(status: u16, body: serde_json::Value) -> (u16, String) {
    (status, body.to_string())
}

fn refusal(message: &str) -> (u16, String) {
    json(400, serde_json::json!({ "errors": [message] }))
}

impl FakeOpenBao {
    fn start(source: &str) -> Self {
        let private = PrivateDecryptingKey::generate(KeySize::Rsa4096).unwrap();
        let wrapping_pem = pem(
            "PUBLIC KEY",
            AsDer::<PublicKeyX509Der>::as_der(&private.public_key())
                .unwrap()
                .as_ref(),
        );
        let private = Arc::new(OaepPrivateDecryptingKey::new(private).unwrap());
        let state = Arc::new(Mutex::new(State {
            source: source.to_string(),
            ..State::default()
        }));
        let server = Arc::new(tiny_http::Server::http("127.0.0.1:0").unwrap());
        let addr = server.server_addr().to_ip().unwrap();
        let (worker_server, worker_state) = (Arc::clone(&server), Arc::clone(&state));
        std::thread::spawn(move || {
            for mut request in worker_server.incoming_requests() {
                let authorized = request
                    .headers()
                    .iter()
                    .any(|h| h.field.equiv("X-Vault-Token") && h.value.as_str() == TOKEN);
                let url = request.url().to_string();
                let method = request.method().clone();
                let mut text = String::new();
                request.as_reader().read_to_string(&mut text).unwrap();
                let body: serde_json::Value = serde_json::from_str(&text).unwrap_or(serde_json::Value::Null);
                let mut state = worker_state.lock().unwrap();
                let key_route = format!("/v1/transit/keys/{TRANSIT_KEY}");
                let (status, answer) = if !authorized {
                    json(403, serde_json::json!({ "errors": ["permission denied"] }))
                } else if url == "/v1/secret/data/trg/me/keys/git" {
                    json(
                        200,
                        serde_json::json!({"data": {
                            "data": {"ssh_private_key": state.source},
                            "metadata": {"version": 1},
                        }}),
                    )
                } else if url == "/v1/transit/wrapping_key" {
                    state.wrapping_fetches += 1;
                    json(200, serde_json::json!({"data": {"public_key": wrapping_pem}}))
                } else if url == key_route && method == tiny_http::Method::Get {
                    match &state.held {
                        None => json(404, serde_json::json!({"errors": []})),
                        Some(held) => json(
                            200,
                            serde_json::json!({"data": {
                                "type": held.key_type,
                                "latest_version": held.versions,
                                "keys": {held.versions.to_string(): {
                                    "public_key": transit_public_key(&held.key_type, &held.pkcs8),
                                }},
                            }}),
                        ),
                    }
                } else if url == format!("{key_route}/import") {
                    if state.held.is_some() {
                        refusal(
                            "the import path cannot be used with an existing key; use import-version to rotate an existing imported key",
                        )
                    } else {
                        let pkcs8 = unwrap(&private, body["ciphertext"].as_str().unwrap());
                        let mut recorded = body.clone();
                        recorded["ciphertext"] = serde_json::Value::Null;
                        state.imports.push(recorded);
                        state.held = Some(Held {
                            key_type: body["type"].as_str().unwrap().to_string(),
                            pkcs8,
                            imported: true,
                            versions: 1,
                        });
                        (204, String::new())
                    }
                } else if url == format!("{key_route}/import_version") {
                    let pkcs8 = unwrap(&private, body["ciphertext"].as_str().unwrap());
                    state.import_versions += 1;
                    match state.held.as_mut() {
                        None => refusal(
                            "no key found with name git-personal; to import a new key, use the import/ endpoint",
                        ),
                        Some(held) if !held.imported => {
                            refusal("the import_version endpoint can only be used with an imported key")
                        }
                        Some(held) => {
                            held.pkcs8 = pkcs8;
                            held.versions += 1;
                            (204, String::new())
                        }
                    }
                } else {
                    json(404, serde_json::json!({"errors": []}))
                };
                drop(state);
                request
                    .respond(tiny_http::Response::from_string(answer).with_status_code(status))
                    .unwrap();
            }
        });
        Self { addr, state, server }
    }

    /// A Transit key that OpenBao generated itself, which `import_version` refuses.
    fn hold_native_ed25519(&self) {
        let document = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).unwrap();
        self.state.lock().unwrap().held = Some(Held {
            key_type: "ed25519".to_string(),
            pkcs8: document.as_ref().to_vec(),
            imported: false,
            versions: 1,
        });
    }

    fn set_source(&self, source: &str) {
        self.state.lock().unwrap().source = source.to_string();
    }

    fn held(&self) -> Option<(String, Vec<u8>, u32)> {
        let state = self.state.lock().unwrap();
        state
            .held
            .as_ref()
            .map(|held| (held.key_type.clone(), held.pkcs8.clone(), held.versions))
    }

    fn imports(&self) -> Vec<serde_json::Value> {
        self.state.lock().unwrap().imports.clone()
    }

    fn import_versions(&self) -> usize {
        self.state.lock().unwrap().import_versions
    }

    fn wrapping_fetches(&self) -> usize {
        self.state.lock().unwrap().wrapping_fetches
    }
}

impl Drop for FakeOpenBao {
    fn drop(&mut self) {
        self.server.unblock();
    }
}

struct SshKey {
    pem: String,
    public_blob: Vec<u8>,
}

impl SshKey {
    fn generate(dir: &Path, name: &str, kind: &[&str]) -> Self {
        let private = dir.join(name);
        let status = Command::new("ssh-keygen")
            .args(["-q", "-N", "", "-C", "fixture"])
            .args(kind)
            .arg("-f")
            .arg(&private)
            .status()
            .unwrap();
        assert!(status.success(), "ssh-keygen could not generate a {kind:?} key");
        let public = std::fs::read_to_string(dir.join(format!("{name}.pub"))).unwrap();
        Self {
            pem: std::fs::read_to_string(&private).unwrap(),
            public_blob: BASE64.decode(public.split_whitespace().nth(1).unwrap()).unwrap(),
        }
    }

    fn ed25519(dir: &Path, name: &str) -> Self {
        Self::generate(dir, name, &["-t", "ed25519"])
    }

    /// The base64 body of the OpenSSH container, which must never be printed.
    fn body(&self) -> String {
        self.pem.lines().filter(|l| !l.starts_with("-----")).collect()
    }
}

struct Fixture {
    dir: tempfile::TempDir,
    config_home: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let config_home = dir.path().join("config");
        std::fs::create_dir_all(config_home.join("trg")).unwrap();
        std::fs::write(dir.path().join("token"), TOKEN).unwrap();
        std::fs::set_permissions(dir.path().join("token"), std::fs::Permissions::from_mode(0o600)).unwrap();
        Self { dir, config_home }
    }

    fn write_config(&self, text: &str) {
        std::fs::write(self.config_home.join("trg/config.toml"), text).unwrap();
    }

    fn openbao_config(&self, addr: SocketAddr, mirror: &str) {
        self.write_config(&format!(
            "[secrets.backends.bao]\nkind = \"openbao\"\naddr = \"http://{addr}\"\nmount = \"secret\"\n\
             path_prefix = \"trg\"\nowner = \"me\"\ntoken_file = \"{}\"\ntimeout_ms = 5000\n\n{mirror}",
            self.dir.path().join("token").display()
        ));
    }

    fn mirror(&self, args: &[&str]) -> Output {
        Command::cargo_bin("trg")
            .unwrap()
            .args(["secret", "mirror"])
            .args(args)
            .env("XDG_CONFIG_HOME", &self.config_home)
            .stdin(Stdio::null())
            .output()
            .unwrap()
    }
}

const MIRROR: &str = "[secrets.mirrors.git-key]\n\
    source = { backend = \"bao\", path = \"keys/git\", key = \"ssh_private_key\" }\n\
    targets = [{ backend = \"bao\", transit_key = \"git-personal\" }]\n";

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn everything(output: &Output) -> String {
    format!("{}{}", stdout(output), stderr(output))
}

fn assert_no_key_material(output: &Output, key: &SshKey, held: Option<&[u8]>) {
    let printed = everything(output);
    let body = key.body();
    assert!(!printed.contains(&body[..40]), "the key's OpenSSH body was printed");
    assert!(
        !printed.contains("PRIVATE KEY"),
        "a private key armor was printed: {printed}"
    );
    if let Some(pkcs8) = held {
        assert!(
            !printed.contains(&BASE64.encode(pkcs8)),
            "the PKCS#8 document was printed"
        );
    }
}

#[test]
fn an_ed25519_key_is_imported_unextractable_and_is_the_key_that_was_mirrored() {
    let fixture = Fixture::new();
    let key = SshKey::ed25519(fixture.dir.path(), "id");
    let bao = FakeOpenBao::start(&key.pem);
    fixture.openbao_config(bao.addr, MIRROR);

    let output = fixture.mirror(&["apply"]);

    assert!(output.status.success(), "{}", everything(&output));
    let text = stdout(&output);
    assert!(
        text.contains("created") && text.contains("git-key -> transit/keys/git-personal (bao)"),
        "{text}"
    );
    let (key_type, pkcs8, versions) = bao.held().expect("the key was imported");
    assert_eq!((key_type.as_str(), versions), ("ed25519", 1));
    let held = Ed25519KeyPair::from_pkcs8_maybe_unchecked(&pkcs8).unwrap();
    let mut blob = &key.public_blob[..];
    let _name = trg::config::signers::read_string(&mut blob).unwrap();
    let public = trg::config::signers::read_string(&mut blob).unwrap();
    assert_eq!(held.public_key().as_ref(), public, "Transit holds a different key");

    let imports = bao.imports();
    assert_eq!(imports.len(), 1);
    assert_eq!(imports[0]["type"], "ed25519");
    assert_eq!(imports[0]["exportable"], false);
    assert_eq!(imports[0]["allow_plaintext_backup"], false);
    assert_eq!(imports[0]["hash_function"], "SHA256");
    assert_no_key_material(&output, &key, Some(&pkcs8));
}

#[test]
fn an_rsa_key_is_imported_with_its_size_as_the_type() {
    let fixture = Fixture::new();
    let key = SshKey::generate(fixture.dir.path(), "rsa", &["-t", "rsa", "-b", "2048"]);
    let bao = FakeOpenBao::start(&key.pem);
    fixture.openbao_config(bao.addr, MIRROR);

    let output = fixture.mirror(&["apply"]);

    assert!(output.status.success(), "{}", everything(&output));
    let (key_type, pkcs8, _) = bao.held().expect("the key was imported");
    assert_eq!(key_type, "rsa-2048");
    assert!(RsaKeyPair::from_pkcs8(&pkcs8).is_ok());
    assert_no_key_material(&output, &key, Some(&pkcs8));

    // The public key Transit reports for it is the source's, so a second run
    // finds nothing to do.
    let again = fixture.mirror(&["apply"]);
    assert!(stdout(&again).contains("up to date"), "{}", everything(&again));
    assert_eq!(bao.imports().len(), 1);
}

#[test]
fn a_dry_run_reports_and_writes_nothing() {
    let fixture = Fixture::new();
    let key = SshKey::ed25519(fixture.dir.path(), "id");
    let bao = FakeOpenBao::start(&key.pem);
    fixture.openbao_config(bao.addr, MIRROR);

    let output = fixture.mirror(&[]);

    assert!(output.status.success(), "{}", everything(&output));
    assert!(stdout(&output).contains("would create"), "{}", stdout(&output));
    assert!(bao.held().is_none());
    assert_eq!(
        bao.wrapping_fetches(),
        0,
        "a dry run has no reason to fetch the wrapping key"
    );
}

#[test]
fn a_key_transit_already_holds_is_up_to_date_and_not_imported_again() {
    let fixture = Fixture::new();
    let key = SshKey::ed25519(fixture.dir.path(), "id");
    let bao = FakeOpenBao::start(&key.pem);
    fixture.openbao_config(bao.addr, MIRROR);
    assert!(fixture.mirror(&["apply"]).status.success());

    let output = fixture.mirror(&["apply"]);

    assert!(output.status.success(), "{}", everything(&output));
    assert!(stdout(&output).contains("up to date"), "{}", stdout(&output));
    assert_eq!(bao.imports().len(), 1);
    assert_eq!(bao.import_versions(), 0);
    assert_eq!(bao.wrapping_fetches(), 1);
}

#[test]
fn a_changed_source_key_becomes_a_new_version_of_the_imported_key() {
    let fixture = Fixture::new();
    let first = SshKey::ed25519(fixture.dir.path(), "first");
    let second = SshKey::ed25519(fixture.dir.path(), "second");
    let bao = FakeOpenBao::start(&first.pem);
    fixture.openbao_config(bao.addr, MIRROR);
    assert!(fixture.mirror(&["apply"]).status.success());
    bao.set_source(&second.pem);

    let dry = fixture.mirror(&[]);
    assert!(stdout(&dry).contains("would update"), "{}", everything(&dry));
    assert_eq!(bao.import_versions(), 0);

    let output = fixture.mirror(&["apply"]);

    assert!(output.status.success(), "{}", everything(&output));
    assert!(stdout(&output).contains("updated"), "{}", stdout(&output));
    assert_eq!(bao.import_versions(), 1);
    assert_eq!(
        bao.imports().len(),
        1,
        "a changed key is a version, not a second import"
    );
    let (_, pkcs8, versions) = bao.held().unwrap();
    assert_eq!(versions, 2);
    let held = Ed25519KeyPair::from_pkcs8_maybe_unchecked(&pkcs8).unwrap();
    let mut blob = &second.public_blob[..];
    let _name = trg::config::signers::read_string(&mut blob).unwrap();
    assert_eq!(
        held.public_key().as_ref(),
        trg::config::signers::read_string(&mut blob).unwrap()
    );
    assert_no_key_material(&output, &second, Some(&pkcs8));
}

#[test]
fn a_key_openbao_generated_itself_cannot_be_versioned_and_the_refusal_is_shown() {
    let fixture = Fixture::new();
    let key = SshKey::ed25519(fixture.dir.path(), "id");
    let bao = FakeOpenBao::start(&key.pem);
    bao.hold_native_ed25519();
    fixture.openbao_config(bao.addr, MIRROR);

    let output = fixture.mirror(&["apply"]);

    assert!(!output.status.success());
    let text = everything(&output);
    assert!(text.contains("failed"), "{text}");
    assert!(
        text.contains("can only be used with an imported key"),
        "OpenBao's own words must reach the operator: {text}"
    );
    assert_no_key_material(&output, &key, None);
}

#[test]
fn a_key_of_another_type_than_the_one_held_is_refused_before_anything_is_sent() {
    let fixture = Fixture::new();
    let key = SshKey::generate(fixture.dir.path(), "rsa", &["-t", "rsa", "-b", "2048"]);
    let bao = FakeOpenBao::start(&key.pem);
    bao.hold_native_ed25519();
    fixture.openbao_config(bao.addr, MIRROR);

    let output = fixture.mirror(&["apply"]);

    assert!(!output.status.success());
    assert!(
        everything(&output).contains("cannot change type"),
        "{}",
        everything(&output)
    );
    assert_eq!(bao.import_versions(), 0);
    assert_eq!(bao.wrapping_fetches(), 0);
}

#[test]
fn a_transit_target_on_a_backend_that_is_not_openbao_is_refused_when_the_config_loads() {
    let fixture = Fixture::new();
    fixture.write_config(
        "[secrets.backends.local]\nkind = \"keychain\"\n\n\
         [secrets.mirrors.git-key]\n\
         source = { backend = \"local\", path = \"keys/git\", key = \"ssh_private_key\" }\n\
         targets = [{ backend = \"local\", transit_key = \"git-personal\" }]\n",
    );

    let output = fixture.mirror(&[]);

    assert!(!output.status.success());
    let text = stderr(&output);
    assert!(text.contains("[secrets.mirrors.git-key]"), "{text}");
    assert!(text.contains("`openbao`"), "{text}");
}

#[test]
fn an_ecdsa_key_is_refused_with_the_types_that_are_supported() {
    let fixture = Fixture::new();
    let key = SshKey::generate(fixture.dir.path(), "ec", &["-t", "ecdsa", "-b", "256"]);
    let bao = FakeOpenBao::start(&key.pem);
    fixture.openbao_config(bao.addr, MIRROR);

    let output = fixture.mirror(&["apply"]);

    assert!(!output.status.success());
    let text = everything(&output);
    assert!(text.contains("ecdsa-sha2-nistp256"), "{text}");
    assert!(text.contains("only ssh-ed25519 and ssh-rsa are supported"), "{text}");
    assert!(bao.held().is_none());
    assert_eq!(bao.wrapping_fetches(), 0);
    assert_no_key_material(&output, &key, None);
}

#[test]
fn a_source_that_is_not_an_openssh_key_is_refused_without_echoing_it() {
    let fixture = Fixture::new();
    let bao = FakeOpenBao::start("hunter2-not-a-key");
    fixture.openbao_config(bao.addr, MIRROR);

    let output = fixture.mirror(&["apply"]);

    assert!(!output.status.success());
    let text = everything(&output);
    assert!(text.contains("ssh-format=openssh"), "{text}");
    assert!(!text.contains("hunter2"), "{text}");
    assert!(bao.held().is_none());
}
