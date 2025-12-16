use std::path::Path;
use std::sync::{LazyLock, Mutex};

/// Global mutex for tests that mutate process-wide environment variables.
/// Many tests tweak `PATH` or other env vars; sharing a single lock avoids
/// cross-test races when they run in parallel.
pub static ENV_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

/// Render a filesystem path so it can be embedded inside TOML without
/// triggering escape sequences on Windows.
#[must_use]
pub fn toml_path(path: &Path) -> String {
    let rendered = path.to_string_lossy();
    #[cfg(windows)]
    {
        rendered.replace('\\', "\\\\")
    }
    #[cfg(not(windows))]
    {
        rendered.to_string()
    }
}
