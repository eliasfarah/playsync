//! TUI minimalista: uma unica tabela [Jogo | Ultimo backup | Status].
//!
//! `event::poll`/`event::read` sao chamadas sincronas do crossterm; bloqueiam a
//! thread do tokio por ate 250ms a cada iteracao do loop. Para uma ferramenta
//! interativa de uso pontual isso e um trade-off aceitavel (evita puxar
//! `crossterm/event-stream` + mais uma dependencia so pra isso); se o app
//! crescer para algo mais ao vivo, trocar por `EventStream` e a evolucao natural.

use std::time::Duration;

use anyhow::{bail, Result};
use playsync_core::ipc::{GameStatus, Request, Response, SyncStatus};
use ratatui::crossterm::event::{self, Event, KeyCode};
use ratatui::layout::Constraint;
use ratatui::style::{Modifier, Style};
use ratatui::widgets::{Block, Borders, Row, Table};
use ratatui::Frame;

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

pub async fn run() -> Result<()> {
    let mut games = fetch_status().await.unwrap_or_default();

    let mut terminal = ratatui::init();
    let mut ticks_since_refresh = 0u32;
    let result = loop {
        if let Err(err) = terminal.draw(|frame| draw(frame, &games)) {
            break Err(err.into());
        }

        match event::poll(POLL_INTERVAL) {
            Ok(true) => match event::read() {
                Ok(Event::Key(key)) => match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => break Ok(()),
                    KeyCode::Char('r') => {
                        games = fetch_status().await.unwrap_or_default();
                        ticks_since_refresh = 0;
                    }
                    KeyCode::Char('s') => {
                        // So dispara — `SyncNow` volta na hora, o sync roda em
                        // background no daemon. O refresh periodico abaixo
                        // (nao esta chamada) e quem mostra o progresso.
                        let _ = ipc_client::send(Request::SyncNow { app_id: None }).await;
                    }
                    _ => {}
                },
                Ok(_) => {}
                Err(err) => break Err(err.into()),
            },
            Ok(false) => {
                ticks_since_refresh += 1;
                if ticks_since_refresh >= AUTO_REFRESH_TICKS {
                    games = fetch_status().await.unwrap_or_default();
                    ticks_since_refresh = 0;
                }
            }
            Err(err) => break Err(err.into()),
        }
    };

    ratatui::restore();
    result
}

async fn fetch_status() -> Result<Vec<GameStatus>> {
    match ipc_client::send(Request::Status).await? {
        Response::Status { games } => Ok(games),
        Response::Error { message } => bail!(message),
        _ => bail!("resposta inesperada do daemon"),
    }
}

fn draw(frame: &mut Frame, games: &[GameStatus]) {
    let rows = games.iter().map(|g| {
        let status = match g.sync_status {
            SyncStatus::NeverSynced => "nunca sincronizado",
            SyncStatus::Idle => "em dia",
            SyncStatus::Running => "sincronizando...",
            SyncStatus::Error => "erro",
        };
        let last_backup = g
            .last_backup
            .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
            .unwrap_or_else(|| "-".to_string());
        Row::new(vec![g.name.clone(), last_backup, status.to_string()])
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
            .title(" PlaySync — [q] sair  [r] atualizar  [s] sincronizar tudo ")
            .borders(Borders::ALL),
    );

    frame.render_widget(table, frame.area());
}
