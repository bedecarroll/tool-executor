use std::borrow::Cow;
use std::ffi::OsString;
use std::process::Command;

use color_eyre::Result;
use color_eyre::eyre::{Context, eyre};
use serde_json::Value;

use crate::config::model::PromptAssemblerConfig;

#[derive(Debug, Clone)]
pub struct VirtualProfile {
    pub key: String,
    pub name: String,
    pub description: Option<String>,
    pub tags: Vec<String>,
    pub stdin_supported: bool,
    pub contents: Vec<String>,
}

#[derive(Debug)]
pub enum PromptStatus {
    Disabled,
    Ready { profiles: Vec<VirtualProfile> },
    Unavailable { message: String },
}

pub struct PromptAssembler {
    config: PromptAssemblerConfig,
}

impl PromptAssembler {
    #[must_use]
    pub fn new(config: PromptAssemblerConfig) -> Self {
        Self { config }
    }

    pub fn refresh(&mut self, _force: bool) -> PromptStatus {
        match fetch_prompts(&self.config) {
            Ok(profiles) => PromptStatus::Ready { profiles },
            Err(err) => PromptStatus::Unavailable {
                message: format!("prompt assembler unavailable: {err:#}")
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .to_string(),
            },
        }
    }
}

fn fetch_prompts(config: &PromptAssemblerConfig) -> Result<Vec<VirtualProfile>> {
    let output = pa_command()
        .args(["list", "--json"])
        .output()
        .context("failed to execute 'pa list --json'")?;

    if !output.status.success() {
        return Err(eyre!("pa exited with status {}", output.status));
    }

    let root: Value =
        serde_json::from_slice(&output.stdout).context("failed to parse JSON output from pa")?;

    let entries = if let Some(array) = root.as_array() {
        Cow::Borrowed(array)
    } else if let Some(array) = root.get("prompts").and_then(Value::as_array) {
        Cow::Borrowed(array)
    } else {
        return Err(eyre!(
            "unexpected JSON shape from pa; expected an array or object with 'prompts'"
        ));
    };

    let mut profiles = Vec::new();
    for entry in entries.iter() {
        let Some(name) = entry.get("name").and_then(Value::as_str) else {
            continue;
        };
        let mut description = entry
            .get("description")
            .or_else(|| entry.get("summary"))
            .and_then(Value::as_str)
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        let mut tags = entry
            .get("tags")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(Value::as_str)
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let mut stdin_supported = entry
            .get("stdin_supported")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        let detail = fetch_prompt_detail(name)?;
        if description.is_none() {
            description = detail
                .get("description")
                .and_then(Value::as_str)
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty());
        }
        if tags.is_empty() {
            tags = detail
                .get("tags")
                .and_then(Value::as_array)
                .map(|arr| {
                    arr.iter()
                        .filter_map(Value::as_str)
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
        }
        stdin_supported = detail
            .get("stdin_supported")
            .and_then(Value::as_bool)
            .unwrap_or(stdin_supported);

        let contents = extract_profile_lines(&detail);

        profiles.push(VirtualProfile {
            key: format!("{}/{}", config.namespace, name),
            name: name.to_string(),
            description,
            tags,
            stdin_supported,
            contents,
        });
    }

    Ok(profiles)
}

fn fetch_prompt_detail(name: &str) -> Result<Value> {
    let output = pa_command()
        .args(["show", "--json", name])
        .output()
        .with_context(|| format!("failed to execute 'pa show --json {name}'"))?;

    if !output.status.success() {
        return Err(eyre!(
            "pa exited with status {} while loading prompt '{name}'",
            output.status
        ));
    }

    serde_json::from_slice(&output.stdout)
        .with_context(|| format!("failed to parse JSON output from 'pa show --json {name}'"))
}

fn pa_command() -> Command {
    let bin = std::env::var_os("TX_TEST_PA_BIN").unwrap_or_else(|| OsString::from("pa"));
    Command::new(bin)
}

fn extract_profile_lines(detail: &Value) -> Vec<String> {
    if let Some(content) = detail
        .get("profile")
        .and_then(|profile| profile.get("content"))
        .and_then(Value::as_str)
    {
        return content.lines().map(str::to_string).collect();
    }

    if let Some(parts) = detail
        .get("profile")
        .and_then(|profile| profile.get("parts"))
        .and_then(Value::as_array)
    {
        let mut lines = Vec::new();
        for part in parts {
            if let Some(content) = part.get("content").and_then(Value::as_str) {
                lines.extend(content.lines().map(str::to_string));
            }
        }
        if !lines.is_empty() {
            return lines;
        }
    }

    Vec::new()
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::test_support::ENV_LOCK;
    use assert_fs::TempDir;
    use assert_fs::prelude::*;
    use serde_json::json;
    use std::env;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    fn with_fake_pa(script: &str) -> (TempDir, PromptAssemblerConfig) {
        let temp = TempDir::new().expect("tempdir");
        let bin = temp.child("pa");
        bin.write_str(script).expect("write script");
        #[cfg(unix)]
        {
            let perms = std::fs::Permissions::from_mode(0o755);
            std::fs::set_permissions(bin.path(), perms).expect("chmod");
        }
        let ns = "tests";
        let cfg = PromptAssemblerConfig {
            namespace: ns.to_string(),
        };
        (temp, cfg)
    }

    struct PathGuard {
        original: Option<String>,
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl PathGuard {
        fn new(dir: &TempDir) -> Self {
            let lock = ENV_LOCK.lock().unwrap();
            let original = env::var("PATH").ok();
            let mut paths = vec![dir.path().to_path_buf()];
            if let Some(existing) = &original {
                paths.extend(env::split_paths(existing).collect::<Vec<_>>());
            }
            let joined = env::join_paths(paths).expect("join paths");
            unsafe {
                env::set_var("PATH", joined);
            }
            Self {
                original,
                _lock: lock,
            }
        }
    }

    impl Drop for PathGuard {
        fn drop(&mut self) {
            if let Some(value) = &self.original {
                unsafe {
                    env::set_var("PATH", value);
                }
            }
        }
    }

    fn set_path(dir: &TempDir) -> PathGuard {
        PathGuard::new(dir)
    }

    fn restore_env(key: &str, value: Option<String>) {
        if let Some(val) = value {
            unsafe { env::set_var(key, val) };
        } else {
            unsafe { env::remove_var(key) };
        }
    }

    fn restore_env_branches(key: &str, value: Option<String>) {
        restore_env(key, Some("tx-test-dummy".into()));
        restore_env(key, None);
        restore_env(key, value);
    }

    #[cfg(unix)]
    #[test]
    fn fetch_prompts_parses_array_root() -> Result<()> {
        let (dir, cfg) = with_fake_pa(
            r#"#!/bin/sh
case "$1" in
  list)
    cat <<'JSON'
[{
  "name": "demo",
  "stdin_supported": true
}]
JSON
    exit 0
    ;;
  show)
    if [ "$3" = "demo" ]; then
      cat <<'JSON'
{
  "name": "demo",
  "stdin_supported": true,
  "profile": {
    "content": "Line one\nLine two\n"
  }
}
JSON
      exit 0
    fi
    ;;
esac
exit 1
"#,
        );
        let _guard = set_path(&dir);
        let profiles = fetch_prompts(&cfg)?;
        assert_eq!(profiles.len(), 1);
        let profile = &profiles[0];
        assert_eq!(profile.key, "tests/demo");
        assert_eq!(profile.name, "demo");
        assert!(profile.stdin_supported);
        assert_eq!(
            profile.contents,
            vec!["Line one".to_string(), "Line two".to_string()]
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn fetch_prompts_parses_object_root() -> Result<()> {
        let (dir, cfg) = with_fake_pa(
            r#"#!/bin/sh
case "$1" in
  list)
    cat <<'JSON'
{
  "prompts": [
    {
      "name": "demo",
      "tags": ["one", "two"]
    }
  ]
}
JSON
    exit 0
    ;;
  show)
    if [ "$3" = "demo" ]; then
      cat <<'JSON'
{
  "name": "demo",
  "tags": ["one", "two"],
  "profile": {
    "content": "Hello\n"
  }
}
JSON
      exit 0
    fi
    ;;
esac
exit 1
"#,
        );
        let _guard = set_path(&dir);
        let profiles = fetch_prompts(&cfg)?;
        assert_eq!(profiles[0].tags, vec!["one", "two"]);
        assert_eq!(profiles[0].contents, vec!["Hello".to_string()]);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn fetch_prompts_reports_process_failure() {
        let (dir, cfg) = with_fake_pa("#!/bin/sh\nexit 42\n");
        let _guard = set_path(&dir);
        let err = fetch_prompts(&cfg).unwrap_err();
        assert!(err.to_string().contains("exited with status"));
    }

    #[cfg(unix)]
    #[test]
    fn fetch_prompts_errors_on_unexpected_shape() {
        let (dir, cfg) = with_fake_pa("#!/bin/sh\necho '{}'\n");
        let _guard = set_path(&dir);
        let err = fetch_prompts(&cfg).unwrap_err();
        assert!(err.to_string().contains("unexpected JSON shape from pa"));
    }

    #[cfg(unix)]
    #[test]
    fn prompt_assembler_refresh_reports_unavailable() {
        let (dir, cfg) = with_fake_pa("#!/bin/sh\nexit 1\n");
        let _guard = set_path(&dir);
        let mut assembler = PromptAssembler::new(cfg.clone());
        let status = assembler.refresh(true);
        match status {
            PromptStatus::Unavailable { message } => {
                assert!(message.contains("prompt assembler unavailable"));
            }
            other => panic!("unexpected status: {other:?}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn fetch_prompts_enriches_entries_with_detail_fallbacks() -> Result<()> {
        let temp = TempDir::new()?;
        let pa = temp.child("pa");
        pa.write_str(
            r#"#!/bin/sh
if [ "$1" = "list" ]; then
  printf '{"prompts":[{"name":"demo"}]}'
  exit 0
fi
if [ "$1" = "show" ]; then
  printf '{"profile":{"content":"Line A\\nLine B\\n"},"description":"From detail","tags":["t1","t2"],"stdin_supported":true}'
  exit 0
fi
exit 1
"#,
        )?;
        #[cfg(unix)]
        {
            let perms = std::fs::Permissions::from_mode(0o755);
            std::fs::set_permissions(pa.path(), perms)?;
        }

        let _guard = set_path(&temp);

        let cfg = PromptAssemblerConfig {
            namespace: "tests".into(),
        };
        let profiles = fetch_prompts(&cfg)?;
        assert_eq!(profiles.len(), 1);
        let profile = &profiles[0];
        assert_eq!(profile.name, "demo");
        assert_eq!(profile.description.as_deref(), Some("From detail"));
        assert_eq!(profile.tags, vec!["t1".to_string(), "t2".to_string()]);
        assert!(profile.stdin_supported);
        assert_eq!(
            profile.contents,
            vec!["Line A".to_string(), "Line B".to_string()]
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn fetch_prompt_detail_reports_process_failure() {
        let (dir, _cfg) = with_fake_pa(
            r#"#!/bin/sh
if [ "$1" = "show" ]; then
  exit 2
fi
exit 1
"#,
        );
        let _guard = set_path(&dir);
        let err = fetch_prompt_detail("demo").unwrap_err();
        assert!(err.to_string().contains("exited with status"));
    }

    #[cfg(unix)]
    #[test]
    fn fetch_prompt_detail_errors_on_invalid_json() {
        let (dir, _cfg) = with_fake_pa(
            r#"#!/bin/sh
if [ "$1" = "show" ]; then
  printf '{'
  exit 0
fi
exit 1
"#,
        );
        let _guard = set_path(&dir);
        let err = fetch_prompt_detail("demo").unwrap_err();
        assert!(err.to_string().contains("failed to parse JSON output"));
    }

    #[test]
    fn extract_profile_lines_uses_parts_when_content_missing() {
        let detail = json!({
            "profile": {
                "parts": [
                    { "content": "Line one\nLine two\n" },
                    { "content": "Line three" }
                ]
            }
        });
        let lines = extract_profile_lines(&detail);
        assert_eq!(
            lines,
            vec![
                "Line one".to_string(),
                "Line two".to_string(),
                "Line three".to_string()
            ]
        );
    }

    #[test]
    fn extract_profile_lines_returns_empty_when_missing() {
        let detail = json!({});
        let lines = extract_profile_lines(&detail);
        assert!(lines.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn fetch_prompts_skips_entries_without_name() -> Result<()> {
        let (dir, cfg) = with_fake_pa(
            r#"#!/bin/sh
case "$1" in
  list)
    cat <<'JSON'
[
  { "name": "demo" },
  { "tags": ["skip"] }
]
JSON
    exit 0
    ;;
  show)
    if [ "$3" = "demo" ]; then
      cat <<'JSON'
{ "profile": { "content": "Hello\n" } }
JSON
      exit 0
    fi
    ;;
esac
exit 1
"#,
        );
        let _guard = set_path(&dir);
        let profiles = fetch_prompts(&cfg)?;
        assert_eq!(profiles.len(), 1);
        assert_eq!(profiles[0].name, "demo");
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn prompt_assembler_refresh_reports_ready() {
        let (dir, cfg) = with_fake_pa(
            r#"#!/bin/sh
case "$1" in
  list)
    cat <<'JSON'
[{
  "name": "demo",
  "stdin_supported": true
}]
JSON
    exit 0
    ;;
  show)
    if [ "$3" = "demo" ]; then
      cat <<'JSON'
{
  "profile": {
    "content": "Line one\n"
  }
}
JSON
      exit 0
    fi
    ;;
esac
exit 1
"#,
        );
        let _guard = set_path(&dir);
        let mut assembler = PromptAssembler::new(cfg);
        match assembler.refresh(true) {
            PromptStatus::Ready { profiles } => {
                assert_eq!(profiles.len(), 1);
                assert_eq!(profiles[0].name, "demo");
            }
            other => panic!("unexpected status: {other:?}"),
        }
    }

    #[test]
    fn pa_command_prefers_env_override() {
        let _guard = ENV_LOCK.lock().unwrap();
        let original = env::var("TX_TEST_PA_BIN").ok();
        unsafe {
            env::set_var("TX_TEST_PA_BIN", "pa-override");
        }
        let cmd = pa_command();
        assert_eq!(cmd.get_program(), "pa-override");
        restore_env_branches("TX_TEST_PA_BIN", original);
    }

    #[cfg(unix)]
    #[test]
    fn fetch_prompts_keeps_entry_stdin_supported() -> Result<()> {
        let (dir, cfg) = with_fake_pa(
            r#"#!/bin/sh
case "$1" in
  list)
    cat <<'JSON'
[{
  "name": "demo",
  "stdin_supported": true
}]
JSON
    exit 0
    ;;
  show)
    if [ "$3" = "demo" ]; then
      cat <<'JSON'
{ "profile": { "content": "Hello\n" } }
JSON
      exit 0
    fi
    ;;
esac
exit 1
"#,
        );
        let _guard = set_path(&dir);
        let profiles = fetch_prompts(&cfg)?;
        assert_eq!(profiles.len(), 1);
        assert!(profiles[0].stdin_supported);
        Ok(())
    }
}
