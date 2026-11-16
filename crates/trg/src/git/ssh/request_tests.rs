use std::ffi::OsString;

use super::{read, Invocation};

fn args(list: &[&str]) -> Vec<OsString> {
    list.iter().map(OsString::from).collect()
}

fn is_sign(list: &[&str]) -> bool {
    matches!(read(&args(list)), Invocation::Sign(_))
}

#[test]
fn a_key_file_named_like_a_mode_is_still_a_key_file() {
    assert!(is_sign(&["-f", "-Yverify", "-n", "git", "message"]));
}

#[test]
fn a_namespace_named_like_a_mode_is_still_a_namespace() {
    assert!(is_sign(&["-n", "-Yverify", "-f", "key.pub", "message"]));
}

#[test]
fn both_spellings_of_the_mode_are_read() {
    assert!(is_sign(&["-Y", "sign", "-n", "git", "-f", "key.pub", "message"]));
    assert!(is_sign(&["-Ysign", "-n", "git", "-f", "key.pub", "message"]));
    assert!(!is_sign(&[
        "-Y", "verify", "-n", "git", "-f", "allowed", "-I", "me", "-s", "sig"
    ]));
    assert!(!is_sign(&["-Yfind-principals", "-f", "allowed", "-s", "sig"]));
}

#[test]
fn a_mode_clustered_after_a_flag_is_read() {
    assert!(!is_sign(&[
        "-qYverify",
        "-n",
        "git",
        "-f",
        "allowed",
        "-I",
        "me",
        "-s",
        "sig"
    ]));
}

#[test]
fn nothing_after_the_terminator_is_a_mode() {
    assert!(is_sign(&["-n", "git", "-f", "key.pub", "--", "-Yverify"]));
}
