//! Gatilho de sincronizacao: detecta jogos abrindo/fechando via `/proc`.
//!
//! Tentativa anterior: observar `~/.steam/registry.vdf` (chave `RunningAppID`)
//! via inotify. Testado ao vivo contra uma instalacao real da Steam e
//! descartado — nesta versao do client (Steam Runtime com pressure-vessel),
//! abrir e fechar um jogo nao gera nenhuma escrita nesse arquivo; a chave
//! nem chega a existir. O `RunningAppID` parece estar amarrado ao subsistema
//! de Steam Input/controller, nao a toda sessao de jogo.
//!
//! Mecanismo atual: toda vez que a Steam lanca um jogo (nativo ou via Proton),
//! ela define a variavel de ambiente `SteamAppId`/`SteamGameId` no processo —
//! confirmado nos binarios do proprio Steam Runtime (`pressure-vessel`,
//! `steam-runtime-launch-options`). E o mesmo sinal que ferramentas como
//! MangoHud, gamescope e protontricks usam pra identificar "isso e um jogo
//! da Steam, appid X". Fazemos polling leve em `/proc/*/environ` (so nossos
//! proprios processos, sem custo de leitura de arquivos grandes) em vez de
//! inotify continuo nas pastas de save.

use std::collections::{HashSet, VecDeque};
use std::fs;
use std::time::Duration;

const POLL_INTERVAL: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GameEvent {
    Started(u32),
    Stopped(u32),
}

pub struct SteamProcessWatcher {
    running: HashSet<u32>,
    pending: VecDeque<GameEvent>,
}

impl SteamProcessWatcher {
    pub fn new() -> Self {
        Self {
            running: HashSet::new(),
            pending: VecDeque::new(),
        }
    }

    /// Aguarda a proxima transicao de estado (inicio ou fim de um jogo).
    pub async fn next_event(&mut self) -> Option<GameEvent> {
        loop {
            if let Some(event) = self.pending.pop_front() {
                return Some(event);
            }

            tokio::time::sleep(POLL_INTERVAL).await;
            let current = tokio::task::spawn_blocking(scan_running_app_ids)
                .await
                .unwrap_or_default();

            for &id in current.difference(&self.running) {
                self.pending.push_back(GameEvent::Started(id));
            }
            for &id in self.running.difference(&current) {
                self.pending.push_back(GameEvent::Stopped(id));
            }
            self.running = current;
        }
    }
}

impl Default for SteamProcessWatcher {
    fn default() -> Self {
        Self::new()
    }
}

/// Varre `/proc/<pid>/environ` de todo mundo (falhas de permissao em processos
/// de outros usuarios sao ignoradas) procurando `SteamAppId=`/`SteamGameId=`.
fn scan_running_app_ids() -> HashSet<u32> {
    let mut ids = HashSet::new();

    let Ok(entries) = fs::read_dir("/proc") else {
        return ids;
    };

    for entry in entries.flatten() {
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        let Ok(environ) = fs::read(format!("/proc/{pid}/environ")) else {
            continue;
        };

        for var in environ.split(|&b| b == 0) {
            let var = String::from_utf8_lossy(var);
            let value = var
                .strip_prefix("SteamGameId=")
                .or_else(|| var.strip_prefix("SteamAppId="));
            if let Some(id) = value.and_then(|v| v.trim().parse::<u32>().ok()) {
                ids.insert(id);
            }
        }
    }

    ids
}
