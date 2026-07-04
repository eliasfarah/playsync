mod ipc_client;
mod tui;

use anyhow::{bail, Context, Result};
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
    /// Restaura um backup (local ou da nuvem) de volta pra pasta de save do jogo.
    Restore {
        /// AppID do jogo (veja `playsync status`).
        #[arg(long)]
        app_id: u32,
        /// De onde restaurar: "local", "google-drive" ou "box".
        #[arg(long)]
        source: String,
        /// Qual pasta de save restaurar, quando o jogo tem mais de uma
        /// (indice mostrado ao rodar sem essa opcao). Se o jogo so tem uma,
        /// e opcional.
        #[arg(long)]
        path_index: Option<usize>,
        /// Pula a confirmacao antes de sobrescrever o save atual.
        #[arg(long, default_value_t = false)]
        yes: bool,
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
        Some(Command::Restore { app_id, source, path_index, yes }) => {
            restore(app_id, &source, path_index, yes).await
        }
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
        CloudProvider::Box => {
            playsync_core::cloud::box_com::BoxBackend::new()
                .connect()
                .await
        }
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

    let result = backend.upload(&tmp, "PlaySync/playsync-test-upload.zip").await;
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

enum RestoreSource {
    Local,
    Cloud(CloudProvider),
}

fn parse_source(source: &str) -> Result<RestoreSource> {
    match source {
        "local" => Ok(RestoreSource::Local),
        other => Ok(RestoreSource::Cloud(parse_provider(other)?)),
    }
}

/// Restaura o backup de um `save_path` do jogo (local ou de um provedor de
/// nuvem) por cima da pasta/arquivo de save atual. Fala diretamente com
/// `playsync-core` (Steam, config, backend de nuvem) em vez de passar pelo
/// daemon — mesmo padrao de `cloud connect`/`cloud test-upload`.
async fn restore(app_id: u32, source: &str, path_index: Option<usize>, yes: bool) -> Result<()> {
    let source = parse_source(source)?;

    let games = playsync_core::steam::discover_games().context("falha ao listar jogos da Steam")?;
    let game = games
        .into_iter()
        .find(|g| g.app_id == app_id)
        .with_context(|| format!("jogo com AppID {app_id} nao encontrado (veja `playsync status`)"))?;

    if game.save_paths.is_empty() {
        bail!("\"{}\" nao tem pasta de save conhecida", game.name);
    }

    let idx = match path_index {
        Some(idx) => idx,
        None if game.save_paths.len() == 1 => 0,
        None => {
            println!(
                "\"{}\" tem {} pastas de save — escolha uma com --path-index:",
                game.name,
                game.save_paths.len()
            );
            for (i, path) in game.save_paths.iter().enumerate() {
                println!("  {i}: {}", path.display());
            }
            return Ok(());
        }
    };
    let target = game
        .save_paths
        .get(idx)
        .with_context(|| {
            format!(
                "--path-index {idx} invalido — \"{}\" so tem {} pasta(s) de save",
                game.name,
                game.save_paths.len()
            )
        })?
        .clone();

    let sanitized = playsync_core::naming::sanitize(&game.name);
    let file_name = if game.save_paths.len() > 1 {
        format!("save-{idx}.zip")
    } else {
        "save.zip".to_string()
    };

    let (source_label, bytes) = match &source {
        RestoreSource::Local => {
            let config = playsync_core::config::Config::load_or_default()?;
            let path = config.local_backup_root()?.join(&sanitized).join(&file_name);
            let bytes = tokio::fs::read(&path)
                .await
                .with_context(|| format!("nao encontrei backup local em {}", path.display()))?;
            ("local".to_string(), bytes)
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
            (format!("{provider:?}"), bytes)
        }
    };

    println!(
        "Restaurar \"{}\" ({file_name}, backup {source_label}) vai APAGAR e sobrescrever:\n  {}",
        game.name,
        target.display()
    );
    if !yes {
        print!("Continuar? [y/N] ");
        std::io::Write::flush(&mut std::io::stdout())?;
        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;
        if !input.trim().eq_ignore_ascii_case("y") {
            println!("cancelado");
            return Ok(());
        }
    }

    if target.is_dir() {
        std::fs::remove_dir_all(&target)
            .with_context(|| format!("nao consegui apagar {}", target.display()))?;
    } else if target.exists() {
        std::fs::remove_file(&target)
            .with_context(|| format!("nao consegui apagar {}", target.display()))?;
    }

    let anchor = target.parent().unwrap_or(&target);
    playsync_core::archive::unzip_to(&bytes, anchor).context("falha ao extrair o backup")?;

    println!("Restaurado com sucesso: {}", target.display());
    Ok(())
}
