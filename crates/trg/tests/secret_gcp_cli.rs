//! `trg secret put` and `get` against a Secret Manager backend, through the
//! binary and a stand-in `gcloud` found on `PATH`.
//!
//! What this pins is what only the whole program shows: the value reaches
//! `gcloud` on stdin and nowhere in argv, a `--key` is refused before stdin is
//! read, and a secret that is not there is reported and not created.

#![expect(
    clippy::unwrap_used,
    reason = "an integration test is test code end to end, so a failed setup step should fail the test"
)]

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use assert_cmd::prelude::*;
use std::process::Command;

const BODY: &str = r#"
case "$1 $2 $3" in
  "secrets versions add")
    [ -f "$0.exists" ] || { echo "NOT_FOUND: Secret not found." >&2; exit 1; }
    cat > "$0.state"
    exit 0;;
  "secrets versions access")
    [ -f "$0.state" ] || { echo "NOT_FOUND: not found." >&2; exit 1; }
    base64 < "$0.state" | tr -d '\n' | tr '+/' '-_'
    exit 0;;
esac
exit 127
"#;

struct Setup {
    home: tempfile::TempDir,
    bin: tempfile::TempDir,
}

impl Setup {
    fn new(secret_exists: bool) -> Self {
        let home = tempfile::tempdir().unwrap();
        fs::create_dir_all(home.path().join("trg")).unwrap();
        fs::write(
            home.path().join("trg/config.toml"),
            "[secrets.backends.gsm]\nkind = \"gcp_secret_manager\"\nproject = \"my-project\"\naccount = \"deployer@my-project.iam.gserviceaccount.com\"\n",
        )
        .unwrap();
        let bin = tempfile::tempdir().unwrap();
        let shim = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/op-stub.sh");
        std::os::unix::fs::symlink(shim, bin.path().join("gcloud")).unwrap();
        fs::write(bin.path().join("gcloud.body"), BODY).unwrap();
        if secret_exists {
            fs::write(bin.path().join("gcloud.body.exists"), "").unwrap();
        }
        Self { home, bin }
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::cargo_bin("trg").unwrap();
        cmd.env("XDG_CONFIG_HOME", self.home.path());
        cmd.env("PATH", format!("{}:/usr/bin:/bin", self.bin.path().display()));
        cmd.args(args);
        cmd
    }

    fn argv(&self) -> String {
        fs::read_to_string(self.bin.path().join("gcloud.argv")).unwrap_or_default()
    }

    fn stored(&self) -> Option<Vec<u8>> {
        fs::read(Path::new(self.bin.path()).join("gcloud.body.state")).ok()
    }
}

fn put(setup: &Setup, args: &[&str], stdin: &[u8]) -> std::process::Output {
    let mut cmd = setup.command(args);
    cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().unwrap();
    let mut pipe = child.stdin.take().unwrap();
    let _ = pipe.write_all(stdin);
    drop(pipe);
    child.wait_with_output().unwrap()
}

#[test]
fn a_put_sends_the_value_on_stdin_and_never_in_argv() {
    let setup = Setup::new(true);
    let out = put(
        &setup,
        &["secret", "put", "--backend", "gsm", "--path", "deploy-token"],
        b"s3cr3t-value\n",
    );
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(setup.stored().as_deref(), Some(b"s3cr3t-value".as_slice()));
    assert!(!setup.argv().contains("s3cr3t-value"), "{}", setup.argv());
    assert!(setup.argv().contains("--project=my-project"), "{}", setup.argv());
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(!stderr.contains("s3cr3t-value"), "{stderr}");
}

#[test]
fn a_get_prints_the_stored_value() {
    let setup = Setup::new(true);
    fs::write(setup.bin.path().join("gcloud.body.state"), b"stored-value").unwrap();
    let out = setup
        .command(&["secret", "get", "--backend", "gsm", "--path", "deploy-token"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(String::from_utf8(out.stdout).unwrap().contains("stored-value"));
}

#[test]
fn a_key_is_refused_before_anything_is_read_or_written() {
    let setup = Setup::new(true);
    let out = put(
        &setup,
        &[
            "secret",
            "put",
            "--backend",
            "gsm",
            "--path",
            "deploy-token",
            "--key",
            "token",
        ],
        b"v\n",
    );
    assert!(!out.status.success());
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(stderr.contains("`key` is not supported"), "{stderr}");
    assert_eq!(setup.stored(), None);
    assert_eq!(setup.argv(), "");
}

#[test]
fn a_secret_that_does_not_exist_is_reported_and_not_created() {
    let setup = Setup::new(false);
    let out = put(
        &setup,
        &["secret", "put", "--backend", "gsm", "--path", "deploy-token"],
        b"v\n",
    );
    assert!(!out.status.success());
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(stderr.contains("does not exist"), "{stderr}");
    assert!(stderr.contains("never creates"), "{stderr}");
    assert!(!setup.argv().contains("create"), "{}", setup.argv());
    assert_eq!(setup.stored(), None);
}
