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
