//! Historico de backups, persistido em sqlite (`~/.local/share/playsync/history.db`).

use std::path::Path;
use std::sync::Mutex;

use anyhow::{Context, Result};
use chrono::{TimeZone, Utc};
use rusqlite::{params, Connection};

use crate::config::Config;
use crate::ipc::BackupEntry;

/// `rusqlite::Connection` usa `RefCell` internamente e por isso nao e `Sync`.
/// Um `Mutex` simples resolve, ja que as operacoes sao pontuais e rapidas
/// (sem isso, `Arc<HistoryDb>` nao poderia ser compartilhado entre tasks do tokio).
pub struct HistoryDb {
    conn: Mutex<Connection>,
}

impl HistoryDb {
    pub fn open_default() -> Result<Self> {
        let dir = Config::state_dir()?;
        std::fs::create_dir_all(&dir)?;
        Self::open(&dir.join("history.db"))
    }

    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)
            .with_context(|| format!("nao consegui abrir {}", path.display()))?;
        conn.execute(
            "CREATE TABLE IF NOT EXISTS backups (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                app_id      INTEGER NOT NULL,
                name        TEXT NOT NULL,
                timestamp   INTEGER NOT NULL,
                destination TEXT NOT NULL,
                success     INTEGER NOT NULL
            )",
            [],
        )?;
        Ok(Self { conn: Mutex::new(conn) })
    }

    pub fn record(&self, entry: &BackupEntry) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "INSERT INTO backups (app_id, name, timestamp, destination, success)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                entry.app_id,
                entry.name,
                entry.timestamp.timestamp(),
                entry.destination,
                entry.success as i64,
            ],
        )?;
        Ok(())
    }

    /// Ultima entrada bem-sucedida de cada AppID (usado no `playsync status`).
    pub fn last_backup(&self, app_id: u32) -> Result<Option<BackupEntry>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT app_id, name, timestamp, destination, success
             FROM backups WHERE app_id = ?1 ORDER BY timestamp DESC LIMIT 1",
        )?;
        let mut rows = stmt.query(params![app_id])?;
        if let Some(row) = rows.next()? {
            Ok(Some(row_to_entry(row)?))
        } else {
            Ok(None)
        }
    }

    pub fn recent(&self, limit: u32) -> Result<Vec<BackupEntry>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT app_id, name, timestamp, destination, success
             FROM backups ORDER BY timestamp DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit], |row| {
            Ok((
                row.get::<_, u32>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
            ))
        })?;

        let mut entries = Vec::new();
        for row in rows {
            let (app_id, name, timestamp, destination, success) = row?;
            entries.push(BackupEntry {
                app_id,
                name,
                timestamp: Utc.timestamp_opt(timestamp, 0).single().unwrap_or_else(Utc::now),
                destination,
                success: success != 0,
            });
        }
        Ok(entries)
    }
}

fn row_to_entry(row: &rusqlite::Row) -> Result<BackupEntry> {
    let timestamp: i64 = row.get(2)?;
    Ok(BackupEntry {
        app_id: row.get(0)?,
        name: row.get(1)?,
        timestamp: Utc.timestamp_opt(timestamp, 0).single().unwrap_or_else(Utc::now),
        destination: row.get(3)?,
        success: row.get::<_, i64>(4)? != 0,
    })
}
