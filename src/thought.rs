//! SQLite-backed, append-only thoughts and a mutable active set.
//!
//! A thought deliberately has no truth state, observation type, or action
//! field.  An agent records what it currently takes to be its premises, and
//! creates a later thought when those premises change.

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;

pub const DEFAULT_CONTEXT_LIMIT: usize = 50;
const CURRENT_SCHEMA_VERSION: i64 = 3;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PremiseDraft {
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThoughtDraft {
    #[serde(default)]
    pub associated_from: Vec<String>,
    #[serde(default)]
    pub premises: Vec<PremiseDraft>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Premise {
    pub id: String,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Thought {
    pub id: String,
    pub associated_from: Vec<String>,
    pub premises: Vec<Premise>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActiveSet {
    pub memory_id: String,
    pub anchor_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelatedLink {
    pub thought_id_a: String,
    pub thought_id_b: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThoughtMerge {
    pub source_thought_id: String,
    pub target_thought_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClearResult {
    pub memory_id: String,
    pub deleted_thoughts: usize,
    pub deleted_associated_links: usize,
    pub deleted_related_links: usize,
    pub deleted_active_anchors: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum ThoughtError {
    #[error("database schema version {0} is newer than this program supports")]
    UnsupportedSchemaVersion(i64),
    #[error("memory_id must not be empty")]
    EmptyMemoryId,
    #[error("memory already exists: {0}")]
    DuplicateMemory(String),
    #[error("unknown memory: {0}")]
    UnknownMemory(String),
    #[error("premise text must not be empty")]
    EmptyPremise,
    #[error("duplicate thought reference: {0}")]
    DuplicateThoughtReference(String),
    #[error("unknown thought in this memory: {0}")]
    UnknownThought(String),
    #[error("active-set anchor already exists: {0}")]
    DuplicateAnchor(String),
    #[error("active-set anchor does not exist: {0}")]
    MissingAnchor(String),
    #[error("a Thought cannot be related to itself: {0}")]
    SelfRelated(String),
    #[error("related link already exists: {0} <-> {1}")]
    DuplicateRelated(String, String),
    #[error("related link does not exist: {0} <-> {1}")]
    MissingRelated(String, String),
    #[error("thought merge already exists: {0} -> {1}")]
    DuplicateMerge(String, String),
    #[error("thought merge does not exist: {0} -> {1}")]
    MissingMerge(String, String),
    #[error(transparent)]
    Database(#[from] rusqlite::Error),
}

/// SQLite-backed store. Thought records are append-only; active-set anchors and
/// related links are mutable navigation settings.
pub struct ThoughtStore {
    connection: Connection,
}

impl ThoughtStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, ThoughtError> {
        let mut connection = Connection::open(path)?;
        connection.execute_batch("PRAGMA foreign_keys = ON;")?;
        migrate_schema(&mut connection)?;
        Ok(Self { connection })
    }

    pub fn create_memory(&mut self, memory_id: &str) -> Result<(), ThoughtError> {
        validate_memory_id(memory_id)?;
        match self.connection.execute(
            "INSERT INTO memories (memory_id) VALUES (?1)",
            params![memory_id],
        ) {
            Ok(_) => Ok(()),
            Err(rusqlite::Error::SqliteFailure(error, _))
                if error.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_PRIMARYKEY =>
            {
                Err(ThoughtError::DuplicateMemory(memory_id.into()))
            }
            Err(error) => Err(error.into()),
        }
    }

    /// Lists persistent memory IDs in creation order.
    pub fn list_memory_ids(&self) -> Result<Vec<String>, ThoughtError> {
        let mut statement = self
            .connection
            .prepare("SELECT memory_id FROM memories ORDER BY rowid")?;
        Ok(statement
            .query_map([], |row| row.get(0))?
            .collect::<Result<Vec<String>, _>>()?)
    }

    /// Lists every Thought in creation order. This is intended for read-only
    /// inspection interfaces; agents should normally use `get_context`.
    pub fn list_thoughts(&self, memory_id: &str) -> Result<Vec<Thought>, ThoughtError> {
        self.ensure_memory(memory_id)?;
        self.load_thoughts(memory_id)
    }

    pub fn record_thought(
        &mut self,
        memory_id: &str,
        draft: ThoughtDraft,
    ) -> Result<Thought, ThoughtError> {
        self.ensure_memory(memory_id)?;
        validate_draft(&draft)?;
        for reference in &draft.associated_from {
            self.ensure_thought(memory_id, reference)?;
        }

        let transaction = self.connection.transaction()?;
        let sequence: i64 = transaction.query_row(
            "SELECT COALESCE(MAX(sequence), 0) + 1 FROM thoughts WHERE memory_id = ?1",
            params![memory_id],
            |row| row.get(0),
        )?;
        let id = format!("T{sequence}");
        transaction.execute(
            "INSERT INTO thoughts (memory_id, thought_id, sequence) VALUES (?1, ?2, ?3)",
            params![memory_id, id, sequence],
        )?;
        for (position, premise) in draft.premises.iter().enumerate() {
            transaction.execute(
                "INSERT INTO premises (memory_id, thought_id, premise_id, position, content)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    memory_id,
                    id,
                    format!("{id}.P{}", position + 1),
                    position as i64,
                    premise.content
                ],
            )?;
        }
        for (position, parent) in draft.associated_from.iter().enumerate() {
            let added_sequence = next_relationship_sequence(&transaction, memory_id)?;
            transaction.execute(
                "INSERT INTO thought_links
                 (memory_id, thought_id, parent_thought_id, position, added_sequence)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![memory_id, id, parent, position as i64, added_sequence],
            )?;
        }
        transaction.commit()?;
        Ok(Thought {
            id,
            associated_from: draft.associated_from,
            premises: draft
                .premises
                .into_iter()
                .enumerate()
                .map(|(index, premise)| Premise {
                    id: format!("T{sequence}.P{}", index + 1),
                    content: premise.content,
                })
                .collect(),
        })
    }

    pub fn get_active_set(&self, memory_id: &str) -> Result<ActiveSet, ThoughtError> {
        self.ensure_memory(memory_id)?;
        let mut statement = self
            .connection
            .prepare("SELECT thought_id FROM active_set WHERE memory_id = ?1 ORDER BY position")?;
        let anchor_ids = statement
            .query_map(params![memory_id], |row| row.get(0))?
            .collect::<Result<Vec<String>, _>>()?;
        Ok(ActiveSet {
            memory_id: memory_id.into(),
            anchor_ids,
        })
    }

    pub fn replace_active_set(
        &mut self,
        memory_id: &str,
        anchor_ids: &[String],
    ) -> Result<ActiveSet, ThoughtError> {
        self.ensure_memory(memory_id)?;
        validate_unique_thought_ids(anchor_ids, ThoughtError::DuplicateAnchor)?;
        for id in anchor_ids {
            self.ensure_thought(memory_id, id)?;
        }
        let transaction = self.connection.transaction()?;
        transaction.execute(
            "DELETE FROM active_set WHERE memory_id = ?1",
            params![memory_id],
        )?;
        for (position, id) in anchor_ids.iter().enumerate() {
            transaction.execute(
                "INSERT INTO active_set (memory_id, position, thought_id) VALUES (?1, ?2, ?3)",
                params![memory_id, position as i64, id],
            )?;
        }
        transaction.commit()?;
        self.get_active_set(memory_id)
    }

    pub fn add_active_anchor(
        &mut self,
        memory_id: &str,
        thought_id: &str,
    ) -> Result<ActiveSet, ThoughtError> {
        self.ensure_memory(memory_id)?;
        self.ensure_thought(memory_id, thought_id)?;
        let position: i64 = self.connection.query_row(
            "SELECT COUNT(*) FROM active_set WHERE memory_id = ?1",
            params![memory_id],
            |row| row.get(0),
        )?;
        match self.connection.execute(
            "INSERT INTO active_set (memory_id, position, thought_id) VALUES (?1, ?2, ?3)",
            params![memory_id, position, thought_id],
        ) {
            Ok(_) => self.get_active_set(memory_id),
            Err(rusqlite::Error::SqliteFailure(error, _))
                if error.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE =>
            {
                Err(ThoughtError::DuplicateAnchor(thought_id.into()))
            }
            Err(error) => Err(error.into()),
        }
    }

    pub fn remove_active_anchor(
        &mut self,
        memory_id: &str,
        thought_id: &str,
    ) -> Result<ActiveSet, ThoughtError> {
        self.ensure_memory(memory_id)?;
        if self.connection.execute(
            "DELETE FROM active_set WHERE memory_id = ?1 AND thought_id = ?2",
            params![memory_id, thought_id],
        )? == 0
        {
            return Err(ThoughtError::MissingAnchor(thought_id.into()));
        }
        self.normalize_active_positions(memory_id)?;
        self.get_active_set(memory_id)
    }

    /// Adds an undirected navigation link between two Thoughts in one memory.
    pub fn add_related_link(
        &mut self,
        memory_id: &str,
        thought_id_a: &str,
        thought_id_b: &str,
    ) -> Result<RelatedLink, ThoughtError> {
        self.ensure_memory(memory_id)?;
        let (thought_id_a, thought_id_b) = canonical_pair(thought_id_a, thought_id_b)?;
        self.ensure_thought(memory_id, &thought_id_a)?;
        self.ensure_thought(memory_id, &thought_id_b)?;
        let transaction = self.connection.transaction()?;
        let added_sequence = next_relationship_sequence(&transaction, memory_id)?;
        match transaction.execute(
            "INSERT INTO related_links (memory_id, thought_id_a, thought_id_b, added_sequence)
             VALUES (?1, ?2, ?3, ?4)",
            params![memory_id, thought_id_a, thought_id_b, added_sequence],
        ) {
            Ok(_) => {
                transaction.commit()?;
                Ok(RelatedLink {
                    thought_id_a,
                    thought_id_b,
                })
            }
            Err(rusqlite::Error::SqliteFailure(error, _))
                if error.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_PRIMARYKEY =>
            {
                Err(ThoughtError::DuplicateRelated(thought_id_a, thought_id_b))
            }
            Err(error) => Err(error.into()),
        }
    }

    pub fn remove_related_link(
        &mut self,
        memory_id: &str,
        thought_id_a: &str,
        thought_id_b: &str,
    ) -> Result<RelatedLink, ThoughtError> {
        self.ensure_memory(memory_id)?;
        let (thought_id_a, thought_id_b) = canonical_pair(thought_id_a, thought_id_b)?;
        if self.connection.execute(
            "DELETE FROM related_links
             WHERE memory_id = ?1 AND thought_id_a = ?2 AND thought_id_b = ?3",
            params![memory_id, thought_id_a, thought_id_b],
        )? == 0
        {
            return Err(ThoughtError::MissingRelated(thought_id_a, thought_id_b));
        }
        Ok(RelatedLink {
            thought_id_a,
            thought_id_b,
        })
    }

    /// Hides the source thought in context results while keeping its edges
    /// traversable through the target at zero exploration cost.
    pub fn merge_thoughts(
        &mut self,
        memory_id: &str,
        source: &str,
        target: &str,
    ) -> Result<ThoughtMerge, ThoughtError> {
        self.ensure_memory(memory_id)?;
        if source == target {
            return Err(ThoughtError::SelfRelated(source.into()));
        }
        self.ensure_thought(memory_id, source)?;
        self.ensure_thought(memory_id, target)?;
        match self.connection.execute("INSERT INTO thought_merges (memory_id, source_thought_id, target_thought_id) VALUES (?1, ?2, ?3)", params![memory_id, source, target]) {
            Ok(_) => Ok(ThoughtMerge { source_thought_id: source.into(), target_thought_id: target.into() }),
            Err(rusqlite::Error::SqliteFailure(error, _)) if error.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_PRIMARYKEY => Err(ThoughtError::DuplicateMerge(source.into(), target.into())),
            Err(error) => Err(error.into()),
        }
    }

    /// Returns Thoughts directly linked by `related`, newest link first.
    pub fn get_related_thoughts(
        &self,
        memory_id: &str,
        thought_id: &str,
    ) -> Result<Vec<Thought>, ThoughtError> {
        self.ensure_memory(memory_id)?;
        self.ensure_thought(memory_id, thought_id)?;
        let mut statement = self.connection.prepare(
            "SELECT CASE WHEN thought_id_a = ?2 THEN thought_id_b ELSE thought_id_a END
             FROM related_links
             WHERE memory_id = ?1 AND (thought_id_a = ?2 OR thought_id_b = ?2)
             ORDER BY added_sequence DESC",
        )?;
        let ids = statement
            .query_map(params![memory_id, thought_id], |row| {
                row.get::<_, String>(0)
            })?
            .collect::<Result<Vec<_>, _>>()?;
        ids.iter()
            .map(|related_id| self.load_thought(memory_id, related_id))
            .collect()
    }

    /// Clears a memory's contents but keeps its memory ID available for reuse.
    /// Calling it on an already empty memory succeeds and returns zero counts.
    pub fn clear_memory(&mut self, memory_id: &str) -> Result<ClearResult, ThoughtError> {
        self.ensure_memory(memory_id)?;
        let transaction = self.connection.transaction()?;
        let deleted_active_anchors = transaction.execute(
            "DELETE FROM active_set WHERE memory_id = ?1",
            params![memory_id],
        )?;
        let deleted_related_links = transaction.execute(
            "DELETE FROM related_links WHERE memory_id = ?1",
            params![memory_id],
        )?;
        transaction.execute(
            "DELETE FROM thought_merges WHERE memory_id = ?1",
            params![memory_id],
        )?;
        transaction.execute(
            "DELETE FROM premises WHERE memory_id = ?1",
            params![memory_id],
        )?;
        let deleted_associated_links = transaction.execute(
            "DELETE FROM thought_links WHERE memory_id = ?1",
            params![memory_id],
        )?;
        let deleted_thoughts = transaction.execute(
            "DELETE FROM thoughts WHERE memory_id = ?1",
            params![memory_id],
        )?;
        transaction.execute(
            "DELETE FROM relationship_sequences WHERE memory_id = ?1",
            params![memory_id],
        )?;
        transaction.commit()?;
        Ok(ClearResult {
            memory_id: memory_id.into(),
            deleted_thoughts,
            deleted_associated_links,
            deleted_related_links,
            deleted_active_anchors,
        })
    }

    /// Traverses `associated_from` and `related` links breadth-first from the
    /// current anchors. Newer relationships are enqueued first; the first
    /// `limit` discovered Thoughts are returned.
    pub fn get_context(&self, memory_id: &str, limit: usize) -> Result<Vec<Thought>, ThoughtError> {
        let anchors = self.get_active_set(memory_id)?.anchor_ids;
        if limit == 0 {
            return Ok(Vec::new());
        }
        let all = self.load_thoughts(memory_id)?;
        let mut index = HashMap::new();
        for thought in all {
            index.insert(thought.id.clone(), thought);
        }
        let neighbors = self.load_neighbors(memory_id)?;
        let merges = self.load_merges(memory_id)?;
        let mut seen = HashSet::new();
        let mut queue = VecDeque::new();
        let mut ids = Vec::new();
        for anchor in anchors {
            queue.push_back(anchor);
        }
        while let Some(id) = queue.pop_front() {
            if !seen.insert(id.clone()) {
                continue;
            }
            let hidden = merges.contains_key(&id);
            if !hidden {
                ids.push(id.clone());
            }
            if ids.len() == limit {
                break;
            }
            for neighbor in neighbors.get(&id).into_iter().flatten() {
                queue.push_back(neighbor.clone());
            }
            if let Some(target) = merges.get(&id) {
                queue.push_front(target.clone());
            }
        }
        Ok(ids
            .into_iter()
            .map(|id| index.remove(&id).expect("BFS ids come from index"))
            .collect())
    }

    fn ensure_memory(&self, memory_id: &str) -> Result<(), ThoughtError> {
        validate_memory_id(memory_id)?;
        let exists = self
            .connection
            .query_row(
                "SELECT 1 FROM memories WHERE memory_id = ?1",
                params![memory_id],
                |_| Ok(()),
            )
            .optional()?;
        if exists.is_none() {
            return Err(ThoughtError::UnknownMemory(memory_id.into()));
        }
        Ok(())
    }

    fn ensure_thought(&self, memory_id: &str, thought_id: &str) -> Result<(), ThoughtError> {
        let exists = self
            .connection
            .query_row(
                "SELECT 1 FROM thoughts WHERE memory_id = ?1 AND thought_id = ?2",
                params![memory_id, thought_id],
                |_| Ok(()),
            )
            .optional()?;
        if exists.is_none() {
            return Err(ThoughtError::UnknownThought(thought_id.into()));
        }
        Ok(())
    }

    fn normalize_active_positions(&mut self, memory_id: &str) -> Result<(), ThoughtError> {
        let ids = self.get_active_set(memory_id)?.anchor_ids;
        let transaction = self.connection.transaction()?;
        transaction.execute(
            "DELETE FROM active_set WHERE memory_id = ?1",
            params![memory_id],
        )?;
        for (position, id) in ids.iter().enumerate() {
            transaction.execute(
                "INSERT INTO active_set (memory_id, position, thought_id) VALUES (?1, ?2, ?3)",
                params![memory_id, position as i64, id],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    fn load_thoughts(&self, memory_id: &str) -> Result<Vec<Thought>, ThoughtError> {
        let mut statement = self
            .connection
            .prepare("SELECT thought_id FROM thoughts WHERE memory_id = ?1 ORDER BY sequence")?;
        let rows = statement.query_map(params![memory_id], |row| row.get::<_, String>(0))?;
        let ids = rows.collect::<Result<Vec<_>, _>>()?;
        ids.iter()
            .map(|id| self.load_thought(memory_id, id))
            .collect()
    }

    fn load_thought(&self, memory_id: &str, id: &str) -> Result<Thought, ThoughtError> {
        let mut links = self.connection.prepare("SELECT parent_thought_id FROM thought_links WHERE memory_id = ?1 AND thought_id = ?2 ORDER BY position")?;
        let associated_from = links
            .query_map(params![memory_id, id], |row| row.get(0))?
            .collect::<Result<Vec<_>, _>>()?;
        let mut premises = self.connection.prepare("SELECT premise_id, content FROM premises WHERE memory_id = ?1 AND thought_id = ?2 ORDER BY position")?;
        let premises = premises
            .query_map(params![memory_id, id], |row| {
                Ok(Premise {
                    id: row.get(0)?,
                    content: row.get(1)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Thought {
            id: id.into(),
            associated_from,
            premises,
        })
    }

    fn load_neighbors(
        &self,
        memory_id: &str,
    ) -> Result<HashMap<String, Vec<String>>, ThoughtError> {
        let mut neighbors: HashMap<String, Vec<(i64, String)>> = HashMap::new();
        let mut thought_links = self.connection.prepare(
            "SELECT thought_id, parent_thought_id, added_sequence
             FROM thought_links WHERE memory_id = ?1",
        )?;
        for row in thought_links.query_map(params![memory_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })? {
            let (child, parent, added_sequence) = row?;
            neighbors
                .entry(child.clone())
                .or_default()
                .push((added_sequence, parent.clone()));
            neighbors
                .entry(parent)
                .or_default()
                .push((added_sequence, child));
        }
        let mut related_links = self.connection.prepare(
            "SELECT thought_id_a, thought_id_b, added_sequence
             FROM related_links WHERE memory_id = ?1",
        )?;
        for row in related_links.query_map(params![memory_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })? {
            let (a, b, added_sequence) = row?;
            neighbors
                .entry(a.clone())
                .or_default()
                .push((added_sequence, b.clone()));
            neighbors.entry(b).or_default().push((added_sequence, a));
        }
        Ok(neighbors
            .into_iter()
            .map(|(id, mut edges)| {
                edges.sort_unstable_by(|(sequence_a, id_a), (sequence_b, id_b)| {
                    sequence_b.cmp(sequence_a).then_with(|| id_a.cmp(id_b))
                });
                (
                    id,
                    edges.into_iter().map(|(_, neighbor)| neighbor).collect(),
                )
            })
            .collect())
    }

    fn load_merges(&self, memory_id: &str) -> Result<HashMap<String, String>, ThoughtError> {
        let mut statement = self.connection.prepare(
            "SELECT source_thought_id, target_thought_id FROM thought_merges WHERE memory_id = ?1",
        )?;
        Ok(statement
            .query_map(params![memory_id], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<Result<HashMap<_, _>, _>>()?)
    }
}

fn migrate_schema(connection: &mut Connection) -> Result<(), ThoughtError> {
    connection.execute(
        "CREATE TABLE IF NOT EXISTS schema_migrations (version INTEGER PRIMARY KEY)",
        [],
    )?;
    let recorded: Option<i64> =
        connection.query_row("SELECT MAX(version) FROM schema_migrations", [], |row| {
            row.get(0)
        })?;
    let mut version = match recorded {
        Some(version) => version,
        None if table_exists(connection, "memories")? => 1,
        None => 0,
    };
    if version > CURRENT_SCHEMA_VERSION {
        return Err(ThoughtError::UnsupportedSchemaVersion(version));
    }
    if version == 1 && recorded.is_none() {
        connection.execute("INSERT INTO schema_migrations (version) VALUES (1)", [])?;
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
            "INSERT INTO schema_migrations (version) VALUES (?1)",
            params![next],
        )?;
        transaction.commit()?;
        version = next;
    }
    Ok(())
}

fn migrate_to_v3(connection: &Connection) -> Result<(), ThoughtError> {
    connection.execute_batch("CREATE TABLE IF NOT EXISTS thought_merges (memory_id TEXT NOT NULL REFERENCES memories(memory_id), source_thought_id TEXT NOT NULL, target_thought_id TEXT NOT NULL, PRIMARY KEY (memory_id, source_thought_id), FOREIGN KEY (memory_id, source_thought_id) REFERENCES thoughts(memory_id, thought_id), FOREIGN KEY (memory_id, target_thought_id) REFERENCES thoughts(memory_id, thought_id));")?;
    Ok(())
}

fn create_v1_schema(transaction: &rusqlite::Transaction<'_>) -> Result<(), ThoughtError> {
    transaction.execute_batch("CREATE TABLE IF NOT EXISTS memories (memory_id TEXT PRIMARY KEY);
      CREATE TABLE IF NOT EXISTS thoughts (memory_id TEXT NOT NULL REFERENCES memories(memory_id), thought_id TEXT NOT NULL, sequence INTEGER NOT NULL, PRIMARY KEY (memory_id, thought_id), UNIQUE (memory_id, sequence));
      CREATE TABLE IF NOT EXISTS premises (memory_id TEXT NOT NULL, thought_id TEXT NOT NULL, premise_id TEXT NOT NULL, position INTEGER NOT NULL, content TEXT NOT NULL, PRIMARY KEY (memory_id, thought_id, premise_id), UNIQUE (memory_id, thought_id, position), FOREIGN KEY (memory_id, thought_id) REFERENCES thoughts(memory_id, thought_id));
      CREATE TABLE IF NOT EXISTS thought_links (memory_id TEXT NOT NULL, thought_id TEXT NOT NULL, parent_thought_id TEXT NOT NULL, position INTEGER NOT NULL, PRIMARY KEY (memory_id, thought_id, parent_thought_id), UNIQUE (memory_id, thought_id, position), FOREIGN KEY (memory_id, thought_id) REFERENCES thoughts(memory_id, thought_id), FOREIGN KEY (memory_id, parent_thought_id) REFERENCES thoughts(memory_id, thought_id));
      CREATE TABLE IF NOT EXISTS active_set (memory_id TEXT NOT NULL REFERENCES memories(memory_id), position INTEGER NOT NULL, thought_id TEXT NOT NULL, PRIMARY KEY (memory_id, position), UNIQUE (memory_id, thought_id), FOREIGN KEY (memory_id, thought_id) REFERENCES thoughts(memory_id, thought_id));")?;
    Ok(())
}

fn migrate_to_v2(connection: &Connection) -> Result<(), ThoughtError> {
    connection.execute_batch("CREATE TABLE IF NOT EXISTS related_links (memory_id TEXT NOT NULL REFERENCES memories(memory_id), thought_id_a TEXT NOT NULL, thought_id_b TEXT NOT NULL, added_sequence INTEGER NOT NULL, PRIMARY KEY (memory_id, thought_id_a, thought_id_b), FOREIGN KEY (memory_id, thought_id_a) REFERENCES thoughts(memory_id, thought_id), FOREIGN KEY (memory_id, thought_id_b) REFERENCES thoughts(memory_id, thought_id));
      CREATE TABLE IF NOT EXISTS relationship_sequences (memory_id TEXT PRIMARY KEY REFERENCES memories(memory_id), last_sequence INTEGER NOT NULL);")?;
    let mut columns = connection.prepare("PRAGMA table_info(thought_links)")?;
    let has_added_sequence = columns
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?
        .iter()
        .any(|name| name == "added_sequence");
    if !has_added_sequence {
        connection.execute(
            "ALTER TABLE thought_links ADD COLUMN added_sequence INTEGER",
            [],
        )?;
    }
    connection.execute(
        "UPDATE thought_links
         SET added_sequence = (
             SELECT thoughts.sequence * 1000000 + thought_links.position
             FROM thoughts
             WHERE thoughts.memory_id = thought_links.memory_id
               AND thoughts.thought_id = thought_links.thought_id
         )
         WHERE added_sequence IS NULL",
        [],
    )?;
    connection.execute(
        "INSERT INTO relationship_sequences (memory_id, last_sequence)
         SELECT memory_id, COALESCE(MAX(added_sequence), 0)
         FROM thought_links GROUP BY memory_id
         ON CONFLICT(memory_id) DO UPDATE SET
           last_sequence = MAX(relationship_sequences.last_sequence, excluded.last_sequence)",
        [],
    )?;
    Ok(())
}

fn table_exists(connection: &Connection, name: &str) -> Result<bool, ThoughtError> {
    Ok(connection
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
            params![name],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

fn next_relationship_sequence(
    transaction: &rusqlite::Transaction<'_>,
    memory_id: &str,
) -> Result<i64, ThoughtError> {
    let last_sequence = transaction
        .query_row(
            "SELECT last_sequence FROM relationship_sequences WHERE memory_id = ?1",
            params![memory_id],
            |row| row.get(0),
        )
        .optional()?
        .unwrap_or(0);
    let next_sequence = last_sequence + 1;
    transaction.execute(
        "INSERT INTO relationship_sequences (memory_id, last_sequence) VALUES (?1, ?2)
         ON CONFLICT(memory_id) DO UPDATE SET last_sequence = excluded.last_sequence",
        params![memory_id, next_sequence],
    )?;
    Ok(next_sequence)
}

fn canonical_pair(a: &str, b: &str) -> Result<(String, String), ThoughtError> {
    if a == b {
        return Err(ThoughtError::SelfRelated(a.into()));
    }
    if a < b {
        Ok((a.into(), b.into()))
    } else {
        Ok((b.into(), a.into()))
    }
}

fn validate_memory_id(memory_id: &str) -> Result<(), ThoughtError> {
    if memory_id.trim().is_empty() {
        Err(ThoughtError::EmptyMemoryId)
    } else {
        Ok(())
    }
}

fn validate_draft(draft: &ThoughtDraft) -> Result<(), ThoughtError> {
    if draft
        .premises
        .iter()
        .any(|premise| premise.content.trim().is_empty())
    {
        return Err(ThoughtError::EmptyPremise);
    }
    validate_unique_thought_ids(
        &draft.associated_from,
        ThoughtError::DuplicateThoughtReference,
    )
}

fn validate_unique_thought_ids(
    ids: &[String],
    error: impl Fn(String) -> ThoughtError,
) -> Result<(), ThoughtError> {
    let mut seen = HashSet::new();
    for id in ids {
        if !seen.insert(id) {
            return Err(error(id.clone()));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> ThoughtStore {
        ThoughtStore::open(":memory:").unwrap()
    }
    fn draft(parents: &[&str], premises: &[&str]) -> ThoughtDraft {
        ThoughtDraft {
            associated_from: parents.iter().map(|s| (*s).into()).collect(),
            premises: premises
                .iter()
                .map(|s| PremiseDraft {
                    content: (*s).into(),
                })
                .collect(),
        }
    }

    #[test]
    fn thoughts_are_append_only_and_scoped_to_memory() {
        let mut store = store();
        store.create_memory("minecraft").unwrap();
        assert_eq!(
            store
                .record_thought("minecraft", draft(&[], &["door is closed"]))
                .unwrap()
                .id,
            "T1"
        );
        assert_eq!(
            store
                .record_thought("minecraft", draft(&["T1"], &["door opens with a lever"]))
                .unwrap()
                .associated_from,
            ["T1"]
        );
        store.create_memory("other").unwrap();
        assert!(matches!(
            store.record_thought("other", draft(&["T1"], &[])),
            Err(ThoughtError::UnknownThought(_))
        ));
    }

    #[test]
    fn empty_premise_list_is_allowed_but_blank_text_is_not() {
        let mut store = store();
        store.create_memory("m").unwrap();
        assert!(store.record_thought("m", draft(&[], &[])).is_ok());
        assert!(matches!(
            store.record_thought("m", draft(&[], &[" "])),
            Err(ThoughtError::EmptyPremise)
        ));
    }

    #[test]
    fn context_is_bidirectional_bfs_with_configurable_limit() {
        let mut store = store();
        store.create_memory("m").unwrap();
        store.record_thought("m", draft(&[], &["one"])).unwrap();
        store.record_thought("m", draft(&["T1"], &["two"])).unwrap();
        store
            .record_thought("m", draft(&["T1"], &["three"]))
            .unwrap();
        store
            .record_thought("m", draft(&["T2"], &["four"]))
            .unwrap();
        store.replace_active_set("m", &["T2".into()]).unwrap();
        assert_eq!(
            store
                .get_context("m", 3)
                .unwrap()
                .iter()
                .map(|t| t.id.as_str())
                .collect::<Vec<_>>(),
            ["T2", "T4", "T1"]
        );
        assert_eq!(
            store
                .get_context("m", 50)
                .unwrap()
                .iter()
                .map(|t| t.id.as_str())
                .collect::<Vec<_>>(),
            ["T2", "T4", "T1", "T3"]
        );
    }

    #[test]
    fn active_set_can_be_added_removed_and_reordered() {
        let mut store = store();
        store.create_memory("m").unwrap();
        store.record_thought("m", draft(&[], &[])).unwrap();
        store.record_thought("m", draft(&[], &[])).unwrap();
        store.add_active_anchor("m", "T1").unwrap();
        store.add_active_anchor("m", "T2").unwrap();
        assert_eq!(
            store.remove_active_anchor("m", "T1").unwrap().anchor_ids,
            ["T2"]
        );
        assert_eq!(
            store
                .replace_active_set("m", &["T1".into(), "T2".into()])
                .unwrap()
                .anchor_ids,
            ["T1", "T2"]
        );
    }

    #[test]
    fn related_links_are_undirected_and_newer_links_are_visited_first() {
        let mut store = store();
        store.create_memory("m").unwrap();
        store.record_thought("m", draft(&[], &["one"])).unwrap();
        store.record_thought("m", draft(&["T1"], &["two"])).unwrap();
        store.record_thought("m", draft(&[], &["three"])).unwrap();
        store.add_related_link("m", "T1", "T3").unwrap();
        store.add_related_link("m", "T1", "T2").unwrap();
        assert_eq!(
            store
                .get_related_thoughts("m", "T1")
                .unwrap()
                .iter()
                .map(|thought| thought.id.as_str())
                .collect::<Vec<_>>(),
            ["T2", "T3"]
        );
        store.replace_active_set("m", &["T1".into()]).unwrap();
        assert_eq!(
            store
                .get_context("m", 3)
                .unwrap()
                .iter()
                .map(|thought| thought.id.as_str())
                .collect::<Vec<_>>(),
            ["T1", "T2", "T3"]
        );
        assert!(matches!(
            store.add_related_link("m", "T3", "T1"),
            Err(ThoughtError::DuplicateRelated(_, _))
        ));
        assert!(matches!(
            store.add_related_link("m", "T1", "T1"),
            Err(ThoughtError::SelfRelated(_))
        ));
        store.remove_related_link("m", "T3", "T1").unwrap();
        assert_eq!(
            store
                .get_context("m", 3)
                .unwrap()
                .iter()
                .map(|thought| thought.id.as_str())
                .collect::<Vec<_>>(),
            ["T1", "T2"]
        );
    }

    #[test]
    fn clear_keeps_memory_id_and_returns_deleted_counts() {
        let mut store = store();
        store.create_memory("m").unwrap();
        store.record_thought("m", draft(&[], &[])).unwrap();
        store.record_thought("m", draft(&["T1"], &[])).unwrap();
        store.add_related_link("m", "T1", "T2").unwrap();
        store.add_active_anchor("m", "T2").unwrap();
        assert_eq!(
            store.clear_memory("m").unwrap(),
            ClearResult {
                memory_id: "m".into(),
                deleted_thoughts: 2,
                deleted_associated_links: 1,
                deleted_related_links: 1,
                deleted_active_anchors: 1,
            }
        );
        assert!(
            store
                .get_context("m", DEFAULT_CONTEXT_LIMIT)
                .unwrap()
                .is_empty()
        );
        assert_eq!(store.clear_memory("m").unwrap().deleted_thoughts, 0);
        assert_eq!(store.record_thought("m", draft(&[], &[])).unwrap().id, "T1");
    }

    #[test]
    fn merged_source_is_hidden_but_zero_cost_edges_are_traversed() {
        let mut store = store();
        store.create_memory("m").unwrap();
        store.record_thought("m", draft(&[], &["source"])).unwrap();
        store
            .record_thought("m", draft(&["T1"], &["middle"]))
            .unwrap();
        store
            .record_thought("m", draft(&["T2"], &["target"]))
            .unwrap();
        store.merge_thoughts("m", "T1", "T3").unwrap();
        store.replace_active_set("m", &["T1".into()]).unwrap();
        let ids: Vec<_> = store
            .get_context("m", 2)
            .unwrap()
            .into_iter()
            .map(|t| t.id)
            .collect();
        assert_eq!(ids, ["T3", "T2"]);
    }

    #[test]
    fn migrates_an_unversioned_v1_database_without_losing_thoughts() {
        let mut connection = Connection::open_in_memory().unwrap();
        {
            let transaction = connection.transaction().unwrap();
            create_v1_schema(&transaction).unwrap();
            transaction.commit().unwrap();
        }
        connection
            .execute("INSERT INTO memories (memory_id) VALUES ('m')", [])
            .unwrap();
        connection
            .execute(
                "INSERT INTO thoughts (memory_id, thought_id, sequence) VALUES ('m', 'T1', 1)",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO thoughts (memory_id, thought_id, sequence) VALUES ('m', 'T2', 2)",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO thought_links (memory_id, thought_id, parent_thought_id, position) VALUES ('m', 'T2', 'T1', 0)",
                [],
            )
            .unwrap();

        migrate_schema(&mut connection).unwrap();
        migrate_schema(&mut connection).unwrap();
        let version: i64 = connection
            .query_row("SELECT MAX(version) FROM schema_migrations", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(version, CURRENT_SCHEMA_VERSION);
        let mut store = ThoughtStore { connection };
        store.replace_active_set("m", &["T2".into()]).unwrap();
        assert_eq!(
            store
                .get_context("m", DEFAULT_CONTEXT_LIMIT)
                .unwrap()
                .iter()
                .map(|thought| thought.id.as_str())
                .collect::<Vec<_>>(),
            ["T2", "T1"]
        );
        assert!(store.get_related_thoughts("m", "T1").unwrap().is_empty());
        assert_eq!(
            store
                .record_thought("m", draft(&["T2"], &["migrated"]))
                .unwrap()
                .id,
            "T3"
        );
    }
}
