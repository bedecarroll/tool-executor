use std::path::Path;

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
