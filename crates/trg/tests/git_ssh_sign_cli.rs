//! `trg` as git's `gpg.ssh.program`, through the binary.
//!
//! Every signature made here is checked by the real `ssh-keygen -Y verify`,
//! and the commit tests by `git verify-commit`, because the claim is that a
//! signature made without the private key on the machine is indistinguishable
//! from one `ssh-keygen` made. The Transit engine is an in-process HTTP server
//! holding a real Ed25519 key; the SSH agent is a real `ssh-agent` where a key
//! has to be held, and a scripted one where the point is that it refuses.

#![expect(
    clippy::unwrap_used,
    reason = "an integration test is test code end to end, so a failed setup step should fail the test"
)]

use std::io::{Read, Write};
use std::net::SocketAddr;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use assert_cmd::prelude::*;
use ring::signature::Ed25519KeyPair;
use trg::config::signers::{base64, read_string};

const TOKEN: &str = "test-token";
const TRANSIT_KEY: &str = "git-personal";

struct Keypair {
    private: PathBuf,
    public_line: String,
}

impl Keypair {
    fn generate(dir: &Path, name: &str, kind: &str) -> Self {
        let private = dir.join(name);
        let status = Command::new("ssh-keygen")
            .args(["-q", "-t", kind, "-N", "", "-C", "fixture", "-f"])
            .arg(&private)
            .status()
            .unwrap();
        assert!(status.success(), "ssh-keygen could not generate a {kind} key");
        let public_line = std::fs::read_to_string(dir.join(format!("{name}.pub")))
            .unwrap()
            .trim()
            .to_string();
        Self { private, public_line }
    }

    fn public_file(&self) -> PathBuf {
        PathBuf::from(format!("{}.pub", self.private.display()))
    }

    /// The Ed25519 seed inside an unencrypted OpenSSH private key file.
    fn seed(&self) -> [u8; 32] {
        let text = std::fs::read_to_string(&self.private).unwrap();
        let body: String = text.lines().filter(|l| !l.starts_with("-----")).collect();
        let raw = base64::decode(&body).unwrap();
        let mut rest = &raw[b"openssh-key-v1\0".len()..];
        for _ in 0..3 {
            read_string(&mut rest).unwrap();
        }
        rest = &rest[4..];
        read_string(&mut rest).unwrap();
        let mut private = read_string(&mut rest).unwrap();
        private = &private[8..];
        read_string(&mut private).unwrap();
        read_string(&mut private).unwrap();
        let secret = read_string(&mut private).unwrap();
        secret[..32].try_into().unwrap()
    }
}

/// The part of Transit that signing talks to, answering from a real key.
struct FakeTransit {
    addr: SocketAddr,
    requests: Arc<AtomicUsize>,
    server: Arc<tiny_http::Server>,
}

impl FakeTransit {
    /// `advertised` is what `GET keys/<name>` reports, which a test can make
    /// differ from the key that actually signs.
    fn start(signing: &Keypair, advertised: &str) -> Self {
        let pair = Ed25519KeyPair::from_seed_unchecked(&signing.seed()).unwrap();
        let advertised = advertised
            .split_whitespace()
            .nth(1)
            .map(|b| base64::decode(b).unwrap())
            .map(|blob| {
                let mut rest = blob.as_slice();
                read_string(&mut rest).unwrap();
                read_string(&mut rest).unwrap().to_vec()
            })
            .unwrap();
        let server = Arc::new(tiny_http::Server::http("127.0.0.1:0").unwrap());
        let addr = server.server_addr().to_ip().unwrap();
        let requests = Arc::new(AtomicUsize::new(0));
        let (worker_server, counter) = (Arc::clone(&server), Arc::clone(&requests));
        std::thread::spawn(move || {
            for mut request in worker_server.incoming_requests() {
                counter.fetch_add(1, Ordering::SeqCst);
                let authorized = request
                    .headers()
                    .iter()
                    .any(|h| h.field.equiv("X-Vault-Token") && h.value.as_str() == TOKEN);
                let url = request.url().to_string();
                let (status, body) = if !authorized {
                    (403, r#"{"errors":["permission denied"]}"#.to_string())
                } else if url == format!("/v1/transit/keys/{TRANSIT_KEY}") {
                    (
                        200,
                        serde_json::json!({"data": {
                            "type": "ed25519",
                            "latest_version": 1,
                            "keys": {"1": {"public_key": base64::encode(&advertised)}},
                        }})
                        .to_string(),
                    )
                } else if url == format!("/v1/transit/sign/{TRANSIT_KEY}") {
                    let mut text = String::new();
                    request.as_reader().read_to_string(&mut text).unwrap();
                    let input: serde_json::Value = serde_json::from_str(&text).unwrap();
                    let message = base64::decode(input["input"].as_str().unwrap()).unwrap();
                    let signature = base64::encode(pair.sign(&message).as_ref());
                    (
                        200,
                        serde_json::json!({"data": {"signature": format!("vault:v1:{signature}")}}).to_string(),
                    )
                } else {
                    (404, r#"{"errors":[]}"#.to_string())
                };
                let response = tiny_http::Response::from_string(body).with_status_code(status);
                request.respond(response).unwrap();
            }
        });
        Self { addr, requests, server }
    }

    fn requests(&self) -> usize {
        self.requests.load(Ordering::SeqCst)
    }
}

impl Drop for FakeTransit {
    fn drop(&mut self) {
        self.server.unblock();
    }
}

/// An address nothing listens on, so a connection is refused at once.
fn closed_port_addr() -> SocketAddr {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap()
}

struct RealAgent {
    child: Child,
    socket: PathBuf,
}

impl RealAgent {
    fn start(dir: &Path, hold: Option<&Keypair>) -> Self {
        let socket = dir.join("a.sock");
        let child = Command::new("ssh-agent")
            .args(["-D", "-a"])
            .arg(&socket)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !socket.exists() {
            assert!(Instant::now() < deadline, "ssh-agent never created its socket");
            std::thread::sleep(Duration::from_millis(20));
        }
        if let Some(key) = hold {
            let status = Command::new("ssh-add")
                .arg(&key.private)
                .env("SSH_AUTH_SOCK", &socket)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .unwrap();
            assert!(status.success(), "ssh-add could not add the fixture key");
        }
        Self { child, socket }
    }
}

impl Drop for RealAgent {
    fn drop(&mut self) {
        self.child.kill().ok();
        self.child.wait().ok();
    }
}

/// An agent that lists `key` and answers every signing request with a failure,
/// as one does when a person denies the approval prompt.
fn denying_agent(dir: &Path, key: &Keypair) -> PathBuf {
    let socket = dir.join("d.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let blob = {
        let b64 = key.public_line.split_whitespace().nth(1).unwrap();
        base64::decode(b64).unwrap()
    };
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let blob = blob.clone();
            std::thread::spawn(move || loop {
                let mut length = [0u8; 4];
                if stream.read_exact(&mut length).is_err() {
                    return;
                }
                let mut message = vec![0u8; u32::from_be_bytes(length) as usize];
                if stream.read_exact(&mut message).is_err() {
                    return;
                }
                let mut answer = Vec::new();
                match message[0] {
                    11 => {
                        answer.push(12);
                        answer.extend_from_slice(&1u32.to_be_bytes());
                        answer.extend_from_slice(&u32::try_from(blob.len()).unwrap().to_be_bytes());
                        answer.extend_from_slice(&blob);
                        answer.extend_from_slice(&0u32.to_be_bytes());
                    }
                    _ => answer.push(5),
                }
                let mut framed = u32::try_from(answer.len()).unwrap().to_be_bytes().to_vec();
                framed.extend_from_slice(&answer);
                if stream.write_all(&framed).is_err() {
                    return;
                }
            });
        }
    });
    socket
}

struct Fixture {
    dir: tempfile::TempDir,
    key: Keypair,
    program: PathBuf,
    config_home: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let key = Keypair::generate(dir.path(), "id", "ed25519");
        let trg = Command::cargo_bin("trg").unwrap().get_program().to_owned();
        let program = dir.path().join("trg-git-ssh-sign");
        symlink(trg, &program).unwrap();
        let config_home = dir.path().join("config");
        std::fs::create_dir_all(config_home.join("trg")).unwrap();
        std::fs::write(dir.path().join("token"), TOKEN).unwrap();
        std::fs::set_permissions(dir.path().join("token"), std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::write(dir.path().join("allowed_signers"), format!("me {}\n", key.public_line)).unwrap();
        Self {
            dir,
            key,
            program,
            config_home,
        }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    fn backend(&self, name: &str, addr: SocketAddr) -> String {
        format!(
            "[secrets.backends.{name}]\nkind = \"openbao\"\naddr = \"http://{addr}\"\nmount = \"secret\"\n\
             path_prefix = \"trg\"\nowner = \"me\"\ntoken_file = \"{}\"\ntimeout_ms = 3000\n\n",
            self.path("token").display()
        )
    }

    fn write_config(&self, text: &str) {
        std::fs::write(self.config_home.join("trg/config.toml"), text).unwrap();
    }

    fn signer(&self, name: &str, signer: &str) -> String {
        format!(
            "[git.ssh.signers.{name}]\npublic_key = \"{}\"\nsigner = {signer}\n",
            self.key.public_line
        )
    }

    fn payload(&self, text: &str) -> PathBuf {
        let path = self.path("payload");
        std::fs::write(&path, text).unwrap();
        std::fs::remove_file(self.path("payload.sig")).ok();
        path
    }

    fn sign(&self, args: &[&str]) -> Output {
        Command::new(&self.program)
            .args(args)
            .env("XDG_CONFIG_HOME", &self.config_home)
            .env_remove("SSH_AUTH_SOCK")
            .stdin(Stdio::null())
            .output()
            .unwrap()
    }

    fn sign_file(&self, payload: &Path) -> Output {
        let key = self.key.public_file();
        self.sign(&[
            "-Y",
            "sign",
            "-n",
            "git",
            "-f",
            key.to_str().unwrap(),
            "-U",
            payload.to_str().unwrap(),
        ])
    }

    fn verifies(&self, payload: &Path, signature: &Path) -> bool {
        Command::new("ssh-keygen")
            .args(["-Y", "verify", "-n", "git", "-I", "me", "-f"])
            .arg(self.path("allowed_signers"))
            .arg("-s")
            .arg(signature)
            .stdin(std::fs::File::open(payload).unwrap())
            .output()
            .unwrap()
            .status
            .success()
    }
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn transit_signer(fixture: &Fixture, backend: &str, fallbacks: &str) -> String {
    fixture.signer(
        "personal",
        &format!("{{ backend = \"{backend}\", transit_key = \"{TRANSIT_KEY}\"{fallbacks} }}"),
    )
}

#[test]
fn a_transit_signature_verifies_with_ssh_keygen() {
    let fixture = Fixture::new();
    let transit = FakeTransit::start(&fixture.key, &fixture.key.public_line);
    fixture.write_config(&format!(
        "{}{}",
        fixture.backend("homelab", transit.addr),
        transit_signer(&fixture, "homelab", "")
    ));
    let payload = fixture.payload("a commit body\n");

    let output = fixture.sign_file(&payload);

    assert!(output.status.success(), "{}", stderr(&output));
    let signature = fixture.path("payload.sig");
    assert!(signature.exists(), "no signature file was written");
    assert!(
        fixture.verifies(&payload, &signature),
        "ssh-keygen rejected the signature"
    );
    let text = std::fs::read_to_string(&signature).unwrap();
    assert!(
        text.starts_with("-----BEGIN SSH SIGNATURE-----\n") && text.ends_with("-----END SSH SIGNATURE-----\n"),
        "the armor is not the one ssh-keygen writes: {text}"
    );
    assert!(
        text.lines().all(|l| l.len() <= 70 || l.starts_with("-----")),
        "armor lines are wrapped at 70 columns"
    );
    let messages = stderr(&output);
    assert!(
        messages.contains("Signing file") && messages.contains("Write signature to"),
        "the progress lines ssh-keygen prints are missing: {messages}"
    );
    assert!(!messages.contains("warning"), "no fallback was involved: {messages}");
}

#[test]
fn the_hash_algorithm_option_is_honoured() {
    let fixture = Fixture::new();
    let transit = FakeTransit::start(&fixture.key, &fixture.key.public_line);
    fixture.write_config(&format!(
        "{}{}",
        fixture.backend("homelab", transit.addr),
        transit_signer(&fixture, "homelab", "")
    ));
    let payload = fixture.payload("hashed with sha256\n");
    let key = fixture.key.public_file();

    let output = fixture.sign(&[
        "-Y",
        "sign",
        "-n",
        "git",
        "-f",
        key.to_str().unwrap(),
        "-Ohashalg=sha256",
        payload.to_str().unwrap(),
    ]);

    assert!(output.status.success(), "{}", stderr(&output));
    assert!(
        fixture.verifies(&payload, &fixture.path("payload.sig")),
        "ssh-keygen rejected a sha256 signature"
    );
}

#[test]
fn a_private_key_path_is_read_through_its_pub_sibling() {
    let fixture = Fixture::new();
    let transit = FakeTransit::start(&fixture.key, &fixture.key.public_line);
    fixture.write_config(&format!(
        "{}{}",
        fixture.backend("homelab", transit.addr),
        transit_signer(&fixture, "homelab", "")
    ));
    let payload = fixture.payload("by private path\n");

    let output = fixture.sign(&[
        "-Y",
        "sign",
        "-n",
        "git",
        "-f",
        fixture.key.private.to_str().unwrap(),
        payload.to_str().unwrap(),
    ]);

    assert!(output.status.success(), "{}", stderr(&output));
    assert!(
        fixture.verifies(&payload, &fixture.path("payload.sig")),
        "ssh-keygen rejected the signature"
    );
}

#[test]
fn the_clap_command_signs_the_same_way() {
    let fixture = Fixture::new();
    let transit = FakeTransit::start(&fixture.key, &fixture.key.public_line);
    fixture.write_config(&format!(
        "{}{}",
        fixture.backend("homelab", transit.addr),
        transit_signer(&fixture, "homelab", "")
    ));
    let payload = fixture.payload("through clap\n");

    let output = Command::cargo_bin("trg")
        .unwrap()
        .args(["git", "ssh", "sign", "-n", "git", "-f"])
        .arg(fixture.key.public_file())
        .arg(&payload)
        .env("XDG_CONFIG_HOME", &fixture.config_home)
        .output()
        .unwrap();

    assert!(output.status.success(), "{}", stderr(&output));
    assert!(
        fixture.verifies(&payload, &fixture.path("payload.sig")),
        "ssh-keygen rejected the signature"
    );
}

#[test]
fn a_key_no_signer_declares_is_refused_with_the_declared_names() {
    let fixture = Fixture::new();
    let other = Keypair::generate(fixture.dir.path(), "other", "ed25519");
    let transit = FakeTransit::start(&fixture.key, &fixture.key.public_line);
    fixture.write_config(&format!(
        "{}{}",
        fixture.backend("homelab", transit.addr),
        transit_signer(&fixture, "homelab", "")
    ));
    let payload = fixture.payload("unknown key\n");

    let output = fixture.sign(&[
        "-Y",
        "sign",
        "-n",
        "git",
        "-f",
        other.public_file().to_str().unwrap(),
        payload.to_str().unwrap(),
    ]);

    assert_eq!(output.status.code(), Some(255), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("declared signers: personal"),
        "the error does not list the declared signers: {}",
        stderr(&output)
    );
    assert_eq!(transit.requests(), 0, "no signer matched, so Transit must not be asked");
    assert!(!fixture.path("payload.sig").exists(), "a signature was written anyway");
}

#[test]
fn an_unreachable_transit_falls_through_to_the_agent_and_says_so() {
    let fixture = Fixture::new();
    let agent = RealAgent::start(fixture.dir.path(), Some(&fixture.key));
    fixture.write_config(&format!(
        "{}{}",
        fixture.backend("homelab", closed_port_addr()),
        transit_signer(
            &fixture,
            "homelab",
            &format!(", fallbacks = [{{ ssh_agent = \"{}\" }}]", agent.socket.display())
        )
    ));
    let payload = fixture.payload("agent answers\n");

    let output = fixture.sign_file(&payload);

    assert!(output.status.success(), "{}", stderr(&output));
    assert!(
        fixture.verifies(&payload, &fixture.path("payload.sig")),
        "ssh-keygen rejected the agent's signature"
    );
    let messages = stderr(&output);
    assert!(
        messages.contains("warning: signer `personal` could not sign with Transit key")
            && messages.contains("SSH agent at")
            && messages.contains("could not connect"),
        "the fallback warning is missing or does not say why: {messages}"
    );
}

#[test]
fn rejected_credentials_fall_through_to_the_agent() {
    let fixture = Fixture::new();
    let transit = FakeTransit::start(&fixture.key, &fixture.key.public_line);
    let agent = RealAgent::start(fixture.dir.path(), Some(&fixture.key));
    std::fs::write(fixture.path("token"), "a-different-token").unwrap();
    fixture.write_config(&format!(
        "{}{}",
        fixture.backend("homelab", transit.addr),
        transit_signer(
            &fixture,
            "homelab",
            &format!(", fallbacks = [{{ ssh_agent = \"{}\" }}]", agent.socket.display())
        )
    ));
    let payload = fixture.payload("token rejected\n");

    let output = fixture.sign_file(&payload);

    assert!(output.status.success(), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("rejected the token"),
        "the warning does not say the token was rejected: {}",
        stderr(&output)
    );
}

#[test]
fn a_transit_key_that_is_not_the_declared_key_stops_without_falling_through() {
    let fixture = Fixture::new();
    let other = Keypair::generate(fixture.dir.path(), "other", "ed25519");
    let transit = FakeTransit::start(&other, &other.public_line);
    let agent = RealAgent::start(fixture.dir.path(), Some(&fixture.key));
    fixture.write_config(&format!(
        "{}{}",
        fixture.backend("homelab", transit.addr),
        transit_signer(
            &fixture,
            "homelab",
            &format!(", fallbacks = [{{ ssh_agent = \"{}\" }}]", agent.socket.display())
        )
    ));
    let payload = fixture.payload("mismatch\n");

    let output = fixture.sign_file(&payload);

    assert_eq!(output.status.code(), Some(255), "{}", stderr(&output));
    let messages = stderr(&output);
    assert!(
        messages.contains("not the `public_key` declared"),
        "the mismatch is not reported: {messages}"
    );
    assert!(
        !messages.contains("warning"),
        "a mismatch must not fall through: {messages}"
    );
    assert!(
        !fixture.path("payload.sig").exists(),
        "the agent signed after a mismatch"
    );
}

#[test]
fn an_agent_primary_answers_without_asking_transit() {
    let fixture = Fixture::new();
    let transit = FakeTransit::start(&fixture.key, &fixture.key.public_line);
    let agent = RealAgent::start(fixture.dir.path(), Some(&fixture.key));
    fixture.write_config(&format!(
        "{}{}",
        fixture.backend("homelab", transit.addr),
        fixture.signer(
            "personal",
            &format!(
                "{{ ssh_agent = \"{}\", fallbacks = [{{ backend = \"homelab\", transit_key = \"{TRANSIT_KEY}\" }}] }}",
                agent.socket.display()
            )
        )
    ));
    let payload = fixture.payload("agent first\n");

    let output = fixture.sign_file(&payload);

    assert!(output.status.success(), "{}", stderr(&output));
    assert!(
        fixture.verifies(&payload, &fixture.path("payload.sig")),
        "ssh-keygen rejected the signature"
    );
    assert_eq!(
        transit.requests(),
        0,
        "the agent answered, so Transit must not be asked"
    );
    assert!(!stderr(&output).contains("warning"), "{}", stderr(&output));
}

#[test]
fn a_missing_agent_socket_falls_through_to_transit() {
    let fixture = Fixture::new();
    let transit = FakeTransit::start(&fixture.key, &fixture.key.public_line);
    fixture.write_config(&format!(
        "{}{}",
        fixture.backend("homelab", transit.addr),
        fixture.signer(
            "personal",
            &format!(
                "{{ ssh_agent = \"{}\", fallbacks = [{{ backend = \"homelab\", transit_key = \"{TRANSIT_KEY}\" }}] }}",
                fixture.path("absent.sock").display()
            )
        )
    ));
    let payload = fixture.payload("agent absent\n");

    let output = fixture.sign_file(&payload);

    assert!(output.status.success(), "{}", stderr(&output));
    assert!(
        fixture.verifies(&payload, &fixture.path("payload.sig")),
        "ssh-keygen rejected the Transit signature"
    );
    let messages = stderr(&output);
    assert!(
        messages.contains("warning: signer `personal` could not sign with SSH agent at")
            && messages.contains("Transit key"),
        "the fallback warning is missing: {messages}"
    );
}

#[test]
fn an_agent_that_does_not_hold_the_key_falls_through_to_transit() {
    let fixture = Fixture::new();
    let transit = FakeTransit::start(&fixture.key, &fixture.key.public_line);
    let agent = RealAgent::start(fixture.dir.path(), None);
    fixture.write_config(&format!(
        "{}{}",
        fixture.backend("homelab", transit.addr),
        fixture.signer(
            "personal",
            &format!(
                "{{ ssh_agent = \"{}\", fallbacks = [{{ backend = \"homelab\", transit_key = \"{TRANSIT_KEY}\" }}] }}",
                agent.socket.display()
            )
        )
    ));
    let payload = fixture.payload("key not held\n");

    let output = fixture.sign_file(&payload);

    assert!(output.status.success(), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("does not hold this public key"),
        "the warning does not say the key was not held: {}",
        stderr(&output)
    );
}

#[test]
fn a_denied_agent_signature_is_never_routed_around() {
    let fixture = Fixture::new();
    let transit = FakeTransit::start(&fixture.key, &fixture.key.public_line);
    let socket = denying_agent(fixture.dir.path(), &fixture.key);
    fixture.write_config(&format!(
        "{}{}",
        fixture.backend("homelab", transit.addr),
        fixture.signer(
            "personal",
            &format!(
                "{{ ssh_agent = \"{}\", fallbacks = [{{ backend = \"homelab\", transit_key = \"{TRANSIT_KEY}\" }}] }}",
                socket.display()
            )
        )
    ));
    let payload = fixture.payload("denied\n");

    let output = fixture.sign_file(&payload);

    assert_eq!(output.status.code(), Some(255), "{}", stderr(&output));
    assert_eq!(transit.requests(), 0, "Transit signed after the agent was denied");
    assert!(
        !fixture.path("payload.sig").exists(),
        "a signature exists after a denial"
    );
    assert!(
        stderr(&output).contains("holds the key"),
        "the stop does not say the agent held the key: {}",
        stderr(&output)
    );
}

#[test]
fn verify_modes_are_ssh_keygens_own_answer() {
    let fixture = Fixture::new();
    let transit = FakeTransit::start(&fixture.key, &fixture.key.public_line);
    fixture.write_config(&format!(
        "{}{}",
        fixture.backend("homelab", transit.addr),
        transit_signer(&fixture, "homelab", "")
    ));
    let payload = fixture.payload("verify me\n");
    assert!(fixture.sign_file(&payload).status.success(), "signing failed");
    let signature = fixture.path("payload.sig");
    let allowed = fixture.path("allowed_signers");

    let through = |program: &Path, args: &[&str], stdin: &Path| {
        Command::new(program)
            .args(args)
            .stdin(std::fs::File::open(stdin).unwrap())
            .output()
            .unwrap()
    };
    let cases: Vec<Vec<&str>> = vec![
        vec![
            "-Y",
            "find-principals",
            "-f",
            allowed.to_str().unwrap(),
            "-s",
            signature.to_str().unwrap(),
        ],
        vec![
            "-Y",
            "verify",
            "-n",
            "git",
            "-f",
            allowed.to_str().unwrap(),
            "-I",
            "me",
            "-s",
            signature.to_str().unwrap(),
        ],
        vec![
            "-Y",
            "verify",
            "-n",
            "other-namespace",
            "-f",
            allowed.to_str().unwrap(),
            "-I",
            "me",
            "-s",
            signature.to_str().unwrap(),
        ],
        vec!["-Y", "check-novalidate", "-n", "git", "-s", signature.to_str().unwrap()],
    ];
    for args in cases {
        let ours = through(&fixture.program, &args, &payload);
        let theirs = through(Path::new("ssh-keygen"), &args, &payload);
        assert_eq!(
            ours.status.code(),
            theirs.status.code(),
            "exit status differs for {args:?}"
        );
        assert_eq!(ours.stdout, theirs.stdout, "stdout differs for {args:?}");
    }
}

#[test]
fn failures_exit_as_ssh_keygen_does() {
    let fixture = Fixture::new();
    let transit = FakeTransit::start(&fixture.key, &fixture.key.public_line);
    fixture.write_config(&format!(
        "{}{}",
        fixture.backend("homelab", transit.addr),
        transit_signer(&fixture, "homelab", "")
    ));
    let payload = fixture.payload("exit codes\n");
    let key = fixture.key.public_file();
    let key = key.to_str().unwrap();
    let file = payload.to_str().unwrap();
    let missing_file = fixture.path("no-such-file");
    let missing_key = fixture.path("no-such-key");
    let missing_file = missing_file.to_str().unwrap();
    let missing_key = missing_key.to_str().unwrap();

    let cases: Vec<Vec<&str>> = vec![
        vec!["-Y", "sign", "-f", key, file],
        vec!["-Y", "sign", "-n", "git", file],
        vec!["-Y", "sign", "-n", "", "-f", key, file],
        vec!["-Y", "sign", "-n", "git", "-f", key, missing_file],
        vec!["-Y", "sign", "-n", "git", "-f", missing_key, file],
        vec!["-Y", "sign", "-n", "git", "-f", key, "-Obogus", file],
        vec!["-Y", "sign", "-n", "git", "-f", key, "-Ohashalg=md5", file],
    ];
    for args in cases {
        let ours = fixture.sign(&args);
        let theirs = Command::new("ssh-keygen")
            .args(&args)
            .env_remove("SSH_AUTH_SOCK")
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert_eq!(
            ours.status.code(),
            theirs.status.code(),
            "exit status differs from ssh-keygen for {args:?}: {}",
            stderr(&ours)
        );
        assert!(
            !fixture.path("payload.sig").exists(),
            "a failed sign wrote a signature for {args:?}"
        );
    }
}

#[test]
fn git_signs_and_verifies_a_commit_through_the_program() {
    let fixture = Fixture::new();
    let transit = FakeTransit::start(&fixture.key, &fixture.key.public_line);
    fixture.write_config(&format!(
        "{}{}",
        fixture.backend("homelab", transit.addr),
        transit_signer(&fixture, "homelab", "")
    ));
    let repo = fixture.path("repo");
    std::fs::create_dir(&repo).unwrap();
    let git = |args: &[&str]| {
        let output = Command::new("git")
            .current_dir(&repo)
            .env("XDG_CONFIG_HOME", &fixture.config_home)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env_remove("SSH_AUTH_SOCK")
            .args(["-c", "core.hooksPath=/dev/null"])
            .args(["-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid"])
            .args(["-c", "gpg.format=ssh"])
            .arg("-c")
            .arg(format!("gpg.ssh.program={}", fixture.program.display()))
            .arg("-c")
            .arg(format!("user.signingkey=key::{}", fixture.key.public_line))
            .arg("-c")
            .arg(format!(
                "gpg.ssh.allowedSignersFile={}",
                fixture.path("allowed_signers").display()
            ))
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output
    };
    git(&["init", "-q", "-b", "feature"]);

    git(&[
        "commit",
        "-q",
        "-S",
        "--allow-empty",
        "-m",
        "signed without a private key",
    ]);
    let verified = git(&["verify-commit", "HEAD"]);

    assert!(
        String::from_utf8_lossy(&verified.stderr).contains("Good \"git\" signature"),
        "git did not report a good signature: {}",
        String::from_utf8_lossy(&verified.stderr)
    );
}

fn load_failure(fixture: &Fixture, config: &str) -> String {
    fixture.write_config(config);
    let payload = fixture.payload("config check\n");
    let output = fixture.sign_file(&payload);
    assert_eq!(
        output.status.code(),
        Some(255),
        "an invalid config must stop signing: {}",
        stderr(&output)
    );
    assert!(
        !fixture.path("payload.sig").exists(),
        "a signature was written under an invalid config"
    );
    stderr(&output)
}

#[test]
fn configuration_errors_are_reported_at_load() {
    let fixture = Fixture::new();
    let backend = fixture.backend("homelab", closed_port_addr());
    let signer = |name: &str, key: &str, address: &str| {
        format!("[git.ssh.signers.{name}]\npublic_key = \"{key}\"\nsigner = {address}\n")
    };
    let valid = format!("{{ backend = \"homelab\", transit_key = \"{TRANSIT_KEY}\" }}");
    let key = fixture.key.public_line.clone();

    let duplicate = load_failure(
        &fixture,
        &format!(
            "{backend}{}{}",
            signer("one", &key, &valid),
            signer("two", &key, &valid)
        ),
    );
    assert!(
        duplicate.contains("repeats the public key"),
        "duplicate key: {duplicate}"
    );

    let ecdsa = Keypair::generate(fixture.dir.path(), "ecdsa", "ecdsa");
    let unsupported = load_failure(
        &fixture,
        &format!("{backend}{}", signer("one", &ecdsa.public_line, &valid)),
    );
    assert!(
        unsupported.contains("only ssh-ed25519"),
        "non-ed25519 key: {unsupported}"
    );

    let undeclared = load_failure(
        &fixture,
        &format!(
            "{backend}{}",
            signer(
                "one",
                &key,
                &format!("{{ backend = \"nowhere\", transit_key = \"{TRANSIT_KEY}\" }}")
            )
        ),
    );
    assert!(
        undeclared.contains("not a declared secrets backend"),
        "undeclared backend: {undeclared}"
    );

    let other_kind = load_failure(
        &fixture,
        &format!(
            "[secrets.backends.op]\nkind = \"onepassword\"\naccount = \"my.1password.com\"\n{}",
            signer(
                "one",
                &key,
                &format!("{{ backend = \"op\", transit_key = \"{TRANSIT_KEY}\" }}")
            )
        ),
    );
    assert!(
        other_kind.contains("needs an `openbao` backend"),
        "non-openbao backend: {other_kind}"
    );

    let empty = load_failure(
        &fixture,
        &format!(
            "{backend}{}",
            signer(
                "one",
                &key,
                &format!("{{ backend = \"homelab\", transit_key = \"{TRANSIT_KEY}\", fallbacks = [] }}")
            )
        ),
    );
    assert!(empty.contains("`fallbacks` is empty"), "empty fallbacks: {empty}");

    let nested = load_failure(
        &fixture,
        &format!(
            "{backend}{}",
            signer(
                "one",
                &key,
                &format!(
                    "{{ backend = \"homelab\", transit_key = \"{TRANSIT_KEY}\", fallbacks = [{{ ssh_agent = \"/x\", fallbacks = [{{ ssh_agent = \"/y\" }}] }}] }}"
                )
            )
        ),
    );
    assert!(
        nested.contains("fallbacks"),
        "nested fallbacks must be rejected: {nested}"
    );
}
