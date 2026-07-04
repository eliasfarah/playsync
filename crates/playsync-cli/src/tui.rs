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

pub async fn run() -> Result<()> {
    let mut games = fetch_status().await.unwrap_or_default();

    let mut terminal = ratatui::init();
    let result = loop {
        if let Err(err) = terminal.draw(|frame| draw(frame, &games)) {
            break Err(err.into());
        }

        match event::poll(Duration::from_millis(250)) {
            Ok(true) => match event::read() {
                Ok(Event::Key(key)) => match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => break Ok(()),
                    KeyCode::Char('r') => games = fetch_status().await.unwrap_or_default(),
                    KeyCode::Char('s') => {
                        let _ = ipc_client::send(Request::SyncNow { app_id: None }).await;
                        games = fetch_status().await.unwrap_or_default();
                    }
                    _ => {}
                },
                Ok(_) => {}
                Err(err) => break Err(err.into()),
            },
            Ok(false) => {}
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
