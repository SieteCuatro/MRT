use anyhow::Result;
use chrono::{DateTime, Utc};
use mrt_core::CommandBlock;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::path::Path;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunbookStep {
    pub command: String,
    pub shell_name: String,
    pub cwd: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Runbook {
    pub id: Uuid,
    pub name: String,
    pub steps: Vec<RunbookStep>,
    pub updated_at: DateTime<Utc>,
}

pub struct HistoryStore(Connection);

/// A block as stored in history: without its styled spans, which hold a second copy
/// of the text plus per-span overhead; they are rebuilt from `raw_output` on load.
fn history_payload(block: &CommandBlock) -> Result<String> {
    let mut value = serde_json::to_value(block)?;
    value["ansi_spans"] = serde_json::Value::Array(Vec::new());
    Ok(serde_json::to_string(&value)?)
}

impl HistoryStore {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let connection = Connection::open(path)?;
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        connection.execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE IF NOT EXISTS blocks (
                 sequence INTEGER PRIMARY KEY,
                 id TEXT NOT NULL UNIQUE,
                 command TEXT NOT NULL,
                 output TEXT NOT NULL,
                 payload TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS runbooks (
                 id TEXT PRIMARY KEY,
                 name TEXT NOT NULL,
                 updated_at TEXT NOT NULL,
                 payload TEXT NOT NULL
             );",
        )?;
        let has_index: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name = 'block_search')",
            [],
            |row| row.get(0),
        )?;
        connection.execute_batch(
            "BEGIN IMMEDIATE;
             CREATE VIRTUAL TABLE IF NOT EXISTS block_search USING fts5(command, output, tokenize='trigram');
             CREATE TRIGGER IF NOT EXISTS blocks_insert AFTER INSERT ON blocks BEGIN
                 INSERT INTO block_search(rowid, command, output) VALUES (new.sequence, new.command, new.output);
             END;
             CREATE TRIGGER IF NOT EXISTS blocks_update AFTER UPDATE ON blocks BEGIN
                 DELETE FROM block_search WHERE rowid=old.sequence;
                 INSERT INTO block_search(rowid, command, output) VALUES (new.sequence, new.command, new.output);
             END;",
        )?;
        if !has_index {
            connection.execute_batch("INSERT OR REPLACE INTO block_search(rowid, command, output) SELECT sequence, command, output FROM blocks;")?;
        }
        connection.execute_batch("COMMIT;")?;
        Ok(Self(connection))
    }

    pub fn save(&self, block: &CommandBlock) -> Result<()> {
        self.0.execute(
            "INSERT INTO blocks (id, command, output, payload) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(id) DO UPDATE SET command=excluded.command,
                 output=excluded.output, payload=excluded.payload",
            params![
                block.id.to_string(),
                block.command,
                block.plain_output,
                history_payload(block)?
            ],
        )?;
        Ok(())
    }

    pub fn search(&self, query: &str) -> Result<Vec<CommandBlock>> {
        let indexed = query.chars().count() >= 3;
        let sql = if indexed {
            "SELECT payload FROM blocks WHERE sequence IN
             (SELECT rowid FROM block_search WHERE block_search MATCH ?1)
             ORDER BY sequence DESC LIMIT 200"
        } else {
            "SELECT payload FROM blocks
             WHERE instr(lower(command), lower(?1)) > 0 OR instr(lower(output), lower(?1)) > 0
             ORDER BY sequence DESC LIMIT 200"
        };
        let parameter = if indexed {
            format!("\"{}\"", query.replace('"', "\"\""))
        } else {
            query.to_string()
        };
        let mut statement = self.0.prepare(sql)?;
        let payloads = statement.query_map([parameter], |row| row.get::<_, String>(0))?;
        payloads
            .map(|payload| {
                let mut block: CommandBlock = serde_json::from_str(&payload?)?;
                // Spans are not stored (see `history_payload`); rebuild them.
                if block.ansi_spans.is_empty() && !block.raw_output.is_empty() {
                    block.refresh_output();
                }
                Ok(block)
            })
            .collect()
    }

    pub fn save_runbook(&self, runbook: &Runbook) -> Result<()> {
        self.0.execute(
            "INSERT INTO runbooks (id, name, updated_at, payload) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(id) DO UPDATE SET name=excluded.name,
                 updated_at=excluded.updated_at, payload=excluded.payload",
            params![
                runbook.id.to_string(),
                runbook.name,
                runbook.updated_at.to_rfc3339(),
                serde_json::to_string(runbook)?
            ],
        )?;
        Ok(())
    }

    pub fn list_runbooks(&self) -> Result<Vec<Runbook>> {
        let mut statement = self
            .0
            .prepare("SELECT payload FROM runbooks ORDER BY updated_at DESC, name ASC")?;
        let payloads = statement.query_map([], |row| row.get::<_, String>(0))?;
        payloads
            .map(|payload| Ok(serde_json::from_str(&payload?)?))
            .collect()
    }

    pub fn delete_runbook(&self, id: Uuid) -> Result<()> {
        self.0
            .execute("DELETE FROM runbooks WHERE id = ?1", [id.to_string()])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_stores_no_spans_and_rebuilds_them() {
        let path = std::env::temp_dir().join(format!("mrt-history-{}", uuid::Uuid::new_v4())).join("history.sqlite");
        let store = HistoryStore::open(&path).unwrap();
        let block = CommandBlock::new_completed("ls --color", "sh", "/", "[34mdir[0m file
", 0, 1);
        store.save(&block).unwrap();
        let payload: String = store.0.query_row("SELECT payload FROM blocks", [], |r| r.get(0)).unwrap();
        assert!(payload.contains("\"ansi_spans\":[]"), "{payload}");
        let found = store.search("file").unwrap();
        assert_eq!(found[0].ansi_spans, block.ansi_spans);
        assert_eq!(found[0].plain_output, block.plain_output);
        drop(store);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    #[ignore = "manual local performance measurement"]
    fn measure_large_history() {
        let path = std::env::temp_dir()
            .join(format!("mrt-benchmark-{}", uuid::Uuid::new_v4()))
            .join("history.sqlite");
        let mut writes = Vec::new();
        let mut searches = Vec::new();
        {
            let store = HistoryStore::open(&path).unwrap();
            for index in 0..10_000 {
                let block = CommandBlock::new_completed(
                    format!("echo record-{index}"),
                    "sh",
                    ".",
                    format!("{} record-{index}", "output ".repeat(150)),
                    0,
                    1,
                );
                let start = std::time::Instant::now();
                store.save(&block).unwrap();
                writes.push(start.elapsed().as_micros());
            }
            for query in ["record-9999", "no-match-at-all", "", "record-1"]
                .iter()
                .cycle()
                .take(100)
            {
                let start = std::time::Instant::now();
                let _ = store.search(query).unwrap();
                searches.push(start.elapsed().as_micros());
            }
        }
        writes.sort_unstable();
        searches.sort_unstable();
        println!("10,000 blocks, ~1KB output each, microseconds: write p50={} p95={}, search p50={} p95={}",
            writes[5000], writes[9500], searches[50], searches[95]);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn history_survives_restart_and_retries_and_searches_literal_text() {
        let path = std::env::temp_dir()
            .join(format!("mrt-history-{}", uuid::Uuid::new_v4()))
            .join("history.sqlite");
        let block = CommandBlock::new_completed("echo test", "sh", ".", "100% finished", 0, 1);
        {
            let store = HistoryStore::open(&path).unwrap();
            store.save(&block).unwrap();
            store.save(&block).unwrap();
            let second = HistoryStore::open(&path).unwrap();
            second
                .save(&CommandBlock::new_completed(
                    "echo next",
                    "sh",
                    ".",
                    "next",
                    0,
                    1,
                ))
                .unwrap();
        }
        {
            let store = HistoryStore::open(&path).unwrap();
            assert_eq!(store.search("").unwrap().len(), 2);
            assert_eq!(store.search("TEST").unwrap()[0].id, block.id);
            assert_eq!(store.search("%").unwrap().len(), 1);
            assert!(store.search("missing").unwrap().is_empty());
        }
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn runbooks_persist_update_and_delete_offline() {
        let path = std::env::temp_dir()
            .join(format!("mrt-runbooks-{}", Uuid::new_v4()))
            .join("history.sqlite");
        let runbook_id = Uuid::new_v4();
        let initial = Runbook {
            id: runbook_id,
            name: "Check this project".into(),
            steps: vec![
                RunbookStep {
                    command: "cargo fmt --check".into(),
                    shell_name: "pwsh".into(),
                    cwd: ".".into(),
                },
                RunbookStep {
                    command: "cargo test".into(),
                    shell_name: "pwsh".into(),
                    cwd: ".".into(),
                },
            ],
            updated_at: Utc::now(),
        };
        {
            let store = HistoryStore::open(&path).unwrap();
            store.save_runbook(&initial).unwrap();
        }
        {
            let store = HistoryStore::open(&path).unwrap();
            let loaded = store.list_runbooks().unwrap();
            assert_eq!(loaded, vec![initial.clone()]);

            let mut updated = initial;
            updated.name = "Check & Clippy".into();
            updated.steps.insert(
                1,
                RunbookStep {
                    command: "cargo clippy".into(),
                    shell_name: "pwsh".into(),
                    cwd: ".".into(),
                },
            );
            updated.updated_at = Utc::now();
            store.save_runbook(&updated).unwrap();

            let loaded = store.list_runbooks().unwrap();
            assert_eq!(loaded.len(), 1);
            assert_eq!(loaded[0].name, "Check & Clippy");
            assert_eq!(loaded[0].steps.len(), 3);

            store.delete_runbook(runbook_id).unwrap();
            assert!(store.list_runbooks().unwrap().is_empty());
        }
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
}

pub enum HistoryRequest {
    Save(CommandBlock),
    Search(String),
    SaveRunbook(Runbook),
    ListRunbooks,
    DeleteRunbook(Uuid),
}

pub enum HistoryResponse {
    Saved(Result<(), String>),
    Loaded(String, Result<Vec<CommandBlock>, String>),
    RunbookSaved(Result<(), String>),
    RunbooksLoaded(Result<Vec<Runbook>, String>),
    RunbookDeleted(Result<(), String>),
}

pub struct HistoryWorker {
    sender: Option<std::sync::mpsc::Sender<HistoryRequest>>,
    pub responses: std::sync::mpsc::Receiver<HistoryResponse>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl HistoryWorker {
    pub fn start(path: std::path::PathBuf) -> Self {
        let (sender, requests) = std::sync::mpsc::channel();
        let (results, responses) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            let store = HistoryStore::open(&path).map_err(|error| error.to_string());
            for request in requests {
                let response = match request {
                    HistoryRequest::Save(block) => {
                        HistoryResponse::Saved(store.as_ref().map_err(Clone::clone).and_then(
                            |store| store.save(&block).map_err(|error| error.to_string()),
                        ))
                    }
                    HistoryRequest::Search(query) => {
                        let result = store.as_ref().map_err(Clone::clone).and_then(|store| {
                            store.search(&query).map_err(|error| error.to_string())
                        });
                        HistoryResponse::Loaded(query, result)
                    }
                    HistoryRequest::SaveRunbook(runbook) => HistoryResponse::RunbookSaved(
                        store.as_ref().map_err(Clone::clone).and_then(|store| {
                            store.save_runbook(&runbook).map_err(|error| error.to_string())
                        }),
                    ),
                    HistoryRequest::ListRunbooks => HistoryResponse::RunbooksLoaded(
                        store
                            .as_ref()
                            .map_err(Clone::clone)
                            .and_then(|store| store.list_runbooks().map_err(|error| error.to_string())),
                    ),
                    HistoryRequest::DeleteRunbook(id) => HistoryResponse::RunbookDeleted(
                        store.as_ref().map_err(Clone::clone).and_then(|store| {
                            store.delete_runbook(id).map_err(|error| error.to_string())
                        }),
                    ),
                };
                if results.send(response).is_err() {
                    break;
                }
            }
        });
        Self {
            sender: Some(sender),
            responses,
            thread: Some(thread),
        }
    }

    pub fn send(&self, request: HistoryRequest) -> bool {
        self.sender
            .as_ref()
            .is_some_and(|sender| sender.send(request).is_ok())
    }
}

impl Drop for HistoryWorker {
    fn drop(&mut self) {
        self.sender.take();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
