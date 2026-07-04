//! Backends de armazenamento em nuvem. Cada provedor implementa [`CloudBackend`];
//! o daemon so enxerga essa trait, nunca os detalhes de Google Drive/Box.

pub mod box_com;
pub mod gdrive;

use std::path::Path;

use anyhow::Result;
use async_trait::async_trait;

use crate::ipc::CloudProvider;

/// Um destino de backup na nuvem. A autenticacao (OAuth2) acontece uma vez via
/// CLI (`playsync cloud connect`); o daemon so precisa do token ja salvo.
#[async_trait]
pub trait CloudBackend: Send + Sync {
    fn provider(&self) -> CloudProvider;

    /// Envia (ou substitui) um arquivo/pasta de save para a nuvem.
    async fn upload(&self, local_path: &Path, remote_name: &str) -> Result<()>;

    /// Verifica se ha um token valido salvo (i.e. `cloud connect` ja foi rodado).
    fn is_connected(&self) -> bool;
}

/// Instancia o backend correspondente ao provedor configurado.
pub fn backend_for(provider: CloudProvider) -> Box<dyn CloudBackend> {
    match provider {
        CloudProvider::GoogleDrive => Box::new(gdrive::GoogleDriveBackend::new()),
        CloudProvider::Box => Box::new(box_com::BoxBackend::new()),
    }
}
