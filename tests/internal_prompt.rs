#![cfg(unix)]

use assert_cmd::Command;
use assert_fs::TempDir;
use assert_fs::prelude::*;
use predicates::str::contains;

/// End-to-end exercise of the internal prompt-assembler helper, including
/// placeholder prompting and printing the assembled text.
#[test]
fn internal_prompt_assembler_renders_and_prints() -> Result<(), Box<dyn std::error::Error>> {
    let temp = TempDir::new()?;
    let pa = temp.child("pa");
    pa.write_str(
        r#"#!/bin/sh
if [ "$1" = "show" ] && [ "${2:-}" = "--json" ]; then
  shift 2
  printf '%s' '{"profile":{"content":"Hello {0}"}}'
  exit 0
fi
shift
printf 'Hello %s\n' "${1:-}"
"#,
    )?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o755);
        std::fs::set_permissions(pa.path(), perms)?;
    }

    let path = pa
        .path()
        .parent()
        .expect("parent directory")
        .display()
        .to_string();

    #[allow(deprecated)]
    Command::cargo_bin("tx-dev")?
        .env(
            "PATH",
            format!("{path}:{}", std::env::var("PATH").unwrap_or_default()),
        )
        .arg("internal")
        .arg("prompt-assembler")
        .arg("--prompt")
        .arg("demo")
        .write_stdin("world\n")
        .assert()
        .success()
        .stdout(contains("Hello world"));

    Ok(())
}

#[test]
fn internal_capture_arg_passes_captured_prompt() -> Result<(), Box<dyn std::error::Error>> {
    let temp = TempDir::new()?;
    let provider = temp.child("provider.sh");
    provider.write_str(
        r#"#!/bin/sh
printf '%s' "$2" > "$1"
"#,
    )?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o755);
        std::fs::set_permissions(provider.path(), perms)?;
    }
    let output_file = temp.child("captured.txt");

    #[allow(deprecated)]
    Command::cargo_bin("tx-dev")?
        .env("TX_CAPTURE_STDIN_DATA", "payload from env")
        .arg("internal")
        .arg("capture-arg")
        .arg("--provider")
        .arg("demo")
        .arg("--bin")
        .arg(provider.path())
        .arg("--arg")
        .arg(output_file.path())
        .arg("--arg")
        .arg("{prompt}")
        .assert()
        .success();

    let captured = std::fs::read_to_string(output_file.path())?;
    assert_eq!(captured, "payload from env");
    Ok(())
}

#[test]
fn internal_capture_arg_enforces_prompt_limit() -> Result<(), Box<dyn std::error::Error>> {
    let temp = TempDir::new()?;
    let provider = temp.child("provider.sh");
    provider.write_str("#!/bin/sh\nexit 0\n")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o755);
        std::fs::set_permissions(provider.path(), perms)?;
    }

    #[allow(deprecated)]
    Command::cargo_bin("tx-dev")?
        .env("TX_CAPTURE_STDIN_DATA", "this exceeds limit")
        .arg("internal")
        .arg("capture-arg")
        .arg("--provider")
        .arg("demo")
        .arg("--bin")
        .arg(provider.path())
        .arg("--prompt-limit")
        .arg("4")
        .assert()
        .failure()
        .stderr(contains("captured prompt exceeds configured limit"));

    Ok(())
}
