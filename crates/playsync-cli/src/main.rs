mod actions;
mod i18n;
mod ipc_client;
mod tui;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use playsync_core::ipc::{CloudProvider, Request, Response, SyncStatus};
use rust_i18n::t;

rust_i18n::i18n!("locales", fallback = "en");

#[derive(Parser)]
#[command(name = "playsync", version, about = "Automatic Steam save backup to the cloud")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Show the sync status of each game (plain-text table).
    Status,
    /// Force a sync right now.
    Sync {
        /// Specific AppID to sync. If omitted, syncs all eligible games.
        #[arg(long)]
        app_id: Option<u32>,
    },
    /// Show backup history.
    History {
        #[arg(long, default_value_t = 20)]
        limit: u32,
    },
    /// Manage the connection to cloud providers.
    Cloud {
        #[command(subcommand)]
        action: CloudCommand,
    },
    /// Manage PlaySync settings (language, auto-restore).
    Config {
        #[command(subcommand)]
        action: ConfigCommand,
    },
    /// Restore a backup (local or cloud) back to the game's save folder.
    Restore {
        /// Game AppID (see `playsync status`).
        #[arg(long)]
        app_id: u32,
        /// Where to restore from: "local", "google-drive" or "box".
        #[arg(long)]
        source: String,
        /// Which save folder to restore, when the game has more than one
        /// (index shown when running without this option). Optional if the
        /// game only has one.
        #[arg(long)]
        path_index: Option<usize>,
        /// Skip the confirmation before overwriting the current save.
        #[arg(long, default_value_t = false)]
        yes: bool,
        /// Only list the available backup versions (most recent last) for
        /// this game/folder/source, without restoring anything.
        #[arg(long, default_value_t = false)]
        list_versions: bool,
        /// Restore a specific version (exact name shown by
        /// `--list-versions`) instead of the most recent one.
        #[arg(long)]
        version: Option<String>,
    },
}

#[derive(Subcommand)]
enum CloudCommand {
    /// Authorize PlaySync to access `google-drive` or `box`.
    Connect { provider: String },
    /// Sends a test zip (empty, generated on the fly) to the connected
    /// provider, just to validate the OAuth2 + upload pipeline end-to-end.
    TestUpload { provider: String },
}

#[derive(Subcommand)]
enum ConfigCommand {
    /// Turn auto-restore-on-launch on or off ("on"/"off").
    AutoRestore { state: String },
    /// Set the CLI/TUI language (e.g. "en", "pt-BR", "es", "fr", "de",
    /// "zh-CN", "ja", "ru").
    Language { code: String },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().with_target(false).init();

    let config = playsync_core::config::Config::load_or_default().unwrap_or_default();
    rust_i18n::set_locale(&i18n::resolve_language(config.language.as_deref()));

    // So importa pros comandos que chamam `discover_games` direto (restore,
    // TUI) — `status`/`sync`/`history` falam com o daemon, que cuida do seu
    // proprio refresh. Cache fresco (< 7 dias) nao gera nenhuma chamada de
    // rede, entao isso e barato na maioria das execucoes.
    tokio::spawn(async {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .expect("configuracao do reqwest::Client e estatica e valida");
        let _ = playsync_core::manifest::refresh_cache(&client, std::time::Duration::from_secs(7 * 24 * 3600)).await;
    });

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
        Some(Command::Config { action }) => match action {
            ConfigCommand::AutoRestore { state } => set_auto_restore(&state).await,
            ConfigCommand::Language { code } => set_language(&code).await,
        },
        Some(Command::Restore { app_id, source, path_index, yes, list_versions, version }) => {
            restore(app_id, &source, path_index, yes, list_versions, version.as_deref()).await
        }
    }
}

async fn print_status() -> Result<()> {
    let games = match ipc_client::send(Request::Status).await? {
        Response::Status { games } => games,
        Response::Error { message } => bail!(message),
        _ => bail!(t!("cli.common.unexpected_response")),
    };

    println!(
        "{:<40} {:<20} {}",
        t!("cli.status.header_game"),
        t!("cli.status.header_last_backup"),
        t!("cli.status.header_status"),
    );
    for game in games {
        let status = if !game.has_save_paths {
            t!("cli.status.no_save_detected")
        } else {
            match game.sync_status {
                SyncStatus::NeverSynced => t!("cli.status.never_synced"),
                SyncStatus::Idle => t!("cli.status.idle"),
                SyncStatus::Running => t!("cli.status.running"),
                SyncStatus::Error => t!("cli.status.error"),
            }
        };
        let last_backup = game
            .last_backup
            .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
            .unwrap_or_else(|| t!("cli.status.none").to_string());
        println!("{:<40} {:<20} {}", game.name, last_backup, status);
    }
    Ok(())
}

async fn sync_now(app_id: Option<u32>) -> Result<()> {
    match ipc_client::send(Request::SyncNow { app_id }).await? {
        Response::Ack => {
            println!("{}", t!("cli.sync.triggered"));
            Ok(())
        }
        Response::Error { message } => bail!(message),
        _ => bail!(t!("cli.common.unexpected_response")),
    }
}

async fn print_history(limit: u32) -> Result<()> {
    let entries = match ipc_client::send(Request::History { limit }).await? {
        Response::History { entries } => entries,
        Response::Error { message } => bail!(message),
        _ => bail!(t!("cli.common.unexpected_response")),
    };

    println!(
        "{:<40} {:<20} {:<15} {}",
        t!("cli.history.header_game"),
        t!("cli.history.header_when"),
        t!("cli.history.header_destination"),
        t!("cli.history.header_ok"),
    );
    for entry in entries {
        let ok = if entry.success { t!("cli.history.yes") } else { t!("cli.history.no") };
        println!(
            "{:<40} {:<20} {:<15} {}",
            entry.name,
            entry.timestamp.format("%Y-%m-%d %H:%M"),
            entry.destination,
            ok,
        );
    }
    Ok(())
}

async fn set_auto_restore(state: &str) -> Result<()> {
    let mut config = playsync_core::config::Config::load_or_default()?;
    let enabled = match state {
        "on" | "true" | "1" => true,
        "off" | "false" | "0" => false,
        other => bail!(t!("cli.config.invalid_auto_restore_state", value = other)),
    };
    config.auto_restore_on_launch = Some(enabled);
    config.save()?;
    if enabled {
        println!("{}", t!("cli.config.auto_restore_enabled"));
    } else {
        println!("{}", t!("cli.config.auto_restore_disabled"));
    }
    Ok(())
}

async fn set_language(code: &str) -> Result<()> {
    if !i18n::SUPPORTED_LANGUAGES.contains(&code) {
        bail!(t!(
            "cli.config.unknown_language",
            lang = code,
            available = i18n::SUPPORTED_LANGUAGES.join(", ")
        ));
    }
    let mut config = playsync_core::config::Config::load_or_default()?;
    config.language = Some(code.to_string());
    config.save()?;
    rust_i18n::set_locale(code);
    println!("{}", t!("cli.config.language_set", language = i18n::display_name(code)));
    Ok(())
}

async fn cloud_connect(provider: &str) -> Result<()> {
    match actions::parse_provider(provider)? {
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
    let provider = actions::parse_provider(provider)?;
    let backend = playsync_core::cloud::backend_for(provider);

    if !backend.is_connected() {
        bail!(t!("cli.cloud.not_connected", provider = format!("{provider:?}")));
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

    println!("{}", t!("cli.cloud.test_upload_done"));
    Ok(())
}

/// Restaura o backup de um `save_path` do jogo (local ou de um provedor de
/// nuvem) por cima da pasta/arquivo de save atual. Fala diretamente com
/// `playsync-core` (Steam, config, backend de nuvem) em vez de passar pelo
/// daemon — mesmo padrao de `cloud connect`/`cloud test-upload`.
async fn restore(
    app_id: u32,
    source: &str,
    path_index: Option<usize>,
    yes: bool,
    list_versions: bool,
    version: Option<&str>,
) -> Result<()> {
    let source = actions::parse_source(source)?;

    let games = playsync_core::steam::discover_games().with_context(|| t!("cli.restore.steam_discovery_failed").to_string())?;
    let game = games
        .into_iter()
        .find(|g| g.app_id == app_id)
        .with_context(|| t!("cli.restore.game_not_found", app_id = app_id).to_string())?;

    let (paths, used_history) = actions::restore_candidate_paths(app_id, &game.save_paths);
    if paths.is_empty() {
        bail!(t!("cli.restore.no_save_path", name = game.name));
    }
    if used_history {
        println!("{}", t!("cli.restore.used_history_warning", name = game.name));
    }

    let idx = match path_index {
        Some(idx) => idx,
        None if paths.len() == 1 => 0,
        None => {
            println!("{}", t!("cli.restore.choose_path_index", name = game.name, n = paths.len()));
            for (i, path) in paths.iter().enumerate() {
                println!("  {i}: {}", path.display());
            }
            return Ok(());
        }
    };
    let target = paths
        .get(idx)
        .with_context(|| {
            t!("cli.restore.invalid_path_index", idx = idx, name = game.name, n = paths.len()).to_string()
        })?
        .clone();

    let sanitized = playsync_core::naming::sanitize(&game.name);

    if list_versions {
        let short_session_warning_secs =
            playsync_core::config::Config::load_or_default()?.short_session_warning_secs;
        let infos = actions::list_versions_with_info(app_id, &source, &sanitized, idx, paths.len()).await?;
        if infos.is_empty() {
            println!("{}", t!("cli.restore.no_versions_found", name = game.name));
        } else {
            println!("{}", t!("cli.restore.versions_available", name = game.name));
            for info in &infos {
                println!("  {}", actions::format_version_label(info, short_session_warning_secs));
            }
        }
        return Ok(());
    }

    let versions = actions::list_versions(&source, &sanitized, idx, paths.len()).await?;
    let file_name = actions::pick_version(&versions, version)?.to_string();
    let (source_label, bytes) = actions::fetch_backup_bytes(&source, &sanitized, &file_name).await?;

    println!(
        "{}",
        t!(
            "cli.restore.confirm_prompt",
            name = game.name,
            file_name = file_name,
            source_label = source_label,
            target = target.display()
        )
    );
    if !yes {
        print!("{}", t!("cli.restore.continue_prompt"));
        std::io::Write::flush(&mut std::io::stdout())?;
        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;
        if !input.trim().eq_ignore_ascii_case("y") {
            println!("{}", t!("cli.restore.cancelled"));
            return Ok(());
        }
    }

    actions::extract_over(&bytes, &target)?;

    println!("{}", t!("cli.restore.success", target = target.display()));
    Ok(())
}
