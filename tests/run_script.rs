use std::path::PathBuf;
use std::process::Command;

fn script() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts/run.sh")
}

#[test]
fn run_script_has_valid_bash_syntax() {
    let status = Command::new("bash")
        .arg("-n")
        .arg(script())
        .status()
        .unwrap();
    assert!(status.success());
}

#[test]
fn run_script_help_documents_build_and_server_options() {
    let output = Command::new("bash")
        .arg(script())
        .arg("--help")
        .output()
        .unwrap();
    assert!(output.status.success());
    let help = String::from_utf8(output.stdout).unwrap();
    for option in ["--release", "--debug", "--test", "--clean", "--config"] {
        assert!(help.contains(option), "missing {option} in:\n{help}");
    }
}
