mod ipc;
mod sync;
mod watcher;

use std::sync::Arc;

use anyhow::{Context, Result};
use playsync_core::config::Config;
use playsync_core::db::HistoryDb;
use steamlocate::SteamDir;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;

use crate::sync::SyncEngine;
use crate::watcher::{GameEvent, SteamProcessWatcher};

#[tokio::main]
async fn main() -> Result<()> {
    init_logging();

    let steam_dir = SteamDir::locate().context("Steam nao encontrada nesta maquina")?;
    tracing::info!(steam_root = %steam_dir.path().display(), "Steam encontrada");

    let config = Config::load_or_default()?;
    let history = Arc::new(HistoryDb::open_default()?);
    let engine = Arc::new(SyncEngine::new(config, history));

    // Atualiza o cache local do manifest da Ludusavi em segundo plano (nao
    // atrasa o startup do daemon nem bloqueia se a rede estiver fora — so
    // revalida de fato se o cache tiver mais de 7 dias, ver `manifest.rs`).
    tokio::spawn(async {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .expect("configuracao do reqwest::Client e estatica e valida");
        match playsync_core::manifest::refresh_cache(&client, std::time::Duration::from_secs(7 * 24 * 3600)).await {
            Ok(()) => tracing::debug!("manifest da Ludusavi em dia"),
            Err(err) => tracing::warn!(%err, "nao consegui atualizar o manifest da Ludusavi, seguindo com o cache existente (ou so heuristica)"),
        }
    });

    let ipc_engine = engine.clone();
    tokio::spawn(async move {
        if let Err(err) = ipc::serve(ipc_engine).await {
            tracing::error!(%err, "servidor IPC encerrou com erro");
        }
    });

    let mut watcher = SteamProcessWatcher::new();

    loop {
        tokio::select! {
            event = watcher.next_event() => {
                match event {
                    Some(GameEvent::Started(app_id)) => {
                        tracing::info!(app_id, "jogo iniciado");
                        engine.mark_running(app_id).await;
                        engine.mark_session_started(app_id).await;
                    }
                    Some(GameEvent::Stopped(app_id)) => {
                        let session_duration_secs = engine.take_session_duration_secs(app_id).await;
                        tracing::info!(app_id, ?session_duration_secs, "jogo fechado, sincronizacao agendada");
                        SyncEngine::schedule_sync(engine.clone(), app_id, session_duration_secs).await;
                    }
                    None => {
                        tracing::error!("watcher de processos encerrou inesperadamente");
                        break;
                    }
                }
            }
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("recebido sinal de encerramento, saindo");
                break;
            }
        }
    }

    Ok(())
}

/// Sob `systemd --user` os logs vao pro journal estruturado; fora dele (ex:
/// `cargo run` durante desenvolvimento) cai para texto formatado em stdout.
fn init_logging() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let registry = tracing_subscriber::registry().with(filter);

    let under_systemd = std::env::var_os("JOURNAL_STREAM").is_some();
    if under_systemd {
        if let Ok(layer) = tracing_journald::layer() {
            registry.with(layer).init();
            return;
        }
    }
    registry.with(tracing_subscriber::fmt::layer()).init();
}
