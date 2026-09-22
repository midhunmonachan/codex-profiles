// Exercise the real terminal adapter as well as the profile transitions. Python's
// standard-library PTY support avoids adding a platform-specific runtime dependency.
#[cfg(unix)]
#[test]
fn terminal_profile_workflows() {
    let output = std::process::Command::new("python3")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/interactive_pty.py"
        ))
        .arg(env!("CARGO_BIN_EXE_codex-profiles"))
        .output()
        .expect("Python 3 is required for the Unix terminal integration tests");
    assert!(
        output.status.success(),
        "terminal workflow failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
