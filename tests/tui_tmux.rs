use assert_cmd::cargo::cargo_bin;
use assert_fs::TempDir;
use assert_fs::prelude::*;
use color_eyre::Result;
use shell_escape::unix::escape;
use std::borrow::Cow;
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

fn tmux_available() -> bool {
    Command::new("tmux")
        .arg("-V")
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn tmux_allowed() -> bool {
    std::env::var_os("CI").is_none()
}

#[test]
fn tui_runs_inside_tmux_and_exits_on_escape() -> Result<()> {
    if !tmux_allowed() {
        eprintln!("CI environment detected; skipping tmux smoke test");
        return Ok(());
    }

    if !tmux_available() {
        eprintln!("tmux not available; skipping TUI smoke test");
        return Ok(());
    }

    let temp = TempDir::new()?;
    let config_dir = temp.child("config");
    config_dir.create_dir_all()?;
    let data_dir = temp.child("data");
    data_dir.create_dir_all()?;
    let cache_dir = temp.child("cache");
    cache_dir.create_dir_all()?;

    let tx_path = cargo_bin("tx");
    let session = format!(
        "txcov_{}_{}",
        std::process::id(),
        Instant::now().elapsed().as_nanos()
    );
    let socket = format!("txcov_socket_{}", std::process::id());
    let wait_token = format!("{session}_done");

    let tx_escaped = escape(Cow::Owned(tx_path.display().to_string())).to_string();
    let escape_path = |p: &std::path::Path| escape(p.as_os_str().to_string_lossy()).to_string();
    let cfg_env = escape_path(config_dir.path());
    let data_env = escape_path(data_dir.path());
    let cache_env = escape_path(cache_dir.path());
    let home_env = escape_path(temp.path());
    let script = format!(
        "export TX_CONFIG_DIR={cfg_env}; export TX_DATA_DIR={data_env}; export TX_CACHE_DIR={cache_env}; \
         export HOME={home_env}; export XDG_DATA_HOME={data_env}; export XDG_CACHE_HOME={cache_env}; \
         {tx_escaped}; tmux -L {socket} wait-for -S {wait_token}"
    );

    Command::new("tmux")
        .args([
            "-L",
            &socket,
            "-f",
            "/dev/null",
            "new-session",
            "-d",
            "-s",
            &session,
            "sh",
            "-lc",
            &script,
        ])
        .status()?;

    thread::sleep(Duration::from_millis(500));

    Command::new("tmux")
        .args([
            "-L",
            &socket,
            "send-keys",
            "-t",
            &session,
            "Escape",
            "Escape",
        ])
        .status()?;

    thread::sleep(Duration::from_millis(200));

    Command::new("tmux")
        .args(["-L", &socket, "wait-for", "-L", &wait_token])
        .status()?;

    let has_session = Command::new("tmux")
        .args(["-L", &socket, "has-session", "-t", &session])
        .status();
    assert!(has_session.map(|s| !s.success()).unwrap_or(true));

    let _ = Command::new("tmux")
        .args(["-L", &socket, "kill-server"])
        .status();

    Ok(())
}
