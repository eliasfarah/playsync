//! Motor de sincronizacao: decide o que fazer quando um jogo fecha, fala com
//! o backend de nuvem configurado e registra o resultado no historico.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use playsync_core::archive;
use playsync_core::cloud::{self, CloudBackend};
use playsync_core::config::Config;
use playsync_core::db::HistoryDb;
use playsync_core::ipc::{BackupEntry, CloudProvider, GameStatus, SyncStatus};
use playsync_core::steam::{self, GameSave};
use tokio::sync::Mutex;

pub struct SyncEngine {
    config: Mutex<Config>,
    history: Arc<HistoryDb>,
    status: Mutex<HashMap<u32, SyncStatus>>,
}

impl SyncEngine {
    pub fn new(config: Config, history: Arc<HistoryDb>) -> Self {
        Self {
            config: Mutex::new(config),
            history,
            status: Mutex::new(HashMap::new()),
        }
    }

    pub async fn mark_running(&self, app_id: u32) {
        self.status.lock().await.insert(app_id, SyncStatus::Running);
    }

    /// Agenda o backup de um jogo apos ele fechar, esperando o debounce
    /// configurado (evita disparar em falsos positivos, ex: crash + relaunch rapido).
    ///
    /// Recebe `Arc<Self>` explicitamente (em vez de `self: Arc<Self>`) para deixar
    /// claro no call-site que estamos clonando o Arc do engine, nao uma referencia.
    pub async fn schedule_sync(engine: Arc<Self>, app_id: u32) {
        let debounce = engine.config.lock().await.sync_debounce_secs;
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(debounce)).await;
            engine.sync_now(Some(app_id)).await;
        });
    }

    /// Sincroniza um jogo especifico (`Some(app_id)`) ou todos os elegiveis (`None`).
    pub async fn sync_now(&self, app_id: Option<u32>) {
        let games = match steam::discover_games() {
            Ok(games) => games,
            Err(err) => {
                tracing::error!(%err, "falha ao listar jogos da Steam");
                return;
            }
        };

        let ignored = self.config.lock().await.ignored_app_ids.clone();
        let targets: Vec<GameSave> = games
            .into_iter()
            .filter(|g| app_id.is_none_or(|id| id == g.app_id))
            .filter(|g| !ignored.contains(&g.app_id))
            .collect();

        let provider = self.config.lock().await.cloud_provider.clone();
        let backend = provider.as_deref().and_then(parse_provider).map(cloud::backend_for);

        for game in targets {
            self.sync_one(&game, backend.as_deref()).await;
        }
    }

    async fn sync_one(&self, game: &GameSave, backend: Option<&dyn CloudBackend>) {
        if game.save_paths.is_empty() {
            tracing::debug!(app_id = game.app_id, "nenhuma pasta de save conhecida, ignorando");
            return;
        }

        let (destination, success, message) = match backend {
            None => (
                "nenhum".to_string(),
                false,
                "nenhum provedor de nuvem configurado (rode `playsync cloud connect`)".to_string(),
            ),
            Some(backend) => {
                let multiple = game.save_paths.len() > 1;
                let mut last_err = None;
                for (idx, path) in game.save_paths.iter().enumerate() {
                    let remote_name = if multiple {
                        format!("{} ({idx}).zip", game.name)
                    } else {
                        format!("{}.zip", game.name)
                    };

                    let zip_result = {
                        let path = path.clone();
                        tokio::task::spawn_blocking(move || archive::zip_path(&path)).await
                    };
                    let zip_path = match zip_result {
                        Ok(Ok(zip_path)) => zip_path,
                        Ok(Err(err)) => {
                            last_err = Some(format!("falha ao compactar {}: {err}", path.display()));
                            continue;
                        }
                        Err(err) => {
                            last_err = Some(format!("tarefa de compactacao falhou: {err}"));
                            continue;
                        }
                    };

                    if let Err(err) = backend.upload(&zip_path, &remote_name).await {
                        last_err = Some(err.to_string());
                    }
                    let _ = tokio::fs::remove_file(&zip_path).await;
                }
                match last_err {
                    None => (format!("{:?}", backend.provider()), true, "ok".to_string()),
                    Some(err) => (format!("{:?}", backend.provider()), false, err),
                }
            }
        };

        if !success {
            tracing::warn!(app_id = game.app_id, %message, "sincronizacao falhou");
        }

        self.status.lock().await.insert(
            game.app_id,
            if success { SyncStatus::Idle } else { SyncStatus::Error },
        );

        if let Err(err) = self.history.record(&BackupEntry {
            app_id: game.app_id,
            name: game.name.clone(),
            timestamp: Utc::now(),
            destination,
            success,
        }) {
            tracing::error!(%err, "falha ao gravar historico de backup");
        }
    }

    pub async fn status_snapshot(&self) -> Vec<GameStatus> {
        let games = steam::discover_games().unwrap_or_default();
        let status = self.status.lock().await;
        games
            .into_iter()
            .map(|g| {
                let last_backup = self
                    .history
                    .last_backup(g.app_id)
                    .ok()
                    .flatten()
                    .map(|e| e.timestamp);
                GameStatus {
                    app_id: g.app_id,
                    name: g.name,
                    last_backup,
                    sync_status: status.get(&g.app_id).cloned().unwrap_or(SyncStatus::NeverSynced),
                }
            })
            .collect()
    }

    pub fn history_entries(&self, limit: u32) -> anyhow::Result<Vec<BackupEntry>> {
        self.history.recent(limit)
    }
}

fn parse_provider(s: &str) -> Option<CloudProvider> {
    match s {
        "google-drive" | "google_drive" => Some(CloudProvider::GoogleDrive),
        "box" => Some(CloudProvider::Box),
        _ => None,
    }
}
