//! Safe sqlite-vec registration boundary.

use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SqliteVecRegistrationError {
    code: i32,
}

impl SqliteVecRegistrationError {
    pub fn code(self) -> i32 {
        self.code
    }
}

impl std::fmt::Display for SqliteVecRegistrationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "sqlite-vec registration failed with SQLite code {}",
            self.code
        )
    }
}

impl std::error::Error for SqliteVecRegistrationError {}

pub fn register_sqlite_vec() -> Result<(), SqliteVecRegistrationError> {
    static REGISTRATION: OnceLock<Result<(), SqliteVecRegistrationError>> = OnceLock::new();
    *REGISTRATION.get_or_init(register_sqlite_vec_once)
}

fn register_sqlite_vec_once() -> Result<(), SqliteVecRegistrationError> {
    use rusqlite::ffi::{SQLITE_OK, sqlite3, sqlite3_api_routines, sqlite3_auto_extension};
    use sqlite_vec::sqlite3_vec_init;

    // sqlite-vec exposes SQLite's C extension entrypoint. Keep the required
    // registration cast contained here so the rest of symdex stays safe Rust.
    type ExtensionInit = unsafe extern "C" fn(
        *mut sqlite3,
        *mut *mut std::os::raw::c_char,
        *const sqlite3_api_routines,
    ) -> std::os::raw::c_int;
    let result = unsafe {
        let entrypoint: ExtensionInit = std::mem::transmute(sqlite3_vec_init as *const ());
        sqlite3_auto_extension(Some(entrypoint))
    };
    if result == SQLITE_OK {
        Ok(())
    } else {
        Err(SqliteVecRegistrationError { code: result })
    }
}
