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

use crate::actions::{self, RestoreSource};
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

const ACTIONS: &[(GameAction, &str)] = &[
    (GameAction::SyncNow, "Sincronizar agora (backup local + nuvem)"),
    (GameAction::PullFromCloud, "Baixar da nuvem (so guarda local, nao mexe no save)"),
    (GameAction::RestoreLocal, "Restaurar no jogo (a partir do backup local)"),
    (GameAction::RestoreFromCloud, "Baixar da nuvem e restaurar no jogo"),
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
    },
    Confirm {
        app_id: u32,
        action: GameAction,
        path_index: usize,
        file_name: String,
    },
    Info(String),
}

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

            Mode::VersionChoice { app_id, action, path_index, versions, cursor } => match key.code {
                KeyCode::Esc => Mode::Table,
                KeyCode::Up | KeyCode::Char('k') => {
                    Mode::VersionChoice { app_id, action, path_index, cursor: cursor.saturating_sub(1), versions }
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    let cursor = (cursor + 1).min(versions.len().saturating_sub(1));
                    Mode::VersionChoice { app_id, action, path_index, cursor, versions }
                }
                KeyCode::Enter => {
                    let file_name = versions[cursor].file_name.clone();
                    confirm_or_run(app_id, action, path_index, file_name, &game_saves).await
                }
                _ => Mode::VersionChoice { app_id, action, path_index, cursor, versions },
            },

            Mode::Confirm { app_id, action, path_index, file_name } => match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    Mode::Info(run_action(app_id, action, path_index, &file_name, &game_saves).await)
                }
                _ => Mode::Table,
            },

            Mode::Info(_) => Mode::Table,
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
        _ => bail!("resposta inesperada do daemon"),
    }
}

fn active_cloud_provider() -> Result<CloudProvider> {
    let config = playsync_core::config::Config::load_or_default()?;
    let raw = config
        .cloud_provider
        .context("nenhum provedor de nuvem configurado (rode `playsync cloud connect`)")?;
    actions::parse_provider(&raw)
}

/// Decide o proximo passo depois que uma acao foi escolhida no menu: dispara
/// na hora (`SyncNow`), pede pra escolher a pasta de save (se houver mais de
/// uma), ou segue pra escolha de versao (`after_path_chosen`).
async fn choose_action(app_id: u32, action: GameAction, game_saves: &[GameSave]) -> Mode {
    let Some(game) = game_saves.iter().find(|g| g.app_id == app_id) else {
        return Mode::Info(format!("jogo (AppID {app_id}) nao encontrado"));
    };

    if action == GameAction::SyncNow {
        let _ = ipc_client::send(Request::SyncNow { app_id: Some(app_id) }).await;
        return Mode::Info(format!("sincronizacao de \"{}\" disparada", game.name));
    }

    let (paths, _) = actions::restore_candidate_paths(app_id, &game.save_paths);
    if paths.is_empty() {
        return Mode::Info(format!(
            "\"{}\" nao tem pasta de save conhecida (nem ao vivo, nem no historico de backups)",
            game.name
        ));
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
        return Mode::Info(format!("jogo (AppID {app_id}) nao encontrado"));
    };
    let (paths, _) = actions::restore_candidate_paths(app_id, &game.save_paths);
    let sanitized = playsync_core::naming::sanitize(&game.name);
    let paths_len = paths.len();

    let source = match version_source_for(action) {
        Ok(source) => source,
        Err(err) => return Mode::Info(format!("Erro: {err}")),
    };

    let versions = match actions::list_versions_with_info(app_id, &source, &sanitized, path_index, paths_len).await {
        Ok(versions) => versions,
        Err(err) => return Mode::Info(format!("Erro: {err}")),
    };

    if versions.is_empty() {
        return Mode::Info(format!("nenhum backup encontrado pra \"{}\" nessa origem", game.name));
    }

    if versions.len() == 1 {
        let file_name = versions[0].file_name.clone();
        confirm_or_run(app_id, action, path_index, file_name, game_saves).await
    } else {
        let cursor = versions.len() - 1; // comeca na mais recente
        Mode::VersionChoice { app_id, action, path_index, versions, cursor }
    }
}

/// Ultimo passo antes de executar: acoes destrutivas pedem confirmacao,
/// as outras (baixar da nuvem, so guarda local) rodam direto.
async fn confirm_or_run(
    app_id: u32,
    action: GameAction,
    path_index: usize,
    file_name: String,
    game_saves: &[GameSave],
) -> Mode {
    if is_destructive(action) {
        Mode::Confirm { app_id, action, path_index, file_name }
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
        return format!("jogo (AppID {app_id}) nao encontrado");
    };
    let (paths, used_history) = actions::restore_candidate_paths(app_id, &game.save_paths);
    let Some(target) = paths.get(path_index) else {
        return format!("indice de save invalido ({path_index})");
    };
    let target = target.clone();
    let sanitized = playsync_core::naming::sanitize(&game.name);
    let warning = if used_history {
        "aviso: pasta de save atual nao encontrada no disco, usando o caminho do ultimo backup — "
    } else {
        ""
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
                Ok(format!("Baixado da nuvem para {}", local_dest.display()))
            }
            GameAction::RestoreLocal => {
                let (_, bytes) =
                    actions::fetch_backup_bytes(&RestoreSource::Local, &sanitized, file_name).await?;
                actions::extract_over(&bytes, &target)?;
                Ok(format!("Restaurado (backup local, {file_name}) em {}", target.display()))
            }
            GameAction::RestoreFromCloud => {
                let provider = active_cloud_provider()?;
                let (_, bytes) =
                    actions::fetch_backup_bytes(&RestoreSource::Cloud(provider), &sanitized, file_name).await?;
                actions::extract_over(&bytes, &target)?;
                Ok(format!("Restaurado (nuvem, {file_name}) em {}", target.display()))
            }
        }
    }
    .await;

    match outcome {
        Ok(msg) => format!("{warning}{msg}"),
        Err(err) => format!("Erro: {err}"),
    }
}

fn draw(frame: &mut Frame, games: &[GameStatus], selected: usize, mode: &Mode) {
    draw_table(frame, games, selected);

    match mode {
        Mode::Table => {}
        Mode::ActionMenu { app_id, cursor } => {
            let title = game_title(games, *app_id);
            let items: Vec<ListItem> = ACTIONS.iter().map(|(_, label)| ListItem::new(*label)).collect();
            draw_menu_popup(
                frame,
                &format!(" {title} — escolha uma acao  ([Esc] cancelar) "),
                items,
                *cursor,
            );
        }
        Mode::PathChoice { app_id, paths, cursor, .. } => {
            let title = game_title(games, *app_id);
            let items: Vec<ListItem> = paths
                .iter()
                .enumerate()
                .map(|(i, p)| ListItem::new(format!("{i}: {}", p.display())))
                .collect();
            draw_menu_popup(
                frame,
                &format!(" {title} — qual pasta de save?  ([Esc] cancelar) "),
                items,
                *cursor,
            );
        }
        Mode::VersionChoice { app_id, versions, cursor, .. } => {
            let title = game_title(games, *app_id);
            let short_session_warning_secs = playsync_core::config::Config::load_or_default()
                .map(|c| c.short_session_warning_secs)
                .unwrap_or(120);
            let items: Vec<ListItem> = versions
                .iter()
                .map(|v| ListItem::new(actions::format_version_label(v, short_session_warning_secs)))
                .collect();
            draw_menu_popup(
                frame,
                &format!(" {title} — qual versao restaurar? (mais recente marcada abaixo)  ([Esc] cancelar) "),
                items,
                *cursor,
            );
        }
        Mode::Confirm { app_id, action, file_name, .. } => {
            let title = game_title(games, *app_id);
            let label = ACTIONS.iter().find(|(a, _)| *a == *action).map(|(_, l)| *l).unwrap_or("?");
            draw_message_popup(
                frame,
                " Confirmar ",
                &format!(
                    "{title}\n\n{label}\n\nVersao: {file_name}\n\n\
                     Isso vai APAGAR o save atual do jogo.\n\n\
                     [y] confirmar   [qualquer outra tecla] cancelar"
                ),
            );
        }
        Mode::Info(message) => {
            draw_message_popup(frame, " PlaySync ", &format!("{message}\n\n(qualquer tecla continua)"));
        }
    }
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
            "⚠ sem save detectado"
        } else {
            match g.sync_status {
                SyncStatus::NeverSynced => "nunca sincronizado",
                SyncStatus::Idle => "em dia",
                SyncStatus::Running => "sincronizando...",
                SyncStatus::Error => "erro",
            }
        };
        let last_backup = g
            .last_backup
            .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
            .unwrap_or_else(|| "-".to_string());
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

    let header = Row::new(vec!["Jogo", "Ultimo backup", "Status"])
        .style(Style::new().add_modifier(Modifier::BOLD));

    let table = Table::new(rows, widths).header(header).block(
        Block::default()
            .title(" PlaySync — [↑↓] navegar  [Enter] acoes  [s] sync tudo  [r] atualizar  [q] sair ")
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

fn draw_message_popup(frame: &mut Frame, title: &str, message: &str) {
    let area = centered_rect(60, 30, frame.area());
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
