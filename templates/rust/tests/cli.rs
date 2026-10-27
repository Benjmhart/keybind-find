//! The built plugin as keybind-find runs it: request on stdin, response on stdout, errors as one stderr line.

use std::io::Write;
use std::process::{Command, Stdio};

const PLUGIN: &str = env!("CARGO_BIN_EXE_keybind-find-toy");

/// Runs the plugin with `request` on stdin: its exit code, stdout and stderr.
fn run(request: &str) -> (Option<i32>, String, String) {
    let mut child = Command::new(PLUGIN)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(request.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    let text = |b: Vec<u8>| String::from_utf8(b).unwrap();
    (out.status.code(), text(out.stdout), text(out.stderr))
}

fn request(text: &str) -> String {
    serde_json::json!({"version": 1, "context": {}, "inputs": [{"path": "k", "text": text}]})
        .to_string()
}

#[test]
fn a_bad_line_is_one_stderr_line_naming_file_and_line() {
    let (code, out, err) = run(&request("ctrl+s = Save\nctrl+q\n"));
    assert_eq!((code, out.as_str()), (Some(1), ""));
    assert_eq!(err, "k: line 2: not `key = description`\n");
    let (code, _, err) = run(&request("ctrl+s = Save ; my mode=insert\n"));
    assert_eq!(code, Some(1));
    assert!(err.starts_with("k: line 1: "), "{err}");
    let (_, _, err) = run(&request("ctrl+s = Save ; mode\n"));
    assert_eq!(err, "k: line 1: a scope is `name=value`\n");
    let (_, _, err) = run(&request("ctrl+s = Save\ncrtl+q = Quit\n"));
    assert!(err.starts_with("k: line 2: "), "{err}");
    let (_, _, err) = run(&request("ctrl+s = ; mode=insert\n"));
    assert_eq!(err, "k: line 1: no description\n");
}

#[test]
fn another_version_or_no_inputs() {
    let (code, _, err) = run(r#"{"version": 2, "inputs": []}"#);
    assert_eq!(code, Some(1));
    assert!(err.contains("request version 2 is not 1"), "{err}");
    // No key file yet: an empty sheet, not an error.
    for req in [
        r#"{"version": 1, "context": {}, "inputs": []}"#,
        r#"{"version": 1}"#,
    ] {
        let (code, out, err) = run(req);
        assert_eq!(code, Some(0), "{req}: {err}");
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["binds"]["General"], serde_json::json!([]));
    }
}

#[test]
fn manifest() {
    let out = Command::new(PLUGIN).arg("--manifest").output().unwrap();
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["name"], "toy");
}
