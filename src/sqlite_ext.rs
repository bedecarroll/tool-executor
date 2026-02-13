use std::sync::OnceLock;

use color_eyre::Result;
use color_eyre::eyre::eyre;
use rusqlite::ffi::{self, sqlite3_auto_extension};
use sqlite_vec::sqlite3_vec_init;

type InitStatus = std::result::Result<(), String>;
type AutoExtensionEntry = unsafe extern "C" fn(
    *mut ffi::sqlite3,
    *mut *mut std::os::raw::c_char,
    *const ffi::sqlite3_api_routines,
) -> std::os::raw::c_int;

static SQLITE_EXTENSIONS_INIT: OnceLock<InitStatus> = OnceLock::new();

/// Register process-wide `SQLite` extensions used by tx.
///
/// # Errors
///
/// Returns an error if sqlite-vec cannot be registered as an auto-extension.
pub fn init_sqlite_extensions() -> Result<()> {
    let status = SQLITE_EXTENSIONS_INIT.get_or_init(|| {
        // SAFETY:
        // - sqlite3_auto_extension requires a C callback matching SQLite's extension init ABI.
        // - sqlite-vec exposes sqlite3_vec_init as that callback.
        // - registration is process-global and guarded by OnceLock to avoid duplicate work.
        let rc = unsafe {
            let init_fn: AutoExtensionEntry =
                std::mem::transmute::<*const (), AutoExtensionEntry>(sqlite3_vec_init as *const ());
            sqlite3_auto_extension(Some(init_fn))
        };
        registration_status_from_rc(rc)
    });

    init_result_from_status(status)
}

fn registration_status_from_rc(rc: std::os::raw::c_int) -> InitStatus {
    if rc == ffi::SQLITE_OK {
        Ok(())
    } else {
        Err(registration_error_message(rc))
    }
}

fn registration_error_message(rc: std::os::raw::c_int) -> String {
    format!("failed to register sqlite-vec auto-extension (sqlite rc={rc})")
}

fn init_result_from_status(status: &InitStatus) -> Result<()> {
    match status {
        Ok(()) => Ok(()),
        Err(message) => Err(eyre!(message.clone())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    #[test]
    fn init_sqlite_extensions_is_idempotent_and_registers_vec() -> Result<()> {
        init_sqlite_extensions()?;
        init_sqlite_extensions()?;

        let conn = Connection::open_in_memory()?;
        let version: String = conn.query_row("SELECT vec_version()", [], |row| row.get(0))?;
        assert!(version.starts_with('v'));
        Ok(())
    }

    #[test]
    fn registration_helpers_surface_error_status() {
        let status = registration_status_from_rc(ffi::SQLITE_ERROR);
        assert_eq!(status, Err(registration_error_message(ffi::SQLITE_ERROR)));

        let err = init_result_from_status(&status).expect_err("status error should propagate");
        assert!(
            err.to_string()
                .contains("failed to register sqlite-vec auto-extension")
        );
    }
}
