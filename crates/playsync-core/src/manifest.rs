//! Le o "Ludusavi Manifest" (github.com/mtkennerly/ludusavi-manifest, MIT):
//! um banco de +19 mil jogos com os locais de save conhecidos, curado a
//! partir do PCGamingWiki mas ja estruturado pra consumo automatico (e a
//! mesma fonte que o Ludusavi, ferramenta de backup de saves, usa). Serve
//! como fonte primaria de deteccao — mais precisa que a heuristica de
//! `steam.rs` (que so entra em cena pra jogos ausentes do manifest), e as
//! vezes corrige a heuristica: por exemplo, jogos com progresso so no
//! servidor (ex: "The Division") tem a pasta local marcada so como
//! `config` no manifest, nunca `save`, evitando back up de lixo achando
//! que e save de verdade.
//!
//! `refresh_cache` (rede, so chamado explicitamente no startup do daemon/CLI)
//! e `appid_index`/`resolve_save_paths` (so leem o cache local em disco, sem
//! rede) sao deliberadamente separados: `discover_games()` roda com
//! frequencia (a cada poll de status da TUI) e nao pode depender de rede.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};
use serde::Deserialize;

const MANIFEST_URL: &str =
    "https://raw.githubusercontent.com/mtkennerly/ludusavi-manifest/master/data/manifest.yaml";

#[derive(Debug, Clone, Deserialize)]
pub struct ManifestEntry {
    #[serde(default)]
    pub files: HashMap<String, FileEntry>,
    pub steam: Option<SteamRef>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct FileEntry {
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub when: Vec<WhenClause>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct WhenClause {
    pub os: Option<String>,
    pub store: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SteamRef {
    pub id: Option<u32>,
}

type RawManifest = HashMap<String, ManifestEntry>;
type Index = HashMap<u32, ManifestEntry>;

fn cache_path() -> Result<PathBuf> {
    Ok(crate::config::Config::state_dir()?.join("ludusavi_manifest.yaml"))
}

fn etag_path() -> Result<PathBuf> {
    Ok(crate::config::Config::state_dir()?.join("ludusavi_manifest.etag"))
}

/// Baixa o manifest se o cache local nao existe, ou revalida por ETag
/// (`If-None-Match`) se o cache tem mais de `max_age`. Cache fresco: nao faz
/// nenhuma chamada de rede (nem revalidacao). So deve ser chamado de fora do
/// caminho quente de `discover_games` — startup do daemon/CLI, nao a cada poll.
pub async fn refresh_cache(client: &reqwest::Client, max_age: Duration) -> Result<()> {
    let cache = cache_path()?;
    let etag_file = etag_path()?;

    if let Ok(meta) = fs::metadata(&cache) {
        if let Ok(modified) = meta.modified() {
            if modified.elapsed().unwrap_or(Duration::MAX) < max_age {
                return Ok(());
            }
        }
    }

    let mut req = client.get(MANIFEST_URL);
    if let Ok(etag) = fs::read_to_string(&etag_file) {
        req = req.header(reqwest::header::IF_NONE_MATCH, etag.trim().to_string());
    }

    let resp = req
        .send()
        .await
        .context("falha ao contatar o manifest da Ludusavi")?;

    if resp.status() == reqwest::StatusCode::NOT_MODIFIED {
        // Nao muda o conteudo, so a data de "ultima checagem" (via mtime do
        // proprio arquivo de etag), pra nao tentar de novo antes do prazo.
        if let Ok(etag) = fs::read_to_string(&etag_file) {
            fs::write(&etag_file, etag)?;
        }
        return Ok(());
    }

    let etag = resp
        .headers()
        .get(reqwest::header::ETAG)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let bytes = resp
        .bytes()
        .await
        .context("falha ao baixar o manifest da Ludusavi")?;

    if let Some(parent) = cache.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&cache, &bytes)
        .with_context(|| format!("nao consegui escrever {}", cache.display()))?;
    if let Some(etag) = etag {
        fs::write(&etag_file, etag)?;
    }
    Ok(())
}

static PARSED: Mutex<Option<(SystemTime, Arc<Index>)>> = Mutex::new(None);

/// Indice AppID -> entrada do manifest, lido do cache local em disco (sem
/// rede). Reparseia so quando o mtime do arquivo muda (ex: apos um
/// `refresh_cache` bem sucedido) — do contrario reusa o parse anterior, ja
/// que o YAML tem ~17MB e `discover_games` roda com frequencia. Vazio (nao
/// erro) se o cache ainda nao existe.
pub fn appid_index() -> Arc<Index> {
    let path = match cache_path() {
        Ok(p) => p,
        Err(_) => return Arc::new(HashMap::new()),
    };
    let mtime = match fs::metadata(&path).and_then(|m| m.modified()) {
        Ok(m) => m,
        Err(_) => return Arc::new(HashMap::new()),
    };

    let mut guard = PARSED.lock().expect("PARSED mutex nao deveria ficar poisoned");
    if let Some((cached_mtime, index)) = guard.as_ref() {
        if *cached_mtime == mtime {
            return index.clone();
        }
    }

    let index = Arc::new(parse_index(&path).unwrap_or_else(|err| {
        tracing::warn!(%err, "nao consegui ler o manifest da Ludusavi, seguindo so com heuristica");
        HashMap::new()
    }));
    *guard = Some((mtime, index.clone()));
    index
}

fn parse_index(path: &Path) -> Result<Index> {
    let text = fs::read_to_string(path)
        .with_context(|| format!("nao consegui ler {}", path.display()))?;
    let raw: RawManifest =
        serde_yaml::from_str(&text).context("manifest da Ludusavi com YAML invalido")?;
    Ok(raw
        .into_values()
        .filter_map(|entry| entry.steam.as_ref().and_then(|s| s.id).map(|id| (id, entry)))
        .collect())
}

fn path_str(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// Substitui os placeholders de um template de caminho do manifest,
/// assumindo que o jogo roda via Proton (o jogo ve um "Windows" dentro do
/// prefixo). Placeholders sem tradução conhecida (`<winPublic>`,
/// `<winProgramData>`, `<winDir>`, `<root>`, `<game>` — raros em entradas de
/// save) fazem a funcao retornar `None`: melhor nao adivinhar do que gerar
/// um caminho errado.
fn substitute_proton(
    template: &str,
    prefix_user_dir: &Path,
    steam_root: &Path,
    install_dir: &Path,
    app_id: u32,
) -> Option<String> {
    let mut s = template.to_string();
    s = s.replace("<home>", &path_str(prefix_user_dir));
    s = s.replace("<winAppData>", &path_str(&prefix_user_dir.join("AppData/Roaming")));
    s = s.replace("<winLocalAppData>", &path_str(&prefix_user_dir.join("AppData/Local")));
    s = s.replace(
        "<winLocalAppDataLow>",
        &path_str(&prefix_user_dir.join("AppData/LocalLow")),
    );
    s = s.replace("<winDocuments>", &path_str(&prefix_user_dir.join("Documents")));
    // `<root>` e a instalacao Steam em si (onde fica `userdata/`, o espelho
    // da Steam Cloud) — NAO a biblioteca onde o jogo esta instalado, que
    // pode estar num disco/mount totalmente diferente (ver doc do modulo).
    s = s.replace("<root>", &path_str(steam_root));
    s = s.replace("<base>", &path_str(install_dir));
    s = s.replace("<storeGameId>", &app_id.to_string());
    s = s.replace("<storeUserId>", "*");
    s = s.replace("<osUserName>", "*");
    s = s.replace("<language>", "*");
    if s.contains('<') {
        return None;
    }
    Some(s)
}

/// Mesma ideia, pra jogos nativos de Linux (sem prefixo Proton) — `<home>`
/// e os `<xdg*>` viram os diretorios reais do usuario, nao os do prefixo.
fn substitute_linux(template: &str, steam_root: &Path, install_dir: &Path, app_id: u32) -> Option<String> {
    let home = dirs::home_dir()?;
    let xdg_data = dirs::data_dir().unwrap_or_else(|| home.join(".local/share"));
    let xdg_config = dirs::config_dir().unwrap_or_else(|| home.join(".config"));

    let mut s = template.to_string();
    s = s.replace("<home>", &path_str(&home));
    s = s.replace("<xdgData>", &path_str(&xdg_data));
    s = s.replace("<xdgConfig>", &path_str(&xdg_config));
    s = s.replace("<root>", &path_str(steam_root));
    s = s.replace("<base>", &path_str(install_dir));
    s = s.replace("<storeGameId>", &app_id.to_string());
    s = s.replace("<storeUserId>", "*");
    s = s.replace("<osUserName>", "*");
    s = s.replace("<language>", "*");
    if s.contains('<') {
        return None;
    }
    Some(s)
}

/// Quais ambientes (Proton "Windows" dentro do prefixo, Linux nativo) uma
/// lista de `when` autoriza. Lista vazia = sem restricao = os dois. Cada
/// clausula da lista e uma alternativa (OR); `store`, quando presente, tem
/// que ser "steam" (ou a clausula nao vale pra gente).
fn allowed_environments(when: &[WhenClause]) -> (bool, bool) {
    if when.is_empty() {
        return (true, true);
    }
    let (mut windows, mut linux) = (false, false);
    for clause in when {
        let store_ok = clause
            .store
            .as_deref()
            .map_or(true, |s| s.eq_ignore_ascii_case("steam"));
        if !store_ok {
            continue;
        }
        match clause.os.as_deref() {
            None => {
                windows = true;
                linux = true;
            }
            Some("windows") => windows = true,
            Some("linux") => linux = true,
            Some(_) => {}
        }
    }
    (windows, linux)
}

fn glob_matches(pattern: &str) -> Vec<PathBuf> {
    match glob::glob(pattern) {
        Ok(paths) => paths.filter_map(Result::ok).filter(|p| p.exists()).collect(),
        Err(err) => {
            tracing::debug!(%err, pattern, "padrao de glob invalido no manifest da Ludusavi");
            Vec::new()
        }
    }
}

/// Como `glob_matches`, mas pra descobrir o destino de um restore, nao pra
/// listar saves reais: nunca temos a garantia de que a pasta ja existe (jogo
/// instalado mas nunca aberto, ou instalado agora numa maquina que nunca
/// sincronizou esse jogo — exatamente o caso em que restaurar da nuvem
/// precisa funcionar). So resolve o que da pra fixar sem chutar:
/// 1. Se ja existe alguma pasta batendo com o padrao, usa ela (igual
///    `glob_matches` — preserva o comportamento de sempre quando a pasta
///    esta la).
/// 2. Sem padrao coringa (`<storeUserId>`/`<osUserName>`/`<language>` viram
///    `*` na substituicao) sobrando no template, o caminho e literal — usa
///    ele mesmo sem existir ainda (`extract_over` cria o que faltar).
/// 3. Com um unico segmento coringa (o caso comum: espelho da Steam Cloud,
///    `<root>/userdata/<storeUserId>/...` — a pasta userdata/<id> existe
///    assim que a conta Steam loga, independente do jogo especifico ter
///    rodado), resolve o coringa contra o disco de verdade: se exatamente
///    UMA pasta bate, junta com o resto do template (que pode nao existir
///    ainda). Zero ou mais de uma pasta batendo (ambiguo) = desiste, melhor
///    nao restaurar no lugar errado do que adivinhar.
fn glob_matches_for_restore(pattern: &str) -> Vec<PathBuf> {
    let existing = glob_matches(pattern);
    if !existing.is_empty() {
        return existing;
    }
    if !pattern.contains('*') {
        return vec![PathBuf::from(pattern)];
    }

    let Some(star_idx) = pattern.find('*') else {
        return Vec::new();
    };
    let seg_end = pattern[star_idx..].find('/').map(|i| star_idx + i).unwrap_or(pattern.len());
    let glob_segment = &pattern[..seg_end];
    let rest = pattern[seg_end..].trim_start_matches('/');

    if rest.contains('*') {
        // Mais de um segmento coringa no template — resolver o primeiro nao
        // elimina a ambiguidade do resto, entao nao vale a pena tentar.
        return Vec::new();
    }

    match glob_matches(glob_segment).as_slice() {
        [only] => {
            let dest = if rest.is_empty() { only.clone() } else { only.join(rest) };
            vec![dest]
        }
        _ => Vec::new(),
    }
}

/// Caminhos de save (so entradas com `tags: [save, ...]`) que a entrada do
/// manifest aponta pra esse AppID, resolvidos nesta maquina.
///
/// `library_path` e a raiz da biblioteca Steam onde o jogo esta instalado
/// (pra achar o prefixo Proton em `steamapps/compatdata/<id>`) — pode ser um
/// disco/mount diferente de `steam_root`, a instalacao Steam "principal"
/// (`<root>` do manifest, onde fica `userdata/`, o espelho da Steam Cloud).
/// `install_dir` e o diretorio real onde o jogo foi instalado (`<base>`).
pub fn resolve_save_paths(
    entry: &ManifestEntry,
    app_id: u32,
    library_path: &Path,
    steam_root: &Path,
    install_dir: &Path,
) -> Vec<PathBuf> {
    resolve_save_paths_with(entry, app_id, library_path, steam_root, install_dir, glob_matches)
}

/// Mesma resolucao de `resolve_save_paths`, mas tolerando que a pasta ainda
/// nao exista no disco — so usada como ultimo fallback de restore
/// (`actions::restore_candidate_paths`), quando nem a deteccao ao vivo nem o
/// historico local acham nada: o cenario de maquina nova, com o jogo
/// instalado mas nunca aberto (ou nunca sincronizado a partir dela), que e
/// justamente quando restaurar da nuvem precisa funcionar (ver
/// `glob_matches_for_restore`). Pode voltar vazio mesmo com manifest, se o
/// template tiver coringa ambiguo — restaurar no lugar errado e pior do que
/// nao restaurar.
pub fn resolve_save_paths_for_restore(
    entry: &ManifestEntry,
    app_id: u32,
    library_path: &Path,
    steam_root: &Path,
    install_dir: &Path,
) -> Vec<PathBuf> {
    resolve_save_paths_with(entry, app_id, library_path, steam_root, install_dir, glob_matches_for_restore)
}

fn resolve_save_paths_with(
    entry: &ManifestEntry,
    app_id: u32,
    library_path: &Path,
    steam_root: &Path,
    install_dir: &Path,
    resolve: impl Fn(&str) -> Vec<PathBuf>,
) -> Vec<PathBuf> {
    let prefix_user_dir = library_path
        .join("steamapps/compatdata")
        .join(app_id.to_string())
        .join("pfx/drive_c/users/steamuser");
    let has_proton_prefix = prefix_user_dir.is_dir();

    let mut results = Vec::new();
    for (template, file_entry) in &entry.files {
        if !file_entry.tags.iter().any(|t| t == "save") {
            continue;
        }
        let (windows_ok, linux_ok) = allowed_environments(&file_entry.when);

        if windows_ok && has_proton_prefix {
            if let Some(pattern) =
                substitute_proton(template, &prefix_user_dir, steam_root, install_dir, app_id)
            {
                results.extend(resolve(&pattern));
            }
        }
        if linux_ok {
            if let Some(pattern) = substitute_linux(template, steam_root, install_dir, app_id) {
                results.extend(resolve(&pattern));
            }
        }
    }
    results.sort();
    results.dedup();
    results
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_minimal_manifest_entry() {
        let yaml = r#"
"God of War":
  files:
    "<home>/Saved Games/God of War":
      tags:
        - save
      when:
        - os: windows
    "<base>/settings.ini":
      tags:
        - config
      when:
        - os: windows
  steam:
    id: 1593500
"#;
        let raw: RawManifest = serde_yaml::from_str(yaml).unwrap();
        let entry = raw.values().next().unwrap();
        assert_eq!(entry.steam.as_ref().unwrap().id, Some(1593500));
        let save_entry = &entry.files["<home>/Saved Games/God of War"];
        assert_eq!(save_entry.tags, vec!["save"]);
    }

    #[test]
    fn allowed_environments_defaults_to_both_when_unrestricted() {
        assert_eq!(allowed_environments(&[]), (true, true));
    }

    #[test]
    fn allowed_environments_filters_by_os_and_store() {
        let when = vec![WhenClause {
            os: Some("windows".into()),
            store: Some("steam".into()),
        }];
        assert_eq!(allowed_environments(&when), (true, false));

        let when = vec![WhenClause {
            os: Some("windows".into()),
            store: Some("epic".into()),
        }];
        assert_eq!(allowed_environments(&when), (false, false));

        let when = vec![WhenClause { os: Some("mac".into()), store: None }];
        assert_eq!(allowed_environments(&when), (false, false));
    }

    #[test]
    fn substitute_proton_replaces_known_placeholders() {
        let prefix = Path::new("/prefix/user");
        let steam_root = Path::new("/steam");
        let install_dir = Path::new("/steam/steamapps/common/Ghost of Tsushima");
        let resolved = substitute_proton(
            "<winDocuments>/Ghost of Tsushima/<storeUserId>",
            prefix,
            steam_root,
            install_dir,
            2215430,
        )
        .unwrap();
        assert_eq!(resolved, "/prefix/user/Documents/Ghost of Tsushima/*");
    }

    #[test]
    fn substitute_proton_resolves_root_to_steam_installation_not_library() {
        // `<root>` (usado por saves espelhados na Steam Cloud, tipo
        // `<root>/userdata/<storeUserId>/<id>/remote`) tem que apontar pra
        // instalacao Steam principal, mesmo quando o jogo esta instalado
        // numa biblioteca secundaria em outro disco.
        let prefix = Path::new("/mnt/games/SteamLibrary/steamapps/compatdata/234140/pfx/drive_c/users/steamuser");
        let steam_root = Path::new("/home/user/.local/share/Steam");
        let install_dir = Path::new("/mnt/games/SteamLibrary/steamapps/common/Mad Max");
        let resolved =
            substitute_proton("<root>/userdata/<storeUserId>/234140/remote", prefix, steam_root, install_dir, 234140)
                .unwrap();
        assert_eq!(resolved, "/home/user/.local/share/Steam/userdata/*/234140/remote");
    }

    #[test]
    fn substitute_proton_gives_up_on_unknown_placeholder() {
        assert!(substitute_proton(
            "<winPublic>/shared",
            Path::new("/prefix/user"),
            Path::new("/steam"),
            Path::new("/steam/steamapps/common/Game"),
            1
        )
        .is_none());
    }

    #[test]
    fn glob_matches_for_restore_prefers_an_existing_match() {
        let tmp = tempfile::tempdir().unwrap();
        let save_dir = tmp.path().join("Game");
        fs::create_dir(&save_dir).unwrap();
        let pattern = save_dir.to_string_lossy().into_owned();
        assert_eq!(glob_matches_for_restore(&pattern), vec![save_dir]);
    }

    #[test]
    fn glob_matches_for_restore_returns_literal_path_when_nothing_exists() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("never-launched/save");
        let pattern = missing.to_string_lossy().into_owned();
        assert_eq!(glob_matches_for_restore(&pattern), vec![missing]);
    }

    #[test]
    fn glob_matches_for_restore_resolves_single_wildcard_ancestor() {
        // Cenario real: `<root>/userdata/<storeUserId>/<appid>/remote` — a
        // pasta `userdata/<id>` ja existe (criada no login da Steam), a
        // subpasta especifica do jogo ainda nao (nunca rodou).
        let tmp = tempfile::tempdir().unwrap();
        let userdata = tmp.path().join("userdata");
        fs::create_dir(&userdata).unwrap();
        fs::create_dir(userdata.join("76561198000000001")).unwrap();
        let pattern = format!("{}/*/234140/remote", userdata.to_string_lossy());
        assert_eq!(
            glob_matches_for_restore(&pattern),
            vec![userdata.join("76561198000000001").join("234140/remote")]
        );
    }

    #[test]
    fn glob_matches_for_restore_refuses_to_guess_on_ambiguous_wildcard() {
        let tmp = tempfile::tempdir().unwrap();
        let userdata = tmp.path().join("userdata");
        fs::create_dir(&userdata).unwrap();
        fs::create_dir(userdata.join("11111")).unwrap();
        fs::create_dir(userdata.join("22222")).unwrap();
        let pattern = format!("{}/*/234140/remote", userdata.to_string_lossy());
        assert!(glob_matches_for_restore(&pattern).is_empty());
    }

    #[test]
    fn glob_matches_for_restore_refuses_to_guess_with_no_wildcard_match() {
        let tmp = tempfile::tempdir().unwrap();
        let userdata = tmp.path().join("userdata");
        fs::create_dir(&userdata).unwrap();
        let pattern = format!("{}/*/234140/remote", userdata.to_string_lossy());
        assert!(glob_matches_for_restore(&pattern).is_empty());
    }
}
