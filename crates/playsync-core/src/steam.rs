//! Deteccao da instalacao Steam: bibliotecas, jogos instalados e pastas de save.

use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use steamlocate::{App, Library, SteamDir};

/// Um jogo instalado, com os provaveis diretorios de save ja resolvidos.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GameSave {
    pub app_id: u32,
    pub name: String,
    pub install_dir: PathBuf,
    /// Diretorios candidatos a conter saves (prefixo Proton e/ou espelho da Steam Cloud).
    /// Pode ficar vazio para jogos que guardam save em outro lugar (ainda nao suportado).
    pub save_paths: Vec<PathBuf>,
}

/// Varre todas as bibliotecas Steam da maquina e retorna os jogos instalados
/// junto com os caminhos de save que conseguimos inferir.
pub fn discover_games() -> Result<Vec<GameSave>> {
    let steam_dir = SteamDir::locate().context("Steam nao encontrada nesta maquina")?;
    let mut games = Vec::new();

    for library in steam_dir
        .libraries()
        .context("falha ao ler as bibliotecas Steam")?
    {
        let library = library.context("biblioteca Steam invalida")?;

        for app in library.apps() {
            let app = match app {
                Ok(app) => app,
                Err(err) => {
                    tracing::warn!(%err, "ignorando appmanifest invalido");
                    continue;
                }
            };

            let install_dir = library.resolve_app_dir(&app);
            let save_paths = find_save_candidates(&steam_dir, &library, &app);
            let name = app.name.clone().unwrap_or_else(|| app.install_dir.clone());

            games.push(GameSave {
                app_id: app.app_id,
                name,
                install_dir,
                save_paths,
            });
        }
    }

    Ok(games)
}

/// Caminhos onde um jogo *costuma* guardar saves no Linux:
/// 1. Prefixo Proton do AppID (`compatdata/<id>/pfx/.../AppData/{Roaming,Local,LocalLow}`);
/// 2. Espelho da Steam Cloud (`userdata/<steamid3>/<id>/remote`), que existe mesmo para
///    jogos nativos de Linux que usam a Steam Cloud API.
///
/// Jogos que gravam save em outros lugares (ex: `~/.config/<jogo>`) ficam de fora por ora;
/// a lista de excecoes conhecidas deve virar um mapa configuravel mais adiante.
fn find_save_candidates(steam_dir: &SteamDir, library: &Library, app: &App) -> Vec<PathBuf> {
    let mut candidates = Vec::new();

    let appdata = library
        .path()
        .join("steamapps/compatdata")
        .join(app.app_id.to_string())
        .join("pfx/drive_c/users/steamuser/AppData");
    for sub in ["Roaming", "Local", "LocalLow"] {
        let path = appdata.join(sub);
        if path.is_dir() {
            candidates.push(path);
        }
    }

    let userdata_root = steam_dir.path().join("userdata");
    if let Ok(entries) = fs::read_dir(&userdata_root) {
        for entry in entries.flatten() {
            let remote = entry.path().join(app.app_id.to_string()).join("remote");
            if remote.is_dir() {
                candidates.push(remote);
            }
        }
    }

    candidates
}
