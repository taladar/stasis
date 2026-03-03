// Author: Dustin Pilgrim
// License: MIT

mod engine;
mod info;
mod list;
mod snapshot;

use crate::core::config::ConfigFile;
use crate::core::events::MediaState;

#[derive(Debug, Clone)]
pub struct Manager {
    cfg_file: ConfigFile,
    last_media: MediaState,
}

impl Manager {
    pub fn new(cfg_file: ConfigFile) -> Self {
        Self {
            cfg_file,
            last_media: MediaState::Idle,
        }
    }

    pub fn set_config(&mut self, cfg_file: ConfigFile) {
        self.cfg_file = cfg_file;
    }

    pub fn cfg_file_ref(&self) -> &crate::core::config::ConfigFile {
        &self.cfg_file
    }
}
