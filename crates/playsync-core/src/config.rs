//! Configuracao do usuario, persistida em `~/.config/playsync/config.toml`.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Provedor de nuvem ativo, definido via `playsync cloud connect <provider>`.
    pub cloud_provider: Option<String>,
    /// AppIDs que o usuario optou por nao sincronizar, mesmo com save detectado.
    pub ignored_app_ids: Vec<u32>,
    /// Segundos de espera apos o jogo fechar antes de disparar o backup.
    /// Evita sync duplicado em fechamentos rapidos (ex: crash seguido de relaunch).
    pub sync_debounce_secs: u64,
    /// Raiz do backup local (um "PlaySync/<jogo>/" espelhando a estrutura da
    /// nuvem). `None` usa o default (`local_backup_root()`).
    pub local_backup_dir: Option<PathBuf>,
    /// Pastas de save adicionais, por AppID (chave string — chave de tabela
    /// TOML e sempre string), pra jogos que a deteccao automatica
    /// (`steam::find_save_candidates`) nao acha sozinha. Util quando o jogo
    /// guarda save num lugar fora da convencao usual — o PCGamingWiki
    /// (pcgamingwiki.com, pagina do jogo, secao "Save game data location")
    /// costuma ter o caminho exato. Precisa ser o caminho absoluto de
    /// verdade no disco (dentro do prefixo Proton do jogo).
    pub extra_save_paths: HashMap<String, Vec<PathBuf>>,
    /// Quantas versoes de cada save_path manter (local e na nuvem) antes de
    /// podar as mais antigas. Existe pra um sync automatico ruim (ex: o jogo
    /// aberto sem save cria um save novo/vazio, e o fechamento sincroniza
    /// isso) nao destruir a unica copia boa que existia — com >1 versao,
    /// `restore` ainda alcanca o que veio antes.
    pub backup_versions_to_keep: usize,
    /// Sessoes de jogo (abrir ate fechar) mais curtas que isso sao marcadas
    /// como suspeitas ao listar versoes de backup (`restore --list-versions`)
    /// — sinal tipico de "abri o jogo sem save, so testei, fechei" em vez de
    /// progresso de verdade.
    pub short_session_warning_secs: i64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            cloud_provider: None,
            ignored_app_ids: Vec::new(),
            sync_debounce_secs: 5,
            local_backup_dir: None,
            extra_save_paths: HashMap::new(),
            backup_versions_to_keep: 5,
            short_session_warning_secs: 120,
        }
    }
}

impl Config {
    pub fn config_path() -> Result<PathBuf> {
        let dir = dirs::config_dir().context("nao encontrei XDG_CONFIG_HOME/~/.config")?;
        Ok(dir.join("playsync").join("config.toml"))
    }

    /// Onde fica o historico (sqlite) e outros dados que nao sao "config" nem
    /// "cache" propriamente ditos. Usa `$XDG_STATE_HOME` (nao `XDG_DATA_HOME`)
    /// de proposito: e o que bate com `StateDirectory=` do systemd, que cria
    /// esse diretorio automaticamente antes de montar o `ReadWritePaths` do
    /// daemon (sem isso, o daemon falha com "226/NAMESPACE" na primeira vez
    /// que roda, porque o alvo do bind-mount ainda nao existe no host).
    pub fn state_dir() -> Result<PathBuf> {
        let dir = dirs::state_dir().context("nao encontrei XDG_STATE_HOME/~/.local/state")?;
        Ok(dir.join("playsync"))
    }

    /// Raiz do backup local: `~/PlaySync` por padrao (facil de achar/navegar,
    /// espelhando a pasta "PlaySync" criada do lado da nuvem), ou o caminho
    /// configurado em `local_backup_dir`.
    pub fn local_backup_root(&self) -> Result<PathBuf> {
        match &self.local_backup_dir {
            Some(dir) => Ok(dir.clone()),
            None => Ok(dirs::home_dir().context("nao encontrei $HOME")?.join("PlaySync")),
        }
    }

    pub fn load_or_default() -> Result<Self> {
        let path = Self::config_path()?;
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = fs::read_to_string(&path)
            .with_context(|| format!("nao consegui ler {}", path.display()))?;
        toml::from_str(&text).with_context(|| format!("config invalida em {}", path.display()))
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::config_path()?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let text = toml::to_string_pretty(self)?;
        fs::write(&path, text)
            .with_context(|| format!("nao consegui escrever {}", path.display()))
    }
}
