#![cfg(unix)]

use assert_fs::TempDir;
use assert_fs::prelude::*;
use tool_executor::config::model::PromptAssemblerConfig;
use tool_executor::prompts::{PromptAssembler, PromptStatus};
use tool_executor::test_support::{ENV_LOCK, EnvOverride};

fn write_pa_script(temp: &TempDir, body: &str) -> color_eyre::Result<std::path::PathBuf> {
    let pa = temp.child("pa");
    pa.write_str(body)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o755);
        std::fs::set_permissions(pa.path(), perms)?;
    }
    Ok(pa.path().to_path_buf())
}

#[test]
fn prompt_assembler_refresh_reports_ready() -> color_eyre::Result<()> {
    let _guard = ENV_LOCK.lock().unwrap();
    let temp = TempDir::new()?;
    let pa = write_pa_script(
        &temp,
        r#"#!/bin/sh
if [ "$1" = "list" ]; then
  printf '[{"name":"demo","stdin_supported":true}]'
  exit 0
fi
if [ "$1" = "show" ]; then
  printf '{"profile":{"content":"Hello\\n"}}'
  exit 0
fi
exit 1
"#,
    )?;
    let _pa_bin = EnvOverride::set_path("TX_TEST_PA_BIN", &pa);
    let mut assembler = PromptAssembler::new(PromptAssemblerConfig {
        namespace: "tests".to_string(),
    });
    let status = assembler.refresh(true);
    match status {
        PromptStatus::Ready { profiles } => {
            assert_eq!(profiles.len(), 1);
            assert_eq!(profiles[0].name, "demo");
        }
        PromptStatus::Unavailable { message } => {
            panic!("expected ready status, got unavailable: {message}");
        }
    }
    temp.close()?;
    Ok(())
}

#[test]
fn prompt_assembler_refresh_reports_unavailable() -> color_eyre::Result<()> {
    let _guard = ENV_LOCK.lock().unwrap();
    let temp = TempDir::new()?;
    let pa = write_pa_script(&temp, "#!/bin/sh\nexit 1\n")?;
    let _pa_bin = EnvOverride::set_path("TX_TEST_PA_BIN", &pa);
    let mut assembler = PromptAssembler::new(PromptAssemblerConfig {
        namespace: "tests".to_string(),
    });
    let status = assembler.refresh(true);
    match status {
        PromptStatus::Unavailable { message } => {
            assert!(message.contains("prompt assembler unavailable"));
        }
        PromptStatus::Ready { .. } => {
            panic!("expected unavailable status");
        }
    }
    temp.close()?;
    Ok(())
}

#[test]
fn prompt_assembler_refresh_reports_unavailable_when_binary_missing() {
    let _guard = ENV_LOCK.lock().unwrap();
    let _pa_bin = EnvOverride::set_var("TX_TEST_PA_BIN", "/definitely/missing/pa");
    let mut assembler = PromptAssembler::new(PromptAssemblerConfig {
        namespace: "tests".to_string(),
    });
    let status = assembler.refresh(true);
    match status {
        PromptStatus::Unavailable { message } => {
            assert!(message.contains("prompt assembler unavailable"));
            assert!(message.contains("failed to execute"));
        }
        PromptStatus::Ready { .. } => {
            panic!("expected unavailable status");
        }
    }
}

#[test]
fn prompt_assembler_refresh_reports_unavailable_when_show_spawn_fails() -> color_eyre::Result<()> {
    let _guard = ENV_LOCK.lock().unwrap();
    let temp = TempDir::new()?;
    let pa = write_pa_script(
        &temp,
        r#"#!/bin/sh
if [ "$1" = "list" ]; then
  printf '[{"name":"demo"}]'
  rm -f "$0"
  exit 0
fi
exit 1
"#,
    )?;
    let _pa_bin = EnvOverride::set_path("TX_TEST_PA_BIN", &pa);
    let mut assembler = PromptAssembler::new(PromptAssemblerConfig {
        namespace: "tests".to_string(),
    });

    let status = assembler.refresh(true);
    match status {
        PromptStatus::Unavailable { message } => {
            assert!(message.contains("prompt assembler unavailable"));
            assert!(message.contains("failed to execute"));
        }
        PromptStatus::Ready { .. } => {
            panic!("expected unavailable status");
        }
    }

    temp.close()?;
    Ok(())
}
