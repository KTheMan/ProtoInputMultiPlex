//! Stable, profile-persisted endpoints for the bounded replication graph.
use crate::{
    Seat,
    automation::{CategoryFilter, ReplicationGraph},
};
use anyhow::{Context, Result, ensure};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const MAX_EDGES: usize = 4096;
pub const MAX_JSON_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_HISTORY: usize = 64;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PositionMode {
    /// Preserve the destination's existing anchor while mirroring source movement.
    #[default]
    Anchored,
    /// Place the destination relative to the immediate source user's position.
    SourceRelative,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct EdgeOptions {
    pub position: PositionMode,
    pub offset_x: i32,
    pub offset_y: i32,
    pub motion: bool,
    pub wheel: bool,
    pub horizontal_wheel: bool,
    pub buttons: [bool; 5],
    pub click_delay_ms: u32,
    /// Temporal cadence in milliseconds; this is not a motion gain.
    pub step_interval_ms: u32,
}

impl Default for EdgeOptions {
    fn default() -> Self {
        Self {
            position: PositionMode::Anchored,
            offset_x: 0,
            offset_y: 0,
            motion: true,
            wheel: true,
            horizontal_wheel: true,
            buttons: [true; 5],
            click_delay_ms: 0,
            step_interval_ms: 0,
        }
    }
}

impl EdgeOptions {
    pub fn validate(self) -> Result<()> {
        const OFFSET_LIMIT: i32 = 32_767;
        ensure!(
            (-OFFSET_LIMIT..=OFFSET_LIMIT).contains(&self.offset_x)
                && (-OFFSET_LIMIT..=OFFSET_LIMIT).contains(&self.offset_y),
            "replication offset is out of range"
        );
        ensure!(
            self.position == PositionMode::SourceRelative
                || (self.offset_x == 0 && self.offset_y == 0),
            "offsets require source-relative positioning"
        );
        ensure!(
            self.click_delay_ms <= 60_000,
            "replication click delay exceeds 60000 ms"
        );
        ensure!(
            self.step_interval_ms <= 1000,
            "replication step interval exceeds 1000 ms"
        );
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Edge {
    pub source: String,
    pub target: String,
    pub mouse: bool,
    pub keyboard: bool,
    pub text: bool,
    #[serde(default)]
    pub options: EdgeOptions,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ResolvedEdge {
    pub source: usize,
    pub target: usize,
    pub mouse: bool,
    pub keyboard: bool,
    pub text: bool,
    #[serde(default)]
    pub options: EdgeOptions,
}

impl Definition {
    /// Check ID-independent structure, including duplicate endpoints and cycles.
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.version == 1,
            "unsupported replication definition version"
        );
        ensure!(
            self.edges.len() <= MAX_EDGES,
            "replication graph has too many edges"
        );
        let mut nodes = BTreeSet::new();
        let mut pairs = BTreeSet::new();
        for edge in &self.edges {
            validate_id(&edge.source)?;
            validate_id(&edge.target)?;
            ensure!(
                edge.source != edge.target,
                "replication edge cannot target itself"
            );
            ensure!(
                edge.mouse || edge.keyboard || edge.text,
                "replication edge must enable an input category"
            );
            edge.options.validate()?;
            ensure!(
                pairs.insert((edge.source.as_str(), edge.target.as_str())),
                "duplicate replication edge"
            );
            nodes.insert(edge.source.as_str());
            nodes.insert(edge.target.as_str());
        }
        let identities: Vec<String> = nodes.into_iter().map(str::to_owned).collect();
        let indexes: BTreeMap<_, _> = identities
            .iter()
            .enumerate()
            .map(|(index, id)| (id.as_str(), index))
            .collect();
        let mut resolved = Vec::with_capacity(self.edges.len());
        for edge in &self.edges {
            resolved.push(ResolvedEdge {
                source: indexes[edge.source.as_str()],
                target: indexes[edge.target.as_str()],
                mouse: edge.mouse,
                keyboard: edge.keyboard,
                text: edge.text,
                options: edge.options,
            });
        }
        validate_paths(&resolved, &identities)
    }

    /// Resolve persistent endpoint identities to the current dense profile indexes.
    pub fn resolve(&self, seats: &[Seat]) -> Result<Vec<ResolvedEdge>> {
        self.validate()?;
        let mut indexes = BTreeMap::new();
        for (index, seat) in seats.iter().enumerate() {
            ensure!(
                indexes.insert(seat.identity(), index).is_none(),
                "duplicate seat identity"
            );
        }
        let mut result = Vec::with_capacity(self.edges.len());
        for edge in &self.edges {
            let source = *indexes
                .get(&edge.source)
                .with_context(|| format!("replication source seat is missing: {}", edge.source))?;
            let target = *indexes
                .get(&edge.target)
                .with_context(|| format!("replication target seat is missing: {}", edge.target))?;
            ensure!(
                !edge.keyboard && !edge.text || seats[target].keyboard.is_some(),
                "replication destination needs a keyboard"
            );
            result.push(ResolvedEdge {
                source,
                target,
                mouse: edge.mouse,
                keyboard: edge.keyboard,
                text: edge.text,
                options: edge.options,
            });
        }
        validate_resolved(
            &result,
            &seats.iter().map(Seat::identity).collect::<Vec<_>>(),
        )?;
        Ok(result)
    }

    /// Convert dense runtime indexes back to stable profile endpoint identities.
    pub fn from_resolved(seats: &[Seat], edges: &[ResolvedEdge]) -> Result<Self> {
        ensure!(
            edges.len() <= MAX_EDGES,
            "replication graph has too many edges"
        );
        let mut result = Self::default();
        for edge in edges {
            let source = seats
                .get(edge.source)
                .context("unknown replication source user")?;
            let target = seats
                .get(edge.target)
                .context("unknown replication target user")?;
            result.edges.push(Edge {
                source: source.identity(),
                target: target.identity(),
                mouse: edge.mouse,
                keyboard: edge.keyboard,
                text: edge.text,
                options: edge.options,
            });
        }
        result.validate()?;
        result.resolve(seats)?;
        Ok(result)
    }

    pub fn to_json(&self) -> Result<String> {
        self.validate()?;
        let json = serde_json::to_string(self)?;
        ensure!(
            json.len() <= MAX_JSON_BYTES,
            "replication JSON exceeds 4 MiB"
        );
        Ok(json)
    }

    pub fn from_json(json: &str) -> Result<Self> {
        ensure!(
            json.len() <= MAX_JSON_BYTES,
            "replication JSON exceeds 4 MiB"
        );
        let definition: Self = serde_json::from_str(json)?;
        definition.validate()?;
        Ok(definition)
    }
}

/// Input family used when asking the graph for its deterministic fan-out routes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Component {
    Motion,
    Button(u8),
    Wheel,
    HorizontalWheel,
    Keyboard,
    Text,
}

/// One target's canonical route from the original source. `hops` is authoritative for
/// placement and timing; the composed offset fields are diagnostics for the trailing run
/// of SourceRelative hops only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route {
    pub target: usize,
    pub path: Vec<usize>,
    pub position: PositionMode,
    pub offset_x: i32,
    pub offset_y: i32,
    pub click_delay_ms: u32,
    pub step_interval_ms: u32,
    pub hops: Vec<ResolvedEdge>,
}

#[derive(Debug, Clone)]
struct CandidatePath {
    path: Vec<usize>,
    stable_path: Vec<String>,
    hops: Vec<ResolvedEdge>,
}

/// Return one stable, deterministic route per reachable target. The graph is evaluated
/// once from the original source, so diamond paths do not feed emitted copies back into
/// this operation. When multiple paths reach a target, the lexicographically smallest
/// sequence of stable identity strings wins.
pub fn routes(
    edges: &[ResolvedEdge],
    identities: &[String],
    source: usize,
    component: Component,
) -> Result<Vec<Route>> {
    ensure!(source < identities.len(), "unknown replication source user");
    validate_resolved(edges, identities)?;
    routes_validated(edges, identities, source, component)
}

/// Validate the resolved graph and every canonical route composition that can be
/// selected for an input component. This is used by persisted definition validation,
/// before profile writes or runtime mutations. Default legacy graphs take a fast path.
pub fn validate_paths(edges: &[ResolvedEdge], identities: &[String]) -> Result<()> {
    validate_resolved(edges, identities)?;
    if edges
        .iter()
        .all(|edge| edge.options == EdgeOptions::default())
    {
        return Ok(());
    }

    let components = [
        Component::Motion,
        Component::Button(0),
        Component::Button(1),
        Component::Button(2),
        Component::Button(3),
        Component::Button(4),
        Component::Wheel,
        Component::HorizontalWheel,
        Component::Keyboard,
        Component::Text,
    ];
    for component in components {
        if !edges
            .iter()
            .any(|edge| enabled(*edge, component) && has_composed_options(*edge, component))
        {
            continue;
        }
        for source in 0..identities.len() {
            if !edges
                .iter()
                .any(|edge| edge.source == source && enabled(*edge, component))
            {
                continue;
            }
            routes_validated(edges, identities, source, component)?;
        }
    }
    Ok(())
}

fn has_composed_options(edge: ResolvedEdge, component: Component) -> bool {
    match component {
        Component::Motion => {
            edge.options.position != PositionMode::Anchored
                || edge.options.offset_x != 0
                || edge.options.offset_y != 0
                || edge.options.step_interval_ms != 0
        }
        Component::Button(_) => {
            edge.options.click_delay_ms != 0 || edge.options.step_interval_ms != 0
        }
        Component::Wheel | Component::HorizontalWheel | Component::Keyboard | Component::Text => {
            edge.options.step_interval_ms != 0
        }
    }
}

fn routes_validated(
    edges: &[ResolvedEdge],
    identities: &[String],
    source: usize,
    component: Component,
) -> Result<Vec<Route>> {
    ensure!(source < identities.len(), "unknown replication source user");
    if let Component::Button(button) = component {
        ensure!(button < 5, "unknown mouse button index");
    }

    let mut adjacency = vec![Vec::<ResolvedEdge>::new(); identities.len()];
    for edge in edges
        .iter()
        .copied()
        .filter(|edge| enabled(*edge, component))
    {
        adjacency[edge.source].push(edge);
    }
    for outgoing in &mut adjacency {
        outgoing.sort_by(|a, b| identities[a.target].cmp(&identities[b.target]));
    }

    let mut indegree = vec![0usize; identities.len()];
    for outgoing in &adjacency {
        for edge in outgoing {
            indegree[edge.target] += 1;
        }
    }
    let mut ready = BTreeSet::<(String, usize)>::new();
    for (index, degree) in indegree.iter().enumerate() {
        if *degree == 0 {
            ready.insert((identities[index].clone(), index));
        }
    }
    let mut topological = Vec::with_capacity(identities.len());
    while let Some((_, index)) = ready.pop_first() {
        topological.push(index);
        for edge in &adjacency[index] {
            indegree[edge.target] -= 1;
            if indegree[edge.target] == 0 {
                ready.insert((identities[edge.target].clone(), edge.target));
            }
        }
    }

    let mut best: Vec<Option<CandidatePath>> = vec![None; identities.len()];
    best[source] = Some(CandidatePath {
        path: vec![source],
        stable_path: vec![identities[source].clone()],
        hops: Vec::new(),
    });
    for node in topological.iter().copied() {
        let Some(candidate) = best[node].clone() else {
            continue;
        };
        for edge in &adjacency[node] {
            let mut next_path = candidate.path.clone();
            next_path.push(edge.target);
            let mut next_stable_path = candidate.stable_path.clone();
            next_stable_path.push(identities[edge.target].clone());
            let mut next_hops = candidate.hops.clone();
            next_hops.push(*edge);
            let replace = best[edge.target].as_ref().is_none_or(|existing| {
                next_stable_path.as_slice() < existing.stable_path.as_slice()
            });
            if replace {
                best[edge.target] = Some(CandidatePath {
                    path: next_path,
                    stable_path: next_stable_path,
                    hops: next_hops,
                });
            }
        }
    }

    let mut result = Vec::new();
    for target in topological {
        if target == source {
            continue;
        }
        let Some(candidate) = best[target].take() else {
            continue;
        };
        let CandidatePath { path, hops, .. } = candidate;
        let mut offset_x = 0i32;
        let mut offset_y = 0i32;
        let mut click_delay_ms = 0u32;
        let mut step_interval_ms = 0u32;
        for hop in &hops {
            click_delay_ms = click_delay_ms
                .checked_add(hop.options.click_delay_ms)
                .context("replication path click delay overflow")?;
            ensure!(
                click_delay_ms <= 60_000,
                "replication path click delay exceeds 60000 ms"
            );
            step_interval_ms = step_interval_ms.max(hop.options.step_interval_ms);
            if hop.options.position == PositionMode::SourceRelative {
                offset_x = offset_x
                    .checked_add(hop.options.offset_x)
                    .context("replication path X offset overflow")?;
                offset_y = offset_y
                    .checked_add(hop.options.offset_y)
                    .context("replication path Y offset overflow")?;
                ensure!(
                    (-32_767..=32_767).contains(&offset_x)
                        && (-32_767..=32_767).contains(&offset_y),
                    "replication path relative offset exceeds limit"
                );
            } else {
                offset_x = 0;
                offset_y = 0;
            }
        }
        let position = hops.last().expect("route has a final hop").options.position;
        result.push(Route {
            target,
            path,
            position,
            offset_x,
            offset_y,
            click_delay_ms,
            step_interval_ms,
            hops,
        });
    }
    Ok(result)
}

fn enabled(edge: ResolvedEdge, component: Component) -> bool {
    match component {
        Component::Motion => edge.mouse && edge.options.motion,
        Component::Button(button) => edge.mouse && edge.options.buttons[button as usize],
        Component::Wheel => edge.mouse && edge.options.wheel,
        Component::HorizontalWheel => edge.mouse && edge.options.horizontal_wheel,
        Component::Keyboard => edge.keyboard,
        Component::Text => edge.text,
    }
}

fn validate_resolved(edges: &[ResolvedEdge], identities: &[String]) -> Result<()> {
    ensure!(
        edges.len() <= MAX_EDGES,
        "replication graph has too many edges"
    );
    let mut ids = BTreeSet::new();
    for id in identities {
        validate_id(id)?;
        ensure!(ids.insert(id.as_str()), "duplicate seat identity");
    }
    let mut pairs = BTreeSet::new();
    let mut graph = ReplicationGraph::default();
    for edge in edges {
        ensure!(
            edge.source < identities.len(),
            "unknown replication source user"
        );
        ensure!(
            edge.target < identities.len(),
            "unknown replication target user"
        );
        ensure!(
            edge.source != edge.target,
            "replication edge cannot target itself"
        );
        ensure!(
            edge.mouse || edge.keyboard || edge.text,
            "replication edge must enable an input category"
        );
        edge.options.validate()?;
        ensure!(
            pairs.insert((edge.source, edge.target)),
            "duplicate replication edge"
        );
        graph.add_edge(
            edge.source,
            edge.target,
            CategoryFilter {
                mouse: edge.mouse,
                keyboard: edge.keyboard,
                text: edge.text,
            },
        )?;
    }
    graph.validate()
}

fn validate_id(id: &str) -> Result<()> {
    ensure!(
        !id.is_empty() && id.len() <= 128,
        "invalid replication endpoint ID"
    );
    ensure!(
        id.trim() == id && !id.chars().any(char::is_control),
        "invalid replication endpoint ID"
    );
    Ok(())
}

/// Bounded in-memory edit history for a draft graph. Seat resolution stays an Apply-time check.
#[derive(Debug, Clone)]
pub struct History {
    current: Definition,
    undo: Vec<Definition>,
    redo: Vec<Definition>,
}

impl History {
    pub fn new(initial: Definition) -> Result<Self> {
        initial.validate()?;
        Ok(Self {
            current: initial,
            undo: Vec::new(),
            redo: Vec::new(),
        })
    }

    pub fn current(&self) -> &Definition {
        &self.current
    }

    pub fn replace(&mut self, candidate: Definition) -> Result<()> {
        candidate.validate()?;
        if candidate == self.current {
            return Ok(());
        }
        self.undo
            .push(std::mem::replace(&mut self.current, candidate));
        if self.undo.len() > MAX_HISTORY {
            self.undo.remove(0);
        }
        self.redo.clear();
        Ok(())
    }

    pub fn undo(&mut self) -> bool {
        let Some(previous) = self.undo.pop() else {
            return false;
        };
        self.redo
            .push(std::mem::replace(&mut self.current, previous));
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(next) = self.redo.pop() else {
            return false;
        };
        self.undo.push(std::mem::replace(&mut self.current, next));
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seat(id: &str, mouse: &str, keyboard: bool) -> Seat {
        Seat {
            id: id.into(),
            name: id.into(),
            mouse: mouse.into(),
            keyboard: keyboard.then(|| format!("key-{id}")),
            target_pid: 100,
            desktop_start: None,
            cursor: Default::default(),
            annotation: Default::default(),
            magnifier: Default::default(),
            output: Default::default(),
        }
    }

    fn resolved(source: usize, target: usize, options: EdgeOptions) -> ResolvedEdge {
        ResolvedEdge {
            source,
            target,
            mouse: true,
            keyboard: true,
            text: true,
            options,
        }
    }

    fn ids(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn stable_ids_survive_reorder_and_removal_never_retargets() {
        let a = seat("a", "mouse-a", true);
        let b = seat("b", "mouse-b", true);
        let definition = Definition {
            edges: vec![Edge {
                source: a.identity(),
                target: b.identity(),
                mouse: true,
                keyboard: true,
                text: true,
                options: EdgeOptions::default(),
            }],
            ..Default::default()
        };
        assert_eq!(
            definition.resolve(&[a.clone(), b.clone()]).unwrap()[0].target,
            1
        );
        let reordered = definition.resolve(&[b.clone(), a.clone()]).unwrap();
        assert_eq!((reordered[0].source, reordered[0].target), (1, 0));
        assert!(definition.resolve(&[b]).is_err());
    }

    #[test]
    fn legacy_identity_is_stable_and_explicit_id_survives_device_change() {
        let mut legacy = seat("", "\\\\?\\HID#Mouse-A", true);
        legacy.id.clear();
        let id = legacy.identity();
        let mut case_variant = legacy.clone();
        case_variant.mouse = "\\\\?\\hid#mouse-a".into();
        assert_eq!(id, case_variant.identity());
        let mut explicit = seat("stable-seat", "mouse-old", true);
        explicit.mouse = "mouse-new".into();
        assert_eq!(explicit.identity(), "stable-seat");
    }

    #[test]
    fn graph_roundtrip_validates_diamond_and_categories() {
        let seats = [
            seat("a", "mouse-a", true),
            seat("b", "mouse-b", true),
            seat("c", "mouse-c", true),
            seat("d", "mouse-d", true),
        ];
        let definition = Definition {
            edges: vec![
                Edge {
                    source: "a".into(),
                    target: "b".into(),
                    mouse: true,
                    keyboard: false,
                    text: false,
                    options: EdgeOptions::default(),
                },
                Edge {
                    source: "a".into(),
                    target: "c".into(),
                    mouse: true,
                    keyboard: false,
                    text: false,
                    options: EdgeOptions::default(),
                },
                Edge {
                    source: "b".into(),
                    target: "d".into(),
                    mouse: true,
                    keyboard: false,
                    text: false,
                    options: EdgeOptions::default(),
                },
                Edge {
                    source: "c".into(),
                    target: "d".into(),
                    mouse: true,
                    keyboard: false,
                    text: false,
                    options: EdgeOptions::default(),
                },
            ],
            ..Default::default()
        };
        let json = definition.to_json().unwrap();
        assert_eq!(Definition::from_json(&json).unwrap(), definition);
        assert_eq!(definition.resolve(&seats).unwrap().len(), 4);
        let mut cycle = definition.clone();
        cycle.edges.push(Edge {
            source: "d".into(),
            target: "a".into(),
            mouse: true,
            keyboard: false,
            text: false,
            options: EdgeOptions::default(),
        });
        assert!(cycle.validate().is_err());
    }

    #[test]
    fn failed_history_replacement_is_atomic_and_undo_redo_roundtrips() {
        let initial = Definition::default();
        let mut history = History::new(initial.clone()).unwrap();
        let edge = Edge {
            source: "a".into(),
            target: "b".into(),
            mouse: true,
            keyboard: false,
            text: false,
            options: EdgeOptions::default(),
        };
        let changed = Definition {
            edges: vec![edge.clone()],
            ..Default::default()
        };
        history.replace(changed.clone()).unwrap();
        let invalid = Definition {
            edges: vec![
                edge.clone(),
                Edge {
                    source: "b".into(),
                    target: "a".into(),
                    mouse: true,
                    keyboard: false,
                    text: false,
                    options: EdgeOptions::default(),
                },
            ],
            ..Default::default()
        };
        assert!(history.replace(invalid).is_err());
        assert_eq!(history.current(), &changed);
        assert!(history.undo());
        assert_eq!(history.current(), &initial);
        assert!(history.redo());
        assert_eq!(history.current(), &changed);
    }

    #[test]
    fn missing_keyboard_destination_is_not_resolved() {
        let source = seat("source", "mouse-source", true);
        let target = seat("target", "mouse-target", false);
        let definition = Definition {
            edges: vec![Edge {
                source: source.identity(),
                target: target.identity(),
                mouse: false,
                keyboard: true,
                text: false,
                options: EdgeOptions::default(),
            }],
            ..Default::default()
        };
        assert!(definition.resolve(&[source, target]).is_err());
    }

    #[test]
    fn old_edge_json_gets_default_options_and_roundtrips() {
        let json = r#"{"version":1,"edges":[{"source":"a","target":"b","mouse":true,"keyboard":false,"text":false}]}"#;
        let definition = Definition::from_json(json).unwrap();
        assert_eq!(definition.edges[0].options, EdgeOptions::default());
        assert_eq!(
            Definition::from_json(&definition.to_json().unwrap()).unwrap(),
            definition
        );
    }

    #[test]
    fn filtered_components_apply_master_and_fine_grained_mouse_gates() {
        let identities = ids(&["a", "b"]);
        let options = EdgeOptions {
            motion: false,
            wheel: false,
            buttons: [true, false, true, true, true],
            ..Default::default()
        };
        let edge = resolved(0, 1, options);
        assert!(
            routes(&[edge], &identities, 0, Component::Motion)
                .unwrap()
                .is_empty()
        );
        assert!(
            routes(&[edge], &identities, 0, Component::Button(1))
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            routes(&[edge], &identities, 0, Component::Button(0))
                .unwrap()
                .len(),
            1
        );
        assert!(
            routes(&[edge], &identities, 0, Component::Wheel)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            routes(&[edge], &identities, 0, Component::HorizontalWheel)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            routes(&[edge], &identities, 0, Component::Keyboard)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            routes(&[edge], &identities, 0, Component::Text)
                .unwrap()
                .len(),
            1
        );
        assert!(routes(&[edge], &identities, 0, Component::Button(5)).is_err());

        let mut mouse_disabled = edge;
        mouse_disabled.mouse = false;
        assert!(
            routes(&[mouse_disabled], &identities, 0, Component::Button(0))
                .unwrap()
                .is_empty()
        );
        let horizontal_disabled = resolved(
            0,
            1,
            EdgeOptions {
                horizontal_wheel: false,
                ..Default::default()
            },
        );
        assert!(
            routes(
                &[horizontal_disabled],
                &identities,
                0,
                Component::HorizontalWheel
            )
            .unwrap()
            .is_empty()
        );
    }

    #[test]
    fn diamond_uses_one_canonical_path_and_stable_topological_order() {
        // Dense indexes deliberately disagree with stable-ID ordering.
        let identities = ids(&["source", "z-branch", "a-branch", "target"]);
        let defaults = EdgeOptions::default();
        let edges = [
            resolved(0, 1, defaults),
            resolved(1, 3, defaults),
            resolved(0, 2, defaults),
            resolved(2, 3, defaults),
        ];
        let planned = routes(&edges, &identities, 0, Component::Motion).unwrap();
        assert_eq!(
            planned.iter().map(|route| route.target).collect::<Vec<_>>(),
            [2, 1, 3]
        );
        let target = planned.iter().find(|route| route.target == 3).unwrap();
        assert_eq!(target.path, [0, 2, 3]);
        assert_eq!(target.hops.len(), 2);

        let reversed = routes(
            &edges.into_iter().rev().collect::<Vec<_>>(),
            &identities,
            0,
            Component::Motion,
        )
        .unwrap();
        let reversed_target = reversed.iter().find(|route| route.target == 3).unwrap();
        assert_eq!(reversed_target.path, target.path);
        assert_eq!(
            reversed
                .iter()
                .map(|route| route.target)
                .collect::<Vec<_>>(),
            [2, 1, 3]
        );
    }

    #[test]
    fn route_composes_relative_offsets_and_timing_with_anchor_reset() {
        let identities = ids(&["a", "b", "c", "d"]);
        let relative = |x, y, delay, interval| EdgeOptions {
            position: PositionMode::SourceRelative,
            offset_x: x,
            offset_y: y,
            click_delay_ms: delay,
            step_interval_ms: interval,
            ..Default::default()
        };
        let edges = [
            resolved(0, 1, relative(10, 20, 100, 30)),
            resolved(1, 2, relative(4, -5, 200, 10)),
            resolved(
                2,
                3,
                EdgeOptions {
                    click_delay_ms: 20,
                    step_interval_ms: 70,
                    ..Default::default()
                },
            ),
        ];
        let planned = routes(&edges, &identities, 0, Component::Button(0)).unwrap();
        let c = planned.iter().find(|route| route.target == 2).unwrap();
        assert_eq!(
            (c.offset_x, c.offset_y, c.click_delay_ms, c.step_interval_ms),
            (14, 15, 300, 30)
        );
        assert_eq!(c.position, PositionMode::SourceRelative);
        let d = planned.iter().find(|route| route.target == 3).unwrap();
        assert_eq!(
            (d.offset_x, d.offset_y, d.click_delay_ms, d.step_interval_ms),
            (0, 0, 320, 70)
        );
        assert_eq!(d.position, PositionMode::Anchored);
        assert_eq!(d.hops.len(), 3);
    }

    #[test]
    fn route_rejects_composed_resource_limits_and_bad_options() {
        let identities = ids(&["a", "b", "c"]);
        let relative = |x, delay| EdgeOptions {
            position: PositionMode::SourceRelative,
            offset_x: x,
            click_delay_ms: delay,
            ..Default::default()
        };
        assert!(
            routes(
                &[
                    resolved(0, 1, relative(20_000, 40_000)),
                    resolved(1, 2, relative(20_000, 20_001))
                ],
                &identities,
                0,
                Component::Motion,
            )
            .is_err()
        );
        assert!(
            routes(
                &[
                    resolved(0, 1, relative(0, 40_000)),
                    resolved(1, 2, relative(0, 20_001))
                ],
                &identities,
                0,
                Component::Motion,
            )
            .is_err()
        );
        let invalid = EdgeOptions {
            offset_x: 1,
            ..Default::default()
        };
        assert!(invalid.validate().is_err());
    }

    #[test]
    fn persisted_definition_rejects_composed_limits_before_serialization_or_history_mutation() {
        let delayed = |source: &str, target: &str, click_delay_ms| Edge {
            source: source.into(),
            target: target.into(),
            mouse: true,
            keyboard: false,
            text: false,
            options: EdgeOptions {
                click_delay_ms,
                ..Default::default()
            },
        };
        let invalid_delay = Definition {
            edges: vec![delayed("a", "b", 40_000), delayed("b", "c", 40_001)],
            ..Default::default()
        };
        assert!(invalid_delay.validate().is_err());
        assert!(invalid_delay.to_json().is_err());
        let raw = serde_json::to_string(&invalid_delay).unwrap();
        assert!(Definition::from_json(&raw).is_err());

        let relative_edge = |source: &str, target: &str| Edge {
            source: source.into(),
            target: target.into(),
            mouse: true,
            keyboard: false,
            text: false,
            options: EdgeOptions {
                position: PositionMode::SourceRelative,
                offset_x: 20_000,
                ..Default::default()
            },
        };
        let invalid_offset = Definition {
            edges: vec![relative_edge("a", "b"), relative_edge("b", "c")],
            ..Default::default()
        };
        assert!(invalid_offset.validate().is_err());

        let initial = Definition::default();
        let mut history = History::new(initial.clone()).unwrap();
        assert!(history.replace(invalid_delay).is_err());
        assert_eq!(history.current(), &initial);
    }
}
