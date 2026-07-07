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
    /// So preenchido quando `save_paths` vem vazio E o manifest da Ludusavi
    /// documenta esse AppID: onde o save DEVERIA estar segundo o manifest,
    /// mesmo que a pasta ainda nao exista no disco (jogo instalado mas nunca
    /// aberto, ou maquina nova que nunca sincronizou esse jogo). Ultimo
    /// fallback do restore (`actions::restore_candidate_paths`) pra alem do
    /// historico local — ver `manifest::resolve_save_paths_for_restore`.
    #[serde(default)]
    pub restore_fallback_paths: Vec<PathBuf>,
}

/// Varre todas as bibliotecas Steam da maquina e retorna os jogos instalados
/// junto com os caminhos de save que conseguimos inferir. Tambem inclui
/// qualquer `extra_save_paths` configurado pelo usuario (`config.toml`) pra
/// jogos que a deteccao automatica nao acha sozinha.
pub fn discover_games() -> Result<Vec<GameSave>> {
    let steam_dir = SteamDir::locate().context("Steam nao encontrada nesta maquina")?;
    let mut games = Vec::new();

    // Falha em carregar a config nao deve impedir a deteccao normal — so
    // significa que nenhum caminho extra e aplicado.
    let extra_save_paths = crate::config::Config::load_or_default()
        .map(|c| c.extra_save_paths)
        .unwrap_or_default();

    // Indice do manifest da Ludusavi (locais de save conhecidos por AppID) —
    // so le o cache local em disco, sem rede (ver `manifest::refresh_cache`).
    // Vazio se o cache ainda nao foi baixado nenhuma vez.
    let manifest_index = crate::manifest::appid_index();

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

            let name = app.name.clone().unwrap_or_else(|| app.install_dir.clone());
            if is_steam_tool(&name) {
                continue;
            }

            let install_dir = library.resolve_app_dir(&app);

            // O manifest manda quando o jogo tem `files` documentado (mais
            // preciso, e as vezes corrige a heuristica — ver doc do modulo
            // `manifest`), MESMO que resolva pra zero caminhos: um jogo
            // documentado so com `tags: [config]` (ex: "The Division", cujo
            // progresso e todo em servidor) informa de verdade que nao ha
            // save local, e nao deveria cair pra heuristica so por isso —
            // senao voltamos a fazer backup de config achando que e save.
            // So cai pra heuristica quando o manifest simplesmente nao tem
            // nenhuma entrada de `files` pro jogo (nada documentado ainda).
            let manifest_entry = manifest_index.get(&app.app_id).filter(|e| !e.files.is_empty());
            let mut save_paths = match manifest_entry {
                Some(entry) => {
                    crate::manifest::resolve_save_paths(entry, app.app_id, library.path(), steam_dir.path(), &install_dir)
                }
                None => find_save_candidates(&steam_dir, &library, &app),
            };

            // So tenta o fallback "tolerante" (pasta ainda nao existe) quando
            // a deteccao normal nao achou nada — ele faz globs extras, entao
            // nao vale pagar esse custo no caso comum (jogo ja rodou e tem
            // save de verdade). So funciona com manifest, nunca com a
            // heuristica (`find_save_candidates`): sem `files` documentado
            // nao ha template pra resolver, so tentativa e erro.
            let restore_fallback_paths = if save_paths.is_empty() {
                manifest_entry
                    .map(|entry| {
                        crate::manifest::resolve_save_paths_for_restore(
                            entry,
                            app.app_id,
                            library.path(),
                            steam_dir.path(),
                            &install_dir,
                        )
                    })
                    .unwrap_or_default()
            } else {
                Vec::new()
            };

            if let Some(extra) = extra_save_paths.get(&app.app_id.to_string()) {
                for path in extra {
                    if path.is_dir() && !save_paths.contains(path) {
                        save_paths.push(path.clone());
                    }
                }
            }

            games.push(GameSave {
                app_id: app.app_id,
                name,
                install_dir,
                save_paths,
                restore_fallback_paths,
            });
        }
    }

    Ok(games)
}

/// Reconhece "apps" da Steam que na verdade sao ferramentas/runtimes
/// instalados como dependencia de outros jogos (Proton, Steam Linux Runtime,
/// redistributables do Steamworks) — nunca tem save de jogador, so poluem a
/// lista. Nao ha campo local confiavel pra "tipo" do app (a distincao
/// game/tool vem do catalogo remoto da Valve, nao do appmanifest local);
/// esses tres seguem convencao de nome estavel o suficiente pra detectar por
/// prefixo, inclusive versoes futuras (ex: "Proton 11.0", "Steam Linux
/// Runtime 5.0").
fn is_steam_tool(name: &str) -> bool {
    name == "Steamworks Common Redistributables"
        || name.starts_with("Proton")
        || name.starts_with("Steam Linux Runtime")
}

/// Subpastas que o Wine/Proton cria por padrao em *todo* prefixo novo (vazias,
/// nada a ver com o jogo) — confirmado comparando o `Documents/` de varios
/// AppIDs diferentes nesta maquina. Filtradas pra nao tratar
/// `Documents/Pictures` etc. como se fosse save de jogo. `"My Games"` tambem
/// entra aqui: e tratada a parte, descendo mais um nivel (ver abaixo), entao
/// nao pode ser candidata ela mesma — senao o mesmo save seria zipado/subido
/// duas vezes (a pasta toda e de novo a subpasta do jogo dentro dela).
const DEFAULT_DOCUMENTS_SUBFOLDERS: &[&str] =
    &["Pictures", "Music", "Videos", "Downloads", "Templates", "My Games"];

/// Caminhos onde um jogo *costuma* guardar saves no Linux:
/// 1. Prefixo Proton do AppID (`compatdata/<id>/pfx/.../AppData/{Roaming,Local,LocalLow}`);
/// 2. `Documents/<algo>` e `Documents/My Games/<algo>` — convencao comum de
///    jogos (sobretudo Unity/Unreal) que nao usam AppData. Como o Wine/Proton
///    cria as mesmas pastas padrao (vazias) em todo prefixo novo, ignoramos
///    os nomes conhecidos de pasta padrao (`DEFAULT_DOCUMENTS_SUBFOLDERS`).
/// 3. `Saved Games/<algo>` — pasta especial do Windows (desde Vista) dedicada
///    a saves; o Wine cria ela vazia, entao qualquer subpasta ali e do jogo.
/// 4. Espelho da Steam Cloud (`userdata/<steamid3>/<id>/remote`), que existe mesmo para
///    jogos nativos de Linux que usam a Steam Cloud API.
///
/// Jogos que gravam save em outros lugares (ex: `~/.config/<jogo>`) ficam de fora por ora;
/// a lista de excecoes conhecidas deve virar um mapa configuravel mais adiante.
fn find_save_candidates(steam_dir: &SteamDir, library: &Library, app: &App) -> Vec<PathBuf> {
    let mut candidates = Vec::new();

    let user_dir = library
        .path()
        .join("steamapps/compatdata")
        .join(app.app_id.to_string())
        .join("pfx/drive_c/users/steamuser");

    let appdata = user_dir.join("AppData");
    for sub in ["Roaming", "Local", "LocalLow"] {
        let path = appdata.join(sub);
        if path.is_dir() {
            candidates.push(path);
        }
    }

    let documents = user_dir.join("Documents");
    candidates.extend(subdirs_excluding(&documents, DEFAULT_DOCUMENTS_SUBFOLDERS));
    candidates.extend(subdirs_excluding(&documents.join("My Games"), &[]));
    candidates.extend(subdirs_excluding(&user_dir.join("Saved Games"), &[]));

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

/// Subdiretorios diretos de `dir` (arquivos soltos ignorados), pulando os
/// nomes em `exclude`. Silenciosamente vazio se `dir` nao existir.
fn subdirs_excluding(dir: &std::path::Path, exclude: &[&str]) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if !path.is_dir() {
                return None;
            }
            let name = path.file_name()?.to_str()?;
            if exclude.contains(&name) {
                return None;
            }
            Some(path)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_known_steam_tools() {
        assert!(is_steam_tool("Steamworks Common Redistributables"));
        assert!(is_steam_tool("Proton Experimental"));
        assert!(is_steam_tool("Proton 10.0"));
        assert!(is_steam_tool("Steam Linux Runtime 3.0 (sniper)"));
        assert!(is_steam_tool("Steam Linux Runtime 4.0"));
    }

    #[test]
    fn recognizes_future_versions_by_prefix() {
        assert!(is_steam_tool("Proton 11.0"));
        assert!(is_steam_tool("Steam Linux Runtime 5.0"));
    }

    #[test]
    fn does_not_flag_real_games() {
        assert!(!is_steam_tool("DARK SOULS™ II: Scholar of the First Sin"));
        assert!(!is_steam_tool("ELDEN RING"));
        assert!(!is_steam_tool("Marvel's Spider-Man 2"));
    }

    #[test]
    fn subdirs_excluding_skips_known_defaults_and_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("Pictures")).unwrap();
        std::fs::create_dir(dir.path().join("My Game Save")).unwrap();
        std::fs::write(dir.path().join("loose_file.txt"), b"x").unwrap();

        let found = subdirs_excluding(dir.path(), DEFAULT_DOCUMENTS_SUBFOLDERS);
        assert_eq!(found, vec![dir.path().join("My Game Save")]);
    }

    #[test]
    fn subdirs_excluding_empty_for_missing_dir() {
        assert!(subdirs_excluding(std::path::Path::new("/nonexistent/playsync-test"), &[]).is_empty());
    }
}
