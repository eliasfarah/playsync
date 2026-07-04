//! Backend do Box.com.
//!
//! TODO: fluxo OAuth2 e chamadas a Box Content API (`POST /files/content` para
//! upload simples, `POST /files/upload_sessions` para arquivos grandes). O
//! token e persistido em `~/.config/playsync/tokens/box.json`.

use std::path::Path;

use anyhow::{bail, Result};
use async_trait::async_trait;

use crate::ipc::CloudProvider;

use super::CloudBackend;

pub struct BoxBackend {
    // TODO: token OAuth2 carregado do disco.
}

impl BoxBackend {
    pub fn new() -> Self {
        Self {}
    }
}

impl Default for BoxBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl CloudBackend for BoxBackend {
    fn provider(&self) -> CloudProvider {
        CloudProvider::Box
    }

    async fn upload(&self, _local_path: &Path, _remote_name: &str) -> Result<()> {
        bail!("Box ainda nao implementado — rode `playsync cloud connect box`")
    }

    fn is_connected(&self) -> bool {
        false
    }
}
