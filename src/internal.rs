use std::io::Read;
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
    let prompt = if cmd.pre_commands.is_empty() {
        read_stdin(cmd.prompt_limit).wrap_err("failed to read prompt from stdin")?
    } else {
        run_pre_pipeline(&cmd.pre_commands, cmd.prompt_limit)?
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

fn run_pre_pipeline(commands: &[String], limit: usize) -> Result<String> {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
    let pipeline = commands.join(" | ");

    let mut child = Command::new(shell);
    child.arg("-c").arg(&pipeline);
    child.stdin(Stdio::inherit());
    child.stdout(Stdio::piped());
    child.stderr(Stdio::inherit());

    let mut process = child
        .spawn()
        .wrap_err_with(|| format!("failed to spawn pre pipeline '{pipeline}'"))?;

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
    let mut buffer = Vec::new();
    read_with_limit(&mut handle, limit, &mut buffer)?;
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
