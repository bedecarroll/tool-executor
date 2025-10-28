use std::io::{Read, Write};
use std::process::{Command, Stdio};

use color_eyre::Result;
use color_eyre::eyre::{WrapErr, eyre};

use crate::cli::{InternalCaptureArgCommand, InternalCommand};

const PROMPT_PLACEHOLDER: &str = "{prompt}";

/// Execute an internal helper command without bootstrapping the full application.
///
/// # Errors
///
/// Returns an error when the helper fails to execute or a subprocess exits
/// unsuccessfully.
pub fn run(command: &InternalCommand) -> Result<()> {
    match command {
        InternalCommand::CaptureArg(cmd) => capture_arg(cmd),
    }
}

fn capture_arg(cmd: &InternalCaptureArgCommand) -> Result<()> {
    let capture_input = std::env::var("TX_CAPTURE_STDIN_DATA").ok();
    let prompt = if cmd.pre_commands.is_empty() {
        if let Some(input) = capture_input.clone() {
            if input.len() > cmd.prompt_limit {
                return Err(eyre!(
                    "captured prompt exceeds configured limit of {} bytes",
                    cmd.prompt_limit
                ));
            }
            input
        } else {
            read_stdin(cmd.prompt_limit).wrap_err("failed to read prompt from stdin")?
        }
    } else {
        run_pre_pipeline(
            &cmd.pre_commands,
            capture_input.as_deref(),
            cmd.prompt_limit,
        )?
    };

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

fn read_stdin(limit: usize) -> Result<String> {
    let stdin = std::io::stdin();
    let mut handle = stdin.lock();
    read_reader(&mut handle, limit)
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
    use crate::cli::{InternalCaptureArgCommand, InternalCommand};
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
    #[cfg(unix)]
    use std::sync::{LazyLock, Mutex};

    #[cfg(unix)]
    static ENV_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

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

        let original = std::env::var("TX_CAPTURE_STDIN_DATA").ok();
        unsafe {
            std::env::set_var("TX_CAPTURE_STDIN_DATA", "prompt payload");
        }

        capture_arg(&cmd)?;

        let contents = std::fs::read_to_string(output.path())?;
        assert_eq!(contents, "prompt payload");

        if let Some(value) = original {
            unsafe { std::env::set_var("TX_CAPTURE_STDIN_DATA", value) };
        } else {
            unsafe { std::env::remove_var("TX_CAPTURE_STDIN_DATA") };
        }
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn capture_arg_uses_pre_pipeline_output() -> Result<()> {
        let _guard = ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::remove_var("TX_CAPTURE_STDIN_DATA");
        }

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

        let original = std::env::var("TX_CAPTURE_STDIN_DATA").ok();
        unsafe {
            std::env::set_var("TX_CAPTURE_STDIN_DATA", "exceeds");
        }

        let err = capture_arg(&cmd).unwrap_err();

        if let Some(value) = original {
            unsafe { std::env::set_var("TX_CAPTURE_STDIN_DATA", value) };
        } else {
            unsafe { std::env::remove_var("TX_CAPTURE_STDIN_DATA") };
        }

        assert!(
            err.to_string().contains("exceeds configured limit"),
            "unexpected error message: {err:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn capture_arg_propagates_provider_failure() -> Result<()> {
        let _guard = ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::remove_var("TX_CAPTURE_STDIN_DATA");
        }

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
        assert!(
            err.to_string().contains("exited with status"),
            "unexpected error message: {err:?}"
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn run_pre_pipeline_errors_when_input_exceeds_limit() {
        let _guard = ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::remove_var("TX_CAPTURE_STDIN_DATA");
        }

        let err = run_pre_pipeline(&["cat".into()], Some("toolong"), 3).unwrap_err();
        assert!(
            err.to_string().contains("exceeds configured limit"),
            "unexpected error: {err:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn run_dispatches_capture_arg_command() -> Result<()> {
        let _guard = ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::remove_var("TX_CAPTURE_STDIN_DATA");
        }

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
        assert!(
            err.to_string().contains("exceeds configured limit"),
            "unexpected error: {err:?}"
        );
    }
}
