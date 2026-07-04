mod ipc_client;
mod tui;

use anyhow::{bail, Result};
use clap::{Parser, Subcommand};
use playsync_core::ipc::{CloudProvider, Request, Response, SyncStatus};

#[derive(Parser)]
#[command(name = "playsync", version, about = "Backup automatico de saves da Steam para a nuvem")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Mostra o status de sincronizacao de cada jogo (tabela em texto puro).
    Status,
    /// Forca uma sincronizacao agora.
    Sync {
        /// AppID especifico a sincronizar. Se omitido, sincroniza todos os jogos elegiveis.
        #[arg(long)]
        app_id: Option<u32>,
    },
    /// Mostra o historico de backups.
    History {
        #[arg(long, default_value_t = 20)]
        limit: u32,
    },
    /// Gerencia a conexao com provedores de nuvem.
    Cloud {
        #[command(subcommand)]
        action: CloudCommand,
    },
}

#[derive(Subcommand)]
enum CloudCommand {
    /// Autoriza o PlaySync a acessar `google-drive` ou `box`.
    Connect { provider: String },
    /// Envia um zip de teste (vazio, gerado na hora) pro provedor conectado,
    /// so pra validar o pipeline OAuth2 + upload de ponta a ponta.
    TestUpload { provider: String },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().with_target(false).init();

    let cli = Cli::parse();
    match cli.command {
        // Sem subcomando: abre a TUI, o modo de uso interativo padrao.
        None => tui::run().await,
        Some(Command::Status) => print_status().await,
        Some(Command::Sync { app_id }) => sync_now(app_id).await,
        Some(Command::History { limit }) => print_history(limit).await,
        Some(Command::Cloud { action }) => match action {
            CloudCommand::Connect { provider } => cloud_connect(&provider).await,
            CloudCommand::TestUpload { provider } => cloud_test_upload(&provider).await,
        },
    }
}

async fn print_status() -> Result<()> {
    let games = match ipc_client::send(Request::Status).await? {
        Response::Status { games } => games,
        Response::Error { message } => bail!(message),
        _ => bail!("resposta inesperada do daemon"),
    };

    println!("{:<40} {:<20} STATUS", "JOGO", "ULTIMO BACKUP");
    for game in games {
        let status = match game.sync_status {
            SyncStatus::NeverSynced => "nunca sincronizado",
            SyncStatus::Idle => "em dia",
            SyncStatus::Running => "sincronizando...",
            SyncStatus::Error => "erro",
        };
        let last_backup = game
            .last_backup
            .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
            .unwrap_or_else(|| "-".to_string());
        println!("{:<40} {:<20} {}", game.name, last_backup, status);
    }
    Ok(())
}

async fn sync_now(app_id: Option<u32>) -> Result<()> {
    match ipc_client::send(Request::SyncNow { app_id }).await? {
        Response::Ack => {
            println!("sincronizacao disparada");
            Ok(())
        }
        Response::Error { message } => bail!(message),
        _ => bail!("resposta inesperada do daemon"),
    }
}

async fn print_history(limit: u32) -> Result<()> {
    let entries = match ipc_client::send(Request::History { limit }).await? {
        Response::History { entries } => entries,
        Response::Error { message } => bail!(message),
        _ => bail!("resposta inesperada do daemon"),
    };

    println!("{:<40} {:<20} {:<15} OK?", "JOGO", "QUANDO", "DESTINO");
    for entry in entries {
        println!(
            "{:<40} {:<20} {:<15} {}",
            entry.name,
            entry.timestamp.format("%Y-%m-%d %H:%M"),
            entry.destination,
            if entry.success { "sim" } else { "nao" },
        );
    }
    Ok(())
}

async fn cloud_connect(provider: &str) -> Result<()> {
    match parse_provider(provider)? {
        CloudProvider::GoogleDrive => {
            playsync_core::cloud::gdrive::GoogleDriveBackend::new()
                .connect()
                .await
        }
        CloudProvider::Box => bail!("fluxo OAuth2 do Box ainda nao implementado"),
    }
}

/// So pra validar o pipeline OAuth2 + upload de ponta a ponta: sobe um zip
/// vazio (gerado na hora, sem depender de nenhum save real) pro provedor.
async fn cloud_test_upload(provider: &str) -> Result<()> {
    let provider = parse_provider(provider)?;
    let backend = playsync_core::cloud::backend_for(provider);

    if !backend.is_connected() {
        bail!("nao conectado ainda — rode `playsync cloud connect {provider:?}` antes");
    }

    // Zip vazio valido: so o registro "End Of Central Directory" (22 bytes).
    const EMPTY_ZIP: [u8; 22] = [
        0x50, 0x4b, 0x05, 0x06, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    ];
    let tmp = std::env::temp_dir().join("playsync-test-upload.zip");
    std::fs::write(&tmp, EMPTY_ZIP)?;

    let result = backend.upload(&tmp, "playsync-test-upload.zip").await;
    let _ = std::fs::remove_file(&tmp);
    result?;

    println!("upload de teste concluido — confira em drive.google.com (ou no Box)");
    Ok(())
}

fn parse_provider(provider: &str) -> Result<CloudProvider> {
    match provider {
        "google-drive" | "gdrive" => Ok(CloudProvider::GoogleDrive),
        "box" => Ok(CloudProvider::Box),
        other => bail!("provedor desconhecido: {other} (use `google-drive` ou `box`)"),
    }
}
