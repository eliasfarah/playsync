//! Logica de backup/restore compartilhada entre o comando `restore` (CLI,
//! com prompts no stdout/stdin) e o menu por-jogo da TUI (sem I/O de
//! terminal aqui dentro — cada chamador cuida da sua propria interacao).

use std::path::Path;

use anyhow::{bail, Context, Result};
use playsync_core::ipc::CloudProvider;

pub enum RestoreSource {
    Local,
    Cloud(CloudProvider),
}

pub fn parse_provider(provider: &str) -> Result<CloudProvider> {
    match provider {
        "google-drive" | "gdrive" => Ok(CloudProvider::GoogleDrive),
        "box" => Ok(CloudProvider::Box),
        other => bail!("provedor desconhecido: {other} (use `google-drive` ou `box`)"),
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
                bail!("{provider:?} nao conectado — rode `playsync cloud connect` antes");
            }
            backend.list_files(&format!("PlaySync/{sanitized}")).await?
        }
    };
    Ok(playsync_core::versions::sort_versions(names, &prefix))
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
            .with_context(|| format!("versao \"{name}\" nao encontrada (use --list-versions pra ver as disponiveis)")),
        None => versions
            .last()
            .map(String::as_str)
            .context("nenhum backup encontrado pra esse jogo/pasta/origem"),
    }
}

/// Atalho pra "quero a versao mais recente, sem oferecer escolha" (usado
/// pelo menu da TUI, que nao tem UI pra listar/escolher versao ainda —
/// `playsync restore --list-versions`/`--version` na CLI cobre esse caso).
/// Devolve o rotulo da origem, o nome do arquivo escolhido (pra quem precisa
/// exibir/persistir) e os bytes baixados.
pub async fn fetch_latest_backup_bytes(
    source: &RestoreSource,
    sanitized: &str,
    path_index: usize,
    paths_len: usize,
) -> Result<(String, String, Vec<u8>)> {
    let versions = list_versions(source, sanitized, path_index, paths_len).await?;
    let file_name = pick_version(&versions, None)?.to_string();
    let (label, bytes) = fetch_backup_bytes(source, sanitized, &file_name).await?;
    Ok((label, file_name, bytes))
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
                .with_context(|| format!("nao encontrei backup local em {}", path.display()))?;
            Ok(("local".to_string(), bytes))
        }
        RestoreSource::Cloud(provider) => {
            let backend = playsync_core::cloud::backend_for(*provider);
            if !backend.is_connected() {
                bail!("{provider:?} nao conectado — rode `playsync cloud connect` antes");
            }
            let remote_path = format!("PlaySync/{sanitized}/{file_name}");
            let bytes = backend
                .download(&remote_path)
                .await
                .with_context(|| format!("nao consegui baixar {remote_path}"))?;
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
            .with_context(|| format!("nao consegui apagar {}", target.display()))?;
    } else if target.exists() {
        std::fs::remove_file(target)
            .with_context(|| format!("nao consegui apagar {}", target.display()))?;
    }

    let anchor = target.parent().unwrap_or(target);
    playsync_core::archive::unzip_to(bytes, anchor).context("falha ao extrair o backup")
}
