//! SQLite-backed notes connected by named previous/next relations.

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;

pub const DEFAULT_FOCUS_LIMIT: usize = 50;
const SCHEMA_VERSION: i64 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Order {
    pub edge_id: i64,
    pub previous: String,
    pub next: String,
    pub edge_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddNoteResult {
    pub added: bool,
    pub note: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddOrderResult {
    pub added: bool,
    pub order: Order,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EdgeToNodeResult {
    pub removed_edge_id: i64,
    pub added_edge_ids: [i64; 2],
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Memory {
    pub memory_id: String,
    pub notes: Vec<String>,
    pub memos: HashMap<String, String>,
    pub orders: Vec<Order>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FocusView {
    pub memory_id: String,
    pub focus: Vec<Vec<String>>,
    pub named_groups: Vec<(String, Vec<Vec<String>>, Vec<Vec<String>>)>,
    pub memos: HashMap<String, String>,
    pub connections: Vec<Order>,
    pub limit: usize,
    pub returned_notes: usize,
    pub returned_connections: usize,
    pub notes_truncated: bool,
    pub connections_truncated: bool,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenameResult {
    pub from: String,
    pub to: String,
    pub changed: bool,
    pub merged: bool,
    pub rewired_orders: usize,
    pub deduplicated_orders: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClearResult {
    pub memory_id: String,
    pub deleted_notes: usize,
    pub deleted_orders: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeleteNodeResult {
    pub node_name: String,
    pub deleted_edges: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum MemoryError {
    #[error("memory_id must not be empty")]
    EmptyMemoryId,
    #[error("note must not be empty")]
    EmptyNote,
    #[error("edge_name must not be empty")]
    EmptyEdgeName,
    #[error("focus must contain at least one note")]
    EmptyFocus,
    #[error("duplicate focus note: {0}")]
    DuplicateFocus(String),
    #[error("focus contains {focus_notes} notes but limit is {limit}")]
    FocusExceedsLimit { focus_notes: usize, limit: usize },
    #[error("memory already exists: {0}")]
    DuplicateMemory(String),
    #[error("unknown memory: {0}")]
    UnknownMemory(String),
    #[error("unknown note in this memory: {0}")]
    UnknownNote(String),
    #[error("unknown edge: {0}")]
    UnknownEdge(i64),
    #[error(
        "existing database uses an incompatible mineplan schema; move or delete the database file before starting"
    )]
    IncompatibleDatabase,
    #[error(transparent)]
    Database(#[from] rusqlite::Error),
}

pub struct MemoryStore {
    connection: Connection,
}

impl MemoryStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, MemoryError> {
        let mut connection = Connection::open(path)?;
        connection.execute_batch("PRAGMA foreign_keys = ON;")?;
        migrate_schema(&mut connection)?;
        Ok(Self { connection })
    }

    pub fn create_memory(&mut self, memory_id: &str) -> Result<(), MemoryError> {
        validate_memory_id(memory_id)?;
        match self.connection.execute(
            "INSERT INTO ordered_memories (memory_id) VALUES (?1)",
            params![memory_id],
        ) {
            Ok(_) => Ok(()),
            Err(rusqlite::Error::SqliteFailure(error, _))
                if error.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_PRIMARYKEY =>
            {
                Err(MemoryError::DuplicateMemory(memory_id.into()))
            }
            Err(error) => Err(error.into()),
        }
    }

    pub fn create_memory_if_missing(&mut self, memory_id: &str) -> Result<(), MemoryError> {
        validate_memory_id(memory_id)?;
        self.connection.execute(
            "INSERT OR IGNORE INTO ordered_memories (memory_id) VALUES (?1)",
            params![memory_id],
        )?;
        Ok(())
    }

    pub fn list_memory_ids(&self) -> Result<Vec<String>, MemoryError> {
        let mut statement = self
            .connection
            .prepare("SELECT memory_id FROM ordered_memories ORDER BY rowid")?;
        Ok(statement
            .query_map([], |row| row.get(0))?
            .collect::<Result<Vec<_>, _>>()?)
    }

    pub fn add_note(&mut self, memory_id: &str, note: &str) -> Result<AddNoteResult, MemoryError> {
        self.ensure_memory(memory_id)?;
        validate_note(note)?;
        let sequence = next_note_sequence(&self.connection, memory_id)?;
        let added = self.connection.execute(
            "INSERT OR IGNORE INTO ordered_notes (memory_id, content, memo, sequence) VALUES (?1, ?2, NULL, ?3)",
            params![memory_id, note, sequence],
        )? == 1;
        Ok(AddNoteResult {
            added,
            note: note.into(),
        })
    }

    pub fn add_note_with_memo(
        &mut self,
        memory_id: &str,
        note: &str,
        memo: &str,
    ) -> Result<AddNoteResult, MemoryError> {
        self.ensure_memory(memory_id)?;
        validate_note(note)?;
        let sequence = next_note_sequence(&self.connection, memory_id)?;
        let added = self.connection.execute(
            "INSERT OR IGNORE INTO ordered_notes (memory_id, content, memo, sequence) VALUES (?1, ?2, ?3, ?4)",
            params![memory_id, note, memo, sequence],
        )? == 1;
        Ok(AddNoteResult {
            added,
            note: note.into(),
        })
    }

    pub fn update_note_memo(
        &mut self,
        memory_id: &str,
        node_name: &str,
        memo: &str,
    ) -> Result<(), MemoryError> {
        self.ensure_memory(memory_id)?;
        self.ensure_note(memory_id, node_name)?;
        self.connection.execute(
            "UPDATE ordered_notes SET memo = ?1 WHERE memory_id = ?2 AND content = ?3",
            params![memo, memory_id, node_name],
        )?;
        Ok(())
    }

    pub fn delete_note(
        &mut self,
        memory_id: &str,
        node_name: &str,
    ) -> Result<DeleteNodeResult, MemoryError> {
        self.ensure_memory(memory_id)?;
        self.ensure_note(memory_id, node_name)?;
        let transaction = self.connection.transaction()?;
        let deleted_edges = transaction.execute(
            "DELETE FROM note_orders WHERE memory_id = ?1 AND (previous_note = ?2 OR next_note = ?2)",
            params![memory_id, node_name],
        )?;
        transaction.execute(
            "DELETE FROM ordered_notes WHERE memory_id = ?1 AND content = ?2",
            params![memory_id, node_name],
        )?;
        transaction.commit()?;
        Ok(DeleteNodeResult {
            node_name: node_name.into(),
            deleted_edges,
        })
    }

    /// Adds `previous -> next`, creating either note when absent.
    pub fn add_order(
        &mut self,
        memory_id: &str,
        previous: &str,
        next: &str,
        edge_name: &str,
    ) -> Result<AddOrderResult, MemoryError> {
        self.ensure_memory(memory_id)?;
        validate_note(previous)?;
        validate_note(next)?;
        validate_edge_name(edge_name)?;
        let transaction = self.connection.transaction()?;
        ensure_note(&transaction, memory_id, previous)?;
        ensure_note(&transaction, memory_id, next)?;
        let sequence: i64 = transaction.query_row(
            "SELECT COALESCE(MAX(sequence), 0) + 1 FROM note_orders WHERE memory_id = ?1",
            params![memory_id],
            |row| row.get(0),
        )?;
        transaction.execute(
            "INSERT INTO note_orders
             (memory_id, previous_note, next_note, edge_name, sequence)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![memory_id, previous, next, edge_name, sequence],
        )?;
        let edge_id = transaction.last_insert_rowid();
        transaction.commit()?;
        Ok(AddOrderResult {
            added: true,
            order: Order {
                edge_id,
                previous: previous.into(),
                next: next.into(),
                edge_name: edge_name.into(),
            },
        })
    }

    /// Changes a note's string identity. If `to` already exists, both nodes are merged.
    /// Orders are rewired and exact `(previous, next, edge_name)` duplicates are collapsed.
    pub fn rename_note(
        &mut self,
        memory_id: &str,
        from: &str,
        to: &str,
    ) -> Result<RenameResult, MemoryError> {
        self.ensure_memory(memory_id)?;
        validate_note(from)?;
        validate_note(to)?;
        self.ensure_note(memory_id, from)?;
        if from == to {
            return Ok(RenameResult {
                from: from.into(),
                to: to.into(),
                changed: false,
                merged: false,
                rewired_orders: 0,
                deduplicated_orders: 0,
            });
        }
        let memory = self.get_memory(memory_id)?;
        let merged = memory.notes.iter().any(|note| note == to);
        let from_memo = memory.memos.get(from).cloned().unwrap_or_default();
        let mut seen_notes = HashSet::new();
        let notes: Vec<String> = memory
            .notes
            .iter()
            .map(|note| {
                if note == from {
                    to.to_string()
                } else {
                    note.clone()
                }
            })
            .filter(|note| seen_notes.insert(note.clone()))
            .collect();
        let rewired_orders = memory
            .orders
            .iter()
            .filter(|order| order.previous == from || order.next == from)
            .count();
        let original_order_count = memory.orders.len();
        let orders: Vec<Order> = memory
            .orders
            .into_iter()
            .map(|order| Order {
                edge_id: order.edge_id,
                previous: if order.previous == from {
                    to.into()
                } else {
                    order.previous
                },
                next: if order.next == from {
                    to.into()
                } else {
                    order.next
                },
                edge_name: order.edge_name,
            })
            .collect();
        let deduplicated_orders = original_order_count - orders.len();
        let transaction = self.connection.transaction()?;
        transaction.execute(
            "DELETE FROM note_orders WHERE memory_id = ?1",
            params![memory_id],
        )?;
        transaction.execute(
            "DELETE FROM ordered_notes WHERE memory_id = ?1",
            params![memory_id],
        )?;
        for (index, note) in notes.iter().enumerate() {
            transaction.execute(
                "INSERT INTO ordered_notes (memory_id, content, memo, sequence) VALUES (?1, ?2, ?3, ?4)",
                params![
                    memory_id,
                    note,
                    if note == to && !merged {
                        Some(from_memo.as_str())
                    } else {
                        memory.memos.get(note).map(String::as_str)
                    },
                    index as i64 + 1
                ],
            )?;
        }
        for (index, order) in orders.iter().enumerate() {
            transaction.execute(
                "INSERT INTO note_orders
                 (edge_id, memory_id, previous_note, next_note, edge_name, sequence)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    order.edge_id,
                    memory_id,
                    order.previous,
                    order.next,
                    order.edge_name,
                    index as i64 + 1
                ],
            )?;
        }
        transaction.commit()?;
        Ok(RenameResult {
            from: from.into(),
            to: to.into(),
            changed: true,
            merged,
            rewired_orders,
            deduplicated_orders,
        })
    }

    pub fn get_memory(&self, memory_id: &str) -> Result<Memory, MemoryError> {
        self.ensure_memory(memory_id)?;
        Ok(Memory {
            memory_id: memory_id.into(),
            notes: self.load_notes(memory_id)?,
            memos: self.load_memos(memory_id)?,
            orders: self.load_orders(memory_id)?,
        })
    }

    pub fn update_order(
        &mut self,
        memory_id: &str,
        edge_id: i64,
        previous: Option<&str>,
        next: Option<&str>,
        edge_name: Option<&str>,
    ) -> Result<Order, MemoryError> {
        self.ensure_memory(memory_id)?;
        let current = self.load_order(memory_id, edge_id)?;
        let previous = previous.unwrap_or(&current.previous);
        let next = next.unwrap_or(&current.next);
        let edge_name = edge_name.unwrap_or(&current.edge_name);
        validate_note(previous)?;
        validate_note(next)?;
        validate_edge_name(edge_name)?;
        let transaction = self.connection.transaction()?;
        ensure_note(&transaction, memory_id, previous)?;
        ensure_note(&transaction, memory_id, next)?;
        transaction.execute(
            "UPDATE note_orders SET previous_note = ?1, next_note = ?2, edge_name = ?3 WHERE memory_id = ?4 AND edge_id = ?5",
            params![previous, next, edge_name, memory_id, edge_id],
        )?;
        transaction.commit()?;
        Ok(Order {
            edge_id,
            previous: previous.into(),
            next: next.into(),
            edge_name: edge_name.into(),
        })
    }

    pub fn delete_order(&mut self, memory_id: &str, edge_id: i64) -> Result<bool, MemoryError> {
        self.ensure_memory(memory_id)?;
        Ok(self.connection.execute(
            "DELETE FROM note_orders WHERE memory_id = ?1 AND edge_id = ?2",
            params![memory_id, edge_id],
        )? == 1)
    }

    /// Replaces one edge with two edges of the same name through `node_name`.
    pub fn edge_to_node(
        &mut self,
        memory_id: &str,
        edge_id: i64,
        node_name: &str,
    ) -> Result<EdgeToNodeResult, MemoryError> {
        self.ensure_memory(memory_id)?;
        validate_note(node_name)?;
        let current = self.load_order(memory_id, edge_id)?;
        let transaction = self.connection.transaction()?;
        ensure_note(&transaction, memory_id, node_name)?;
        transaction.execute(
            "DELETE FROM note_orders WHERE memory_id = ?1 AND edge_id = ?2",
            params![memory_id, edge_id],
        )?;
        let mut added_edge_ids = [0; 2];
        for (index, (previous, next)) in [
            (current.previous.as_str(), node_name),
            (node_name, current.next.as_str()),
        ]
        .into_iter()
        .enumerate()
        {
            let sequence: i64 = transaction.query_row(
                "SELECT COALESCE(MAX(sequence), 0) + 1 FROM note_orders WHERE memory_id = ?1",
                params![memory_id],
                |row| row.get(0),
            )?;
            transaction.execute(
                "INSERT INTO note_orders
                 (memory_id, previous_note, next_note, edge_name, sequence)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![memory_id, previous, next, current.edge_name, sequence],
            )?;
            added_edge_ids[index] = transaction.last_insert_rowid();
        }
        transaction.commit()?;
        Ok(EdgeToNodeResult {
            removed_edge_id: edge_id,
            added_edge_ids,
        })
    }

    /// Builds a bounded local graph around one or more notes, then classifies its SCCs.
    /// `limit` counts every unique returned note, including explicit focus notes.
    pub fn focus(
        &self,
        memory_id: &str,
        focus: &[String],
        limit: usize,
    ) -> Result<FocusView, MemoryError> {
        self.ensure_memory(memory_id)?;
        if focus.is_empty() {
            return Err(MemoryError::EmptyFocus);
        }
        let mut focus_set = HashSet::new();
        for note in focus {
            if !focus_set.insert(note.as_str()) {
                return Err(MemoryError::DuplicateFocus(note.clone()));
            }
            self.ensure_note(memory_id, note)?;
        }
        if focus.len() > limit {
            return Err(MemoryError::FocusExceedsLimit {
                focus_notes: focus.len(),
                limit,
            });
        }
        let memory = self.get_memory(memory_id)?;
        Ok(analyze_local_focus(memory, focus, limit))
    }

    pub fn clear_memory(&mut self, memory_id: &str) -> Result<ClearResult, MemoryError> {
        self.ensure_memory(memory_id)?;
        let transaction = self.connection.transaction()?;
        let deleted_orders = transaction.execute(
            "DELETE FROM note_orders WHERE memory_id = ?1",
            params![memory_id],
        )?;
        let deleted_notes = transaction.execute(
            "DELETE FROM ordered_notes WHERE memory_id = ?1",
            params![memory_id],
        )?;
        transaction.commit()?;
        Ok(ClearResult {
            memory_id: memory_id.into(),
            deleted_notes,
            deleted_orders,
        })
    }

    fn ensure_memory(&self, memory_id: &str) -> Result<(), MemoryError> {
        validate_memory_id(memory_id)?;
        let exists = self
            .connection
            .query_row(
                "SELECT 1 FROM ordered_memories WHERE memory_id = ?1",
                params![memory_id],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if exists {
            Ok(())
        } else {
            Err(MemoryError::UnknownMemory(memory_id.into()))
        }
    }

    fn ensure_note(&self, memory_id: &str, note: &str) -> Result<(), MemoryError> {
        validate_note(note)?;
        let exists = self
            .connection
            .query_row(
                "SELECT 1 FROM ordered_notes WHERE memory_id = ?1 AND content = ?2",
                params![memory_id, note],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if exists {
            Ok(())
        } else {
            Err(MemoryError::UnknownNote(note.into()))
        }
    }

    fn load_notes(&self, memory_id: &str) -> Result<Vec<String>, MemoryError> {
        let mut statement = self
            .connection
            .prepare("SELECT content FROM ordered_notes WHERE memory_id = ?1 ORDER BY sequence")?;
        Ok(statement
            .query_map(params![memory_id], |row| row.get(0))?
            .collect::<Result<Vec<_>, _>>()?)
    }

    fn load_memos(&self, memory_id: &str) -> Result<HashMap<String, String>, MemoryError> {
        let mut statement = self.connection.prepare(
            "SELECT content, memo FROM ordered_notes WHERE memory_id = ?1 AND memo IS NOT NULL",
        )?;
        Ok(statement
            .query_map(params![memory_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<Result<HashMap<_, _>, _>>()?)
    }

    fn load_orders(&self, memory_id: &str) -> Result<Vec<Order>, MemoryError> {
        let mut statement = self.connection.prepare(
            "SELECT note_orders.edge_id, note_orders.previous_note, note_orders.next_note, note_orders.edge_name
             FROM note_orders
             JOIN ordered_notes AS previous_nodes
               ON previous_nodes.memory_id = note_orders.memory_id AND previous_nodes.content = note_orders.previous_note
             JOIN ordered_notes AS next_nodes
               ON next_nodes.memory_id = note_orders.memory_id AND next_nodes.content = note_orders.next_note
             WHERE note_orders.memory_id = ?1
             ORDER BY note_orders.sequence",
        )?;
        Ok(statement
            .query_map(params![memory_id], |row| {
                Ok(Order {
                    edge_id: row.get(0)?,
                    previous: row.get(1)?,
                    next: row.get(2)?,
                    edge_name: row.get(3)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?)
    }

    fn load_order(&self, memory_id: &str, edge_id: i64) -> Result<Order, MemoryError> {
        self.connection.query_row(
            "SELECT edge_id, previous_note, next_note, edge_name FROM note_orders WHERE memory_id = ?1 AND edge_id = ?2",
            params![memory_id, edge_id], |row| Ok(Order { edge_id: row.get(0)?, previous: row.get(1)?, next: row.get(2)?, edge_name: row.get(3)? }))
            .optional()?.ok_or(MemoryError::UnknownEdge(edge_id))
    }
}

fn migrate_schema(connection: &mut Connection) -> Result<(), MemoryError> {
    if table_exists(connection, "mineplan_schema")? {
        let version: i64 =
            connection.query_row("SELECT version FROM mineplan_schema LIMIT 1", [], |row| {
                row.get(0)
            })?;
        return if version == SCHEMA_VERSION {
            Ok(())
        } else {
            Err(MemoryError::IncompatibleDatabase)
        };
    }
    let has_old_schema = [
        "ordered_memories",
        "ordered_notes",
        "note_orders",
        "ordered_memory_schema_migrations",
    ]
    .into_iter()
    .any(|table| table_exists(connection, table).unwrap_or(false));
    if has_old_schema {
        return Err(MemoryError::IncompatibleDatabase);
    }
    connection.execute_batch(
        "CREATE TABLE mineplan_schema (
            version INTEGER PRIMARY KEY
        );
        INSERT INTO mineplan_schema (version) VALUES (1);
        CREATE TABLE ordered_memories (
            memory_id TEXT PRIMARY KEY
        );
        CREATE TABLE ordered_notes (
            memory_id TEXT NOT NULL REFERENCES ordered_memories(memory_id),
            content TEXT NOT NULL,
            memo TEXT,
            sequence INTEGER NOT NULL,
            PRIMARY KEY (memory_id, content),
            UNIQUE (memory_id, sequence)
        );
        CREATE TABLE note_orders (
            edge_id INTEGER PRIMARY KEY AUTOINCREMENT,
            memory_id TEXT NOT NULL REFERENCES ordered_memories(memory_id),
            previous_note TEXT NOT NULL,
            next_note TEXT NOT NULL,
            edge_name TEXT NOT NULL,
            sequence INTEGER NOT NULL,
            UNIQUE (memory_id, sequence),
            FOREIGN KEY (memory_id, previous_note) REFERENCES ordered_notes(memory_id, content),
            FOREIGN KEY (memory_id, next_note) REFERENCES ordered_notes(memory_id, content)
        );",
    )?;
    Ok(())
}

fn table_exists(connection: &Connection, table: &str) -> Result<bool, MemoryError> {
    Ok(connection
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
            params![table],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

fn next_note_sequence(connection: &Connection, memory_id: &str) -> Result<i64, MemoryError> {
    Ok(connection.query_row(
        "SELECT COALESCE(MAX(sequence), 0) + 1 FROM ordered_notes WHERE memory_id = ?1",
        params![memory_id],
        |row| row.get(0),
    )?)
}

fn ensure_note(
    transaction: &rusqlite::Transaction<'_>,
    memory_id: &str,
    note: &str,
) -> Result<(), MemoryError> {
    let sequence: i64 = transaction.query_row(
        "SELECT COALESCE(MAX(sequence), 0) + 1 FROM ordered_notes WHERE memory_id = ?1",
        params![memory_id],
        |row| row.get(0),
    )?;
    transaction.execute(
        "INSERT OR IGNORE INTO ordered_notes (memory_id, content, sequence) VALUES (?1, ?2, ?3)",
        params![memory_id, note, sequence],
    )?;
    Ok(())
}

fn analyze_local_focus(memory: Memory, focus: &[String], limit: usize) -> FocusView {
    let edge_name_rank: HashMap<String, usize> = memory
        .orders
        .iter()
        .enumerate()
        .map(|(index, edge)| (edge.edge_name.clone(), index))
        .collect();
    let (forward, reverse) = adjacency(&memory.orders);
    let (selected, named_nodes) = typed_bounded_selection(focus, limit, &memory.orders);
    let selected_set: HashSet<&str> = selected.iter().map(String::as_str).collect();
    let memos = memory
        .memos
        .into_iter()
        .filter(|(node, _)| selected_set.contains(node.as_str()))
        .collect();
    let notes_truncated = selected.iter().any(|note| {
        forward
            .get(note)
            .into_iter()
            .flatten()
            .chain(reverse.get(note).into_iter().flatten())
            .any(|neighbor| !selected_set.contains(neighbor.as_str()))
    });
    let all_connections: Vec<Order> = memory
        .orders
        .into_iter()
        .filter(|order| {
            selected_set.contains(order.previous.as_str())
                && selected_set.contains(order.next.as_str())
        })
        .collect();
    let all_connection_count = all_connections.len();
    let connections_truncated = all_connection_count > limit;
    let connections: Vec<Order> = all_connections.into_iter().take(limit).collect();
    let local_edges: Vec<(String, String)> =
        connections_for_analysis(&memory.notes, &selected_set, &forward);
    let components = strongly_connected_components(&selected, &local_edges);
    let component_of: HashMap<&str, usize> = components
        .iter()
        .enumerate()
        .flat_map(|(component, notes)| notes.iter().map(move |note| (note.as_str(), component)))
        .collect();
    let (component_forward, component_reverse) = component_adjacency(&local_edges, &component_of);
    let focus_components: Vec<usize> = focus.iter().map(|note| component_of[note.as_str()]).fold(
        Vec::new(),
        |mut result, component| {
            if !result.contains(&component) {
                result.push(component);
            }
            result
        },
    );
    let previous_distance = component_distances(&focus_components, &component_reverse);
    let next_distance = component_distances(&focus_components, &component_forward);
    let rank: Vec<usize> = components
        .iter()
        .map(|component| {
            component
                .iter()
                .filter_map(|note| selected.iter().position(|selected| selected == note))
                .min()
                .expect("component is non-empty")
        })
        .collect();
    let mut previous_ids = Vec::new();
    let mut focus_ids = Vec::new();
    let mut next_ids = Vec::new();
    for component in 0..components.len() {
        let reaches_focus = previous_distance.contains_key(&component);
        let reached_from_focus = next_distance.contains_key(&component);
        match (reaches_focus, reached_from_focus) {
            (true, true) => focus_ids.push(component),
            (true, false) => previous_ids.push(component),
            (false, true) => next_ids.push(component),
            (false, false) => {}
        }
    }
    previous_ids.sort_by_key(|component| {
        (
            std::cmp::Reverse(previous_distance[component]),
            rank[*component],
        )
    });
    focus_ids.sort_by_key(|component| rank[*component]);
    next_ids.sort_by_key(|component| (next_distance[component], rank[*component]));
    let groups = |ids: Vec<usize>| {
        ids.into_iter()
            .map(|component| components[component].clone())
            .collect()
    };
    let mut named_groups: Vec<_> = named_nodes
        .into_iter()
        .map(|(edge_name, directional_nodes)| {
            let mut previous_ids: Vec<usize> = components
                .iter()
                .enumerate()
                .filter(|(_, component)| {
                    component
                        .iter()
                        .any(|note| directional_nodes.previous.contains(note))
                })
                .map(|(id, _)| id)
                .collect();
            let mut next_ids: Vec<usize> = components
                .iter()
                .enumerate()
                .filter(|(_, component)| {
                    component
                        .iter()
                        .any(|note| directional_nodes.next.contains(note))
                })
                .map(|(id, _)| id)
                .collect();
            previous_ids.sort_by_key(|component| {
                (
                    std::cmp::Reverse(previous_distance.get(component).copied().unwrap_or(0)),
                    rank[*component],
                )
            });
            next_ids.sort_by_key(|component| {
                (
                    next_distance.get(component).copied().unwrap_or(0),
                    rank[*component],
                )
            });
            (edge_name, groups(previous_ids), groups(next_ids))
        })
        .collect();
    named_groups.sort_by_key(|(edge_name, _, _)| {
        edge_name_rank
            .get(edge_name.as_str())
            .copied()
            .unwrap_or(usize::MAX)
    });
    FocusView {
        memory_id: memory.memory_id,
        focus: groups(focus_ids),
        named_groups,
        memos,
        connections,
        limit,
        returned_notes: selected.len(),
        returned_connections: usize::min(limit, all_connection_count),
        notes_truncated,
        connections_truncated,
        truncated: notes_truncated || connections_truncated,
    }
}

#[derive(Default)]
struct DirectionalNodes {
    previous: HashSet<String>,
    next: HashSet<String>,
}

#[derive(Clone, Copy, Hash, PartialEq, Eq)]
enum TraversalDirection {
    Previous,
    Next,
}

fn typed_bounded_selection(
    focus: &[String],
    limit: usize,
    orders: &[Order],
) -> (Vec<String>, HashMap<String, DirectionalNodes>) {
    let mut selected = focus.to_vec();
    let mut selected_set: HashSet<String> = focus.iter().cloned().collect();
    let mut queue = VecDeque::new();
    let mut groups: HashMap<String, DirectionalNodes> = HashMap::new();
    let mut seen: HashSet<(String, String, TraversalDirection)> = HashSet::new();
    for note in focus {
        for order in orders.iter().filter(|order| order.previous == *note) {
            queue.push_back((
                order.next.clone(),
                order.edge_name.clone(),
                TraversalDirection::Next,
            ));
        }
        for order in orders.iter().filter(|order| order.next == *note) {
            queue.push_back((
                order.previous.clone(),
                order.edge_name.clone(),
                TraversalDirection::Previous,
            ));
        }
    }
    while let Some((note, edge_name, direction)) = queue.pop_front() {
        if !seen.insert((note.clone(), edge_name.clone(), direction)) {
            continue;
        }
        if !selected_set.contains(&note) {
            if selected.len() == limit {
                continue;
            }
            selected_set.insert(note.clone());
            selected.push(note.clone());
        }
        let group = groups.entry(edge_name.clone()).or_default();
        match direction {
            TraversalDirection::Previous => group.previous.insert(note.clone()),
            TraversalDirection::Next => group.next.insert(note.clone()),
        };
        for order in orders.iter().filter(|order| order.edge_name == edge_name) {
            match direction {
                TraversalDirection::Previous if order.next == note => {
                    queue.push_back((order.previous.clone(), edge_name.clone(), direction))
                }
                TraversalDirection::Next if order.previous == note => {
                    queue.push_back((order.next.clone(), edge_name.clone(), direction))
                }
                _ => {}
            }
        }
    }
    (selected, groups)
}

fn adjacency(orders: &[Order]) -> (HashMap<String, Vec<String>>, HashMap<String, Vec<String>>) {
    let mut forward: HashMap<String, Vec<String>> = HashMap::new();
    let mut reverse: HashMap<String, Vec<String>> = HashMap::new();
    for order in orders {
        let next = forward.entry(order.previous.clone()).or_default();
        if !next.contains(&order.next) {
            next.push(order.next.clone());
        }
        let previous = reverse.entry(order.next.clone()).or_default();
        if !previous.contains(&order.previous) {
            previous.push(order.previous.clone());
        }
    }
    (forward, reverse)
}

fn connections_for_analysis(
    note_order: &[String],
    selected: &HashSet<&str>,
    forward: &HashMap<String, Vec<String>>,
) -> Vec<(String, String)> {
    let rank: HashMap<&str, usize> = note_order
        .iter()
        .enumerate()
        .map(|(index, note)| (note.as_str(), index))
        .collect();
    let mut edges = Vec::new();
    for previous in note_order
        .iter()
        .filter(|note| selected.contains(note.as_str()))
    {
        for next in forward.get(previous).into_iter().flatten() {
            if selected.contains(next.as_str()) {
                edges.push((previous.clone(), next.clone()));
            }
        }
    }
    edges.sort_by_key(|(previous, next)| (rank[previous.as_str()], rank[next.as_str()]));
    edges
}

fn strongly_connected_components(
    selected: &[String],
    edges: &[(String, String)],
) -> Vec<Vec<String>> {
    let index: HashMap<&str, usize> = selected
        .iter()
        .enumerate()
        .map(|(index, note)| (note.as_str(), index))
        .collect();
    let mut forward = vec![Vec::new(); selected.len()];
    let mut reverse = vec![Vec::new(); selected.len()];
    for (previous, next) in edges {
        let from = index[previous.as_str()];
        let to = index[next.as_str()];
        if !forward[from].contains(&to) {
            forward[from].push(to);
            reverse[to].push(from);
        }
    }
    fn finish(node: usize, graph: &[Vec<usize>], seen: &mut [bool], order: &mut Vec<usize>) {
        if seen[node] {
            return;
        }
        seen[node] = true;
        for &neighbor in &graph[node] {
            finish(neighbor, graph, seen, order);
        }
        order.push(node);
    }
    fn assign(node: usize, graph: &[Vec<usize>], component: usize, result: &mut [usize]) {
        if result[node] != usize::MAX {
            return;
        }
        result[node] = component;
        for &neighbor in &graph[node] {
            assign(neighbor, graph, component, result);
        }
    }
    let mut seen = vec![false; selected.len()];
    let mut order = Vec::new();
    for node in 0..selected.len() {
        finish(node, &forward, &mut seen, &mut order);
    }
    let mut component_of = vec![usize::MAX; selected.len()];
    let mut component_count = 0;
    for &node in order.iter().rev() {
        if component_of[node] == usize::MAX {
            assign(node, &reverse, component_count, &mut component_of);
            component_count += 1;
        }
    }
    let mut components = vec![Vec::new(); component_count];
    for (node, note) in selected.iter().enumerate() {
        components[component_of[node]].push(note.clone());
    }
    components
}

fn component_adjacency(
    edges: &[(String, String)],
    component_of: &HashMap<&str, usize>,
) -> (HashMap<usize, Vec<usize>>, HashMap<usize, Vec<usize>>) {
    let mut forward: HashMap<usize, Vec<usize>> = HashMap::new();
    let mut reverse: HashMap<usize, Vec<usize>> = HashMap::new();
    for (previous, next) in edges {
        let from = component_of[previous.as_str()];
        let to = component_of[next.as_str()];
        if from == to {
            continue;
        }
        let outgoing = forward.entry(from).or_default();
        if !outgoing.contains(&to) {
            outgoing.push(to);
            reverse.entry(to).or_default().push(from);
        }
    }
    (forward, reverse)
}

fn component_distances(
    focus: &[usize],
    neighbors: &HashMap<usize, Vec<usize>>,
) -> HashMap<usize, usize> {
    let mut distances = HashMap::new();
    let mut queue = VecDeque::new();
    for &component in focus {
        distances.insert(component, 0);
        queue.push_back(component);
    }
    while let Some(component) = queue.pop_front() {
        let distance = distances[&component] + 1;
        for &neighbor in neighbors.get(&component).into_iter().flatten() {
            if let std::collections::hash_map::Entry::Vacant(entry) = distances.entry(neighbor) {
                entry.insert(distance);
                queue.push_back(neighbor);
            }
        }
    }
    distances
}

fn validate_memory_id(memory_id: &str) -> Result<(), MemoryError> {
    if memory_id.trim().is_empty() {
        Err(MemoryError::EmptyMemoryId)
    } else {
        Ok(())
    }
}

fn validate_note(note: &str) -> Result<(), MemoryError> {
    if note.trim().is_empty() {
        Err(MemoryError::EmptyNote)
    } else {
        Ok(())
    }
}

fn validate_edge_name(edge_name: &str) -> Result<(), MemoryError> {
    if edge_name.trim().is_empty() {
        Err(MemoryError::EmptyEdgeName)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> MemoryStore {
        let mut store = MemoryStore::open(":memory:").unwrap();
        store.create_memory("minecraft").unwrap();
        store
    }

    #[test]
    fn exact_duplicate_edges_are_preserved_with_distinct_ids() {
        let mut store = store();
        let first = store
            .add_order("minecraft", "木を集める", "板材を作る", "材料の順序")
            .unwrap();
        let duplicate = store
            .add_order("minecraft", "木を集める", "板材を作る", "材料の順序")
            .unwrap();
        let another = store
            .add_order("minecraft", "木を集める", "板材を作る", "作業の順序")
            .unwrap();
        assert!(first.added);
        assert!(duplicate.added);
        assert_ne!(first.order.edge_id, duplicate.order.edge_id);
        assert!(another.added);
        assert_ne!(first.order.edge_name, another.order.edge_name);
        assert_eq!(store.get_memory("minecraft").unwrap().orders.len(), 3);
    }

    #[test]
    fn edge_id_updates_and_deletes_one_edge_without_touching_nodes() {
        let mut store = store();
        let added = store.add_order("minecraft", "A", "B", "関係").unwrap();
        let edge_id = added.order.edge_id;
        let updated = store
            .update_order("minecraft", edge_id, None, Some("C"), Some("更新"))
            .unwrap();
        assert_eq!(updated.edge_id, edge_id);
        assert_eq!(updated.next, "C");
        assert_eq!(updated.edge_name, "更新");
        assert!(store.delete_order("minecraft", edge_id).unwrap());
        assert!(!store.delete_order("minecraft", edge_id).unwrap());
        assert_eq!(
            store.get_memory("minecraft").unwrap().notes,
            ["A", "B", "C"]
        );
        assert!(store.get_memory("minecraft").unwrap().orders.is_empty());
    }

    #[test]
    fn edge_to_node_replaces_only_the_selected_edge_atomically() {
        let mut store = store();
        let selected = store.add_order("minecraft", "A", "B", "task").unwrap();
        let duplicate = store.add_order("minecraft", "A", "B", "task").unwrap();
        store
            .add_note_with_memo("minecraft", "X", "existing memo")
            .unwrap();

        let result = store
            .edge_to_node("minecraft", selected.order.edge_id, "X")
            .unwrap();
        assert_eq!(result.removed_edge_id, selected.order.edge_id);
        assert_ne!(result.added_edge_ids[0], result.added_edge_ids[1]);

        let memory = store.get_memory("minecraft").unwrap();
        assert!(memory.notes.contains(&"X".into()));
        assert_eq!(memory.memos["X"], "existing memo");
        assert_eq!(memory.orders.len(), 3);
        assert!(
            memory
                .orders
                .iter()
                .any(|edge| edge.edge_id == duplicate.order.edge_id)
        );
        assert!(
            memory.orders.iter().any(|edge| {
                edge.previous == "A" && edge.next == "X" && edge.edge_name == "task"
            })
        );
        assert!(
            memory.orders.iter().any(|edge| {
                edge.previous == "X" && edge.next == "B" && edge.edge_name == "task"
            })
        );
        assert!(matches!(
            store.edge_to_node("minecraft", selected.order.edge_id, "Y"),
            Err(MemoryError::UnknownEdge(id)) if id == selected.order.edge_id
        ));
    }

    #[test]
    fn deleting_node_removes_node_and_incident_edges() {
        let mut store = store();
        store.add_order("minecraft", "A", "B", "関係").unwrap();
        let deleted = store.delete_note("minecraft", "B").unwrap();
        assert_eq!(deleted.deleted_edges, 1);
        let visible = store.focus("minecraft", &["A".into()], 50).unwrap();
        assert!(visible.named_groups.is_empty());
        assert!(visible.connections.is_empty());
        store.add_note("minecraft", "B").unwrap();
        let fresh = store.focus("minecraft", &["A".into()], 50).unwrap();
        assert!(fresh.named_groups.is_empty());
    }

    #[test]
    fn cycles_become_local_sccs_and_can_be_cut_by_limit() {
        let mut store = store();
        store.add_order("minecraft", "A", "B", "AB").unwrap();
        store.add_order("minecraft", "B", "C", "BC").unwrap();
        store.add_order("minecraft", "C", "A", "CA").unwrap();
        let complete = store.focus("minecraft", &["A".into()], 3).unwrap();
        assert_eq!(complete.focus, [vec!["A", "B", "C"]]);
        assert!(!complete.notes_truncated);
        let partial = store.focus("minecraft", &["A".into()], 2).unwrap();
        assert_eq!(partial.focus, [vec!["A"]]);
        assert!(partial.notes_truncated);
    }

    #[test]
    fn one_scc_can_appear_in_both_directions_of_the_same_edge_name() {
        let mut store = store();
        store.add_order("minecraft", "A", "B", "task").unwrap();
        store.add_order("minecraft", "B", "C", "task").unwrap();
        store.add_order("minecraft", "C", "A", "task").unwrap();

        let view = store.focus("minecraft", &["A".into()], 50).unwrap();
        let (edge_name, previous, next) = &view.named_groups[0];
        assert_eq!(edge_name, "task");
        assert_eq!(previous.len(), 1);
        assert_eq!(next.len(), 1);
        assert_eq!(previous[0], next[0]);
        assert_eq!(previous[0].len(), 3);
        assert!(
            previous[0]
                .iter()
                .all(|node| ["A", "B", "C"].contains(&node.as_str()))
        );
    }

    #[test]
    fn bidirectional_orders_are_independent_edges_in_one_scc() {
        let mut store = store();
        store
            .add_order("minecraft", "A", "B", "AからBを見る理由")
            .unwrap();
        store
            .add_order("minecraft", "B", "A", "BからAを見る理由")
            .unwrap();

        let view = store.focus("minecraft", &["A".into()], 50).unwrap();
        assert_eq!(view.focus, [vec!["A", "B"]]);
        assert_eq!(view.named_groups.len(), 2);
        assert_eq!(view.connections.len(), 2);
        assert_eq!(view.connections[0].edge_name, "AからBを見る理由");
        assert_eq!(view.connections[1].edge_name, "BからAを見る理由");
    }

    #[test]
    fn focus_returns_directional_scc_groups_in_order() {
        let mut store = store();
        store.add_order("minecraft", "木", "板材", "加工").unwrap();
        store.add_order("minecraft", "板材", "棒", "加工").unwrap();
        store
            .add_order("minecraft", "棒", "つるはし", "材料")
            .unwrap();
        store
            .add_order("minecraft", "つるはし", "採掘", "使用")
            .unwrap();
        let view = store.focus("minecraft", &["棒".into()], 50).unwrap();
        assert_eq!(view.focus, [vec!["棒"]]);
        assert_eq!(view.named_groups.len(), 2);
        assert_eq!(view.connections.len(), 3);
        assert_eq!(view.returned_notes, 4);
        assert!(view.truncated);
    }

    #[test]
    fn multiple_focus_notes_are_not_repeated_in_surroundings() {
        let mut store = store();
        store.add_order("minecraft", "A", "B", "AB").unwrap();
        store.add_order("minecraft", "B", "C", "BC").unwrap();
        store.add_order("minecraft", "C", "D", "CD").unwrap();
        let view = store
            .focus("minecraft", &["B".into(), "C".into()], 50)
            .unwrap();
        assert_eq!(view.focus, [vec!["B"], vec!["C"]]);
        assert_eq!(view.named_groups.len(), 3);
    }

    #[test]
    fn disconnected_notes_do_not_appear_around_focus() {
        let mut store = store();
        store.add_order("minecraft", "A", "B", "AB").unwrap();
        store.add_note("minecraft", "unrelated").unwrap();
        let view = store.focus("minecraft", &["B".into()], 50).unwrap();
        assert_eq!(view.named_groups.len(), 1);
        assert_eq!(view.named_groups[0].1, [vec!["A"]]);
    }

    #[test]
    fn focus_limit_counts_focus_notes_and_caps_connections_separately() {
        let mut store = store();
        store.add_order("minecraft", "A", "B", "AB").unwrap();
        store.add_order("minecraft", "B", "C", "BC").unwrap();
        store.add_order("minecraft", "C", "D", "CD").unwrap();
        store.add_order("minecraft", "C", "X", "CX").unwrap();
        let count_limited = store.focus("minecraft", &["C".into()], 3).unwrap();
        assert_eq!(count_limited.returned_notes, 3);
        assert!(count_limited.truncated);
        assert_eq!(count_limited.focus, [vec!["C"]]);
        store
            .add_order("minecraft", "B", "C", "second edge_name")
            .unwrap();
        store
            .add_order("minecraft", "B", "C", "third edge_name")
            .unwrap();
        let connection_limited = store.focus("minecraft", &["C".into()], 2).unwrap();
        assert_eq!(connection_limited.returned_connections, 1);
        assert!(!connection_limited.connections_truncated);
    }

    #[test]
    fn focus_continues_only_the_edge_name_used_to_arrive() {
        let mut store = store();
        store.add_order("minecraft", "A", "B", "previous").unwrap();
        store.add_order("minecraft", "B", "C", "previous").unwrap();
        store.add_order("minecraft", "B", "D", "depend_on").unwrap();
        store.add_order("minecraft", "A", "E", "depend_on").unwrap();
        store.add_order("minecraft", "E", "F", "depend_on").unwrap();

        let view = store.focus("minecraft", &["A".into()], 50).unwrap();
        let groups: HashMap<_, _> = view
            .named_groups
            .into_iter()
            .map(|(edge_name, previous, next)| (edge_name, (previous, next)))
            .collect();
        assert_eq!(groups["previous"].1, [vec!["B"], vec!["C"]]);
        assert_eq!(groups["depend_on"].1, [vec!["E"], vec!["F"]]);
        assert!(
            !groups["previous"]
                .1
                .iter()
                .flatten()
                .any(|node| node == "D")
        );
    }

    #[test]
    fn node_memos_are_saved_updated_and_returned_by_focus() {
        let mut store = store();
        store
            .add_note_with_memo("minecraft", "A", "最初のメモ")
            .unwrap();
        store
            .add_note_with_memo("minecraft", "B", "周辺のメモ")
            .unwrap();
        store.add_order("minecraft", "A", "B", "related").unwrap();
        store
            .update_note_memo("minecraft", "A", "更新したメモ")
            .unwrap();

        let view = store.focus("minecraft", &["A".into()], 50).unwrap();
        assert_eq!(view.memos["A"], "更新したメモ");
        assert_eq!(view.memos["B"], "周辺のメモ");
    }

    #[test]
    fn absent_and_empty_memos_are_distinct() {
        let mut store = store();
        store.add_note("minecraft", "未登録").unwrap();
        store.add_note_with_memo("minecraft", "空文字", "").unwrap();
        let view = store.focus("minecraft", &["未登録".into()], 50).unwrap();
        assert!(!view.memos.contains_key("未登録"));
        let view = store.focus("minecraft", &["空文字".into()], 50).unwrap();
        assert_eq!(view.memos.get("空文字"), Some(&String::new()));
    }

    #[test]
    fn rename_to_existing_note_merges_nodes_rewires_and_deduplicates_orders() {
        let mut store = store();
        store.add_order("minecraft", "X", "A", "incoming").unwrap();
        store.add_order("minecraft", "A", "B", "same").unwrap();
        store.add_order("minecraft", "B", "B", "same").unwrap();
        let result = store.rename_note("minecraft", "A", "B").unwrap();
        assert!(result.changed);
        assert!(result.merged);
        assert_eq!(result.rewired_orders, 2);
        assert_eq!(result.deduplicated_orders, 0);
        let memory = store.get_memory("minecraft").unwrap();
        assert_eq!(memory.notes, ["X", "B"]);
        assert_eq!(
            memory.orders,
            [
                Order {
                    edge_id: 1,
                    previous: "X".into(),
                    next: "B".into(),
                    edge_name: "incoming".into()
                },
                Order {
                    edge_id: 2,
                    previous: "B".into(),
                    next: "B".into(),
                    edge_name: "same".into(),
                },
                Order {
                    edge_id: 3,
                    previous: "B".into(),
                    next: "B".into(),
                    edge_name: "same".into()
                }
            ]
        );
    }

    #[test]
    fn existing_database_schema_is_rejected_without_migration() {
        let mut connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch("PRAGMA foreign_keys = ON;")
            .unwrap();
        connection
            .execute(
                "CREATE TABLE ordered_memories (memory_id TEXT PRIMARY KEY)",
                [],
            )
            .unwrap();
        assert!(matches!(
            migrate_schema(&mut connection),
            Err(MemoryError::IncompatibleDatabase)
        ));
    }

    #[test]
    fn current_database_schema_can_be_opened_again() {
        let mut connection = Connection::open_in_memory().unwrap();
        migrate_schema(&mut connection).unwrap();
        migrate_schema(&mut connection).unwrap();
    }
}
