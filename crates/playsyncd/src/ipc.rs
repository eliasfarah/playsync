//! Servidor IPC: um Unix Domain Socket onde a CLI conecta para consultar
//! status, forcar sync ou ler o historico. Ver `playsync_core::ipc` pro protocolo.

use std::sync::Arc;

use anyhow::{Context, Result};
use playsync_core::ipc::{socket_path, Request, Response};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

use crate::sync::SyncEngine;

pub async fn serve(engine: Arc<SyncEngine>) -> Result<()> {
    let path = socket_path();
    if path.exists() {
        std::fs::remove_file(&path).ok();
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let listener = UnixListener::bind(&path)
        .with_context(|| format!("nao consegui abrir o socket {}", path.display()))?;
    tracing::info!(socket = %path.display(), "IPC pronto");

    loop {
        let (stream, _) = listener.accept().await?;
        let engine = engine.clone();
        tokio::spawn(async move {
            if let Err(err) = handle_client(stream, engine).await {
                tracing::warn!(%err, "conexao IPC encerrada com erro");
            }
        });
    }
}

async fn handle_client(stream: UnixStream, engine: Arc<SyncEngine>) -> Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();

    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }

        let response = match serde_json::from_str::<Request>(&line) {
            Ok(request) => handle_request(&engine, request).await,
            Err(err) => Response::Error {
                message: format!("requisicao invalida: {err}"),
            },
        };

        let mut payload = serde_json::to_string(&response)?;
        payload.push('\n');
        writer.write_all(payload.as_bytes()).await?;
    }

    Ok(())
}

async fn handle_request(engine: &Arc<SyncEngine>, request: Request) -> Response {
    match request {
        Request::Status => Response::Status {
            games: engine.status_snapshot().await,
        },
        Request::SyncNow { app_id } => {
            // Dispara em background e responde na hora: sincronizar "tudo" pode
            // levar bastante tempo (um jogo de cada vez, rede pra cada upload),
            // e o cliente (CLI/TUI) nao pode ficar bloqueado esperando terminar
            // so pra saber que foi aceito. Progresso real fica visivel via
            // `Status` (cada jogo marca `Running` antes de zipar/subir).
            let engine = engine.clone();
            tokio::spawn(async move {
                engine.sync_now(app_id).await;
            });
            Response::Ack
        }
        Request::History { limit } => match engine.history_entries(limit) {
            Ok(entries) => Response::History { entries },
            Err(err) => Response::Error {
                message: err.to_string(),
            },
        },
        // O fluxo OAuth2 roda inteiramente na CLI (`playsync cloud connect`), que
        // abre o navegador e escreve o token/config direto no disco. O daemon so
        // le a config na proxima sincronizacao; nao precisa participar do fluxo.
        Request::CloudConnect { .. } => Response::Error {
            message: "rode `playsync cloud connect <provider>` na CLI".to_string(),
        },
    }
}
