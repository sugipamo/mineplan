use crate::ordered_memory::{Memory, Order};
use serde::Serialize;
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TaskSegment {
    pub edge_name: String,
    pub sequence: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FoundPath {
    pub turns: usize,
    pub tasks: Vec<TaskSegment>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct State {
    node: String,
    edge_name: String,
}

#[derive(Debug, Clone)]
struct Step {
    prior: State,
    edge: Order,
}

struct SnapshotFocus<'a> {
    memory: &'a Memory,
    outgoing: HashMap<&'a str, Vec<usize>>,
    incoming: HashMap<&'a str, Vec<usize>>,
}

#[derive(Clone, Copy, Hash, PartialEq, Eq)]
enum Direction {
    Previous,
    Next,
}

impl<'a> SnapshotFocus<'a> {
    fn new(memory: &'a Memory) -> Self {
        let mut outgoing: HashMap<&str, Vec<usize>> = HashMap::new();
        let mut incoming: HashMap<&str, Vec<usize>> = HashMap::new();
        for (index, edge) in memory.orders.iter().enumerate() {
            outgoing.entry(&edge.previous).or_default().push(index);
            incoming.entry(&edge.next).or_default().push(index);
        }
        Self {
            memory,
            outgoing,
            incoming,
        }
    }

    /// Mirrors mineplan focus selection for one node, but uses indexes built once per snapshot.
    fn connections(&self, focus: &str, limit: usize) -> Vec<Order> {
        if limit == 0 {
            return Vec::new();
        }
        let mut selected = HashSet::from([focus.to_string()]);
        let mut queue = VecDeque::new();
        let mut seen = HashSet::new();
        for index in self.outgoing.get(focus).into_iter().flatten() {
            let edge = &self.memory.orders[*index];
            queue.push_back((edge.next.clone(), edge.edge_name.clone(), Direction::Next));
        }
        for index in self.incoming.get(focus).into_iter().flatten() {
            let edge = &self.memory.orders[*index];
            queue.push_back((
                edge.previous.clone(),
                edge.edge_name.clone(),
                Direction::Previous,
            ));
        }
        while let Some((node, edge_name, direction)) = queue.pop_front() {
            if !seen.insert((node.clone(), edge_name.clone(), direction)) {
                continue;
            }
            if !selected.contains(&node) {
                if selected.len() == limit {
                    continue;
                }
                selected.insert(node.clone());
            }
            let candidates = match direction {
                Direction::Previous => self.incoming.get(node.as_str()),
                Direction::Next => self.outgoing.get(node.as_str()),
            };
            for index in candidates.into_iter().flatten() {
                let edge = &self.memory.orders[*index];
                if edge.edge_name != edge_name {
                    continue;
                }
                let next_node = match direction {
                    Direction::Previous => edge.previous.clone(),
                    Direction::Next => edge.next.clone(),
                };
                queue.push_back((next_node, edge_name.clone(), direction));
            }
        }
        let mut connection_indexes = HashSet::new();
        for node in &selected {
            connection_indexes.extend(self.outgoing.get(node.as_str()).into_iter().flatten());
            connection_indexes.extend(self.incoming.get(node.as_str()).into_iter().flatten());
        }
        let mut connection_indexes: Vec<usize> = connection_indexes.into_iter().copied().collect();
        connection_indexes.sort_unstable();
        connection_indexes
            .into_iter()
            .map(|index| &self.memory.orders[index])
            .filter(|edge| selected.contains(&edge.previous) && selected.contains(&edge.next))
            .take(limit)
            .cloned()
            .collect()
    }
}

pub fn find_observed_path(
    memory: &Memory,
    from: &str,
    to: &str,
    max_focus_calls: usize,
    focus_limit: usize,
) -> Result<Option<FoundPath>, String> {
    if from == to {
        return memory
            .notes
            .contains(&from.to_string())
            .then_some(Some(FoundPath {
                turns: 0,
                tasks: Vec::new(),
            }))
            .ok_or_else(|| format!("unknown note in this memory: {from}"));
    }
    if !memory.notes.contains(&from.to_string()) {
        return Err(format!("unknown note in this memory: {from}"));
    }
    if !memory.notes.contains(&to.to_string()) {
        return Err(format!("unknown note in this memory: {to}"));
    }

    let snapshot_focus = SnapshotFocus::new(memory);
    let mut edges = BTreeMap::new();
    let mut queue = VecDeque::from([from.to_string()]);
    let mut scheduled = HashSet::from([from.to_string()]);
    let mut focused = HashSet::new();
    while focused.len() < max_focus_calls {
        let Some(node) = queue.pop_front() else {
            break;
        };
        if !focused.insert(node.clone()) {
            continue;
        }
        for edge in snapshot_focus.connections(&node, focus_limit) {
            for endpoint in [&edge.previous, &edge.next] {
                if scheduled.insert(endpoint.clone()) {
                    queue.push_back(endpoint.clone());
                }
            }
            edges.entry(edge.edge_id).or_insert(edge);
        }
        if let Some(path) =
            shortest_turn_path(&edges.values().cloned().collect::<Vec<_>>(), from, to)
        {
            return Ok(Some(path));
        }
    }
    Ok(None)
}

fn shortest_turn_path(edges: &[Order], from: &str, to: &str) -> Option<FoundPath> {
    let mut adjacency: HashMap<&str, Vec<&Order>> = HashMap::new();
    for edge in edges {
        adjacency.entry(&edge.previous).or_default().push(edge);
        adjacency.entry(&edge.next).or_default().push(edge);
    }
    for incident in adjacency.values_mut() {
        incident.sort_by_key(|edge| edge.edge_id);
    }
    let mut initial_names: Vec<String> = adjacency
        .get(from)?
        .iter()
        .map(|edge| edge.edge_name.clone())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    initial_names.sort();

    let mut distance = HashMap::new();
    let mut prior = HashMap::new();
    let mut queue = VecDeque::new();
    for edge_name in initial_names {
        let state = State {
            node: from.into(),
            edge_name,
        };
        distance.insert(state.clone(), 0usize);
        queue.push_back(state);
    }
    let mut destination = None;
    while let Some(state) = queue.pop_front() {
        let current_distance = distance[&state];
        if state.node == to {
            destination = Some(state);
            break;
        }
        for edge in adjacency.get(state.node.as_str()).into_iter().flatten() {
            let next_node = if edge.previous == state.node {
                &edge.next
            } else {
                &edge.previous
            };
            let turn = usize::from(edge.edge_name != state.edge_name);
            let candidate = current_distance + turn;
            let next_state = State {
                node: next_node.clone(),
                edge_name: edge.edge_name.clone(),
            };
            if distance
                .get(&next_state)
                .is_some_and(|known| *known <= candidate)
            {
                continue;
            }
            distance.insert(next_state.clone(), candidate);
            prior.insert(
                next_state.clone(),
                Step {
                    prior: state.clone(),
                    edge: (*edge).clone(),
                },
            );
            if turn == 0 {
                queue.push_front(next_state);
            } else {
                queue.push_back(next_state);
            }
        }
    }

    let destination = destination?;
    let turns = distance[&destination];
    let mut state = destination;
    let mut traversed = Vec::new();
    while let Some(step) = prior.get(&state) {
        traversed.push((
            step.prior.node.clone(),
            state.node.clone(),
            step.edge.clone(),
        ));
        state = step.prior.clone();
    }
    traversed.reverse();
    let mut tasks: Vec<TaskSegment> = Vec::new();
    for (enter, exit, edge) in traversed {
        match tasks.last_mut() {
            Some(task) if task.edge_name == edge.edge_name => task.sequence.push(exit),
            _ => tasks.push(TaskSegment {
                edge_name: edge.edge_name,
                sequence: vec![enter, exit],
            }),
        }
    }
    Some(FoundPath { turns, tasks })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn memory() -> Memory {
        Memory {
            memory_id: "test".into(),
            notes: ["A", "B", "C", "D", "E"]
                .into_iter()
                .map(String::from)
                .collect(),
            memos: HashMap::new(),
            orders: vec![
                Order {
                    edge_id: 1,
                    edge_name: "x".into(),
                    previous: "A".into(),
                    next: "B".into(),
                },
                Order {
                    edge_id: 2,
                    edge_name: "x".into(),
                    previous: "B".into(),
                    next: "C".into(),
                },
                Order {
                    edge_id: 3,
                    edge_name: "y".into(),
                    previous: "C".into(),
                    next: "D".into(),
                },
                Order {
                    edge_id: 4,
                    edge_name: "y".into(),
                    previous: "D".into(),
                    next: "E".into(),
                },
            ],
        }
    }

    #[test]
    fn repeatedly_focuses_one_snapshot_and_returns_sequences() {
        let path = find_observed_path(&memory(), "A", "E", 50, 2)
            .unwrap()
            .unwrap();
        assert_eq!(path.turns, 1);
        assert_eq!(path.tasks[0].sequence, ["A", "B", "C"]);
        assert_eq!(path.tasks[1].sequence, ["C", "D", "E"]);
    }

    #[test]
    fn bounded_observation_can_report_no_path() {
        assert_eq!(find_observed_path(&memory(), "A", "E", 1, 2).unwrap(), None);
    }
}
