use super::*;

fn account() -> GcpAccount {
    GcpAccount::parse("deployer@my-project.iam.gserviceaccount.com").expect("account")
}

fn write(body: &str) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("creds.json");
    std::fs::write(&path, body).expect("write");
    (dir, path)
}

const IMPERSONATION: &str = "https://iamcredentials.googleapis.com/v1/projects/-/serviceAccounts/deployer@my-project.iam.gserviceaccount.com:generateAccessToken";

#[test]
fn a_service_account_key_for_the_account_is_verified() {
    let (_dir, path) = write(
        r#"{"type":"service_account","client_email":"deployer@my-project.iam.gserviceaccount.com","private_key":"-----BEGIN PRIVATE KEY-----\nx\n-----END PRIVATE KEY-----\n"}"#,
    );
    let file = verify(&path, &account()).expect("verified");
    assert_eq!(file.principal(), "deployer@my-project.iam.gserviceaccount.com");
    assert_eq!(file.path(), path);
}

#[test]
fn the_principal_is_compared_without_regard_to_case() {
    let (_dir, path) =
        write(r#"{"type":"service_account","client_email":"Deployer@My-Project.iam.gserviceaccount.com"}"#);
    assert!(verify(&path, &account()).is_ok());
}

#[test]
fn a_workload_identity_file_that_impersonates_the_account_is_verified() {
    let (_dir, path) = write(&format!(
        r#"{{"type":"external_account","audience":"//iam.googleapis.com/x","service_account_impersonation_url":"{IMPERSONATION}"}}"#
    ));
    assert_eq!(
        verify(&path, &account()).expect("verified").principal(),
        "deployer@my-project.iam.gserviceaccount.com"
    );
}

#[test]
fn a_key_for_another_account_is_a_mismatch_that_names_both() {
    let (_dir, path) = write(r#"{"type":"service_account","client_email":"other@my-project.iam.gserviceaccount.com"}"#);
    let err = verify(&path, &account()).expect_err("mismatch");
    assert!(matches!(err, CredentialFileError::Mismatch { .. }), "{err}");
    let text = err.to_string();
    assert!(
        text.contains("other@my-project") && text.contains("deployer@my-project"),
        "{text}"
    );
}

#[test]
fn files_that_name_no_checkable_principal_are_refused() {
    for body in [
        r#"{"type":"external_account","audience":"//iam.googleapis.com/x"}"#,
        r#"{"type":"authorized_user","client_id":"a","refresh_token":"r"}"#,
        r#"{"type":"service_account"}"#,
        r#"{"type":"something_new"}"#,
        r#"{}"#,
        r#"{"type":"external_account","service_account_impersonation_url":"https://example.test/not-an-impersonation-url"}"#,
    ] {
        let (_dir, path) = write(body);
        let err = verify(&path, &account()).expect_err(body);
        assert!(matches!(err, CredentialFileError::NoPrincipal { .. }), "{body}: {err}");
        assert!(err.to_string().contains("gcloud auth login --cred-file"), "{err}");
    }
}

#[test]
fn a_file_that_is_missing_or_not_json_or_huge_is_refused() {
    let dir = tempfile::tempdir().expect("tempdir");
    assert!(matches!(
        verify(&dir.path().join("absent.json"), &account()),
        Err(CredentialFileError::Unreadable { .. })
    ));
    let (_dir, path) = write("not json");
    assert!(matches!(
        verify(&path, &account()),
        Err(CredentialFileError::NotJson { .. })
    ));
    let (_dir, path) = write(&" ".repeat(usize::try_from(CREDENTIAL_FILE_SIZE_LIMIT).expect("fits") + 1));
    assert!(matches!(
        verify(&path, &account()),
        Err(CredentialFileError::TooLarge { .. })
    ));
}

/// The key material must not travel into a message, whatever failed.
#[test]
fn no_message_carries_the_private_key() {
    let (_dir, path) = write(
        r#"{"type":"service_account","client_email":"other@x.iam.gserviceaccount.com","private_key":"TOP-SECRET-KEY"}"#,
    );
    let err = verify(&path, &account()).expect_err("mismatch");
    assert!(!format!("{err} {err:?}").contains("TOP-SECRET-KEY"));
}
