//! Logica de backup/restore compartilhada entre o comando `restore` (CLI,
//! com prompts no stdout/stdin) e o menu por-jogo da TUI (sem I/O de
//! terminal aqui dentro — cada chamador cuida da sua propria interacao).

use std::path::Path;

use anyhow::{bail, Context, Result};
use playsync_core::ipc::CloudProvider;
use playsync_core::steam::GameSave;

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

/// Nome sanitizado da pasta do jogo + nome do arquivo de zip pro `path_index`
/// dado — a mesma formula usada por `playsyncd::sync` pra decidir onde cada
/// save_path fica dentro de `PlaySync/<jogo>/`.
pub fn sanitized_and_file_name(game: &GameSave, path_index: usize) -> (String, String) {
    let sanitized = playsync_core::naming::sanitize(&game.name);
    let file_name = if game.save_paths.len() > 1 {
        format!("save-{path_index}.zip")
    } else {
        "save.zip".to_string()
    };
    (sanitized, file_name)
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

/// Baixa o backup de `provider` e so guarda em `local_dest` — nao mexe na
/// pasta de save do jogo. Serve pra trazer de volta pro disco local um
/// backup que so existia na nuvem (ex: apos trocar de maquina), sem
/// restaurar de imediato.
pub async fn pull_from_cloud(
    provider: CloudProvider,
    sanitized: &str,
    file_name: &str,
    local_dest: &Path,
) -> Result<()> {
    let backend = playsync_core::cloud::backend_for(provider);
    if !backend.is_connected() {
        bail!("{provider:?} nao conectado — rode `playsync cloud connect` antes");
    }
    let remote_path = format!("PlaySync/{sanitized}/{file_name}");
    let bytes = backend
        .download(&remote_path)
        .await
        .with_context(|| format!("nao consegui baixar {remote_path}"))?;

    if let Some(parent) = local_dest.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("nao consegui criar {}", parent.display()))?;
    }
    tokio::fs::write(local_dest, bytes)
        .await
        .with_context(|| format!("nao consegui escrever {}", local_dest.display()))
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
