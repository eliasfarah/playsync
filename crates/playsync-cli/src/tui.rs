//! TUI: tabela [Jogo | Ultimo backup | Status] navegavel, com um menu de
//! acoes por jogo (`Enter`): sincronizar agora, baixar da nuvem (so local),
//! restaurar do backup local, ou baixar da nuvem e restaurar.
//!
//! `event::poll`/`event::read` sao chamadas sincronas do crossterm; bloqueiam a
//! thread do tokio por ate 250ms a cada iteracao do loop. Para uma ferramenta
//! interativa de uso pontual isso e um trade-off aceitavel (evita puxar
//! `crossterm/event-stream` + mais uma dependencia so pra isso); se o app
//! crescer para algo mais ao vivo, trocar por `EventStream` e a evolucao natural.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use playsync_core::ipc::{CloudProvider, GameStatus, Request, Response, SyncStatus};
use playsync_core::steam::GameSave;
use ratatui::crossterm::event::{self, Event, KeyCode};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::widgets::{
    Block, Borders, Clear, List, ListItem, ListState, Paragraph, Row, Table, Wrap,
};
use ratatui::Frame;
use rust_i18n::t;

use crate::actions::{self, RestoreSource};
use crate::i18n;
use crate::ipc_client;

/// Intervalo entre polls de teclado; tambem usado como base pro auto-refresh
/// periodico do status (ver `AUTO_REFRESH_TICKS` abaixo).
const POLL_INTERVAL: Duration = Duration::from_millis(250);
/// A cada quantos polls (sem tecla nenhuma) o status e reconsultado sozinho —
/// sem isso, depois de `[s]` a tela so mostra progresso se o usuario ficar
/// apertando `[r]` na mao. `SyncNow` responde na hora (o sync roda em
/// background no daemon), entao esse refresh periodico e a unica forma de
/// ver os jogos passando por "sincronizando..." ate acabar.
const AUTO_REFRESH_TICKS: u32 = 4; // 4 * 250ms = ~1s

#[derive(Clone, Copy, PartialEq, Eq)]
enum GameAction {
    SyncNow,
    PullFromCloud,
    RestoreLocal,
    RestoreFromCloud,
}

/// Segundo campo e a chave de traducao (`tui.actions.*`), nao o rotulo em si
/// — o rotulo muda com o idioma, entao precisa ser resolvido via `t!()` no
/// momento de exibir, nao guardado como `&'static str` fixo.
const ACTIONS: &[(GameAction, &str)] = &[
    (GameAction::SyncNow, "tui.actions.sync_now"),
    (GameAction::PullFromCloud, "tui.actions.pull_from_cloud"),
    (GameAction::RestoreLocal, "tui.actions.restore_local"),
    (GameAction::RestoreFromCloud, "tui.actions.restore_from_cloud"),
];

fn is_destructive(action: GameAction) -> bool {
    matches!(action, GameAction::RestoreLocal | GameAction::RestoreFromCloud)
}

enum Mode {
    Table,
    ActionMenu {
        app_id: u32,
        cursor: usize,
    },
    PathChoice {
        app_id: u32,
        action: GameAction,
        paths: Vec<PathBuf>,
        cursor: usize,
    },
    VersionChoice {
        app_id: u32,
        action: GameAction,
        path_index: usize,
        versions: Vec<actions::VersionInfo>,
        cursor: usize,
        used_history: bool,
    },
    Confirm {
        app_id: u32,
        action: GameAction,
        path_index: usize,
        file_name: String,
        /// A pasta de save ao vivo nao foi encontrada — o alvo abaixo veio
        /// do historico (ultimo backup bem sucedido), nao da deteccao atual.
        /// Mostrado ANTES de confirmar (nao so depois, no resultado) pra dar
        /// chance de cancelar sabendo disso.
        used_history: bool,
    },
    Info(String),
    Settings {
        cursor: usize,
    },
}

/// Linhas da tela de configuracoes, na ordem mostrada.
const SETTINGS_ROWS: usize = 3;
const SETTINGS_CLOUD_ROW: usize = 0;
const SETTINGS_AUTO_RESTORE_ROW: usize = 1;
const SETTINGS_LANGUAGE_ROW: usize = 2;

/// Provedores que o `[Enter]` na linha de nuvem cicla entre si (`None` inclusive,
/// pra permitir desativar a nuvem sem editar o config.toml na mao).
const CYCLABLE_PROVIDERS: &[Option<&str>] = &[None, Some("google-drive"), Some("box")];

pub async fn run() -> Result<()> {
    let mut games = fetch_status().await.unwrap_or_default();
    let mut game_saves = discover_saves();
    let mut selected: usize = 0;
    let mut mode = Mode::Table;

    let mut terminal = ratatui::init();
    let mut ticks_since_refresh = 0u32;

    let result: Result<()> = loop {
        if let Err(err) = terminal.draw(|frame| draw(frame, &games, selected, &mode)) {
            break Err(err.into());
        }

        let event_ready = match event::poll(POLL_INTERVAL) {
            Ok(ready) => ready,
            Err(err) => break Err(err.into()),
        };

        if !event_ready {
            ticks_since_refresh += 1;
            if ticks_since_refresh >= AUTO_REFRESH_TICKS && matches!(mode, Mode::Table) {
                games = fetch_status().await.unwrap_or_default();
                ticks_since_refresh = 0;
            }
            continue;
        }
        ticks_since_refresh = 0;

        let key = match event::read() {
            Ok(Event::Key(key)) => key,
            Ok(_) => continue,
            Err(err) => break Err(err.into()),
        };

        let current = std::mem::replace(&mut mode, Mode::Table);
        mode = match current {
            Mode::Table => match key.code {
                KeyCode::Char('q') | KeyCode::Esc => break Ok(()),
                KeyCode::Char('r') => {
                    games = fetch_status().await.unwrap_or_default();
                    game_saves = discover_saves();
                    Mode::Table
                }
                KeyCode::Char('s') => {
                    let _ = ipc_client::send(Request::SyncNow { app_id: None }).await;
                    Mode::Table
                }
                KeyCode::Char('c') => Mode::Settings { cursor: 0 },
                KeyCode::Up | KeyCode::Char('k') => {
                    selected = selected.saturating_sub(1);
                    Mode::Table
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    if selected + 1 < games.len() {
                        selected += 1;
                    }
                    Mode::Table
                }
                KeyCode::Enter => match games.get(selected) {
                    Some(game) => Mode::ActionMenu { app_id: game.app_id, cursor: 0 },
                    None => Mode::Table,
                },
                _ => Mode::Table,
            },

            Mode::ActionMenu { app_id, cursor } => match key.code {
                KeyCode::Esc => Mode::Table,
                KeyCode::Up | KeyCode::Char('k') => {
                    Mode::ActionMenu { app_id, cursor: cursor.saturating_sub(1) }
                }
                KeyCode::Down | KeyCode::Char('j') => Mode::ActionMenu {
                    app_id,
                    cursor: (cursor + 1).min(ACTIONS.len() - 1),
                },
                KeyCode::Enter => {
                    let action = ACTIONS[cursor].0;
                    choose_action(app_id, action, &game_saves).await
                }
                _ => Mode::ActionMenu { app_id, cursor },
            },

            Mode::PathChoice { app_id, action, paths, cursor } => match key.code {
                KeyCode::Esc => Mode::Table,
                KeyCode::Up | KeyCode::Char('k') => {
                    Mode::PathChoice { app_id, action, cursor: cursor.saturating_sub(1), paths }
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    let cursor = (cursor + 1).min(paths.len().saturating_sub(1));
                    Mode::PathChoice { app_id, action, cursor, paths }
                }
                KeyCode::Enter => after_path_chosen(app_id, action, cursor, &game_saves).await,
                _ => Mode::PathChoice { app_id, action, cursor, paths },
            },

            Mode::VersionChoice { app_id, action, path_index, versions, cursor, used_history } => {
                match key.code {
                    KeyCode::Esc => Mode::Table,
                    KeyCode::Up | KeyCode::Char('k') => Mode::VersionChoice {
                        app_id,
                        action,
                        path_index,
                        cursor: cursor.saturating_sub(1),
                        versions,
                        used_history,
                    },
                    KeyCode::Down | KeyCode::Char('j') => {
                        let cursor = (cursor + 1).min(versions.len().saturating_sub(1));
                        Mode::VersionChoice { app_id, action, path_index, cursor, versions, used_history }
                    }
                    KeyCode::Enter => {
                        let file_name = versions[cursor].file_name.clone();
                        confirm_or_run(app_id, action, path_index, file_name, used_history, &game_saves).await
                    }
                    _ => Mode::VersionChoice { app_id, action, path_index, cursor, versions, used_history },
                }
            }

            // [Enter]/[y] confirma, [Esc] cancela — mesmo padrao das outras
            // telas (Enter = prosseguir, Esc = cancelar). Qualquer outra
            // tecla fica parado aqui (nao cancela sem querer): antes,
            // qualquer tecla que nao fosse 'y' voltava pra tabela em
            // silencio, o que o usuario confundiu com "apertei Enter e nao
            // aconteceu nada" — Enter *parecia* ter cancelado, mas na
            // verdade nunca tinha confirmado nada pra comecar.
            Mode::Confirm { app_id, action, path_index, file_name, used_history } => match key.code {
                KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => {
                    Mode::Info(run_action(app_id, action, path_index, &file_name, &game_saves).await)
                }
                KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => Mode::Table,
                _ => Mode::Confirm { app_id, action, path_index, file_name, used_history },
            },

            Mode::Info(_) => Mode::Table,

            Mode::Settings { cursor } => match key.code {
                KeyCode::Esc => Mode::Table,
                KeyCode::Up | KeyCode::Char('k') => Mode::Settings { cursor: cursor.saturating_sub(1) },
                KeyCode::Down | KeyCode::Char('j') => {
                    Mode::Settings { cursor: (cursor + 1).min(SETTINGS_ROWS - 1) }
                }
                KeyCode::Enter => {
                    apply_settings_action(cursor);
                    Mode::Settings { cursor }
                }
                _ => Mode::Settings { cursor },
            },
        };

        // Qualquer transicao pra Info significa que uma acao acabou de rodar
        // (ou foi disparada) — reconsulta pra refletir o resultado na hora,
        // em vez de esperar o proximo auto-refresh.
        if matches!(mode, Mode::Info(_)) {
            games = fetch_status().await.unwrap_or_default();
            game_saves = discover_saves();
        }
    };

    ratatui::restore();
    result
}

fn discover_saves() -> Vec<GameSave> {
    playsync_core::steam::discover_games().unwrap_or_default()
}

async fn fetch_status() -> Result<Vec<GameStatus>> {
    match ipc_client::send(Request::Status).await? {
        Response::Status { games } => Ok(games),
        Response::Error { message } => bail!(message),
        _ => bail!(t!("cli.common.unexpected_response")),
    }
}

fn active_cloud_provider() -> Result<CloudProvider> {
    let config = playsync_core::config::Config::load_or_default()?;
    let raw = config
        .cloud_provider
        .with_context(|| t!("tui.cloud_provider_not_configured").to_string())?;
    actions::parse_provider(&raw)
}

/// Aplica a acao de `[Enter]` na linha `cursor` da tela de configuracoes:
/// cicla o provedor de nuvem, alterna o auto-restore, ou cicla o idioma —
/// persiste em `config.toml` na hora (mesmo padrao de `playsync config ...`
/// na CLI). Idioma tambem aplica `rust_i18n::set_locale` na hora, sem
/// precisar reiniciar a TUI pra ver o efeito.
fn apply_settings_action(cursor: usize) {
    let Ok(mut config) = playsync_core::config::Config::load_or_default() else {
        return;
    };
    match cursor {
        SETTINGS_CLOUD_ROW => {
            let current = config.cloud_provider.as_deref();
            let idx = CYCLABLE_PROVIDERS.iter().position(|p| *p == current).unwrap_or(0);
            let next = CYCLABLE_PROVIDERS[(idx + 1) % CYCLABLE_PROVIDERS.len()];
            config.cloud_provider = next.map(str::to_string);
        }
        SETTINGS_AUTO_RESTORE_ROW => {
            config.auto_restore_on_launch = Some(!config.auto_restore_on_launch_effective());
        }
        SETTINGS_LANGUAGE_ROW => {
            let current = rust_i18n::locale().to_string();
            let idx = i18n::SUPPORTED_LANGUAGES.iter().position(|l| *l == current).unwrap_or(0);
            let next = i18n::SUPPORTED_LANGUAGES[(idx + 1) % i18n::SUPPORTED_LANGUAGES.len()];
            config.language = Some(next.to_string());
            rust_i18n::set_locale(next);
        }
        _ => {}
    }
    let _ = config.save();
}

/// Decide o proximo passo depois que uma acao foi escolhida no menu: dispara
/// na hora (`SyncNow`), pede pra escolher a pasta de save (se houver mais de
/// uma), ou segue pra escolha de versao (`after_path_chosen`).
async fn choose_action(app_id: u32, action: GameAction, game_saves: &[GameSave]) -> Mode {
    let Some(game) = game_saves.iter().find(|g| g.app_id == app_id) else {
        return Mode::Info(t!("tui.game_not_found", app_id = app_id).to_string());
    };

    if action == GameAction::SyncNow {
        let _ = ipc_client::send(Request::SyncNow { app_id: Some(app_id) }).await;
        return Mode::Info(t!("tui.sync_triggered", name = game.name).to_string());
    }

    let (paths, _) = actions::restore_candidate_paths(app_id, &game.save_paths);
    if paths.is_empty() {
        return Mode::Info(t!("cli.restore.no_save_path", name = game.name).to_string());
    }

    if paths.len() == 1 {
        after_path_chosen(app_id, action, 0, game_saves).await
    } else {
        Mode::PathChoice { app_id, action, paths, cursor: 0 }
    }
}

/// De onde a acao le/escreve o backup — `PullFromCloud`/`RestoreFromCloud`
/// usam o provedor de nuvem ativo, `RestoreLocal` e sempre local.
fn version_source_for(action: GameAction) -> Result<RestoreSource> {
    match action {
        GameAction::SyncNow => unreachable!("SyncNow nao usa origem de restore"),
        GameAction::PullFromCloud | GameAction::RestoreFromCloud => {
            Ok(RestoreSource::Cloud(active_cloud_provider()?))
        }
        GameAction::RestoreLocal => Ok(RestoreSource::Local),
    }
}

/// Depois que o path_index esta resolvido (so tinha um, ou o usuario
/// escolheu no `PathChoice`): lista as versoes disponiveis pra esse
/// path_index+origem e, se houver mais de uma, deixa o usuario escolher
/// (`VersionChoice`) — senao segue direto pra confirmar/rodar com a unica
/// que existe.
async fn after_path_chosen(app_id: u32, action: GameAction, path_index: usize, game_saves: &[GameSave]) -> Mode {
    let Some(game) = game_saves.iter().find(|g| g.app_id == app_id) else {
        return Mode::Info(t!("tui.game_not_found", app_id = app_id).to_string());
    };
    let (paths, used_history) = actions::restore_candidate_paths(app_id, &game.save_paths);
    let sanitized = playsync_core::naming::sanitize(&game.name);
    let paths_len = paths.len();

    let source = match version_source_for(action) {
        Ok(source) => source,
        Err(err) => return Mode::Info(t!("tui.error_prefix", err = err).to_string()),
    };

    let versions = match actions::list_versions_with_info(app_id, &source, &sanitized, path_index, paths_len).await {
        Ok(versions) => versions,
        Err(err) => return Mode::Info(t!("tui.error_prefix", err = err).to_string()),
    };

    if versions.is_empty() {
        return Mode::Info(t!("cli.restore.no_versions_found", name = game.name).to_string());
    }

    if versions.len() == 1 {
        let file_name = versions[0].file_name.clone();
        confirm_or_run(app_id, action, path_index, file_name, used_history, game_saves).await
    } else {
        let cursor = versions.len() - 1; // comeca na mais recente
        Mode::VersionChoice { app_id, action, path_index, versions, cursor, used_history }
    }
}

/// Ultimo passo antes de executar: acoes destrutivas pedem confirmacao (com
/// o aviso de fallback pro historico ja visivel ali, nao so no resultado —
/// da chance de cancelar sabendo que o alvo veio do ultimo backup, nao da
/// pasta de save ao vivo), as outras (baixar da nuvem, so guarda local) rodam direto.
async fn confirm_or_run(
    app_id: u32,
    action: GameAction,
    path_index: usize,
    file_name: String,
    used_history: bool,
    game_saves: &[GameSave],
) -> Mode {
    if is_destructive(action) {
        Mode::Confirm { app_id, action, path_index, file_name, used_history }
    } else {
        Mode::Info(run_action(app_id, action, path_index, &file_name, game_saves).await)
    }
}

/// Executa de fato a acao (bloqueia a tela ate terminar — pra um unico jogo/
/// pasta isso e rapido, ao contrario do "sincronizar tudo"). `file_name` ja
/// vem resolvido (a unica versao existente, ou a que o usuario escolheu em
/// `VersionChoice`).
async fn run_action(app_id: u32, action: GameAction, path_index: usize, file_name: &str, game_saves: &[GameSave]) -> String {
    let Some(game) = game_saves.iter().find(|g| g.app_id == app_id) else {
        return t!("tui.game_not_found", app_id = app_id).to_string();
    };
    let (paths, used_history) = actions::restore_candidate_paths(app_id, &game.save_paths);
    let Some(target) = paths.get(path_index) else {
        return t!("tui.invalid_path_index", path_index = path_index).to_string();
    };
    let target = target.clone();
    let sanitized = playsync_core::naming::sanitize(&game.name);
    let warning = if used_history {
        t!("tui.history_fallback_prefix").to_string()
    } else {
        String::new()
    };

    let outcome: Result<String> = async {
        match action {
            GameAction::SyncNow => unreachable!("SyncNow nao usa path_index"),
            GameAction::PullFromCloud => {
                let provider = active_cloud_provider()?;
                let source = RestoreSource::Cloud(provider);
                let (_, bytes) = actions::fetch_backup_bytes(&source, &sanitized, file_name).await?;
                let local_dest = playsync_core::config::Config::load_or_default()?
                    .local_backup_root()?
                    .join(&sanitized)
                    .join(file_name);
                if let Some(parent) = local_dest.parent() {
                    tokio::fs::create_dir_all(parent).await?;
                }
                tokio::fs::write(&local_dest, bytes).await?;
                Ok(t!("tui.pulled_from_cloud", path = local_dest.display()).to_string())
            }
            GameAction::RestoreLocal => {
                let (_, bytes) =
                    actions::fetch_backup_bytes(&RestoreSource::Local, &sanitized, file_name).await?;
                actions::extract_over(&bytes, &target)?;
                Ok(t!("tui.restored_local", file_name = file_name, path = target.display()).to_string())
            }
            GameAction::RestoreFromCloud => {
                let provider = active_cloud_provider()?;
                let (_, bytes) =
                    actions::fetch_backup_bytes(&RestoreSource::Cloud(provider), &sanitized, file_name).await?;
                actions::extract_over(&bytes, &target)?;
                Ok(t!("tui.restored_cloud", file_name = file_name, path = target.display()).to_string())
            }
        }
    }
    .await;

    match outcome {
        Ok(msg) => format!("{warning}{msg}"),
        Err(err) => t!("tui.error_prefix", err = err).to_string(),
    }
}

fn draw(frame: &mut Frame, games: &[GameStatus], selected: usize, mode: &Mode) {
    draw_table(frame, games, selected);

    match mode {
        Mode::Table => {}
        Mode::ActionMenu { app_id, cursor } => {
            let title = game_title(games, *app_id);
            let items: Vec<ListItem> = ACTIONS.iter().map(|(_, key)| ListItem::new(t!(*key).to_string())).collect();
            draw_menu_popup(frame, &t!("tui.action_menu_title", title = title).to_string(), items, *cursor);
        }
        Mode::PathChoice { app_id, paths, cursor, .. } => {
            let title = game_title(games, *app_id);
            let items: Vec<ListItem> = paths
                .iter()
                .enumerate()
                .map(|(i, p)| ListItem::new(format!("{i}: {}", p.display())))
                .collect();
            draw_menu_popup(frame, &t!("tui.path_choice_title", title = title).to_string(), items, *cursor);
        }
        Mode::VersionChoice { app_id, versions, cursor, used_history, .. } => {
            let title = game_title(games, *app_id);
            let short_session_warning_secs = playsync_core::config::Config::load_or_default()
                .map(|c| c.short_session_warning_secs)
                .unwrap_or(120);
            let items: Vec<ListItem> = versions
                .iter()
                .map(|v| ListItem::new(actions::format_version_label(v, short_session_warning_secs)))
                .collect();
            let warning = if *used_history { t!("tui.version_choice_warning").to_string() } else { String::new() };
            draw_menu_popup(
                frame,
                &t!("tui.version_choice_title", title = title, warning = warning).to_string(),
                items,
                *cursor,
            );
        }
        Mode::Confirm { app_id, action, file_name, used_history, .. } => {
            let title = game_title(games, *app_id);
            let label = ACTIONS
                .iter()
                .find(|(a, _)| *a == *action)
                .map(|(_, key)| t!(*key).to_string())
                .unwrap_or_else(|| "?".to_string());
            let history_warning = if *used_history {
                t!("tui.confirm_history_warning").to_string()
            } else {
                String::new()
            };
            // Os comandos ficam no TITULO (nunca corta, e desenhado direto na
            // borda) alem do corpo — o corpo sozinho pode estourar a altura
            // fixa do popup (ex: com o aviso de historico) e cortar a ultima
            // linha sem avisar, foi exatamente o bug que o usuario achou.
            draw_message_popup(
                frame,
                &t!("tui.confirm_title").to_string(),
                &t!(
                    "tui.confirm_body",
                    title = title,
                    label = label,
                    file_name = file_name,
                    history_warning = history_warning
                ),
                45,
            );
        }
        Mode::Info(message) => {
            draw_message_popup(
                frame,
                &t!("tui.info_title").to_string(),
                &format!("{message}{}", t!("tui.info_body_suffix")),
                30,
            );
        }
        Mode::Settings { cursor } => draw_settings(frame, *cursor),
    }
}

fn draw_settings(frame: &mut Frame, cursor: usize) {
    let config = playsync_core::config::Config::load_or_default().unwrap_or_default();

    let cloud_value = config
        .cloud_provider
        .clone()
        .unwrap_or_else(|| t!("tui.settings.cloud_provider_none").to_string());
    let auto_restore_value = if config.auto_restore_on_launch_effective() {
        t!("tui.settings.state_on").to_string()
    } else {
        t!("tui.settings.state_off").to_string()
    };
    let current_locale = rust_i18n::locale().to_string();
    let language_value = i18n::display_name(&current_locale).to_string();

    let items = vec![
        ListItem::new(format!("{}: {cloud_value}", t!("tui.settings.cloud_provider_label"))),
        ListItem::new(format!("{}: {auto_restore_value}", t!("tui.settings.auto_restore_label"))),
        ListItem::new(format!("{}: {language_value}", t!("tui.settings.language_label"))),
    ];

    let title = format!("{}{}", t!("tui.settings.title"), t!("tui.settings.hint"));
    draw_menu_popup(frame, &title, items, cursor);
}

fn game_title(games: &[GameStatus], app_id: u32) -> String {
    games
        .iter()
        .find(|g| g.app_id == app_id)
        .map(|g| g.name.clone())
        .unwrap_or_else(|| format!("AppID {app_id}"))
}

fn draw_table(frame: &mut Frame, games: &[GameStatus], selected: usize) {
    let rows = games.iter().enumerate().map(|(i, g)| {
        let status = if !g.has_save_paths {
            t!("tui.table.no_save_detected")
        } else {
            match g.sync_status {
                SyncStatus::NeverSynced => t!("cli.status.never_synced"),
                SyncStatus::Idle => t!("cli.status.idle"),
                SyncStatus::Running => t!("cli.status.running"),
                SyncStatus::Error => t!("cli.status.error"),
            }
        };
        let last_backup = g
            .last_backup
            .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
            .unwrap_or_else(|| t!("cli.status.none").to_string());
        let row = Row::new(vec![g.name.clone(), last_backup, status.to_string()]);
        if i == selected {
            row.style(Style::new().add_modifier(Modifier::REVERSED))
        } else {
            row
        }
    });

    let widths = [
        Constraint::Percentage(50),
        Constraint::Percentage(25),
        Constraint::Percentage(25),
    ];

    let header = Row::new(vec![
        t!("tui.table.header_game").to_string(),
        t!("tui.table.header_last_backup").to_string(),
        t!("tui.table.header_status").to_string(),
    ])
    .style(Style::new().add_modifier(Modifier::BOLD));

    let table = Table::new(rows, widths).header(header).block(
        Block::default()
            .title(t!("tui.table.title").to_string())
            .borders(Borders::ALL),
    );

    frame.render_widget(table, frame.area());
}

fn draw_menu_popup(frame: &mut Frame, title: &str, items: Vec<ListItem>, cursor: usize) {
    let area = centered_rect(60, 40, frame.area());
    frame.render_widget(Clear, area);

    let mut state = ListState::default();
    state.select(Some(cursor));

    let list = List::new(items)
        .block(Block::default().title(title.to_string()).borders(Borders::ALL))
        .highlight_style(Style::new().add_modifier(Modifier::REVERSED))
        .highlight_symbol("> ");

    frame.render_stateful_widget(list, area, &mut state);
}

/// `height_percent` deve ser generoso o bastante pro `message` inteiro caber
/// sem cortar — o `Paragraph` com `Wrap` nao rola sozinho, se o texto for
/// mais alto que a area ele so corta em silencio (foi assim que o aviso de
/// historico no `Confirm` acabou escondendo a linha de comandos). Os
/// comandos tambem ficam no `title` (nunca corta) como reforco.
fn draw_message_popup(frame: &mut Frame, title: &str, message: &str, height_percent: u16) {
    let area = centered_rect(70, height_percent, frame.area());
    frame.render_widget(Clear, area);

    let paragraph = Paragraph::new(message.to_string())
        .block(Block::default().title(title.to_string()).borders(Borders::ALL))
        .wrap(Wrap { trim: false });

    frame.render_widget(paragraph, area);
}

/// Recorta um retangulo centralizado (percentual da tela) — recipe padrao do
/// ratatui pra popups.
fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let vertical = Layout::vertical([
        Constraint::Percentage((100 - percent_y) / 2),
        Constraint::Percentage(percent_y),
        Constraint::Percentage((100 - percent_y) / 2),
    ])
    .split(area);

    Layout::horizontal([
        Constraint::Percentage((100 - percent_x) / 2),
        Constraint::Percentage(percent_x),
        Constraint::Percentage((100 - percent_x) / 2),
    ])
    .split(vertical[1])[1]
}
