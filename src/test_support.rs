use std::ffi::{OsStr, OsString};
use std::path::Path;
use std::sync::{LazyLock, Mutex};

/// Global mutex for tests that mutate process-wide environment variables.
/// Many tests tweak `PATH` or other env vars; sharing a single lock avoids
/// cross-test races when they run in parallel.
pub static ENV_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

/// Restore environment variables in tests while preserving the previous value.
///
/// Callers should hold [`ENV_LOCK`] while constructing instances of this type.
#[derive(Debug)]
pub struct EnvOverride {
    key: String,
    original: Option<OsString>,
}

impl EnvOverride {
    /// Set an environment variable for the current scope.
    #[must_use]
    pub fn set_var(key: impl Into<String>, value: impl AsRef<OsStr>) -> Self {
        let key = key.into();
        let original = std::env::var_os(&key);
        // SAFETY: tests that use this helper hold ENV_LOCK to serialize process-wide env mutation.
        unsafe {
            std::env::set_var(&key, value);
        }
        Self { key, original }
    }

    /// Set an environment variable to a filesystem path for the current scope.
    #[must_use]
    pub fn set_path(key: impl Into<String>, path: &Path) -> Self {
        Self::set_var(key, path.as_os_str())
    }

    /// Remove an environment variable for the current scope.
    #[must_use]
    pub fn remove(key: impl Into<String>) -> Self {
        let key = key.into();
        let original = std::env::var_os(&key);
        // SAFETY: tests that use this helper hold ENV_LOCK to serialize process-wide env mutation.
        unsafe {
            std::env::remove_var(&key);
        }
        Self { key, original }
    }
}

impl Drop for EnvOverride {
    fn drop(&mut self) {
        if let Some(value) = &self.original {
            // SAFETY: tests that use this helper hold ENV_LOCK to serialize process-wide env mutation.
            unsafe {
                std::env::set_var(&self.key, value);
            }
        } else {
            // SAFETY: tests that use this helper hold ENV_LOCK to serialize process-wide env mutation.
            unsafe {
                std::env::remove_var(&self.key);
            }
        }
    }
}

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
