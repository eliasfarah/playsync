//! Motor de sincronizacao: decide o que fazer quando um jogo fecha, fala com
//! o backend de nuvem configurado e registra o resultado no historico.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use playsync_core::archive;
use playsync_core::cloud::{self, CloudBackend};
use playsync_core::config::Config;
use playsync_core::db::HistoryDb;
use playsync_core::ipc::{BackupEntry, CloudProvider, GameStatus, SyncStatus};
use playsync_core::naming;
use playsync_core::steam::{self, GameSave};
use playsync_core::versions;
use tokio::sync::Mutex;

pub struct SyncEngine {
    config: Mutex<Config>,
    history: Arc<HistoryDb>,
    status: Mutex<HashMap<u32, SyncStatus>>,
    /// Horario (UTC) em que cada AppID rodando foi visto abrir, pra calcular
    /// a duracao real da sessao quando ele fechar (ver `take_session_duration_secs`).
    session_started: Mutex<HashMap<u32, DateTime<Utc>>>,
}

impl SyncEngine {
    pub fn new(config: Config, history: Arc<HistoryDb>) -> Self {
        Self {
            config: Mutex::new(config),
            history,
            status: Mutex::new(HashMap::new()),
            session_started: Mutex::new(HashMap::new()),
        }
    }

    pub async fn mark_running(&self, app_id: u32) {
        self.status.lock().await.insert(app_id, SyncStatus::Running);
    }

    /// Chamado quando o watcher detecta o jogo abrindo — guarda o horario
    /// pra calcular a duracao da sessao quando ele fechar.
    pub async fn mark_session_started(&self, app_id: u32) {
        self.session_started.lock().await.insert(app_id, Utc::now());
    }

    /// Chamado quando o watcher detecta o jogo fechando — `None` se por
    /// algum motivo nao vimos o inicio dessa sessao (ex: daemon reiniciado
    /// com o jogo ja aberto). Remove a entrada: cada sessao so conta uma vez.
    pub async fn take_session_duration_secs(&self, app_id: u32) -> Option<i64> {
        let start = self.session_started.lock().await.remove(&app_id)?;
        Some((Utc::now() - start).num_seconds())
    }

    /// Agenda o backup de um jogo apos ele fechar, esperando o debounce
    /// configurado (evita disparar em falsos positivos, ex: crash + relaunch rapido).
    /// `session_duration_secs` (calculado no momento do fechamento, nao apos
    /// o debounce) fica gravado no historico junto com o backup — ver `sync_one`.
    ///
    /// Recebe `Arc<Self>` explicitamente (em vez de `self: Arc<Self>`) para deixar
    /// claro no call-site que estamos clonando o Arc do engine, nao uma referencia.
    pub async fn schedule_sync(engine: Arc<Self>, app_id: u32, session_duration_secs: Option<i64>) {
        let debounce = engine.config.lock().await.sync_debounce_secs;
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(debounce)).await;
            engine.sync_now(Some(app_id), session_duration_secs).await;
        });
    }

    /// Sincroniza um jogo especifico (`Some(app_id)`) ou todos os elegiveis
    /// (`None`). `session_duration_secs` so faz sentido pro caso de um unico
    /// jogo, disparado pelo fechamento dele — `None` pra sync manual
    /// (`playsync sync`/TUI) ou "sincronizar tudo".
    pub async fn sync_now(&self, app_id: Option<u32>, session_duration_secs: Option<i64>) {
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

        let config = self.config.lock().await;
        let provider = config.cloud_provider.clone();
        let versions_to_keep = config.backup_versions_to_keep;
        let local_root = match config.local_backup_root() {
            Ok(root) => root,
            Err(err) => {
                tracing::error!(%err, "nao consegui resolver a raiz do backup local");
                return;
            }
        };
        drop(config);

        let backend = provider.as_deref().and_then(parse_provider).map(cloud::backend_for);

        for game in targets {
            self.sync_one(&game, backend.as_deref(), &local_root, versions_to_keep, session_duration_secs)
                .await;
        }
    }

    /// Compacta cada `save_paths` do jogo em `~/PlaySync/<jogo>/` (backup
    /// local, sempre) e, se houver um provedor conectado, sobe o mesmo zip
    /// pra `PlaySync/<jogo>/` la tambem — a mesma estrutura de pastas dos
    /// dois lados, so que uma e local e a outra e na nuvem.
    async fn sync_one(
        &self,
        game: &GameSave,
        backend: Option<&dyn CloudBackend>,
        local_root: &Path,
        versions_to_keep: usize,
        session_duration_secs: Option<i64>,
    ) {
        if game.save_paths.is_empty() {
            tracing::debug!(app_id = game.app_id, "nenhuma pasta de save conhecida, ignorando");
            return;
        }

        // Marca "sincronizando..." *antes* de zipar/subir, nao so no fim — sem
        // isso, `playsync status`/a TUI nao tem como distinguir "parado" de
        // "no meio de um sync de varios jogos" enquanto ele roda.
        self.mark_running(game.app_id).await;

        let sanitized_name = naming::sanitize(&game.name);
        let total_paths = game.save_paths.len();
        let game_dir = local_root.join(&sanitized_name);
        let now = Utc::now();
        let mut last_err = None;

        for (idx, path) in game.save_paths.iter().enumerate() {
            // Timestamp em vez de sempre sobrescrever o mesmo `save.zip`: um
            // sync automatico ruim (ex: jogo aberto sem save, cria um save
            // novo/vazio, o fechamento sincroniza isso) nao destroi a unica
            // copia boa que existia — `versions::names_to_prune` limpa as
            // mais antigas logo abaixo, mantendo so as `versions_to_keep` mais
            // recentes.
            let file_name = versions::version_file_name(idx, total_paths, now);
            let local_dest = game_dir.join(&file_name);

            let zip_result = {
                let path = path.clone();
                let local_dest = local_dest.clone();
                tokio::task::spawn_blocking(move || archive::zip_path(&path, &local_dest)).await
            };
            match zip_result {
                Ok(Ok(())) => {}
                Ok(Err(err)) => {
                    last_err = Some(format!("falha ao compactar {}: {err}", path.display()));
                    continue;
                }
                Err(err) => {
                    last_err = Some(format!("tarefa de compactacao falhou: {err}"));
                    continue;
                }
            }

            if let Some(backend) = backend {
                let remote_path = format!("PlaySync/{sanitized_name}/{file_name}");
                if let Err(err) = backend.upload(&local_dest, &remote_path).await {
                    last_err = Some(err.to_string());
                }
            }

            let prefix = versions::file_prefix(idx, total_paths);
            prune_local_versions(&game_dir, &prefix, versions_to_keep);
            if let Some(backend) = backend {
                prune_cloud_versions(backend, &sanitized_name, &prefix, versions_to_keep).await;
            }
        }

        let destination = match backend {
            Some(backend) => format!("Local + {:?}", backend.provider()),
            None => "Local".to_string(),
        };
        let success = last_err.is_none();
        let message = last_err.unwrap_or_else(|| "ok".to_string());

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
            // O MESMO `now` usado pra nomear os arquivos de versao (nao um
            // `Utc::now()` novo): o zip+upload pode levar alguns segundos,
            // entao um timestamp separado aqui derivaria do nome do arquivo
            // o suficiente pra quebrar a correlacao em
            // `actions::list_versions_with_info` (tolerancia de 5s).
            timestamp: now,
            destination,
            success,
            // Guarda onde o save estava agora, pro `restore` conseguir achar
            // o alvo mesmo se ele sumir de verdade do disco depois (a
            // deteccao ao vivo nao acha mais nada nesse caso).
            source_paths: game.save_paths.clone(),
            // Duracao real da sessao (abrir->fechar) que disparou esse sync,
            // se foi por fechamento de jogo — ajuda a identificar depois se
            // uma versao veio de progresso de verdade ou de um teste rapido
            // (ver `Config::short_session_warning_secs`). `None` pra sync
            // manual, sem sessao associada.
            session_duration_secs,
        }) {
            tracing::error!(%err, "falha ao gravar historico de backup");
        }
    }

    /// Se `auto_restore_on_launch` estiver efetivamente ligado (ver
    /// `Config::auto_restore_on_launch_effective`) e houver um provedor de
    /// nuvem conectado, compara a versao mais recente de cada save_path do
    /// jogo (local vs nuvem) e baixa+restaura automaticamente se a da nuvem
    /// for mais nova — cobre o caso de ter jogado em outra maquina e a Steam
    /// abrir aqui antes de qualquer sync local ter rodado.
    ///
    /// Best-effort: qualquer erro (rede, io) so loga e desiste por esse
    /// save_path, nunca impede o jogo de abrir normalmente. Chamado a partir
    /// de uma task separada (ver `main.rs`), entao roda em paralelo ao resto
    /// do watcher — nao ha debounce aqui de proposito (diferente do sync no
    /// fechamento): quanto mais cedo, maior a chance de vencer o jogo lendo
    /// o save.
    pub async fn maybe_auto_restore_on_launch(&self, app_id: u32) {
        self.try_auto_restore_on_launch(app_id).await;

        // `main.rs` marca "sincronizando" (`mark_running`) ao ver o jogo
        // abrir, pra cobrir esse check. Sem voltar pra `Idle` aqui no final —
        // em TODOS os caminhos de saida do metodo acima, nao so o de sucesso
        // — o status ficava preso em "sincronizando" pelo tempo inteiro que o
        // jogo continuasse aberto, mesmo com o download/restore ja concluido
        // (bug real: usuario via o download funcionar certinho, mas o status
        // nunca saia de "sincronizando").
        self.status.lock().await.insert(app_id, SyncStatus::Idle);
    }

    async fn try_auto_restore_on_launch(&self, app_id: u32) {
        let config = self.config.lock().await.clone();
        if !config.auto_restore_on_launch_effective() {
            return;
        }
        let Some(provider) = config.cloud_provider.as_deref().and_then(parse_provider) else {
            return;
        };
        let backend = cloud::backend_for(provider);
        if !backend.is_connected() {
            return;
        }

        let games = match steam::discover_games() {
            Ok(games) => games,
            Err(err) => {
                tracing::warn!(%err, "auto-restore: falha ao listar jogos da Steam");
                return;
            }
        };
        let Some(game) = games.into_iter().find(|g| g.app_id == app_id) else {
            return;
        };
        if game.save_paths.is_empty() {
            return;
        }

        let sanitized = naming::sanitize(&game.name);
        let local_root = match config.local_backup_root() {
            Ok(root) => root,
            Err(err) => {
                tracing::warn!(%err, "auto-restore: nao consegui resolver a raiz do backup local");
                return;
            }
        };
        let total_paths = game.save_paths.len();
        let remote_dir = format!("PlaySync/{sanitized}");
        let local_dir = local_root.join(&sanitized);

        for (idx, path) in game.save_paths.iter().enumerate() {
            let prefix = versions::file_prefix(idx, total_paths);

            let local_names: Vec<String> = std::fs::read_dir(&local_dir)
                .map(|entries| entries.flatten().filter_map(|e| e.file_name().into_string().ok()).collect())
                .unwrap_or_default();
            let local_latest = versions::sort_versions(local_names, &prefix).pop();

            let cloud_names = match backend.list_files(&remote_dir).await {
                Ok(names) => names,
                Err(err) => {
                    tracing::warn!(%err, app_id, remote_dir, "auto-restore: falha ao listar versoes na nuvem");
                    continue;
                }
            };
            let Some(cloud_latest) = versions::sort_versions(cloud_names, &prefix).pop() else {
                continue; // nada na nuvem pra esse save_path ainda
            };

            // Nomes de versao ordenam lexicograficamente = cronologicamente
            // (ver `versions.rs`), entao comparar as strings direto ja diz
            // qual e mais recente, sem precisar parsear timestamp.
            let cloud_is_newer = local_latest.as_deref().is_none_or(|local| cloud_latest.as_str() > local);
            if !cloud_is_newer {
                continue;
            }

            let remote_path = format!("{remote_dir}/{cloud_latest}");
            let bytes = match backend.download(&remote_path).await {
                Ok(bytes) => bytes,
                Err(err) => {
                    tracing::warn!(%err, remote_path, "auto-restore: falha ao baixar versao mais recente da nuvem");
                    continue;
                }
            };

            let remove_result = if path.is_dir() {
                std::fs::remove_dir_all(path)
            } else if path.exists() {
                std::fs::remove_file(path)
            } else {
                Ok(())
            };
            if let Err(err) = remove_result {
                tracing::warn!(%err, path = %path.display(), "auto-restore: falha ao apagar save atual antes de restaurar");
                continue;
            }

            let anchor = path.parent().unwrap_or(path);
            if let Err(err) = archive::unzip_to(&bytes, anchor) {
                tracing::warn!(%err, "auto-restore: falha ao extrair versao da nuvem");
                continue;
            }

            tracing::info!(
                app_id,
                name = %game.name,
                version = %cloud_latest,
                "auto-restore: save da nuvem era mais recente, restaurado antes do jogo ler o save"
            );
        }
    }

    pub async fn status_snapshot(&self) -> Vec<GameStatus> {
        let games = steam::discover_games().unwrap_or_default();
        let ignored = self.config.lock().await.ignored_app_ids.clone();
        let status = self.status.lock().await;
        games
            .into_iter()
            .filter(|g| !ignored.contains(&g.app_id))
            .map(|g| {
                let last_backup = self
                    .history
                    .last_backup(g.app_id)
                    .ok()
                    .flatten()
                    .map(|e| e.timestamp);
                let save_size_bytes = if g.save_paths.is_empty() {
                    None
                } else {
                    Some(g.save_paths.iter().filter_map(|p| archive::path_size_bytes(p)).sum())
                };
                GameStatus {
                    app_id: g.app_id,
                    name: g.name,
                    last_backup,
                    sync_status: status.get(&g.app_id).cloned().unwrap_or(SyncStatus::NeverSynced),
                    has_save_paths: !g.save_paths.is_empty(),
                    save_size_bytes,
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

/// Apaga as versoes locais mais antigas de `prefix` em `game_dir`, mantendo
/// so as `keep` mais recentes (ver modulo `versions`). Falha silenciosa por
/// arquivo (loga e continua) — um erro ao apagar uma versao antiga nao deve
/// derrubar o sync que acabou de dar certo.
fn prune_local_versions(game_dir: &Path, prefix: &str, keep: usize) {
    let Ok(entries) = std::fs::read_dir(game_dir) else {
        return;
    };
    let names: Vec<String> = entries
        .flatten()
        .filter_map(|entry| entry.file_name().into_string().ok())
        .collect();

    let sorted = versions::sort_versions(names, prefix);
    for old_name in versions::names_to_prune(&sorted, keep) {
        if let Err(err) = std::fs::remove_file(game_dir.join(old_name)) {
            tracing::warn!(%err, old_name, "falha ao podar versao local antiga");
        }
    }
}

/// Mesma poda, na nuvem — lista o que ja esta em `PlaySync/<jogo>/` e apaga
/// as versoes mais antigas alem de `keep`. So roda apos um upload bem
/// sucedido; se listar ou apagar falhar (ex: rede), so loga — o backup novo
/// ja subiu, nao vale a pena marcar a sincronizacao inteira como erro por
/// causa da limpeza.
async fn prune_cloud_versions(backend: &dyn CloudBackend, sanitized_name: &str, prefix: &str, keep: usize) {
    let remote_dir = format!("PlaySync/{sanitized_name}");
    let names = match backend.list_files(&remote_dir).await {
        Ok(names) => names,
        Err(err) => {
            tracing::warn!(%err, remote_dir, "falha ao listar versoes na nuvem pra podar");
            return;
        }
    };

    let sorted = versions::sort_versions(names, prefix);
    for old_name in versions::names_to_prune(&sorted, keep) {
        let remote_path = format!("{remote_dir}/{old_name}");
        if let Err(err) = backend.delete(&remote_path).await {
            tracing::warn!(%err, remote_path, "falha ao podar versao antiga na nuvem");
        }
    }
}
