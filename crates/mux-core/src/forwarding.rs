//! Stable-ID, single-successor input forwarding graphs.
use crate::Seat;
use anyhow::{Context, Result, ensure};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    hash::Hash,
};

pub const MAX_EDGES: usize = 4096;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Edge {
    pub source: String,
    pub target: String,
    #[serde(default)]
    pub keyboard: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Definition {
    pub version: u32,
    pub edges: Vec<Edge>,
}
impl Default for Definition {
    fn default() -> Self {
        Self {
            version: 1,
            edges: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolvedEdge {
    pub source: usize,
    pub target: usize,
    pub keyboard: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route {
    pub source: usize,
    pub target: usize,
    pub path: Vec<usize>,
}

impl Definition {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.version == 1,
            "unsupported forwarding definition version"
        );
        ensure!(
            self.edges.len() <= MAX_EDGES,
            "forwarding graph has too many edges"
        );
        let mut outgoing = BTreeSet::new();
        let mut pairs = BTreeSet::new();
        let mut next = HashMap::new();
        for edge in &self.edges {
            validate_id(&edge.source)?;
            validate_id(&edge.target)?;
            ensure!(
                edge.source != edge.target,
                "forwarding edge cannot target itself"
            );
            ensure!(
                outgoing.insert(edge.source.as_str()),
                "forwarding source has multiple outgoing edges"
            );
            ensure!(
                pairs.insert((edge.source.as_str(), edge.target.as_str())),
                "duplicate forwarding edge"
            );
            next.insert(edge.source.as_str(), edge.target.as_str());
        }
        ensure_acyclic(&next)?;
        Ok(())
    }

    pub fn resolve(&self, seats: &[Seat]) -> Result<Vec<ResolvedEdge>> {
        self.validate()?;
        let mut indexes = BTreeMap::new();
        for (index, seat) in seats.iter().enumerate() {
            ensure!(
                indexes.insert(seat.identity(), index).is_none(),
                "duplicate seat identity"
            );
        }
        let mut edges = Vec::with_capacity(self.edges.len());
        for edge in &self.edges {
            let source = *indexes
                .get(&edge.source)
                .with_context(|| format!("forwarding source seat is missing: {}", edge.source))?;
            let target = *indexes
                .get(&edge.target)
                .with_context(|| format!("forwarding target seat is missing: {}", edge.target))?;
            ensure!(
                !edge.keyboard || seats[target].keyboard.is_some(),
                "forwarding keyboard destination needs a keyboard"
            );
            edges.push(ResolvedEdge {
                source,
                target,
                keyboard: edge.keyboard,
            });
        }
        validate_resolved(&edges, &vec![true; seats.len()])?;
        Ok(edges)
    }

    pub fn from_resolved(seats: &[Seat], edges: &[ResolvedEdge]) -> Result<Self> {
        validate_resolved(edges, &vec![true; seats.len()])?;
        let mut definition = Self::default();
        for edge in edges {
            let source = seats
                .get(edge.source)
                .context("forwarding source index is out of range")?;
            let target = seats
                .get(edge.target)
                .context("forwarding target index is out of range")?;
            ensure!(
                !edge.keyboard || target.keyboard.is_some(),
                "forwarding keyboard destination needs a keyboard"
            );
            definition.edges.push(Edge {
                source: source.identity(),
                target: target.identity(),
                keyboard: edge.keyboard,
            });
        }
        definition.validate()?;
        Ok(definition)
    }
}

fn validate_id(id: &str) -> Result<()> {
    ensure!(
        !id.trim().is_empty()
            && id.trim() == id
            && id.len() <= 128
            && !id.chars().any(char::is_control),
        "invalid forwarding seat ID"
    );
    Ok(())
}

fn ensure_acyclic<T: Copy + Eq + Hash>(next: &HashMap<T, T>) -> Result<()> {
    let mut completed = HashSet::new();
    for &start in next.keys() {
        if completed.contains(&start) {
            continue;
        }
        let mut path = HashSet::new();
        let mut node = start;
        while let Some(&target) = next.get(&node) {
            if completed.contains(&node) {
                break;
            }
            ensure!(path.insert(node), "forwarding graph contains a cycle");
            node = target;
        }
        completed.extend(path);
    }
    Ok(())
}

pub fn validate_resolved(edges: &[ResolvedEdge], active: &[bool]) -> Result<()> {
    ensure!(
        edges.len() <= MAX_EDGES,
        "forwarding graph has too many edges"
    );
    let mut outgoing = BTreeSet::new();
    let mut pairs = BTreeSet::new();
    let mut next = HashMap::new();
    for edge in edges {
        ensure!(
            edge.source < active.len() && edge.target < active.len(),
            "forwarding endpoint index is out of range"
        );
        ensure!(
            active[edge.source] && active[edge.target],
            "forwarding endpoint is inactive"
        );
        ensure!(
            edge.source != edge.target,
            "forwarding edge cannot target itself"
        );
        ensure!(
            outgoing.insert(edge.source),
            "forwarding source has multiple outgoing edges"
        );
        ensure!(
            pairs.insert((edge.source, edge.target)),
            "duplicate forwarding edge"
        );
        next.insert(edge.source, edge.target);
    }
    ensure_acyclic(&next)?;
    Ok(())
}

pub fn route(edges: &[ResolvedEdge], source: usize, keyboard: bool) -> Option<Route> {
    let next: HashMap<usize, &ResolvedEdge> =
        edges.iter().map(|edge| (edge.source, edge)).collect();
    let mut path = vec![source];
    let mut current = source;
    let mut seen = BTreeSet::from([source]);
    while let Some(edge) = next.get(&current) {
        if keyboard && !edge.keyboard {
            break;
        }
        current = edge.target;
        if !seen.insert(current) {
            return None;
        }
        path.push(current);
    }
    if path.len() == 1 {
        None
    } else {
        Some(Route {
            source,
            target: *path.last()?,
            path,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn seats() -> Vec<Seat> {
        (0..4)
            .map(|i| Seat {
                id: format!("s{i}"),
                name: format!("Seat {i}"),
                mouse: format!("mouse{i}"),
                keyboard: Some(format!("key{i}")),
                target_pid: i + 1,
                desktop_start: None,
                cursor: Default::default(),
                annotation: Default::default(),
                magnifier: Default::default(),
                output: Default::default(),
            })
            .collect()
    }
    fn e(source: usize, target: usize, keyboard: bool) -> ResolvedEdge {
        ResolvedEdge {
            source,
            target,
            keyboard,
        }
    }

    #[test]
    fn chain_routes_pointer_and_keyboard_until_disabled_hop() {
        let edges = [e(0, 1, true), e(1, 2, false), e(2, 3, true)];
        assert_eq!(route(&edges, 0, false).unwrap().path, vec![0, 1, 2, 3]);
        assert_eq!(route(&edges, 0, true).unwrap().path, vec![0, 1]);
        assert_eq!(route(&edges, 1, true), None);
    }
    #[test]
    fn converging_chains_are_valid_and_routes_end_at_sink() {
        let edges = [e(0, 2, true), e(1, 2, true)];
        validate_resolved(&edges, &[true; 3]).unwrap();
        assert_eq!(route(&edges, 1, false).unwrap().target, 2);
    }
    #[test]
    fn rejects_invalid_graphs_and_inactive_endpoints() {
        for edges in [
            vec![e(0, 0, true)],
            vec![e(0, 1, true), e(0, 2, false)],
            vec![e(0, 1, true), e(1, 0, true)],
        ] {
            assert!(validate_resolved(&edges, &[true; 3]).is_err());
        }
        assert!(validate_resolved(&[e(0, 1, true)], &[true, false]).is_err());
        assert!(validate_resolved(&[e(0, 4, true)], &[true; 3]).is_err());
    }
    #[test]
    fn definition_round_trip_uses_stable_ids_and_checks_keyboard_target() {
        let seats = seats();
        let edges = [e(0, 1, true)];
        let def = Definition::from_resolved(&seats, &edges).unwrap();
        assert_eq!(def.edges[0].source, "s0");
        assert_eq!(def.resolve(&seats).unwrap(), edges);
        let legacy: Definition = serde_json::from_str("{}").unwrap();
        assert_eq!(legacy, Definition::default());
        let mut no_keyboard = seats.clone();
        no_keyboard[1].keyboard = None;
        assert!(def.resolve(&no_keyboard).is_err());
    }

    #[test]
    fn maximum_size_chain_validates_and_omitted_keyboard_is_pointer_only() {
        let edges: Vec<_> = (0..MAX_EDGES)
            .map(|source| e(source, source + 1, false))
            .collect();
        validate_resolved(&edges, &vec![true; MAX_EDGES + 1]).unwrap();
        assert_eq!(route(&edges, 0, false).unwrap().target, MAX_EDGES);
        let definition = Definition {
            edges: (0..MAX_EDGES)
                .map(|source| Edge {
                    source: format!("seat-{source}"),
                    target: format!("seat-{}", source + 1),
                    keyboard: false,
                })
                .collect(),
            ..Default::default()
        };
        definition.validate().unwrap();
        let pointer_only: Edge = serde_json::from_str(r#"{"source":"a","target":"b"}"#).unwrap();
        assert!(!pointer_only.keyboard);
    }
}
