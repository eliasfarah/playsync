//! Logica de backup/restore compartilhada entre o comando `restore` (CLI,
//! com prompts no stdout/stdin) e o menu por-jogo da TUI (sem I/O de
//! terminal aqui dentro — cada chamador cuida da sua propria interacao).

use std::path::Path;

use anyhow::{bail, Context, Result};
use playsync_core::ipc::CloudProvider;
use rust_i18n::t;

pub enum RestoreSource {
    Local,
    Cloud(CloudProvider),
}

pub fn parse_provider(provider: &str) -> Result<CloudProvider> {
    match provider {
        "google-drive" | "gdrive" => Ok(CloudProvider::GoogleDrive),
        "box" => Ok(CloudProvider::Box),
        other => bail!(t!("cli.cloud.unknown_provider", other = other)),
    }
}

pub fn parse_source(source: &str) -> Result<RestoreSource> {
    match source {
        "local" => Ok(RestoreSource::Local),
        other => Ok(RestoreSource::Cloud(parse_provider(other)?)),
    }
}

/// Caminhos candidatos pra restaurar: os detectados ao vivo
/// (`GameSave::save_paths`), ou — se vazio, o cenario mais importante pra
/// `restore` existir, quando o save sumiu de verdade do disco e a deteccao
/// ao vivo nao acha mais nada — o ultimo caminho conhecido, gravado no
/// historico na hora do ultimo backup bem sucedido. O segundo valor indica
/// se veio do historico (pra avisar o usuario, ja que o caminho pode nao
/// existir mais no disco de verdade).
pub fn restore_candidate_paths(app_id: u32, live_paths: &[std::path::PathBuf]) -> (Vec<std::path::PathBuf>, bool) {
    if !live_paths.is_empty() {
        return (live_paths.to_vec(), false);
    }
    let Ok(history) = playsync_core::db::HistoryDb::open_default() else {
        return (Vec::new(), false);
    };
    let fallback = history
        .last_backup(app_id)
        .ok()
        .flatten()
        .map(|entry| entry.source_paths)
        .unwrap_or_default();
    let used_history = !fallback.is_empty();
    (fallback, used_history)
}

/// Versoes existentes (nomes de arquivo, ordenadas da mais antiga pra mais
/// nova — ver modulo `versions`) do save_path `path_index`, local ou num
/// provedor de nuvem. `paths_len` e a quantidade de save_paths *usada no
/// backup que estamos restaurando*, que pode vir do historico (ver
/// `restore_candidate_paths`) e por isso nao e so `game.save_paths.len()`.
/// Vazio (nao erro) se esse jogo/path_index nunca foi sincronizado por esse
/// `source`.
pub async fn list_versions(
    source: &RestoreSource,
    sanitized: &str,
    path_index: usize,
    paths_len: usize,
) -> Result<Vec<String>> {
    let prefix = playsync_core::versions::file_prefix(path_index, paths_len);
    let names = match source {
        RestoreSource::Local => {
            let config = playsync_core::config::Config::load_or_default()?;
            let dir = config.local_backup_root()?.join(sanitized);
            match std::fs::read_dir(&dir) {
                Ok(entries) => entries.flatten().filter_map(|e| e.file_name().into_string().ok()).collect(),
                Err(_) => Vec::new(),
            }
        }
        RestoreSource::Cloud(provider) => {
            let backend = playsync_core::cloud::backend_for(*provider);
            if !backend.is_connected() {
                bail!(t!("cli.restore.provider_not_connected", provider = format!("{provider:?}")));
            }
            backend.list_files(&format!("PlaySync/{sanitized}")).await?
        }
    };
    Ok(playsync_core::versions::sort_versions(names, &prefix))
}

/// De onde veio uma versao de backup, pra ajudar a escolher qual restaurar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionInfo {
    /// Sem entrada de historico correlacionada (arquivo de antes dessa
    /// feature, ou historico limpo/rotacionado).
    Unknown,
    /// Sync manual (`playsync sync` ou "sincronizar tudo" na TUI) — sem
    /// sessao de jogo associada.
    Manual,
    /// Sync automatico, disparado pelo fechamento do jogo, com a duracao
    /// real da sessao (abrir ate fechar).
    Session { duration_secs: i64 },
}

#[derive(Debug, Clone)]
pub struct VersionInfo {
    pub file_name: String,
    pub session: SessionInfo,
}

/// Mesma lista de `list_versions`, mas com a sessao que gerou cada versao
/// correlacionada (duracao real, manual, ou desconhecida) — usado por
/// `restore --list-versions` e pelo seletor de versao da TUI, pra ajudar a
/// identificar qual e progresso de verdade em vez de um teste rapido.
pub async fn list_versions_with_info(
    app_id: u32,
    source: &RestoreSource,
    sanitized: &str,
    path_index: usize,
    paths_len: usize,
) -> Result<Vec<VersionInfo>> {
    let names = list_versions(source, sanitized, path_index, paths_len).await?;
    let prefix = playsync_core::versions::file_prefix(path_index, paths_len);

    // So pra exibicao — se o historico nao abrir por algum motivo, mostra as
    // versoes sem a info de sessao em vez de falhar o comando inteiro.
    let history_entries = playsync_core::db::HistoryDb::open_default()
        .ok()
        .and_then(|h| h.entries_for_app(app_id, 500).ok())
        .unwrap_or_default();

    Ok(names
        .into_iter()
        .map(|file_name| {
            // Os dois `Utc::now()` (nome do arquivo, `BackupEntry.timestamp`)
            // vem do mesmo `sync_one`, a poucos milissegundos um do outro —
            // 5s de tolerancia cobre isso com folga sem risco de casar com o
            // sync errado de um jogo que sincroniza com frequencia.
            let session = playsync_core::versions::parse_version_timestamp(&file_name, &prefix)
                .and_then(|ts| {
                    history_entries
                        .iter()
                        .min_by_key(|e| (e.timestamp - ts).num_seconds().abs())
                        .filter(|e| (e.timestamp - ts).num_seconds().abs() <= 5)
                })
                .map(|entry| match entry.session_duration_secs {
                    Some(duration_secs) => SessionInfo::Session { duration_secs },
                    None => SessionInfo::Manual,
                })
                .unwrap_or(SessionInfo::Unknown);
            VersionInfo { file_name, session }
        })
        .collect())
}

/// Rotulo pronto pra exibir ao lado do nome do arquivo, com aviso se a
/// sessao foi mais curta que `short_session_warning_secs` (config) — sinal
/// tipico de "abri o jogo sem save, so testei, fechei", nao progresso real.
pub fn format_version_label(info: &VersionInfo, short_session_warning_secs: i64) -> String {
    match info.session {
        SessionInfo::Unknown => info.file_name.clone(),
        SessionInfo::Manual => format!("{}{}", info.file_name, t!("cli.restore.label_manual")),
        SessionInfo::Session { duration_secs } => {
            let duration_secs = duration_secs.max(0);
            let label = if duration_secs >= 60 {
                t!("cli.restore.unit_min", n = duration_secs / 60).to_string()
            } else {
                t!("cli.restore.unit_sec", n = duration_secs).to_string()
            };
            if duration_secs < short_session_warning_secs {
                format!("{}{}", info.file_name, t!("cli.restore.label_short_session", label = label))
            } else {
                format!("{}{}", info.file_name, t!("cli.restore.label_session", label = label))
            }
        }
    }
}

/// Escolhe qual versao usar: `explicit` (nome exato, de `--version` ou do
/// menu da TUI) se informado, senao a mais recente. `versions` deve vir
/// ordenada da mais antiga pra mais nova (`list_versions`).
pub fn pick_version<'a>(versions: &'a [String], explicit: Option<&str>) -> Result<&'a str> {
    match explicit {
        Some(name) => versions
            .iter()
            .find(|v| v.as_str() == name)
            .map(String::as_str)
            .with_context(|| t!("cli.restore.version_not_found", name = name).to_string()),
        None => versions
            .last()
            .map(String::as_str)
            .with_context(|| t!("cli.restore.no_backup_for_slot").to_string()),
    }
}

/// Le os bytes do backup de `PlaySync/<sanitized>/<file_name>`, local ou de
/// um provedor de nuvem. Devolve tambem um rotulo pra exibir (`"local"` ou
/// `"GoogleDrive"`/`"Box"`).
pub async fn fetch_backup_bytes(
    source: &RestoreSource,
    sanitized: &str,
    file_name: &str,
) -> Result<(String, Vec<u8>)> {
    match source {
        RestoreSource::Local => {
            let config = playsync_core::config::Config::load_or_default()?;
            let path = config.local_backup_root()?.join(sanitized).join(file_name);
            let bytes = tokio::fs::read(&path)
                .await
                .with_context(|| t!("cli.restore.local_backup_not_found", path = path.display()).to_string())?;
            Ok(("local".to_string(), bytes))
        }
        RestoreSource::Cloud(provider) => {
            let backend = playsync_core::cloud::backend_for(*provider);
            if !backend.is_connected() {
                bail!(t!("cli.restore.provider_not_connected", provider = format!("{provider:?}")));
            }
            let remote_path = format!("PlaySync/{sanitized}/{file_name}");
            let bytes = backend
                .download(&remote_path)
                .await
                .with_context(|| t!("cli.restore.download_failed", remote_path = remote_path).to_string())?;
            Ok((format!("{provider:?}"), bytes))
        }
    }
}

/// Apaga `target` (arquivo ou diretorio, se existir) e extrai `bytes` no
/// lugar — o `unzip_to` fica ancorado no pai de `target`, espelhando como
/// `zip_path` compacta.
pub fn extract_over(bytes: &[u8], target: &Path) -> Result<()> {
    if target.is_dir() {
        std::fs::remove_dir_all(target)
            .with_context(|| t!("cli.restore.delete_failed", path = target.display()).to_string())?;
    } else if target.exists() {
        std::fs::remove_file(target)
            .with_context(|| t!("cli.restore.delete_failed", path = target.display()).to_string())?;
    }

    let anchor = target.parent().unwrap_or(target);
    playsync_core::archive::unzip_to(bytes, anchor).with_context(|| t!("cli.restore.extract_failed").to_string())
}
