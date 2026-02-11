use std::collections::HashSet;
use std::io::{self, BufRead, Read, Write};
use std::process::{Command, Stdio};
use std::sync::LazyLock;

use color_eyre::Result;
use color_eyre::eyre::{Context, eyre};
use regex::Regex;
use serde_json::Value;

use crate::cli::{InternalCaptureArgCommand, InternalCommand, InternalPromptAssemblerCommand};

const PROMPT_PLACEHOLDER: &str = "{prompt}";
static PROMPT_ARG_TOKEN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\{(\d+)\}").expect("valid placeholder regex"));

/// Execute an internal helper command without bootstrapping the full application.
///
/// # Errors
///
/// Returns an error when the helper fails to execute or a subprocess exits
/// unsuccessfully.
pub fn run(command: &InternalCommand) -> Result<()> {
    match command {
        InternalCommand::CaptureArg(cmd) => capture_arg(cmd),
        InternalCommand::PromptAssembler(cmd) => {
            let text = assemble_prompt(cmd)?;
            print!("{text}");
            Ok(())
        }
    }
}

fn capture_arg(cmd: &InternalCaptureArgCommand) -> Result<()> {
    let capture_input = std::env::var("TX_CAPTURE_STDIN_DATA").ok();
    let stdin = std::io::stdin();
    let mut handle = stdin.lock();
    let prompt = capture_prompt(cmd, capture_input.as_deref(), &mut handle)?;

    let resolved_args = resolve_provider_args(&cmd.provider_args, &prompt);

    let mut child = Command::new(&cmd.bin);
    child.args(&resolved_args);
    child.stdin(Stdio::inherit());
    child.stdout(Stdio::inherit());
    child.stderr(Stdio::inherit());

    let status = child
        .status()
        .wrap_err_with(|| format!("failed to launch provider '{}'", cmd.provider))?;
    if !status.success() {
        return Err(eyre!(
            "provider '{}' exited with status {status}",
            cmd.provider
        ));
    }

    Ok(())
}

fn capture_prompt(
    cmd: &InternalCaptureArgCommand,
    capture_input: Option<&str>,
    stdin: &mut dyn Read,
) -> Result<String> {
    if cmd.pre_commands.is_empty() {
        if let Some(input) = capture_input {
            if input.len() > cmd.prompt_limit {
                return Err(eyre!(
                    "captured prompt exceeds configured limit of {} bytes",
                    cmd.prompt_limit
                ));
            }
            Ok(input.to_string())
        } else {
            read_reader(stdin, cmd.prompt_limit).wrap_err("failed to read prompt from stdin")
        }
    } else {
        run_pre_pipeline(&cmd.pre_commands, capture_input, cmd.prompt_limit)
    }
}

/// Assemble a prompt via the `pa` CLI, prompting for any missing positional arguments.
///
/// # Errors
///
/// Returns an error if prompt metadata cannot be read, argument collection fails, the
/// assembled prompt exceeds the configured size limit, or `pa` exits unsuccessfully.
pub fn assemble_prompt(cmd: &InternalPromptAssemblerCommand) -> Result<String> {
    let mut args = cmd.prompt_args.clone();
    let required = required_argument_count(&cmd.prompt)?;
    while args.len() < required {
        let index = args.len();
        let value = prompt_for_argument(&cmd.prompt, index)?;
        args.push(value);
    }

    let mut child = Command::new("pa");
    child.arg(&cmd.prompt);
    child.args(&args);
    child.stdin(Stdio::inherit());
    child.stderr(Stdio::inherit());
    child.stdout(Stdio::piped());

    let mut process = child
        .spawn()
        .wrap_err_with(|| format!("failed to execute 'pa {}'", cmd.prompt))?;

    let mut stdout = process
        .stdout
        .take()
        .ok_or_else(|| eyre!("failed to capture prompt output"))?;
    let mut buffer = Vec::new();
    read_with_limit(&mut stdout, cmd.prompt_limit, &mut buffer)?;

    let status = process.wait().wrap_err("failed to wait for pa to exit")?;
    if !status.success() {
        return Err(eyre!("pa exited with status {status}"));
    }

    buffer_to_string(buffer)
}

fn required_argument_count(prompt: &str) -> Result<usize> {
    let detail = fetch_prompt_detail(prompt)?;
    let content = detail
        .get("profile")
        .and_then(|profile| profile.get("content"))
        .and_then(Value::as_str)
        .unwrap_or_default();

    let mut seen = HashSet::new();
    let mut max_index = None;
    for caps in PROMPT_ARG_TOKEN.captures_iter(content) {
        let index = caps
            .get(1)
            .and_then(|m| m.as_str().parse::<usize>().ok())
            .unwrap_or(0);
        if seen.insert(index) {
            max_index = Some(max_index.map_or(index, |max: usize| max.max(index)));
        }
    }

    Ok(max_index.map_or(0, |idx| idx + 1))
}

fn fetch_prompt_detail(name: &str) -> Result<Value> {
    let output = Command::new("pa")
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

fn prompt_for_argument(prompt: &str, index: usize) -> Result<String> {
    let stdin = io::stdin();
    let mut stdin = stdin.lock();
    let mut stderr = io::stderr();
    prompt_for_argument_with_io(prompt, index, &mut stdin, &mut stderr)
}

fn prompt_for_argument_with_io(
    prompt: &str,
    index: usize,
    stdin: &mut dyn BufRead,
    stderr: &mut dyn Write,
) -> Result<String> {
    let prompt_text = format!("tx: prompt '{prompt}' requires a value for placeholder {{{index}}}");
    writeln!(stderr, "{prompt_text}")?;
    write!(stderr, "> ")?;
    stderr.flush()?;

    let mut line = String::new();
    stdin
        .read_line(&mut line)
        .wrap_err("failed to read prompt argument")?;
    while line.ends_with(['\n', '\r']) {
        line.pop();
    }
    Ok(line)
}

fn run_pre_pipeline(commands: &[String], input: Option<&str>, limit: usize) -> Result<String> {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
    let pipeline = commands.join(" | ");

    let mut child = Command::new(shell);
    child.arg("-c").arg(&pipeline);
    if input.is_some() {
        child.stdin(Stdio::piped());
    } else {
        child.stdin(Stdio::inherit());
    }
    child.stdout(Stdio::piped());
    child.stderr(Stdio::inherit());

    let mut process = child
        .spawn()
        .wrap_err_with(|| format!("failed to spawn pre pipeline '{pipeline}'"))?;

    if let Some(data) = input {
        if data.len() > limit {
            return Err(eyre!(
                "captured prompt exceeds configured limit of {limit} bytes"
            ));
        }
        let mut stdin = process
            .stdin
            .take()
            .ok_or_else(|| eyre!("failed to write to pre pipeline stdin"))?;
        stdin
            .write_all(data.as_bytes())
            .wrap_err("failed to write prompt data to pre pipeline")?;
    }

    let mut stdout = process
        .stdout
        .take()
        .ok_or_else(|| eyre!("failed to capture pre pipeline output"))?;
    let mut buffer = Vec::new();
    read_with_limit(&mut stdout, limit, &mut buffer)?;

    let status = process
        .wait()
        .wrap_err("failed to wait for pre pipeline to exit")?;
    if !status.success() {
        return Err(eyre!("pre pipeline exited with status {status}"));
    }

    buffer_to_string(buffer)
}

fn read_reader(reader: &mut dyn Read, limit: usize) -> Result<String> {
    let mut buffer = Vec::new();
    read_with_limit(reader, limit, &mut buffer)?;
    buffer_to_string(buffer)
}

fn read_with_limit(reader: &mut dyn Read, limit: usize, buffer: &mut Vec<u8>) -> Result<()> {
    let mut chunk = [0u8; 8192];
    loop {
        let read = reader
            .read(&mut chunk)
            .wrap_err("failed to read prompt data")?;
        if read == 0 {
            break;
        }
        if buffer.len() + read > limit {
            return Err(eyre!(
                "captured prompt exceeds configured limit of {limit} bytes"
            ));
        }
        buffer.extend_from_slice(&chunk[..read]);
    }
    Ok(())
}

fn buffer_to_string(buffer: Vec<u8>) -> Result<String> {
    String::from_utf8(buffer).map_err(|err| eyre!("captured prompt is not valid UTF-8: {err}"))
}

fn resolve_provider_args(args: &[String], prompt: &str) -> Vec<String> {
    let mut resolved = Vec::with_capacity(args.len() + 1);
    let mut replaced = false;
    for raw in args {
        if raw.contains(PROMPT_PLACEHOLDER) {
            resolved.push(raw.replace(PROMPT_PLACEHOLDER, prompt));
            replaced = true;
        } else {
            resolved.push(raw.clone());
        }
    }
    if !replaced {
        resolved.push(prompt.to_string());
    }
    resolved
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use crate::cli::{InternalCaptureArgCommand, InternalCommand, InternalPromptAssemblerCommand};
    #[cfg(unix)]
    use crate::test_support::{ENV_LOCK, EnvOverride};
    #[cfg(unix)]
    use assert_fs::TempDir;
    #[cfg(unix)]
    use assert_fs::prelude::*;
    #[cfg(unix)]
    use std::fs::{self, File};
    use std::io::Cursor;
    #[cfg(unix)]
    use std::io::Read;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn resolve_provider_args_appends_prompt_when_missing_placeholder() {
        let args = vec!["--flag".into(), "--prompt".into()];
        let resolved = resolve_provider_args(&args, "hello");
        assert_eq!(resolved, vec!["--flag", "--prompt", "hello"]);
    }

    #[test]
    fn resolve_provider_args_replaces_placeholder() {
        let args = vec!["--flag".into(), "{prompt}".into()];
        let resolved = resolve_provider_args(&args, "hi");
        assert_eq!(resolved, vec!["--flag", "hi"]);
    }

    #[test]
    fn read_reader_consumes_data_within_limit() -> Result<()> {
        let mut cursor = Cursor::new("payload");
        let output = read_reader(&mut cursor, 16)?;
        assert_eq!(output, "payload");
        Ok(())
    }

    #[test]
    fn read_reader_errors_when_limit_exceeded() {
        let mut cursor = Cursor::new("payload");
        let err = read_reader(&mut cursor, 3).unwrap_err();
        assert!(
            err.to_string()
                .contains("captured prompt exceeds configured limit")
        );
    }

    #[cfg(unix)]
    #[test]
    fn capture_prompt_reads_stdin_when_capture_env_is_missing() -> Result<()> {
        let cmd = InternalCaptureArgCommand {
            provider: "demo".into(),
            bin: "echo".into(),
            pre_commands: Vec::new(),
            provider_args: Vec::new(),
            prompt_limit: 64,
        };
        let mut stdin = Cursor::new("prompt from stdin");
        let prompt = capture_prompt(&cmd, None, &mut stdin)?;
        assert_eq!(prompt, "prompt from stdin");
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn capture_prompt_propagates_pre_pipeline_failure() {
        let cmd = InternalCaptureArgCommand {
            provider: "demo".into(),
            bin: "echo".into(),
            pre_commands: vec!["false".into()],
            provider_args: Vec::new(),
            prompt_limit: 64,
        };
        let mut stdin = Cursor::new("");
        let err = capture_prompt(&cmd, None, &mut stdin).unwrap_err();
        assert!(err.to_string().contains("pre pipeline exited with status"));
    }

    #[test]
    fn prompt_for_argument_with_io_trims_newline_and_writes_prompt() -> Result<()> {
        let mut input = Cursor::new("value\r\n");
        let mut stderr = Vec::new();
        let value = prompt_for_argument_with_io("demo", 2, &mut input, &mut stderr)?;
        assert_eq!(value, "value");
        let output = String::from_utf8(stderr).expect("valid utf8");
        assert!(output.contains("placeholder {2}"));
        assert!(output.contains("> "));
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn run_pre_pipeline_uses_stdin_input() -> Result<()> {
        let commands = vec!["cat".to_string()];
        let output = run_pre_pipeline(&commands, Some("ping"), 32)?;
        assert_eq!(output, "ping");
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn run_pre_pipeline_errors_when_output_exceeds_limit() {
        let commands = vec!["printf 'abcdef'".to_string()];
        let err = run_pre_pipeline(&commands, None, 3).unwrap_err();
        assert!(err.to_string().contains("exceeds configured limit"));
    }

    #[cfg(unix)]
    #[test]
    fn run_pre_pipeline_propagates_non_zero_status() {
        let commands = vec!["false".to_string()];
        let err = run_pre_pipeline(&commands, None, 16).unwrap_err();
        assert!(err.to_string().contains("pre pipeline exited with status"));
    }

    #[cfg(unix)]
    #[test]
    fn run_pre_pipeline_writes_input_to_command() -> Result<()> {
        let temp = TempDir::new()?;
        let log_path = temp.child("log.txt");
        let script = temp.child("capture.sh");
        script.write_str("#!/bin/sh\ncat > \"$1\"\n")?;
        #[cfg(unix)]
        {
            let perms = fs::Permissions::from_mode(0o755);
            fs::set_permissions(script.path(), perms)?;
        }

        let commands = vec![format!(
            "{} {}",
            script.path().display(),
            log_path.path().display()
        )];
        run_pre_pipeline(&commands, Some("payload"), 64)?;

        let mut contents = String::new();
        File::open(log_path.path())?.read_to_string(&mut contents)?;
        assert_eq!(contents, "payload");
        Ok(())
    }

    #[test]
    fn buffer_to_string_reports_invalid_utf8() {
        let err = buffer_to_string(vec![0xff, 0xfe]).unwrap_err();
        assert!(err.to_string().contains("not valid UTF-8"));
    }

    #[cfg(unix)]
    #[test]
    fn capture_arg_uses_env_data_and_invokes_provider() -> Result<()> {
        let _guard = ENV_LOCK.lock().unwrap();
        let temp = TempDir::new()?;
        let output = temp.child("prompt.txt");
        let script = temp.child("provider.sh");
        script.write_str("#!/bin/sh\nprintf '%s' \"$2\" > \"$1\"\n")?;
        let perms = fs::Permissions::from_mode(0o755);
        fs::set_permissions(script.path(), perms)?;

        let cmd = InternalCaptureArgCommand {
            provider: "demo".into(),
            bin: script.path().display().to_string(),
            pre_commands: Vec::new(),
            provider_args: vec![output.path().display().to_string(), "{prompt}".into()],
            prompt_limit: 64,
        };

        let _capture = EnvOverride::set_var("TX_CAPTURE_STDIN_DATA", "prompt payload");

        capture_arg(&cmd)?;

        let contents = std::fs::read_to_string(output.path())?;
        assert_eq!(contents, "prompt payload");
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn capture_arg_uses_pre_pipeline_output() -> Result<()> {
        let _guard = ENV_LOCK.lock().unwrap();
        let _capture = EnvOverride::remove("TX_CAPTURE_STDIN_DATA");

        let temp = TempDir::new()?;
        let output = temp.child("prompt.txt");
        let script = temp.child("provider.sh");
        script.write_str("#!/bin/sh\nprintf '%s' \"$2\" > \"$1\"\n")?;
        let perms = fs::Permissions::from_mode(0o755);
        fs::set_permissions(script.path(), perms)?;

        let cmd = InternalCaptureArgCommand {
            provider: "demo".into(),
            bin: script.path().display().to_string(),
            pre_commands: vec!["printf 'filtered data'".into()],
            provider_args: vec![output.path().display().to_string(), "{prompt}".into()],
            prompt_limit: 128,
        };

        capture_arg(&cmd)?;
        let contents = std::fs::read_to_string(output.path())?;
        assert_eq!(contents, "filtered data");
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn capture_arg_errors_when_prompt_exceeds_limit() {
        let _guard = ENV_LOCK.lock().unwrap();
        let temp = TempDir::new().expect("temp dir");
        let script = temp.child("provider.sh");
        script
            .write_str("#!/bin/sh\nexit 0\n")
            .expect("write script");
        let perms = fs::Permissions::from_mode(0o755);
        fs::set_permissions(script.path(), perms).expect("set perms");

        let cmd = InternalCaptureArgCommand {
            provider: "demo".into(),
            bin: script.path().display().to_string(),
            pre_commands: Vec::new(),
            provider_args: Vec::new(),
            prompt_limit: 4,
        };

        let _capture = EnvOverride::set_var("TX_CAPTURE_STDIN_DATA", "exceeds");

        let err = capture_arg(&cmd).unwrap_err();
        assert!(err.to_string().contains("exceeds configured limit"));
    }

    #[cfg(unix)]
    #[test]
    fn capture_arg_propagates_provider_failure() -> Result<()> {
        let _guard = ENV_LOCK.lock().unwrap();
        let _capture = EnvOverride::remove("TX_CAPTURE_STDIN_DATA");

        let temp = TempDir::new()?;
        let script = temp.child("provider.sh");
        script.write_str("#!/bin/sh\nexit 42\n")?;
        let perms = fs::Permissions::from_mode(0o755);
        fs::set_permissions(script.path(), perms)?;

        let cmd = InternalCaptureArgCommand {
            provider: "demo".into(),
            bin: script.path().display().to_string(),
            pre_commands: vec!["printf 'prompt'".into()],
            provider_args: Vec::new(),
            prompt_limit: 64,
        };

        let err = capture_arg(&cmd).unwrap_err();
        assert!(err.to_string().contains("exited with status"));
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn run_pre_pipeline_errors_when_input_exceeds_limit() {
        let _guard = ENV_LOCK.lock().unwrap();
        let _capture = EnvOverride::remove("TX_CAPTURE_STDIN_DATA");

        let err = run_pre_pipeline(&["cat".into()], Some("toolong"), 3).unwrap_err();
        assert!(err.to_string().contains("exceeds configured limit"));
    }

    #[cfg(unix)]
    #[test]
    fn run_dispatches_capture_arg_command() -> Result<()> {
        let _guard = ENV_LOCK.lock().unwrap();
        let _capture = EnvOverride::remove("TX_CAPTURE_STDIN_DATA");

        let temp = TempDir::new()?;
        let output = temp.child("prompt.txt");
        let script = temp.child("provider.sh");
        script.write_str("#!/bin/sh\nprintf '%s' \"$2\" > \"$1\"\n")?;
        let perms = fs::Permissions::from_mode(0o755);
        fs::set_permissions(script.path(), perms)?;

        let command = InternalCommand::CaptureArg(InternalCaptureArgCommand {
            provider: "demo".into(),
            bin: script.path().display().to_string(),
            pre_commands: vec!["printf 'pipeline data'".into()],
            provider_args: vec![output.path().display().to_string(), "{prompt}".into()],
            prompt_limit: 64,
        });

        run(&command)?;

        let contents = std::fs::read_to_string(output.path())?;
        assert_eq!(contents, "pipeline data");
        Ok(())
    }

    #[test]
    fn read_reader_errors_when_exceeding_limit() {
        let mut cursor = std::io::Cursor::new(b"abcdef".to_vec());
        let err = read_reader(&mut cursor, 3).unwrap_err();
        assert!(err.to_string().contains("exceeds configured limit"));
    }

    #[cfg(unix)]
    #[test]
    fn prompt_assembler_uses_placeholder_metadata() -> Result<()> {
        let _guard = ENV_LOCK.lock().unwrap();
        let temp = TempDir::new()?;
        let args_log = temp.child("args.log");
        let output_log = temp.child("output.log");
        let pa = temp.child("pa");
        pa.write_str(
            r#"#!/bin/sh
if [ "$1" = "show" ] && [ "${2:-}" = "--json" ]; then
  shift 2
  printf '%s' '{"profile":{"content":"Demo {0}\n"}}'
  exit 0
fi
if [ $# -gt 0 ]; then
  shift
fi
if [ -n "${PA_ARGS_LOG:-}" ]; then
  printf '%s' "$*" > "$PA_ARGS_LOG"
fi
first_arg=${1:-}
printf 'Demo %s\n' "$first_arg"
if [ -n "${PA_OUTPUT_LOG:-}" ]; then
  printf 'Demo %s\n' "$first_arg" > "$PA_OUTPUT_LOG"
fi
"#,
        )
        .expect("write pa script");
        let perms = fs::Permissions::from_mode(0o755);
        fs::set_permissions(pa.path(), perms)?;

        let original_path = std::env::var("PATH").unwrap_or_default();
        let pa_dir = pa.path().parent().unwrap().display().to_string();
        let new_path = format!("{pa_dir}:{original_path}");
        let _path = EnvOverride::set_var("PATH", &new_path);
        let _args_log = EnvOverride::set_path("PA_ARGS_LOG", args_log.path());
        let _output_log = EnvOverride::set_path("PA_OUTPUT_LOG", output_log.path());

        let cmd = InternalPromptAssemblerCommand {
            prompt: "demo".into(),
            prompt_args: vec!["value".into()],
            prompt_limit: 1024,
        };

        let text = assemble_prompt(&cmd)?;

        let args_written = std::fs::read_to_string(args_log.path())?;
        assert_eq!(args_written, "value");

        let output_written = std::fs::read_to_string(output_log.path())?;
        assert_eq!(output_written.trim_end(), "Demo value");
        assert_eq!(text.trim_end(), "Demo value");
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn assemble_prompt_propagates_non_zero_status() {
        let _guard = ENV_LOCK.lock().unwrap();
        let temp = TempDir::new().expect("tempdir");
        let pa = temp.child("pa");
        pa.write_str(
            r#"#!/bin/sh
if [ "$1" = "show" ] && [ "${2:-}" = "--json" ]; then
  printf '%s' '{"profile":{"content":"Demo {0}"}}'
  exit 0
fi
exit 3
"#,
        )
        .expect("write script");
        #[cfg(unix)]
        {
            let perms = std::fs::Permissions::from_mode(0o755);
            std::fs::set_permissions(pa.path(), perms).expect("chmod");
        }

        let original_path = std::env::var("PATH").unwrap_or_default();
        let pa_dir = pa.path().parent().unwrap().display().to_string();
        let new_path = format!("{pa_dir}:{original_path}");
        let _path = EnvOverride::set_var("PATH", &new_path);

        let cmd = InternalPromptAssemblerCommand {
            prompt: "demo".into(),
            prompt_args: vec!["value".into()],
            prompt_limit: 256,
        };

        let err = assemble_prompt(&cmd).expect_err("pa failure should bubble up");
        assert!(err.to_string().contains("exited with status"));
    }

    #[cfg(unix)]
    #[test]
    fn fetch_prompt_detail_reports_non_zero_status_for_show() -> Result<()> {
        let _guard = ENV_LOCK.lock().unwrap();
        let temp = TempDir::new()?;
        let pa = temp.child("pa");
        pa.write_str("#!/bin/sh\nif [ \"$1\" = \"show\" ] && [ \"${2:-}\" = \"--json\" ]; then\n  exit 7\nfi\nexit 0\n")?;
        let perms = fs::Permissions::from_mode(0o755);
        fs::set_permissions(pa.path(), perms)?;

        let original_path = std::env::var("PATH").unwrap_or_default();
        let pa_dir = pa.path().parent().unwrap().display().to_string();
        let new_path = format!("{pa_dir}:{original_path}");
        let _path = EnvOverride::set_var("PATH", &new_path);

        let err = fetch_prompt_detail("demo").expect_err("show command should fail");
        assert!(err.to_string().contains("pa exited with status"));
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn required_argument_count_tracks_highest_unique_placeholder() -> Result<()> {
        let _guard = ENV_LOCK.lock().unwrap();
        let temp = TempDir::new()?;
        let pa = temp.child("pa");
        pa.write_str(
            r#"#!/bin/sh
if [ "$1" = "show" ] && [ "${2:-}" = "--json" ]; then
  printf '%s' '{"profile":{"content":"A {0} B {2} C {2}"}}'
  exit 0
fi
exit 1
"#,
        )
        .expect("write script");
        let perms = std::fs::Permissions::from_mode(0o755);
        std::fs::set_permissions(pa.path(), perms)?;

        let original_path = std::env::var("PATH").unwrap_or_default();
        let pa_dir = pa.path().parent().unwrap().display().to_string();
        let new_path = format!("{pa_dir}:{original_path}");
        let _path = EnvOverride::set_var("PATH", &new_path);

        let count = required_argument_count("demo")?;
        assert_eq!(count, 3);
        Ok(())
    }
}
