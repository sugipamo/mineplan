//! SQLite-backed notes connected by acyclic before/after relations.

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;

pub const DEFAULT_FOCUS_LIMIT: usize = 50;
const CURRENT_SCHEMA_VERSION: i64 = 3;
const LEGACY_REASON: &str = "未登録";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Order {
    pub before: String,
    pub after: String,
    pub reason: String,
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
pub struct Memory {
    pub memory_id: String,
    pub notes: Vec<String>,
    pub orders: Vec<Order>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FocusView {
    pub memory_id: String,
    pub before: Vec<Vec<String>>,
    pub focus: Vec<Vec<String>>,
    pub after: Vec<Vec<String>>,
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

#[derive(Debug, thiserror::Error)]
pub enum MemoryError {
    #[error("memory_id must not be empty")]
    EmptyMemoryId,
    #[error("note must not be empty")]
    EmptyNote,
    #[error("reason must not be empty")]
    EmptyReason,
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
    #[error("database schema version {0} is newer than this program supports")]
    UnsupportedSchemaVersion(i64),
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
            "INSERT OR IGNORE INTO ordered_notes (memory_id, content, sequence) VALUES (?1, ?2, ?3)",
            params![memory_id, note, sequence],
        )? == 1;
        Ok(AddNoteResult {
            added,
            note: note.into(),
        })
    }

    /// Adds `before -> after`, creating either note when absent.
    pub fn add_order(
        &mut self,
        memory_id: &str,
        before: &str,
        after: &str,
        reason: &str,
    ) -> Result<AddOrderResult, MemoryError> {
        self.ensure_memory(memory_id)?;
        validate_note(before)?;
        validate_note(after)?;
        validate_reason(reason)?;
        let transaction = self.connection.transaction()?;
        ensure_note(&transaction, memory_id, before)?;
        ensure_note(&transaction, memory_id, after)?;
        let exists = transaction
            .query_row(
                "SELECT 1 FROM note_orders
                 WHERE memory_id = ?1 AND before_note = ?2 AND after_note = ?3 AND reason = ?4",
                params![memory_id, before, after, reason],
                |_| Ok(()),
            )
            .optional()?;
        if exists.is_some() {
            transaction.commit()?;
            return Ok(AddOrderResult {
                added: false,
                order: Order {
                    before: before.into(),
                    after: after.into(),
                    reason: reason.into(),
                },
            });
        }
        let sequence: i64 = transaction.query_row(
            "SELECT COALESCE(MAX(sequence), 0) + 1 FROM note_orders WHERE memory_id = ?1",
            params![memory_id],
            |row| row.get(0),
        )?;
        transaction.execute(
            "INSERT INTO note_orders
             (memory_id, before_note, after_note, reason, sequence)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![memory_id, before, after, reason, sequence],
        )?;
        transaction.commit()?;
        Ok(AddOrderResult {
            added: true,
            order: Order {
                before: before.into(),
                after: after.into(),
                reason: reason.into(),
            },
        })
    }

    /// Changes a note's string identity. If `to` already exists, both nodes are merged.
    /// Orders are rewired and exact `(before, after, reason)` duplicates are collapsed.
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
            .filter(|order| order.before == from || order.after == from)
            .count();
        let original_order_count = memory.orders.len();
        let mut seen_orders = HashSet::new();
        let orders: Vec<Order> = memory
            .orders
            .into_iter()
            .map(|order| Order {
                before: if order.before == from {
                    to.into()
                } else {
                    order.before
                },
                after: if order.after == from {
                    to.into()
                } else {
                    order.after
                },
                reason: order.reason,
            })
            .filter(|order| {
                seen_orders.insert((
                    order.before.clone(),
                    order.after.clone(),
                    order.reason.clone(),
                ))
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
                "INSERT INTO ordered_notes (memory_id, content, sequence) VALUES (?1, ?2, ?3)",
                params![memory_id, note, index as i64 + 1],
            )?;
        }
        for (index, order) in orders.iter().enumerate() {
            transaction.execute(
                "INSERT INTO note_orders
                 (memory_id, before_note, after_note, reason, sequence)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    memory_id,
                    order.before,
                    order.after,
                    order.reason,
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
            orders: self.load_orders(memory_id)?,
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

    fn load_orders(&self, memory_id: &str) -> Result<Vec<Order>, MemoryError> {
        let mut statement = self.connection.prepare(
            "SELECT before_note, after_note, reason
             FROM note_orders WHERE memory_id = ?1 ORDER BY sequence",
        )?;
        Ok(statement
            .query_map(params![memory_id], |row| {
                Ok(Order {
                    before: row.get(0)?,
                    after: row.get(1)?,
                    reason: row.get(2)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?)
    }
}

fn migrate_schema(connection: &mut Connection) -> Result<(), MemoryError> {
    connection.execute(
        "CREATE TABLE IF NOT EXISTS ordered_memory_schema_migrations (
            version INTEGER PRIMARY KEY
        )",
        [],
    )?;
    let recorded: Option<i64> = connection.query_row(
        "SELECT MAX(version) FROM ordered_memory_schema_migrations",
        [],
        |row| row.get(0),
    )?;
    let mut version = match recorded {
        Some(version) => version,
        None if table_exists(connection, "ordered_memories")? => {
            if column_exists(connection, "note_orders", "reason")? {
                if column_exists(connection, "note_orders", "edge_id")? {
                    2
                } else {
                    3
                }
            } else {
                1
            }
        }
        None => 0,
    };
    if version > CURRENT_SCHEMA_VERSION {
        return Err(MemoryError::UnsupportedSchemaVersion(version));
    }
    if recorded.is_none() && version > 0 {
        connection.execute(
            "INSERT INTO ordered_memory_schema_migrations (version) VALUES (?1)",
            params![version],
        )?;
    }
    while version < CURRENT_SCHEMA_VERSION {
        let next = version + 1;
        let transaction = connection.transaction()?;
        match next {
            1 => create_v1_schema(&transaction)?,
            2 => migrate_to_v2(&transaction)?,
            3 => migrate_to_v3(&transaction)?,
            _ => unreachable!(),
        }
        transaction.execute(
            "INSERT INTO ordered_memory_schema_migrations (version) VALUES (?1)",
            params![next],
        )?;
        transaction.commit()?;
        version = next;
    }
    Ok(())
}

fn create_v1_schema(transaction: &rusqlite::Transaction<'_>) -> Result<(), MemoryError> {
    transaction.execute_batch(
        "CREATE TABLE IF NOT EXISTS ordered_memories (
            memory_id TEXT PRIMARY KEY
        );
        CREATE TABLE IF NOT EXISTS ordered_notes (
            memory_id TEXT NOT NULL REFERENCES ordered_memories(memory_id),
            content TEXT NOT NULL,
            sequence INTEGER NOT NULL,
            PRIMARY KEY (memory_id, content),
            UNIQUE (memory_id, sequence)
        );
        CREATE TABLE IF NOT EXISTS note_orders (
            memory_id TEXT NOT NULL REFERENCES ordered_memories(memory_id),
            before_note TEXT NOT NULL,
            after_note TEXT NOT NULL,
            sequence INTEGER NOT NULL,
            PRIMARY KEY (memory_id, before_note, after_note),
            UNIQUE (memory_id, sequence),
            FOREIGN KEY (memory_id, before_note) REFERENCES ordered_notes(memory_id, content),
            FOREIGN KEY (memory_id, after_note) REFERENCES ordered_notes(memory_id, content)
        );",
    )?;
    Ok(())
}

fn migrate_to_v2(transaction: &rusqlite::Transaction<'_>) -> Result<(), MemoryError> {
    transaction.execute_batch(
        "ALTER TABLE note_orders RENAME TO note_orders_v1;
         CREATE TABLE note_orders (
            memory_id TEXT NOT NULL REFERENCES ordered_memories(memory_id),
            edge_id TEXT NOT NULL,
            before_note TEXT NOT NULL,
            after_note TEXT NOT NULL,
            reason TEXT NOT NULL,
            sequence INTEGER NOT NULL,
            PRIMARY KEY (memory_id, edge_id),
            UNIQUE (memory_id, before_note, after_note, reason),
            UNIQUE (memory_id, sequence),
            FOREIGN KEY (memory_id, before_note) REFERENCES ordered_notes(memory_id, content),
            FOREIGN KEY (memory_id, after_note) REFERENCES ordered_notes(memory_id, content)
         );",
    )?;
    transaction.execute(
        "INSERT INTO note_orders
         (memory_id, edge_id, before_note, after_note, reason, sequence)
         SELECT memory_id, 'E' || sequence, before_note, after_note, ?1, sequence
         FROM note_orders_v1",
        params![LEGACY_REASON],
    )?;
    transaction.execute("DROP TABLE note_orders_v1", [])?;
    Ok(())
}

fn migrate_to_v3(transaction: &rusqlite::Transaction<'_>) -> Result<(), MemoryError> {
    transaction.execute_batch(
        "ALTER TABLE note_orders RENAME TO note_orders_v2;
         CREATE TABLE note_orders (
            memory_id TEXT NOT NULL REFERENCES ordered_memories(memory_id),
            before_note TEXT NOT NULL,
            after_note TEXT NOT NULL,
            reason TEXT NOT NULL,
            sequence INTEGER NOT NULL,
            PRIMARY KEY (memory_id, before_note, after_note, reason),
            UNIQUE (memory_id, sequence),
            FOREIGN KEY (memory_id, before_note) REFERENCES ordered_notes(memory_id, content),
            FOREIGN KEY (memory_id, after_note) REFERENCES ordered_notes(memory_id, content)
         );
         INSERT INTO note_orders
         (memory_id, before_note, after_note, reason, sequence)
         SELECT memory_id, before_note, after_note, reason, sequence
         FROM note_orders_v2;
         DROP TABLE note_orders_v2;",
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

fn column_exists(connection: &Connection, table: &str, column: &str) -> Result<bool, MemoryError> {
    let mut statement = connection.prepare(&format!("PRAGMA table_info({table})"))?;
    Ok(statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?
        .iter()
        .any(|name| name == column))
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

#[derive(Clone, Copy)]
enum Direction {
    Before,
    After,
}

fn analyze_local_focus(memory: Memory, focus: &[String], limit: usize) -> FocusView {
    let (forward, reverse) = adjacency(&memory.orders);
    let selected = bounded_selection(focus, limit, &forward, &reverse);
    let selected_set: HashSet<&str> = selected.iter().map(String::as_str).collect();
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
            selected_set.contains(order.before.as_str())
                && selected_set.contains(order.after.as_str())
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
    let before_distance = component_distances(&focus_components, &component_reverse);
    let after_distance = component_distances(&focus_components, &component_forward);
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
    let mut before_ids = Vec::new();
    let mut focus_ids = Vec::new();
    let mut after_ids = Vec::new();
    for component in 0..components.len() {
        let reaches_focus = before_distance.contains_key(&component);
        let reached_from_focus = after_distance.contains_key(&component);
        match (reaches_focus, reached_from_focus) {
            (true, true) => focus_ids.push(component),
            (true, false) => before_ids.push(component),
            (false, true) => after_ids.push(component),
            (false, false) => {}
        }
    }
    before_ids.sort_by_key(|component| {
        (
            std::cmp::Reverse(before_distance[component]),
            rank[*component],
        )
    });
    focus_ids.sort_by_key(|component| rank[*component]);
    after_ids.sort_by_key(|component| (after_distance[component], rank[*component]));
    let groups = |ids: Vec<usize>| {
        ids.into_iter()
            .map(|component| components[component].clone())
            .collect()
    };
    FocusView {
        memory_id: memory.memory_id,
        before: groups(before_ids),
        focus: groups(focus_ids),
        after: groups(after_ids),
        connections,
        limit,
        returned_notes: selected.len(),
        returned_connections: usize::min(limit, all_connection_count),
        notes_truncated,
        connections_truncated,
        truncated: notes_truncated || connections_truncated,
    }
}

fn adjacency(orders: &[Order]) -> (HashMap<String, Vec<String>>, HashMap<String, Vec<String>>) {
    let mut forward: HashMap<String, Vec<String>> = HashMap::new();
    let mut reverse: HashMap<String, Vec<String>> = HashMap::new();
    for order in orders {
        let after = forward.entry(order.before.clone()).or_default();
        if !after.contains(&order.after) {
            after.push(order.after.clone());
        }
        let before = reverse.entry(order.after.clone()).or_default();
        if !before.contains(&order.before) {
            before.push(order.before.clone());
        }
    }
    (forward, reverse)
}

fn bounded_selection(
    focus: &[String],
    limit: usize,
    forward: &HashMap<String, Vec<String>>,
    reverse: &HashMap<String, Vec<String>>,
) -> Vec<String> {
    let mut selected = focus.to_vec();
    let mut selected_set: HashSet<String> = focus.iter().cloned().collect();
    let mut seen_before: HashSet<String> = focus.iter().cloned().collect();
    let mut seen_after = seen_before.clone();
    let mut queue = VecDeque::new();
    for note in focus {
        queue.push_back((note.clone(), Direction::Before));
        queue.push_back((note.clone(), Direction::After));
    }
    while let Some((note, direction)) = queue.pop_front() {
        if selected.len() == limit {
            break;
        }
        let (neighbors, seen) = match direction {
            Direction::Before => (reverse, &mut seen_before),
            Direction::After => (forward, &mut seen_after),
        };
        for neighbor in neighbors.get(&note).into_iter().flatten() {
            if !seen.insert(neighbor.clone()) {
                continue;
            }
            if selected_set.contains(neighbor) {
                queue.push_back((neighbor.clone(), direction));
                continue;
            }
            if selected.len() == limit {
                break;
            }
            selected_set.insert(neighbor.clone());
            selected.push(neighbor.clone());
            queue.push_back((neighbor.clone(), direction));
        }
    }
    selected
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
    for before in note_order
        .iter()
        .filter(|note| selected.contains(note.as_str()))
    {
        for after in forward.get(before).into_iter().flatten() {
            if selected.contains(after.as_str()) {
                edges.push((before.clone(), after.clone()));
            }
        }
    }
    edges.sort_by_key(|(before, after)| (rank[before.as_str()], rank[after.as_str()]));
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
    for (before, after) in edges {
        let from = index[before.as_str()];
        let to = index[after.as_str()];
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
    for (before, after) in edges {
        let from = component_of[before.as_str()];
        let to = component_of[after.as_str()];
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

fn validate_reason(reason: &str) -> Result<(), MemoryError> {
    if reason.trim().is_empty() {
        Err(MemoryError::EmptyReason)
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
    fn exact_order_is_idempotent_but_another_reason_is_preserved() {
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
        assert!(!duplicate.added);
        assert_eq!(first.order, duplicate.order);
        assert!(another.added);
        assert_ne!(first.order.reason, another.order.reason);
        assert_eq!(store.get_memory("minecraft").unwrap().orders.len(), 2);
    }

    #[test]
    fn cycles_become_local_sccs_and_can_be_cut_by_limit() {
        let mut store = store();
        store.add_order("minecraft", "A", "B", "AB").unwrap();
        store.add_order("minecraft", "B", "C", "BC").unwrap();
        store.add_order("minecraft", "C", "A", "CA").unwrap();
        let complete = store.focus("minecraft", &["A".into()], 3).unwrap();
        assert_eq!(complete.focus, [vec!["A", "C", "B"]]);
        assert!(!complete.notes_truncated);
        let partial = store.focus("minecraft", &["A".into()], 2).unwrap();
        assert_eq!(partial.before, [vec!["C"]]);
        assert_eq!(partial.focus, [vec!["A"]]);
        assert!(partial.notes_truncated);
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
        assert!(view.before.is_empty());
        assert!(view.after.is_empty());
        assert_eq!(view.connections.len(), 2);
        assert_eq!(view.connections[0].reason, "AからBを見る理由");
        assert_eq!(view.connections[1].reason, "BからAを見る理由");
    }

    #[test]
    fn focus_returns_uniform_scc_lists_in_before_focus_after_order() {
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
        assert_eq!(view.before, [vec!["木"], vec!["板材"]]);
        assert_eq!(view.focus, [vec!["棒"]]);
        assert_eq!(view.after, [vec!["つるはし"], vec!["採掘"]]);
        assert_eq!(view.connections.len(), 4);
        assert_eq!(view.returned_notes, 5);
        assert!(!view.truncated);
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
        assert_eq!(view.before, [vec!["A"]]);
        assert_eq!(view.focus, [vec!["B"], vec!["C"]]);
        assert_eq!(view.after, [vec!["D"]]);
    }

    #[test]
    fn disconnected_notes_do_not_appear_around_focus() {
        let mut store = store();
        store.add_order("minecraft", "A", "B", "AB").unwrap();
        store.add_note("minecraft", "unrelated").unwrap();
        let view = store.focus("minecraft", &["B".into()], 50).unwrap();
        assert_eq!(view.before, [vec!["A"]]);
        assert!(view.after.is_empty());
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
        assert_eq!(count_limited.before, [vec!["B"]]);
        assert_eq!(count_limited.after, [vec!["D"]]);
        store
            .add_order("minecraft", "B", "C", "second reason")
            .unwrap();
        store
            .add_order("minecraft", "B", "C", "third reason")
            .unwrap();
        let connection_limited = store.focus("minecraft", &["C".into()], 2).unwrap();
        assert_eq!(connection_limited.returned_connections, 2);
        assert!(connection_limited.connections_truncated);
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
        assert_eq!(result.deduplicated_orders, 1);
        let memory = store.get_memory("minecraft").unwrap();
        assert_eq!(memory.notes, ["X", "B"]);
        assert_eq!(
            memory.orders,
            [
                Order {
                    before: "X".into(),
                    after: "B".into(),
                    reason: "incoming".into()
                },
                Order {
                    before: "B".into(),
                    after: "B".into(),
                    reason: "same".into()
                }
            ]
        );
    }

    #[test]
    fn migrates_v1_orders_without_losing_them() {
        let mut connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch("PRAGMA foreign_keys = ON;")
            .unwrap();
        {
            let transaction = connection.transaction().unwrap();
            create_v1_schema(&transaction).unwrap();
            transaction.commit().unwrap();
        }
        connection
            .execute("INSERT INTO ordered_memories (memory_id) VALUES ('m')", [])
            .unwrap();
        connection.execute("INSERT INTO ordered_notes (memory_id, content, sequence) VALUES ('m', 'A', 1), ('m', 'B', 2)", []).unwrap();
        connection.execute("INSERT INTO note_orders (memory_id, before_note, after_note, sequence) VALUES ('m', 'A', 'B', 1)", []).unwrap();
        migrate_schema(&mut connection).unwrap();
        let reason: String = connection
            .query_row(
                "SELECT reason FROM note_orders WHERE memory_id = 'm'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(reason, LEGACY_REASON);
        assert!(!column_exists(&connection, "note_orders", "edge_id").unwrap());
        migrate_schema(&mut connection).unwrap();
        let version: i64 = connection
            .query_row(
                "SELECT MAX(version) FROM ordered_memory_schema_migrations",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(version, CURRENT_SCHEMA_VERSION);
    }
}
