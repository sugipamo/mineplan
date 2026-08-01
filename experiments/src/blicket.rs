//! Deterministic hidden-rule environment used only for memory experiments.
//!
//! The environment deliberately owns only its state and observations. An agent
//! decides independently what, if anything, becomes a Thought in memory.

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub const OBJECTS: [&str; 4] = ["A", "B", "C", "D"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlicketMode {
    Fixed,
    PhaseShift,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlicketObservation {
    pub turn: usize,
    pub max_turns: usize,
    pub charge: usize,
    pub target_charge: usize,
    pub activated: Option<bool>,
    pub selection: Option<Vec<String>>,
    pub previous_selection: Option<Vec<String>>,
    pub completed: bool,
    pub failure: Option<String>,
}

#[derive(Debug, Clone)]
pub struct BlicketEnvironment {
    mode: BlicketMode,
    turn: usize,
    max_turns: usize,
    target_charge: usize,
    charge: usize,
    previous_selection: Option<Vec<String>>,
    completed: bool,
}

impl BlicketEnvironment {
    pub fn new(mode: BlicketMode, max_turns: usize, target_charge: usize) -> Self {
        Self {
            mode,
            turn: 0,
            max_turns,
            target_charge,
            charge: 0,
            previous_selection: None,
            completed: false,
        }
    }

    pub fn observation(&self) -> BlicketObservation {
        BlicketObservation {
            turn: self.turn,
            max_turns: self.max_turns,
            charge: self.charge,
            target_charge: self.target_charge,
            activated: None,
            selection: None,
            previous_selection: self.previous_selection.clone(),
            completed: self.completed,
            failure: None,
        }
    }

    pub fn act(&mut self, selection: &[String]) -> BlicketObservation {
        if self.completed {
            return self.failure(selection, "goal_already_reached");
        }
        if self.turn >= self.max_turns {
            return self.failure(selection, "turn_limit_reached");
        }
        let Some(selection) = normalize_selection(selection) else {
            return self.failure(selection, "invalid_selection");
        };
        if self.previous_selection.as_ref() == Some(&selection) {
            return self.failure(&selection, "same_selection_consecutively_forbidden");
        }

        self.turn += 1;
        let activated = self.activates(&selection);
        if activated {
            self.charge += 1;
        }
        self.completed = self.charge >= self.target_charge;
        self.previous_selection = Some(selection.clone());
        BlicketObservation {
            turn: self.turn,
            max_turns: self.max_turns,
            charge: self.charge,
            target_charge: self.target_charge,
            activated: Some(activated),
            selection: Some(selection),
            previous_selection: self.previous_selection.clone(),
            completed: self.completed,
            failure: None,
        }
    }

    fn activates(&self, selection: &[String]) -> bool {
        match self.mode {
            // Hidden rule: C alone or the A+B pair activates the machine.
            BlicketMode::Fixed => is_exact(selection, &["C"]) || is_exact(selection, &["A", "B"]),
            // The rule changes immediately before turn four. The transition is
            // intentionally not announced by the observation API.
            BlicketMode::PhaseShift if self.turn <= 3 => {
                is_exact(selection, &["C"]) || is_exact(selection, &["A", "B"])
            }
            BlicketMode::PhaseShift => {
                is_exact(selection, &["B"]) || is_exact(selection, &["A", "D"])
            }
        }
    }

    fn failure(&self, selection: &[String], reason: &str) -> BlicketObservation {
        BlicketObservation {
            turn: self.turn,
            max_turns: self.max_turns,
            charge: self.charge,
            target_charge: self.target_charge,
            activated: None,
            selection: Some(selection.to_vec()),
            previous_selection: self.previous_selection.clone(),
            completed: self.completed,
            failure: Some(reason.into()),
        }
    }
}

fn normalize_selection(selection: &[String]) -> Option<Vec<String>> {
    if selection.is_empty()
        || selection
            .iter()
            .any(|item| !OBJECTS.contains(&item.as_str()))
    {
        return None;
    }
    let unique = selection.iter().cloned().collect::<BTreeSet<_>>();
    if unique.len() != selection.len() {
        return None;
    }
    Some(unique.into_iter().collect())
}

fn is_exact(selection: &[String], expected: &[&str]) -> bool {
    selection
        .iter()
        .map(String::as_str)
        .eq(expected.iter().copied())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn selection(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).into()).collect()
    }

    #[test]
    fn fixed_rule_is_hidden_but_deterministic() {
        let mut environment = BlicketEnvironment::new(BlicketMode::Fixed, 8, 3);
        assert_eq!(environment.act(&selection(&["A"])).activated, Some(false));
        assert_eq!(environment.act(&selection(&["C"])).activated, Some(true));
        assert_eq!(
            environment.act(&selection(&["A", "B"])).activated,
            Some(true)
        );
        assert!(environment.act(&selection(&["C"])).completed);
    }

    #[test]
    fn phase_shift_changes_the_hidden_rule_after_third_turn() {
        let mut environment = BlicketEnvironment::new(BlicketMode::PhaseShift, 8, 3);
        environment.act(&selection(&["A"]));
        environment.act(&selection(&["C"]));
        assert_eq!(environment.act(&selection(&["D"])).activated, Some(false));
        assert_eq!(environment.act(&selection(&["C"])).activated, Some(false));
        assert_eq!(environment.act(&selection(&["B"])).activated, Some(true));
    }

    #[test]
    fn invalid_or_repeated_actions_do_not_advance_the_turn() {
        let mut environment = BlicketEnvironment::new(BlicketMode::Fixed, 8, 3);
        assert_eq!(
            environment.act(&selection(&["A", "A"])).failure.as_deref(),
            Some("invalid_selection")
        );
        environment.act(&selection(&["A"]));
        assert_eq!(
            environment.act(&selection(&["A"])).failure.as_deref(),
            Some("same_selection_consecutively_forbidden")
        );
        assert_eq!(environment.observation().turn, 1);
    }
}
