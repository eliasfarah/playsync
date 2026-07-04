//! Protocolo de IPC entre `playsyncd` (daemon) e `playsync` (CLI/TUI).
//!
//! Transporte: um Unix Domain Socket, uma mensagem JSON por linha (`\n`-delimited).
//! Simples o bastante para inspecionar com `socat - UNIX-CONNECT:$XDG_RUNTIME_DIR/playsync.sock`
//! durante o desenvolvimento, sem precisar de um framework de RPC completo.

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Request {
    /// Lista todos os jogos detectados e o status da ultima sincronizacao de cada um.
    Status,
    /// Forca uma sincronizacao agora. `app_id = None` sincroniza todos os jogos elegiveis.
    SyncNow { app_id: Option<u32> },
    /// Retorna as ultimas N entradas do historico de backups.
    History { limit: u32 },
    /// Inicia o fluxo OAuth2 de um provedor de nuvem (abre o navegador via CLI).
    CloudConnect { provider: CloudProvider },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CloudProvider {
    GoogleDrive,
    Box,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Response {
    Status { games: Vec<GameStatus> },
    History { entries: Vec<BackupEntry> },
    Ack,
    Error { message: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GameStatus {
    pub app_id: u32,
    pub name: String,
    pub last_backup: Option<DateTime<Utc>>,
    pub sync_status: SyncStatus,
    /// `false` quando nenhuma pasta de save foi detectada pra esse jogo —
    /// distingue "ainda nao sincronizou" de "nunca vai sincronizar sozinho
    /// porque nao acha onde o save fica" (o jogo pode guardar save num
    /// lugar fora do convencional; ver `extra_save_paths` no config.toml).
    pub has_save_paths: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SyncStatus {
    NeverSynced,
    Idle,
    Running,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupEntry {
    pub app_id: u32,
    pub name: String,
    pub timestamp: DateTime<Utc>,
    pub destination: String,
    pub success: bool,
}

/// Caminho do socket UDS usado para a comunicacao CLI <-> daemon.
/// Sob `$XDG_RUNTIME_DIR` (tmpfs, por usuario) para nao deixar residuo em disco
/// nem exigir permissoes especiais.
pub fn socket_path() -> PathBuf {
    let runtime_dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    runtime_dir.join("playsync.sock")
}
