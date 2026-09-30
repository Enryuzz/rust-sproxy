use std::process::Command;

#[test]
fn help_exposes_listener_type_with_socks5_default() {
    let output = Command::new(env!("CARGO_BIN_EXE_rust-sproxy"))
        .arg("--help")
        .output()
        .unwrap();
    assert!(output.status.success());
    let help = String::from_utf8(output.stdout).unwrap();
    assert!(help.contains("--type <TYPE>"), "{help}");
    assert!(help.contains("[default: auto]"), "{help}");
    assert!(
        help.contains("[possible values: socks5, socks4, http, auto]"),
        "{help}"
    );
}

#[test]
fn invalid_listener_type_is_rejected() {
    let output = Command::new(env!("CARGO_BIN_EXE_rust-sproxy"))
        .args(["--type", "invalid"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let error = String::from_utf8(output.stderr).unwrap();
    assert!(error.contains("invalid value 'invalid'"), "{error}");
}
