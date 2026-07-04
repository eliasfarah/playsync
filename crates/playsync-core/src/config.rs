//! Configuracao do usuario, persistida em `~/.config/playsync/config.toml`.

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
}

impl Default for Config {
    fn default() -> Self {
        Self {
            cloud_provider: None,
            ignored_app_ids: Vec::new(),
            sync_debounce_secs: 5,
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
