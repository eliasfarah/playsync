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
                id            INTEGER PRIMARY KEY AUTOINCREMENT,
                app_id        INTEGER NOT NULL,
                name          TEXT NOT NULL,
                timestamp     INTEGER NOT NULL,
                destination   TEXT NOT NULL,
                success       INTEGER NOT NULL,
                source_paths  TEXT NOT NULL DEFAULT '[]'
            )",
            [],
        )?;
        // Bancos de sessoes anteriores a essa coluna nao tem `source_paths`
        // (o `CREATE TABLE IF NOT EXISTS` acima so vale pra banco novo) —
        // migracao idempotente: ignora o erro "duplicate column" se ja rodou.
        match conn.execute("ALTER TABLE backups ADD COLUMN source_paths TEXT NOT NULL DEFAULT '[]'", []) {
            Ok(_) => {}
            Err(rusqlite::Error::SqliteFailure(_, Some(msg))) if msg.contains("duplicate column name") => {}
            Err(err) => return Err(err).context("falha ao migrar o schema de historico (source_paths)"),
        }
        Ok(Self { conn: Mutex::new(conn) })
    }

    pub fn record(&self, entry: &BackupEntry) -> Result<()> {
        let source_paths = serde_json::to_string(&entry.source_paths)
            .context("falha ao serializar source_paths")?;
        self.conn.lock().unwrap().execute(
            "INSERT INTO backups (app_id, name, timestamp, destination, success, source_paths)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                entry.app_id,
                entry.name,
                entry.timestamp.timestamp(),
                entry.destination,
                entry.success as i64,
                source_paths,
            ],
        )?;
        Ok(())
    }

    /// Ultima entrada bem-sucedida de cada AppID (usado no `playsync status`).
    pub fn last_backup(&self, app_id: u32) -> Result<Option<BackupEntry>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT app_id, name, timestamp, destination, success, source_paths
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
            "SELECT app_id, name, timestamp, destination, success, source_paths
             FROM backups ORDER BY timestamp DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit], |row| row_to_entry(row))?;

        let mut entries = Vec::new();
        for row in rows {
            entries.push(row?);
        }
        Ok(entries)
    }
}

fn row_to_entry(row: &rusqlite::Row) -> rusqlite::Result<BackupEntry> {
    let timestamp: i64 = row.get(2)?;
    let source_paths_json: String = row.get(5)?;
    let source_paths = serde_json::from_str(&source_paths_json).unwrap_or_default();
    Ok(BackupEntry {
        app_id: row.get(0)?,
        name: row.get(1)?,
        timestamp: Utc.timestamp_opt(timestamp, 0).single().unwrap_or_else(Utc::now),
        destination: row.get(3)?,
        success: row.get::<_, i64>(4)? != 0,
        source_paths,
    })
}
