// Author: Dustin Pilgrim
// License: GPL-3.0-only

pub mod client;
pub mod handlers;
pub mod router;
pub mod server;

use std::path::PathBuf;

pub fn runtime_dir() -> Result<PathBuf, String> {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .ok_or_else(|| "XDG_RUNTIME_DIR is not set".to_string())
}

pub fn socket_path() -> Result<PathBuf, String> {
    Ok(runtime_dir()?.join("stasis").join("stasis.sock"))
}
