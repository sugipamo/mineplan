//! Deterministic reference run for the Blicket environment and Thought memory.
//!
//! This is not an LLM benchmark. It gives a reproducible trace of the current
//! memory operations that an MCP-connected agent will use in a hidden-rule task.

use memory_experiments::blicket::{BlicketEnvironment, BlicketMode, BlicketObservation};
use memory_server::thought::{
    DEFAULT_CONTEXT_LIMIT, PremiseDraft, ThoughtDraft, ThoughtError, ThoughtStore,
};
use serde::Serialize;
use std::env;

#[derive(Debug, Serialize)]
struct TurnLog {
    action: Vec<String>,
    observation: BlicketObservation,
    thought_id: String,
    context_ids: Vec<String>,
}

#[derive(Debug, Serialize)]
struct RunLog {
    mode: BlicketMode,
    memory_id: String,
    completed: bool,
    turns: Vec<TurnLog>,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mode = match env::var("BLICKET_MODE").as_deref() {
        Ok("phase_shift") => BlicketMode::PhaseShift,
        Ok("fixed") | Err(_) => BlicketMode::Fixed,
        Ok(value) => {
            return Err(format!("BLICKET_MODE must be fixed or phase_shift, got {value:?}").into());
        }
    };
    let database_path = env::var("BLICKET_MEMORY_DB_PATH").unwrap_or_else(|_| ":memory:".into());
    let memory_id = env::var("BLICKET_MEMORY_ID").unwrap_or_else(|_| "blicket-reference".into());
    let mut store = ThoughtStore::open(database_path)?;
    match store.create_memory(&memory_id) {
        Ok(()) | Err(ThoughtError::DuplicateMemory(_)) => {}
        Err(error) => return Err(error.into()),
    }
    // This ID is reserved for the reference run, making repeated runs comparable.
    store.clear_memory(&memory_id)?;

    let actions = match mode {
        BlicketMode::Fixed => vec![vec!["A"], vec!["B"], vec!["C"], vec!["A", "B"], vec!["C"]],
        BlicketMode::PhaseShift => vec![
            vec!["A"],
            vec!["B"],
            vec!["C"],
            vec!["A", "B"],
            vec!["B"],
            vec!["A", "D"],
        ],
    };
    let mut environment = BlicketEnvironment::new(mode, 8, 3);
    let mut turns = Vec::new();
    let mut previous_thought = None;
    let mut positive_thoughts = Vec::new();

    for action in actions {
        let action = action.into_iter().map(String::from).collect::<Vec<_>>();
        let observation = environment.act(&action);
        let premise = format_observation(&observation);
        let thought = store.record_thought(
            &memory_id,
            ThoughtDraft {
                associated_from: previous_thought.iter().cloned().collect(),
                premises: vec![PremiseDraft { content: premise }],
            },
        )?;
        if observation.activated == Some(true) {
            positive_thoughts.push(thought.id.clone());
            if positive_thoughts.len() >= 2 {
                let earlier = &positive_thoughts[positive_thoughts.len() - 2];
                store.add_related_link(&memory_id, earlier, &thought.id)?;
            }
        }
        store.replace_active_set(&memory_id, std::slice::from_ref(&thought.id))?;
        let context_ids = store
            .get_context(&memory_id, DEFAULT_CONTEXT_LIMIT)?
            .into_iter()
            .map(|item| item.id)
            .collect();
        previous_thought = Some(thought.id.clone());
        turns.push(TurnLog {
            action,
            observation: observation.clone(),
            thought_id: thought.id,
            context_ids,
        });
        if observation.completed || observation.failure.is_some() {
            break;
        }
    }

    serde_json::to_writer_pretty(
        std::io::stdout(),
        &RunLog {
            mode,
            memory_id,
            completed: environment.observation().completed,
            turns,
        },
    )?;
    println!();
    Ok(())
}

fn format_observation(observation: &BlicketObservation) -> String {
    let selection = observation
        .selection
        .as_deref()
        .unwrap_or_default()
        .join("+");
    match observation.activated {
        Some(activated) => format!(
            "turn {}: selection {} activated={} charge={}/{}",
            observation.turn, selection, activated, observation.charge, observation.target_charge
        ),
        None => format!(
            "turn {}: selection {} failed={}",
            observation.turn,
            selection,
            observation.failure.as_deref().unwrap_or("unknown")
        ),
    }
}
