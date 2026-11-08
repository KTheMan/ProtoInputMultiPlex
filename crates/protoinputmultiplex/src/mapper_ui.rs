//! Original structural authoring for the typed virtual-input mapper.
//! This page stages definitions and returns commands; it performs no I/O or input.
use crate::mapper_flow_store::{self, BrowsePage, Flow, MAX_BROWSE_ENTRIES, Preview, Store};
use crate::{mapper_runtime, ui_script::Script};
use eframe::egui::{self, Align2, Color32, Rect, Sense, Stroke, Vec2};
use mux_core::{
    Seat,
    mapper::{
        self, Definition, Edge, GroupEdit, Node, NodeCategory, NodeDescriptor, NodeGroup, NodeKind,
        PortRef, TemplateKind,
    },
};
use ouroboros_ui::{
    atoms::{Button, ButtonVariant, Checkbox, Divider, Heading, Input, Surface, Text},
    molecules::Collapsible,
    organisms::Select,
    tokens::core,
};
use rmcp::schemars;
use serde::Serialize;
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, SyncSender, TryRecvError},
    thread,
    time::{Duration, Instant},
};

const HISTORY_LIMIT: usize = 64;
const ERROR_LIMIT: usize = 1024;
const FORM_DEPTH: usize = 8;
const MONITOR_INTERVAL: Duration = Duration::from_millis(250);

#[derive(Clone, Debug)]
pub enum PageAction {
    Apply {
        command_id: u64,
        definition: Definition,
        expected_revision: u64,
    },
    Arm {
        command_id: u64,
        expected_revision: u64,
    },
    Disarm {
        command_id: u64,
        expected_revision: u64,
    },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub enum View {
    #[default]
    Graph,
    Nodes,
    Templates,
    Inspector,
    Activity,
    SavedFlows,
    Browser,
}

#[derive(Clone, Debug, Serialize)]
pub enum Pending {
    Apply {
        command_id: u64,
        definition: Definition,
        expected_revision: u64,
    },
    Arm {
        command_id: u64,
        expected_revision: u64,
    },
}

impl Pending {
    fn command_id(&self) -> u64 {
        match self {
            Self::Apply { command_id, .. } | Self::Arm { command_id, .. } => *command_id,
        }
    }

    fn expected_apply(&self, state: &mapper_runtime::State) -> bool {
        matches!(self, Self::Apply { definition, expected_revision, .. }
            if expected_revision.checked_add(1) == Some(state.revision) && *definition == state.definition)
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct DisarmPending {
    pub command_id: u64,
    pub expected_revision: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct ControlFlags {
    pub frame: u64,
    pub enabled: bool,
    pub visible: bool,
    pub whole_visible: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct Snapshot {
    pub draft: Definition,
    pub applied: Definition,
    pub revision: u64,
    pub sequence: u64,
    pub dirty: bool,
    pub parameter_dirty: bool,
    pub pending: Option<Pending>,
    pub disarm_pending: Option<DisarmPending>,
    pub armed: bool,
    pub cleanup_pending: bool,
    pub processed_inputs: u64,
    pub emitted_outputs: u64,
    pub rejected_inputs: u64,
    pub actor_contexts: usize,
    pub pending_timers: usize,
    pub timer_pulses: u64,
    pub pending_delays: usize,
    pub pending_observations: usize,
    pub observation_completions: u64,
    pub dropped_observations: u64,
    pub capture_worker_busy: bool,
    pub delayed_events: u64,
    pub selected_node: Option<String>,
    pub canvas_selection: Vec<String>,
    pub selected_group: Option<String>,
    pub view: View,
    pub validation: Option<String>,
    pub error: Option<String>,
    /// Fresh-frame geometry qualifications, never authorization or saved settings.
    pub controls: BTreeMap<String, ControlFlags>,
    pub saved_flows: Vec<String>,
    pub selected_flow: Option<String>,
    pub flow_operation_pending: bool,
    pub flow_error: Option<String>,
    pub flow_success: Option<String>,
    pub browser: Option<BrowserSnapshot>,
    pub palette: Option<PaletteSnapshot>,
}

#[derive(Clone, Debug, Serialize)]
pub struct PaletteSnapshot {
    pub query: String,
    pub category: Option<NodeCategory>,
    pub selected: Option<String>,
    pub matches: Vec<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub enum BrowserSource {
    #[default]
    Templates,
    Saved,
}

#[derive(Clone, Debug, Serialize)]
pub struct BrowserEntrySnapshot {
    pub name: String,
    pub available: bool,
    pub node_count: usize,
    pub edge_count: usize,
    pub group_count: usize,
}

#[derive(Clone, Debug, Serialize)]
pub struct BrowserSnapshot {
    pub source: BrowserSource,
    pub query: String,
    pub offset: usize,
    pub total: usize,
    pub pending: bool,
    pub entries: Vec<BrowserEntrySnapshot>,
}

#[derive(Clone)]
struct BrowserCard {
    name: String,
    summary: String,
    template: Option<usize>,
    preview: Option<Preview>,
    error: Option<String>,
}

struct FlowBrowser {
    source: BrowserSource,
    origin: View,
    search: String,
    query: String,
    offset: usize,
    generation: u64,
    requested: bool,
    pending: bool,
    scroll_to_top: bool,
    total: usize,
    cards: Vec<BrowserCard>,
    error: Option<String>,
}

impl Default for FlowBrowser {
    fn default() -> Self {
        Self {
            source: BrowserSource::Templates,
            origin: View::Templates,
            search: String::new(),
            query: String::new(),
            offset: 0,
            generation: 0,
            requested: false,
            pending: false,
            scroll_to_top: false,
            total: 0,
            cards: Vec::new(),
            error: None,
        }
    }
}

impl FlowBrowser {
    fn invalidate(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.requested = false;
        self.pending = true;
        self.scroll_to_top = true;
        self.total = 0;
        self.cards.clear();
        self.error = None;
    }
}

struct MonitorRequest {
    id: u64,
    revision: u64,
    run: u64,
}

pub struct MapperPage {
    draft: Definition,
    applied: Definition,
    revision: u64,
    sequence: u64,
    pending: Option<Pending>,
    disarm_pending: Option<DisarmPending>,
    next_command: u64,
    armed: bool,
    run: u64,
    monitor_pending: Option<MonitorRequest>,
    monitor_last_request: Option<Instant>,
    monitor_samples: Vec<mapper::MonitorSample>,
    scope_traces: Vec<mapper::ScopeTrace>,
    cleanup_pending: bool,
    processed_inputs: u64,
    emitted_outputs: u64,
    rejected_inputs: u64,
    actor_contexts: usize,
    pending_timers: usize,
    timer_pulses: u64,
    pending_delays: usize,
    pending_observations: usize,
    observation_completions: u64,
    dropped_observations: u64,
    capture_worker_busy: bool,
    ocr_available: bool,
    delayed_events: u64,
    error: Option<String>,
    worker_error: Option<String>,
    view: View,
    selected_node: Option<String>,
    selected_nodes: BTreeSet<String>,
    selected_group: Option<String>,
    group_label: String,
    canvas_select_mode: bool,
    marquee: Option<(egui::Pos2, egui::Pos2)>,
    selected_edge: Option<Edge>,
    catalogue: Vec<NodeDescriptor>,
    catalogue_index: usize,
    node_search: String,
    node_category: Option<NodeCategory>,
    node_filter_error: Option<String>,
    template_index: usize,
    template_source: Option<String>,
    template_target: Option<String>,
    parameter_value: Value,
    parameter_text: BTreeMap<String, String>,
    parameter_error: Option<String>,
    connection_sources: BTreeMap<(String, String), PortRef>,
    undo: Vec<Definition>,
    redo: Vec<Definition>,
    next_node: u64,
    canvas: bool,
    canvas_drag: Option<CanvasDrag>,
    canvas_wire_source: Option<PortRef>,
    canvas_wire_drag: Option<PortRef>,
    schema: Value,
    controls: BTreeMap<String, ControlFlags>,
    flow_store: Option<FlowService>,
    flow_names: Vec<String>,
    flow_selected: Option<String>,
    flow_name: String,
    flow_path: String,
    flow_error: Option<String>,
    flow_success: Option<String>,
    flow_load_baseline: Option<(Definition, Definition, u64)>,
    flow_initialized: bool,
    flow_browser: FlowBrowser,
}

enum FlowJob {
    Refresh,
    Browse {
        request: u64,
        query: String,
        offset: usize,
    },
    Save(Flow),
    Load(String),
    Import(PathBuf),
    Export(PathBuf, Flow),
}
enum FlowResult {
    Browsed { request: u64, page: BrowsePage },
    BrowseFailed { request: u64, error: String },
    Names(Vec<String>),
    Loaded(Flow),
    Saved,
    Exported,
    Failed(String),
}
struct FlowService {
    sender: SyncSender<(FlowJob, egui::Context)>,
    receiver: Receiver<FlowResult>,
    pending: bool,
    dead: bool,
    reported_dead: bool,
}
impl FlowService {
    fn new(root: &Path, guards: std::sync::Arc<crate::gui::RuntimeGuards>) -> Self {
        let store = Store::new(root);
        Self::with_operation(guards, move |job| match job {
            FlowJob::Refresh => store.list().map(FlowResult::Names),
            FlowJob::Browse {
                request,
                query,
                offset,
            } => Ok(match store.browse(&query, offset, MAX_BROWSE_ENTRIES) {
                Ok(page) => FlowResult::Browsed { request, page },
                Err(error) => FlowResult::BrowseFailed {
                    request,
                    error: format!("{error:#}"),
                },
            }),
            FlowJob::Save(flow) => store.save(&flow).map(|_| FlowResult::Saved),
            FlowJob::Load(name) => store.load(&name).map(FlowResult::Loaded),
            FlowJob::Import(path) => Store::import(&path).map(FlowResult::Loaded),
            FlowJob::Export(path, flow) => {
                Store::export(&path, &flow).map(|_| FlowResult::Exported)
            }
        })
    }

    fn with_operation(
        guards: std::sync::Arc<crate::gui::RuntimeGuards>,
        mut operation: impl FnMut(FlowJob) -> anyhow::Result<FlowResult> + Send + 'static,
    ) -> Self {
        let (sender, jobs) = mpsc::sync_channel::<(FlowJob, egui::Context)>(1);
        let (results, receiver) = mpsc::sync_channel(1);
        let dead = thread::Builder::new()
            .name("mapper-flow-library".into())
            .spawn(move || {
                let retained_guards = guards;
                while let Ok((job, ctx)) = jobs.recv() {
                    let browse_request = match &job {
                        FlowJob::Browse { request, .. } => Some(*request),
                        _ => None,
                    };
                    let result =
                        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| operation(job)));
                    let reply = match result {
                        Ok(Ok(value)) => value,
                        Ok(Err(error)) => FlowResult::Failed(format!("{error:#}")),
                        Err(_) => FlowResult::Failed(
                            "Mapper flow storage worker stopped unexpectedly.".into(),
                        ),
                    };
                    let reply = match (browse_request, reply) {
                        (Some(request), FlowResult::Failed(error)) => {
                            FlowResult::BrowseFailed { request, error }
                        }
                        (_, reply) => reply,
                    };
                    if results.try_send(reply).is_err() {
                        break;
                    }
                    ctx.request_repaint();
                }
                drop(operation);
                drop(retained_guards);
            })
            .is_err();
        Self {
            sender,
            receiver,
            pending: false,
            dead,
            reported_dead: false,
        }
    }
    fn start(&mut self, job: FlowJob, ctx: &egui::Context) -> bool {
        if self.pending || self.dead {
            return false;
        }
        self.pending = self.sender.try_send((job, ctx.clone())).is_ok();
        self.pending
    }
    fn poll(&mut self) -> Option<FlowResult> {
        if self.dead {
            if self.reported_dead {
                return None;
            }
            self.reported_dead = true;
            return Some(FlowResult::Failed(
                "Mapper flow storage worker is unavailable.".into(),
            ));
        }
        match self.receiver.try_recv() {
            Ok(result) => {
                self.pending = false;
                Some(result)
            }
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                self.pending = false;
                self.dead = true;
                self.reported_dead = true;
                Some(FlowResult::Failed(
                    "Mapper flow storage worker is unavailable.".into(),
                ))
            }
        }
    }
}

struct CanvasDrag {
    id: String,
    grab_offset: Vec2,
    origins: BTreeMap<String, [f64; 2]>,
    delta: [f64; 2],
}

impl CanvasDrag {
    fn move_to(&mut self, position: [f64; 2]) {
        let anchor = self.origins[&self.id];
        self.delta = std::array::from_fn(|axis| {
            let (min, max) = self
                .origins
                .values()
                .fold((1.0_f64, 0.0_f64), |(min, max), origin| {
                    (min.min(origin[axis]), max.max(origin[axis]))
                });
            (position[axis] - anchor[axis]).clamp(-min, 1.0 - max)
        });
    }

    fn position(&self, id: &str) -> Option<[f64; 2]> {
        self.origins
            .get(id)
            .map(|origin| std::array::from_fn(|axis| origin[axis] + self.delta[axis]))
    }
}

#[derive(Clone)]
struct CanvasPortHandle {
    reference: PortRef,
    signal: mapper::SignalType,
    rect: Rect,
    output: bool,
}

impl Default for MapperPage {
    fn default() -> Self {
        Self {
            draft: Definition::default(),
            applied: Definition::default(),
            revision: 0,
            sequence: 0,
            pending: None,
            disarm_pending: None,
            next_command: 1,
            armed: false,
            run: 0,
            monitor_pending: None,
            monitor_last_request: None,
            monitor_samples: Vec::new(),
            scope_traces: Vec::new(),
            cleanup_pending: false,
            processed_inputs: 0,
            emitted_outputs: 0,
            rejected_inputs: 0,
            actor_contexts: 0,
            pending_timers: 0,
            timer_pulses: 0,
            pending_delays: 0,
            pending_observations: 0,
            observation_completions: 0,
            dropped_observations: 0,
            capture_worker_busy: false,
            ocr_available: false,
            delayed_events: 0,
            error: None,
            worker_error: None,
            view: View::Graph,
            selected_node: None,
            selected_group: None,
            group_label: String::new(),
            selected_nodes: BTreeSet::new(),
            canvas_select_mode: false,
            marquee: None,
            selected_edge: None,
            catalogue: mapper::catalogue(),
            catalogue_index: usize::MAX,
            node_search: String::new(),
            node_category: None,
            node_filter_error: None,
            template_index: 0,
            template_source: None,
            template_target: None,
            parameter_value: Value::Null,
            parameter_text: BTreeMap::new(),
            parameter_error: None,
            connection_sources: BTreeMap::new(),
            undo: Vec::new(),
            redo: Vec::new(),
            next_node: 1,
            canvas: true,
            canvas_drag: None,
            canvas_wire_source: None,
            canvas_wire_drag: None,
            schema: serde_json::to_value(schemars::schema_for!(NodeKind)).unwrap_or(Value::Null),
            controls: BTreeMap::new(),
            flow_store: None,
            flow_names: Vec::new(),
            flow_selected: None,
            flow_name: String::new(),
            flow_path: String::new(),
            flow_error: None,
            flow_load_baseline: None,
            flow_success: None,
            flow_initialized: false,
            flow_browser: FlowBrowser::default(),
        }
    }
}

impl MapperPage {
    pub fn set_ocr_available(&mut self, available: bool) {
        self.ocr_available = available;
    }
    pub fn with_flow_root(
        data_root: &Path,
        guards: std::sync::Arc<crate::gui::RuntimeGuards>,
    ) -> Self {
        Self {
            flow_store: Some(FlowService::new(data_root, guards)),
            ..Self::default()
        }
    }
    pub fn reset_profile(&mut self, definition: &Definition) {
        self.close_flow_browser();
        self.flow_load_baseline = None;
        self.canvas_drag = None;
        self.clear_canvas_wire();
        self.reset_monitors();
        self.draft = definition.clone();
        self.applied = definition.clone();
        self.revision = 0;
        self.sequence = 0;
        self.pending = None;
        self.disarm_pending = None;
        self.armed = false;
        self.cleanup_pending = false;
        self.processed_inputs = 0;
        self.emitted_outputs = 0;
        self.rejected_inputs = 0;
        self.actor_contexts = 0;
        self.pending_timers = 0;
        self.timer_pulses = 0;
        self.pending_delays = 0;
        self.pending_observations = 0;
        self.observation_completions = 0;
        self.dropped_observations = 0;
        self.capture_worker_busy = false;
        self.delayed_events = 0;
        self.error = None;
        self.worker_error = None;
        self.selected_node = None;
        self.selected_nodes.clear();
        self.selected_group = None;
        self.group_label.clear();
        self.marquee = None;
        self.selected_edge = None;
        self.parameter_value = Value::Null;
        self.parameter_text.clear();
        self.parameter_error = None;
        self.connection_sources.clear();
        self.template_source = None;
        self.template_target = None;
        self.undo.clear();
        self.redo.clear();
        self.controls.clear();
    }

    pub fn has_profile_draft(&self) -> bool {
        self.waiting() || self.draft != self.applied || self.parameter_dirty()
    }

    /// A terminal worker stop retires runtime authority while preserving unsubmitted authoring.
    pub fn session_stopped(&mut self, definition: &Definition) {
        self.flow_load_baseline = None;
        self.cancel_canvas_interaction();
        self.reset_monitors();
        let dirty =
            self.draft != self.applied || self.draft != *definition || self.parameter_dirty();
        self.applied = definition.clone();
        self.pending = None;
        self.disarm_pending = None;
        self.revision = 0;
        self.sequence = 0;
        self.armed = false;
        self.cleanup_pending = false;
        self.processed_inputs = 0;
        self.emitted_outputs = 0;
        self.rejected_inputs = 0;
        self.actor_contexts = 0;
        self.pending_timers = 0;
        self.timer_pulses = 0;
        self.pending_delays = 0;
        self.pending_observations = 0;
        self.observation_completions = 0;
        self.dropped_observations = 0;
        self.capture_worker_busy = false;
        self.delayed_events = 0;
        self.error = None;
        self.worker_error = None;
        self.controls.clear();
        if !dirty {
            self.draft = definition.clone();
            self.reconcile_group_selection();
            self.undo.clear();
            self.redo.clear();
            let selected = self.selected_node.clone();
            self.select_node(selected.as_deref());
        } else if self
            .selected_node
            .as_ref()
            .is_some_and(|id| !self.draft.nodes.iter().any(|node| &node.id == id))
        {
            self.select_node(None);
        }
    }

    fn parameter_dirty(&self) -> bool {
        self.parameter_error.is_some()
            || self
                .selected_node
                .as_ref()
                .and_then(|id| self.draft.nodes.iter().find(|node| &node.id == id))
                .and_then(|node| serde_json::to_value(&node.kind).ok())
                .is_some_and(|value| value != self.parameter_value)
    }

    pub(crate) fn worker_error_text(&self) -> Option<&str> {
        self.worker_error.as_deref()
    }

    pub fn can_stage_screen_template(&self) -> bool {
        self.view == View::Inspector
            && !self.waiting()
            && !self.armed
            && !self.cleanup_pending
            && self
                .selected_node
                .as_ref()
                .and_then(|id| self.draft.nodes.iter().find(|node| &node.id == id))
                .is_some_and(|node| matches!(node.kind, NodeKind::ImageMatch { .. }))
    }
    pub fn stage_screen_template(&mut self, template: &str) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.can_stage_screen_template(),
            "Select a disarmed Image Match node in Inspector first"
        );
        anyhow::ensure!(
            template.len() <= crate::screen_analysis::MAX_TEMPLATE_BASE64_BYTES,
            "Template exceeds the encoded size limit"
        );
        let _ = png_template_summary(template)?;
        self.parameter_value["template_png_base64"] = Value::String(template.to_owned());
        self.parameter_text.remove("template_png_base64");
        Ok(())
    }
    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            draft: self.draft.clone(),
            applied: self.applied.clone(),
            revision: self.revision,
            sequence: self.sequence,
            dirty: self.draft != self.applied,
            parameter_dirty: self.parameter_dirty(),
            pending: self.pending.clone(),
            disarm_pending: self.disarm_pending.clone(),
            armed: self.armed,
            cleanup_pending: self.cleanup_pending,
            processed_inputs: self.processed_inputs,
            emitted_outputs: self.emitted_outputs,
            rejected_inputs: self.rejected_inputs,
            actor_contexts: self.actor_contexts,
            pending_timers: self.pending_timers,
            timer_pulses: self.timer_pulses,
            pending_delays: self.pending_delays,
            pending_observations: self.pending_observations,
            observation_completions: self.observation_completions,
            dropped_observations: self.dropped_observations,
            capture_worker_busy: self.capture_worker_busy,
            delayed_events: self.delayed_events,
            selected_node: self.selected_node.clone(),
            canvas_selection: self.selected_nodes.iter().cloned().collect(),
            selected_group: self.selected_group.clone(),
            view: self.view,
            validation: self.validation(),
            error: self.error.clone().or_else(|| self.worker_error.clone()),
            controls: self.controls.clone(),
            saved_flows: self.flow_names.clone(),
            selected_flow: self.flow_selected.clone(),
            flow_operation_pending: self
                .flow_store
                .as_ref()
                .is_some_and(|service| service.pending),
            flow_error: self.flow_error.clone(),
            flow_success: self.flow_success.clone(),
            palette: self.palette_visible().then(|| {
                let matches = mapper::browse_catalogue(&self.node_search, self.node_category)
                    .unwrap_or_default();
                PaletteSnapshot {
                    query: self.node_search.clone(),
                    category: self.node_category,
                    selected: self
                        .catalogue
                        .get(self.catalogue_index)
                        .filter(|item| matches.iter().any(|entry| entry.name == item.name))
                        .map(|item| item.name.clone()),
                    matches: matches.into_iter().map(|item| item.name).collect(),
                }
            }),
            browser: (self.view == View::Browser).then(|| BrowserSnapshot {
                source: self.flow_browser.source,
                query: self.flow_browser.query.clone(),
                offset: self.flow_browser.offset,
                total: self.flow_browser.total,
                pending: self.flow_browser.pending,
                entries: self
                    .flow_browser
                    .cards
                    .iter()
                    .map(|card| BrowserEntrySnapshot {
                        name: card.name.clone(),
                        available: card.preview.is_some() && card.error.is_none(),
                        node_count: card
                            .preview
                            .as_ref()
                            .map_or(0, |preview| preview.node_count),
                        edge_count: card
                            .preview
                            .as_ref()
                            .map_or(0, |preview| preview.edge_count),
                        group_count: card
                            .preview
                            .as_ref()
                            .map_or(0, |preview| preview.group_count),
                    })
                    .collect(),
            }),
        }
    }

    fn waiting(&self) -> bool {
        self.pending.is_some() || self.disarm_pending.is_some()
    }

    fn command_id(&mut self) -> Option<u64> {
        let id = self.next_command;
        let Some(next) = id.checked_add(1) else {
            self.error = Some(
                "Mapper command identities are exhausted; restart this configuration session."
                    .into(),
            );
            return None;
        };
        self.next_command = next;
        Some(id)
    }

    fn reset_monitors(&mut self) {
        self.run = 0;
        self.monitor_pending = None;
        self.monitor_last_request = None;
        self.monitor_samples.clear();
        self.scope_traces.clear();
    }

    fn monitor_active(&self) -> bool {
        self.armed && !self.cleanup_pending && self.disarm_pending.is_none()
    }

    /// Poll only from the visible mapper page after processing its actions.
    pub(crate) fn monitor_request(&mut self, now: Instant) -> Option<u64> {
        if self.view != View::Activity
            || !self.monitor_active()
            || self.monitor_pending.is_some()
            || self
                .monitor_last_request
                .is_some_and(|last| now.saturating_duration_since(last) < MONITOR_INTERVAL)
            || !self.applied.nodes.iter().any(|node| {
                node.kind.latest_observer_signal().is_some()
                    || node.kind.trace_observer_signal().is_some()
            })
        {
            return None;
        }
        let id = self.command_id()?;
        self.monitor_pending = Some(MonitorRequest {
            id,
            revision: self.revision,
            run: self.run,
        });
        self.monitor_last_request = Some(now);
        Some(id)
    }

    pub(crate) fn receive_monitors(&mut self, receipt: &mapper_runtime::MonitorReceipt) {
        let Some(pending) = self
            .monitor_pending
            .take_if(|pending| pending.id == receipt.request_id)
        else {
            return;
        };
        let snapshot = &receipt.snapshot;
        if self.view == View::Activity
            && self.monitor_active()
            && pending.revision == self.revision
            && pending.run == self.run
            && snapshot.revision == self.revision
            && snapshot.run == self.run
        {
            self.monitor_samples = snapshot
                .samples
                .iter()
                .filter(|sample| {
                    self.applied.nodes.iter().any(|node| {
                        node.id == sample.node
                            && node.kind.latest_observer_signal() == Some(sample.value.signal())
                    })
                })
                .take(mapper::MAX_MONITORS)
                .cloned()
                .collect();
            self.scope_traces = snapshot
                .traces
                .iter()
                .filter(|trace| {
                    self.applied.nodes.iter().any(|node| {
                        node.id == trace.node
                            && node.kind.trace_observer_signal().is_some()
                            && trace.samples.len() <= mapper::MAX_SCOPE_SAMPLES
                            && trace.samples.iter().all(|sample| {
                                sample.value.signal() == node.kind.trace_observer_signal().unwrap()
                            })
                    })
                })
                .take(mapper::MAX_MONITORS)
                .cloned()
                .collect();
        }
    }

    pub fn request_failed(&mut self, command_id: u64, error: impl Into<String>) {
        if self
            .monitor_pending
            .take_if(|pending| pending.id == command_id)
            .is_some()
        {
            return;
        }
        let primary = self
            .pending
            .as_ref()
            .is_some_and(|pending| pending.command_id() == command_id);
        let disarm = self
            .disarm_pending
            .as_ref()
            .is_some_and(|pending| pending.command_id == command_id);
        if !primary && !disarm {
            return;
        }
        if primary {
            self.pending = None;
        }
        if disarm {
            self.disarm_pending = None;
        }
        self.error = Some(bounded(&error.into()));
    }

    /// The stopped configuration path acknowledges its own staged Apply without arming.
    pub fn applied_local(&mut self, command_id: u64, definition: &Definition) {
        if !matches!(&self.pending, Some(Pending::Apply { command_id: pending_id, definition: candidate, .. })
            if *pending_id == command_id && candidate == definition)
        {
            return;
        }
        self.applied = definition.clone();
        self.pending = None;
        self.armed = false;
        self.monitor_samples.clear();
        self.scope_traces.clear();
        self.error = None;
    }

    /// A direct, correlated reply completes exactly one command. Publications cannot commit profiles.
    pub fn receive_reply(
        &mut self,
        command_id: u64,
        state: &mapper_runtime::State,
    ) -> Option<Definition> {
        if state.reply_to != Some(command_id) {
            return None;
        }
        let primary = self
            .pending
            .as_ref()
            .is_some_and(|pending| pending.command_id() == command_id);
        let disarm = self
            .disarm_pending
            .as_ref()
            .is_some_and(|pending| pending.command_id == command_id);
        if !primary && !disarm {
            return None;
        }
        let mut committed = None;
        let matches = if primary {
            match self.pending.take().expect("matched pending command") {
                Pending::Apply {
                    definition,
                    expected_revision,
                    ..
                } => {
                    let matched = state.acknowledged
                        && expected_revision.checked_add(1) == Some(state.revision)
                        && state.revision >= self.revision
                        && (state.revision > self.revision || self.applied == definition)
                        && state.definition == definition
                        && !state.armed
                        && !state.cleanup_pending
                        && state.error.is_none();
                    if matched {
                        committed = Some(definition);
                    }
                    matched
                }
                Pending::Arm {
                    expected_revision, ..
                } => {
                    state.acknowledged
                        && state.revision == expected_revision
                        && state.armed
                        && !state.cleanup_pending
                        && state.error.is_none()
                }
            }
        } else {
            let pending = self.disarm_pending.take().expect("matched disarm command");
            state.acknowledged
                && state.revision == pending.expected_revision
                && !state.armed
                && state.error.is_none()
        };
        if state.sequence >= self.sequence {
            self.error = state.error.as_deref().map(bounded).or_else(|| (!matches).then(||
                "Mapper acknowledgement did not match this command; inspect before retrying.".into()));
        } else if !matches && self.error.is_none() {
            self.error = Some("An earlier mapper command was not acknowledged; inspect current state before retrying.".into());
        }
        self.publish(state);
        committed
    }

    /// An expected Apply publication may precede its direct reply; retain that command's identity.
    pub fn receive_external(&mut self, state: &mapper_runtime::State) {
        if state.sequence < self.sequence || state.revision < self.revision {
            return;
        }
        let changed = state.definition != self.applied || state.revision != self.revision;
        if changed
            && self
                .pending
                .as_ref()
                .is_some_and(|pending| !pending.expected_apply(state))
        {
            self.pending = None;
            self.error = Some(
                "Mapper changed externally while a command was pending; inspect before retrying."
                    .into(),
            );
        }
        self.publish(state);
    }

    fn publish(&mut self, state: &mapper_runtime::State) {
        // `run` is only an actor namespace; sequence orders all authority and telemetry publications.
        if state.sequence < self.sequence || state.revision < self.revision {
            return;
        }
        // Worker diagnostics have their own provenance. A newer accepted
        // worker recovery clears only that diagnostic, never local command or
        // authoring errors, even if their wording happens to be identical.
        self.worker_error = state.error.as_deref().map(bounded);
        let clean = self.draft == self.applied && !self.parameter_dirty();
        let changed = state.definition != self.applied || state.revision != self.revision;
        if changed || !state.armed || state.run != self.run {
            self.monitor_samples.clear();
            self.scope_traces.clear();
        }
        self.run = state.run;
        self.applied = state.definition.clone();
        self.revision = state.revision;
        self.sequence = state.sequence;
        self.armed = state.armed;
        self.cleanup_pending = state.cleanup_pending;
        self.processed_inputs = state.processed_inputs;
        self.emitted_outputs = state.emitted_outputs;
        self.rejected_inputs = state.rejected_inputs;
        self.actor_contexts = state.actor_contexts;
        self.pending_timers = state.pending_timers;
        self.timer_pulses = state.timer_pulses;
        self.pending_delays = state.pending_delays;
        self.pending_observations = state.pending_observations;
        self.observation_completions = state.observation_completions;
        self.dropped_observations = state.dropped_observations;
        self.capture_worker_busy = state.capture_worker_busy;
        self.delayed_events = state.delayed_events;
        if clean && !self.waiting() && changed {
            self.cancel_canvas_interaction();
            self.selected_edge = None;
            self.draft = self.applied.clone();
            self.undo.clear();
            self.redo.clear();
            let selected = self.selected_node.clone();
            self.select_node(selected.as_deref());
        }
    }

    fn validation(&self) -> Option<String> {
        self.parameter_error
            .clone()
            .or_else(|| {
                self.parameter_dirty()
                    .then(|| "Update or discard the edited node parameters before Apply.".into())
            })
            .or_else(|| {
                self.draft
                    .validate()
                    .err()
                    .map(|error| bounded(&format!("{error:#}")))
            })
    }

    fn replace(&mut self, mut definition: Definition) {
        for group in &mut definition.groups {
            group
                .nodes
                .retain(|id| definition.nodes.iter().any(|node| &node.id == id));
        }
        definition.groups.retain(|group| !group.nodes.is_empty());
        self.flow_success = None;
        if definition == self.draft {
            return;
        }
        self.canvas_drag = None;
        self.marquee = None;
        self.clear_canvas_wire();
        if self.undo.len() == HISTORY_LIMIT {
            self.undo.remove(0);
        }
        self.undo.push(self.draft.clone());
        self.redo.clear();
        self.draft = definition;
        self.reconcile_group_selection();
        self.selected_nodes
            .retain(|id| self.draft.nodes.iter().any(|node| &node.id == id));
        if self
            .selected_edge
            .as_ref()
            .is_some_and(|edge| !self.draft.edges.contains(edge))
        {
            self.selected_edge = None;
        }
        self.error = None;
    }

    pub fn leave_page(&mut self) {
        self.close_flow_browser();
        self.flow_load_baseline = None;
        self.cancel_canvas_interaction();
    }

    fn cancel_canvas_interaction(&mut self) {
        self.clear_canvas_gestures();
        self.selected_nodes.clear();
        self.selected_group = None;
        self.canvas_select_mode = false;
    }

    fn clear_canvas_gestures(&mut self) {
        self.canvas_drag = None;
        self.marquee = None;
        self.clear_canvas_wire();
    }

    fn clear_canvas_wire(&mut self) {
        self.canvas_wire_source = None;
        self.canvas_wire_drag = None;
    }

    fn insert_canvas_edge(&mut self, from: &PortRef, to: &PortRef) -> bool {
        if self.waiting()
            || self.parameter_dirty()
            || self.cleanup_pending
            || from.node == to.node
            || self.draft.edges.len() >= mapper::MAX_EDGES
        {
            self.error = Some(
                "Connection unavailable: finish pending mapper edits and choose different nodes."
                    .into(),
            );
            return false;
        }
        let output = self
            .draft
            .nodes
            .iter()
            .find(|n| n.id == from.node)
            .and_then(|n| {
                n.kind
                    .ports()
                    .outputs
                    .into_iter()
                    .find(|p| p.name == from.port)
            });
        let input = self
            .draft
            .nodes
            .iter()
            .find(|n| n.id == to.node)
            .and_then(|n| {
                n.kind
                    .ports()
                    .inputs
                    .into_iter()
                    .find(|p| p.name == to.port)
            });
        if output
            .as_ref()
            .zip(input.as_ref())
            .is_none_or(|(a, b)| a.signal != b.signal)
            || self.draft.edges.iter().any(|e| e.to == *to)
        {
            self.error = Some("Connection rejected: choose an existing output and an unconnected input with the same signal type.".into());
            return false;
        }
        let mut next = self.draft.clone();
        next.edges.push(Edge {
            from: from.clone(),
            to: to.clone(),
        });
        if path_reaches(&self.draft, &to.node, &from.node) {
            self.error = Some("Connection rejected: this wire would create a cycle.".into());
            return false;
        }
        self.replace(next);
        true
    }

    fn fresh_node_id(&mut self) -> Option<String> {
        for _ in 0..=self.draft.nodes.len() {
            let candidate = format!("node-{}", self.next_node);
            self.next_node = self.next_node.checked_add(1)?;
            if !self.draft.nodes.iter().any(|node| node.id == candidate) {
                return Some(candidate);
            }
        }
        None
    }

    fn duplicate_node(&mut self, id: &str) {
        let Some(source) = self.draft.nodes.iter().find(|node| node.id == id).cloned() else {
            return;
        };
        if is_input_source(&source.kind)
            || self.draft.nodes.len() >= mapper::MAX_NODES
            || self.parameter_dirty()
        {
            return;
        }
        let Some(new_id) = self.fresh_node_id() else {
            self.error = Some("A fresh stable node ID could not be allocated.".into());
            return;
        };
        let mut next = self.draft.clone();
        next.nodes.push(Node {
            id: new_id.clone(),
            kind: source.kind,
            position: None,
        });
        self.replace(next);
        self.select_node(Some(&new_id));
        self.view = View::Inspector;
    }

    fn duplicate_selected_nodes(&mut self) {
        if self.parameter_dirty()
            || self.waiting()
            || self.cleanup_pending
            || self.selected_nodes.is_empty()
        {
            return;
        }
        let selected: Vec<_> = self
            .draft
            .nodes
            .iter()
            .filter(|n| self.selected_nodes.contains(&n.id))
            .cloned()
            .collect();
        if selected.is_empty()
            || selected.iter().any(|n| is_input_source(&n.kind))
            || self.draft.nodes.len().saturating_add(selected.len()) > mapper::MAX_NODES
        {
            return;
        }
        let selected_ids: BTreeSet<_> = selected.iter().map(|n| n.id.clone()).collect();
        let copied_edges: Vec<_> = self
            .draft
            .edges
            .iter()
            .filter(|e| selected_ids.contains(&e.from.node) && selected_ids.contains(&e.to.node))
            .cloned()
            .collect();
        if self.draft.edges.len().saturating_add(copied_edges.len()) > mapper::MAX_EDGES {
            return;
        }
        let mut counter = self.next_node;
        let mut ids = BTreeMap::new();
        for node in &selected {
            loop {
                let Some(next) = counter.checked_add(1) else {
                    self.error = Some("A fresh stable node ID could not be allocated.".into());
                    return;
                };
                let candidate = format!("node-{counter}");
                counter = next;
                if !self.draft.nodes.iter().any(|n| n.id == candidate)
                    && !ids.values().any(|v| v == &candidate)
                {
                    ids.insert(node.id.clone(), candidate);
                    break;
                }
            }
        }
        let mut next = self.draft.clone();
        let normalized = |position: [f64; 2]| position.map(|value| value.clamp(0.0, 1.0));
        let positioned: Vec<_> = selected
            .iter()
            .filter_map(|node| node.position.map(normalized))
            .collect();
        let offset = [0.10_f64, 0.22_f64];
        let delta: [f64; 2] = std::array::from_fn(|axis| {
            let (min, max) = positioned
                .iter()
                .fold((1.0_f64, 0.0_f64), |(min, max), position| {
                    (min.min(position[axis]), max.max(position[axis]))
                });
            let forward = offset[axis].min(1.0 - max);
            let backward = offset[axis].min(min);
            if forward >= backward {
                forward
            } else {
                -backward
            }
        });
        for node in &selected {
            let mut copy = node.clone();
            copy.id = ids[&node.id].clone();
            copy.position = node
                .position
                .map(normalized)
                .map(|position| std::array::from_fn(|axis| position[axis] + delta[axis]));
            next.nodes.push(copy);
        }
        for edge in copied_edges {
            next.edges.push(Edge {
                from: PortRef {
                    node: ids[&edge.from.node].clone(),
                    port: edge.from.port,
                },
                to: PortRef {
                    node: ids[&edge.to.node].clone(),
                    port: edge.to.port,
                },
            });
        }
        self.next_node = counter;
        let new_ids: BTreeSet<_> = ids.values().cloned().collect();
        self.replace(next);
        self.selected_nodes = new_ids;
        self.selected_group = None;
    }

    fn delete_selected_nodes(&mut self) {
        if self.parameter_dirty()
            || self.waiting()
            || self.cleanup_pending
            || self.selected_nodes.is_empty()
        {
            return;
        }
        let ids = self.selected_nodes.clone();
        let mut next = self.draft.clone();
        next.nodes.retain(|n| !ids.contains(&n.id));
        next.edges
            .retain(|e| !ids.contains(&e.from.node) && !ids.contains(&e.to.node));
        self.replace(next);
        self.selected_nodes.clear();
        self.selected_edge = None;
        if self
            .selected_node
            .as_ref()
            .is_some_and(|id| ids.contains(id))
        {
            self.select_node(None);
        }
    }

    fn history_step(&mut self, redo: bool) {
        if self.parameter_dirty() {
            return;
        }
        let (from, to) = if redo {
            (&mut self.redo, &mut self.undo)
        } else {
            (&mut self.undo, &mut self.redo)
        };
        if let Some(definition) = from.pop() {
            self.canvas_drag = None;
            self.canvas_wire_source = None;
            self.canvas_wire_drag = None;
            self.selected_edge = None;
            if to.len() == HISTORY_LIMIT {
                to.remove(0);
            }
            to.push(self.draft.clone());
            self.draft = definition;
            self.reconcile_group_selection();
            self.selected_nodes
                .retain(|id| self.draft.nodes.iter().any(|node| &node.id == id));
            let selected = self.selected_node.clone();
            self.select_node(selected.as_deref());
        }
    }

    fn delete_node(&mut self, id: &str) {
        let mut next = self.draft.clone();
        next.nodes.retain(|node| node.id != id);
        next.edges
            .retain(|edge| edge.from.node != id && edge.to.node != id);
        self.replace(next);
        self.select_node(None);
        self.selected_edge = None;
        self.view = View::Graph;
    }

    fn handle_authoring_keys(&mut self, ctx: &egui::Context, editable: bool) {
        if !editable
            || self.parameter_dirty()
            || !ctx.input(|input| input.focused)
            || ctx.text_edit_focused()
            || egui::Popup::is_any_open(ctx)
            || ctx.any_popup_open()
        {
            return;
        }
        let events = ctx.input(|input| input.events.clone());
        for event in events {
            let egui::Event::Key {
                key,
                pressed: true,
                repeat: false,
                modifiers,
                ..
            } = event
            else {
                continue;
            };
            let command =
                (modifiers.ctrl || modifiers.command) && !modifiers.shift && !modifiers.alt;
            let recognized = if command {
                match key {
                    egui::Key::Z => {
                        self.history_step(false);
                        true
                    }
                    egui::Key::Y => {
                        self.history_step(true);
                        true
                    }
                    egui::Key::D => {
                        if self.view == View::Graph
                            && self.canvas
                            && !self.selected_nodes.is_empty()
                        {
                            self.duplicate_selected_nodes();
                        } else if let Some(id) = self.selected_node.clone() {
                            self.duplicate_node(&id);
                        }
                        true
                    }
                    _ => false,
                }
            } else if modifiers == egui::Modifiers::NONE && key == egui::Key::Delete {
                if self.view == View::Graph && self.canvas && !self.selected_nodes.is_empty() {
                    self.delete_selected_nodes();
                } else if let Some(id) = self.selected_node.clone() {
                    self.delete_node(&id);
                }
                true
            } else {
                false
            };
            if recognized {
                ctx.input_mut(|input| {
                    input.consume_key(modifiers, key);
                });
                break;
            }
        }
    }

    fn select_node(&mut self, id: Option<&str>) {
        self.selected_node = id
            .filter(|id| self.draft.nodes.iter().any(|node| node.id == *id))
            .map(str::to_owned);
        self.parameter_value = self
            .selected_node
            .as_ref()
            .and_then(|id| self.draft.nodes.iter().find(|node| &node.id == id))
            .and_then(|node| serde_json::to_value(&node.kind).ok())
            .unwrap_or(Value::Null);
        self.parameter_text.clear();
        self.parameter_error = None;
    }

    fn choose_node(&mut self, id: &str) {
        self.selected_nodes.clear();
        self.selected_group = None;
        self.canvas_select_mode = false;
        if self.selected_node.as_deref() == Some(id) {
            self.view = View::Inspector;
            return;
        }
        if self.parameter_dirty() {
            self.error = Some("Update or discard the current node's parameter edits before selecting another node.".into());
        } else {
            self.select_node(Some(id));
        }
        self.view = View::Inspector;
    }

    fn palette_visible(&self) -> bool {
        self.view == View::Nodes
            || self.view == View::Graph && self.controls.contains_key("Mapper node search")
    }

    fn stage_node_filters(&mut self, query: &str, category: Option<NodeCategory>) {
        match mapper::browse_catalogue(query, category) {
            Ok(matches) => {
                self.node_search = query.to_owned();
                self.node_category = category;
                self.node_filter_error = None;
                if self
                    .catalogue
                    .get(self.catalogue_index)
                    .is_none_or(|item| !matches.iter().any(|entry| entry.name == item.name))
                {
                    self.catalogue_index = usize::MAX;
                }
            }
            Err(error) => self.node_filter_error = Some(bounded(&format!("{error:#}"))),
        }
    }

    /// Script fields only stage visible form values; they never edit the graph or emit commands.
    pub fn apply_script_fields(&mut self, fields: &BTreeMap<String, String>) {
        if self.palette_visible() {
            let query = fields
                .get("mapper_node_search")
                .unwrap_or(&self.node_search)
                .clone();
            let category = fields
                .get("mapper_node_category")
                .map(|value| {
                    if value == "all" {
                        Ok(None)
                    } else {
                        serde_json::from_value::<NodeCategory>(Value::String(value.clone()))
                            .map(Some)
                    }
                })
                .transpose();
            match category {
                Ok(category) => {
                    self.stage_node_filters(&query, category.unwrap_or(self.node_category))
                }
                Err(_) => self.node_filter_error = Some("Choose a listed node category.".into()),
            }
            if let Some(name) = fields.get("mapper_catalogue")
                && mapper::browse_catalogue(&self.node_search, self.node_category)
                    .is_ok_and(|matches| matches.iter().any(|item| &item.name == name))
                && let Some(index) = self.catalogue.iter().position(|item| &item.name == name)
            {
                self.catalogue_index = index;
            }
        }
        for (key, value) in fields {
            match key.as_str() {
                "mapper_group_label" => self.group_label = value.clone(),
                "mapper_flow_search" if self.view == View::Browser => {
                    self.flow_browser.search = value.clone();
                }
                "mapper_flow_name" => self.flow_name = value.clone(),
                "mapper_flow_path" => self.flow_path = value.clone(),
                _ => {}
            }
            if let Some(path) = key.strip_prefix("mapper_parameter.") {
                if matches!(path, "needle" | "expected") {
                    if value.len() <= mapper::MAX_TEXT_LITERAL_BYTES
                        && !value
                            .chars()
                            .any(|c| c.is_control() && !matches!(c, '\t' | '\r' | '\n'))
                        && self.parameter_value.get(path).is_some_and(Value::is_string)
                    {
                        self.parameter_value[path] = Value::String(value.clone());
                    }
                } else {
                    self.parameter_text.insert(path.to_owned(), value.clone());
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        seats: &[Seat],
        active_users: &[bool],
        session_active: bool,
        ready: bool,
        busy: bool,
        editable: bool,
        script: Option<&mut Script>,
        action_frame: &mut u64,
        frame_actions: &mut BTreeSet<String>,
    ) -> Option<PageAction> {
        let mut flags = BTreeMap::new();
        let mut controls = Controls {
            script,
            action_frame,
            frame_actions,
            flags: &mut flags,
        };
        let flow_result = self.flow_store.as_mut().and_then(FlowService::poll);
        if let Some(result) = flow_result {
            match result {
                FlowResult::Browsed { request, page } => {
                    self.receive_browser_page(request, page);
                }
                FlowResult::BrowseFailed { request, error } => {
                    if self.view == View::Browser
                        && self.flow_browser.source == BrowserSource::Saved
                        && request == self.flow_browser.generation
                    {
                        self.flow_browser.pending = false;
                        self.flow_browser.error = Some(bounded(&error));
                    }
                }
                FlowResult::Names(names) => {
                    self.flow_selected = self
                        .flow_selected
                        .take()
                        .filter(|name| names.contains(name));
                    if self.flow_selected.is_none() {
                        self.flow_selected = names.first().cloned();
                    }
                    self.flow_names = names;
                    self.flow_error = None;
                }
                FlowResult::Loaded(flow) => {
                    let safe = editable
                        && self.view == View::SavedFlows
                        && !busy
                        && !self.armed
                        && !self.cleanup_pending
                        && !self.waiting();
                    let unchanged =
                        self.flow_load_baseline
                            .take()
                            .is_some_and(|(draft, applied, revision)| {
                                self.draft == draft
                                    && self.applied == applied
                                    && self.revision == revision
                                    && !self.parameter_dirty()
                            });
                    if safe && unchanged {
                        self.flow_name = flow.name.clone();
                        self.replace(flow.mapper);
                        self.select_node(None);
                        self.selected_edge = None;
                        self.connection_sources.clear();
                        self.canvas = true;
                        self.view = View::Graph;
                        self.flow_error = None;
                        self.flow_success = Some(
                            "Flow loaded into the draft. Review it before Apply and Arm.".into(),
                        );
                    } else {
                        self.flow_error = Some("Flow was read, but the draft or session context changed. Review the current draft and try again.".into());
                    }
                }
                FlowResult::Saved => {
                    self.flow_error = None;
                    self.flow_success = Some("Flow saved to the library.".into());
                    if let Some(service) = &mut self.flow_store {
                        let _ = service.start(FlowJob::Refresh, ui.ctx());
                    }
                }
                FlowResult::Exported => {
                    self.flow_error = None;
                    self.flow_success = Some("Flow exported.".into());
                }
                FlowResult::Failed(error) => {
                    self.flow_load_baseline = None;
                    self.flow_error = Some(bounded(&error));
                    self.flow_success = None;
                }
            }
        }
        if editable
            && !busy
            && !self.waiting()
            && let Some(fields) = controls.script.as_deref().and_then(Script::field_edits)
        {
            self.apply_script_fields(fields);
        }
        let validation = self.validation();
        let tabs = [
            ("Graph", View::Graph),
            ("Saved flows", View::SavedFlows),
            ("Templates", View::Templates),
            ("Inspector", View::Inspector),
            ("Activity", View::Activity),
        ];
        let prior_view = self.view;
        let per_row = if visible_width(ui) < 420.0 {
            3
        } else {
            tabs.len()
        };
        if self.view != View::Nodes {
            for row in tabs.chunks(per_row) {
                ui.horizontal(|ui| {
                    for &(label, view) in row {
                        if button(
                            ui,
                            &format!("Mapper {label}"),
                            label,
                            true,
                            if self.view == view {
                                ButtonVariant::Secondary
                            } else {
                                ButtonVariant::Ghost
                            },
                            &mut controls,
                        ) {
                            self.view = view;
                        }
                    }
                });
            }
        }
        if self.view != prior_view {
            if prior_view == View::Browser {
                self.flow_browser.invalidate();
            }
            if prior_view == View::SavedFlows {
                self.flow_load_baseline = None;
            }
            self.cancel_canvas_interaction();
        }
        if self.view != View::Nodes {
            Text::new(format!(
                "r{} · {} · {}{}",
                self.revision,
                if self.armed { "Armed" } else { "Disarmed" },
                if self.draft != self.applied || self.parameter_dirty() {
                    "Draft changed"
                } else {
                    "Applied"
                },
                if self.waiting() { " · pending" } else { "" }
            ))
            .caption()
            .wrap()
            .show(ui);
        }
        let mut action = None;
        let browsing_flows = matches!(
            self.view,
            View::Nodes | View::Templates | View::SavedFlows | View::Browser
        );
        let needs_disarm = self.armed || self.cleanup_pending || self.waiting();
        let draft_ocr_ready = self.ocr_available
            || !self
                .draft
                .nodes
                .iter()
                .any(|node| matches!(node.kind, NodeKind::Ocr { .. }));
        let applied_ocr_ready = self.ocr_available
            || !self
                .applied
                .nodes
                .iter()
                .any(|node| matches!(node.kind, NodeKind::Ocr { .. }));
        if !draft_ocr_ready {
            Text::new("Local OCR models are unavailable or being checked. OCR graphs can be edited and saved; Apply and Arm wait for the models.").caption().wrap().show(ui);
        }
        if !browsing_flows || needs_disarm {
            crate::agent_ui::action_bar(
                ui,
                "mapper-actions",
                if browsing_flows { 1 } else { 2 },
                |ui| {
                    let can_apply = editable
                        && draft_ocr_ready
                        && !busy
                        && !self.waiting()
                        && !self.cleanup_pending
                        && !self.capture_worker_busy
                        && validation.is_none()
                        && (!session_active || ready)
                        && self.draft != self.applied;
                    if !browsing_flows
                        && button(
                            ui,
                            "Apply input mapper",
                            "Apply mapper",
                            can_apply,
                            ButtonVariant::Secondary,
                            &mut controls,
                        )
                        && let Some(command_id) = self.command_id()
                    {
                        let definition = self.draft.clone();
                        self.pending = Some(Pending::Apply {
                            command_id,
                            definition: definition.clone(),
                            expected_revision: self.revision,
                        });
                        action = Some(PageAction::Apply {
                            command_id,
                            definition,
                            expected_revision: self.revision,
                        });
                    }
                    if needs_disarm || self.waiting() {
                        if button(
                            ui,
                            "Disarm input mapper",
                            "Disarm mapper",
                            session_active && self.disarm_pending.is_none(),
                            ButtonVariant::Outline,
                            &mut controls,
                        ) && let Some(command_id) = self.command_id()
                        {
                            self.disarm_pending = Some(DisarmPending {
                                command_id,
                                expected_revision: self.revision,
                            });
                            action = Some(PageAction::Disarm {
                                command_id,
                                expected_revision: self.revision,
                            });
                        }
                    } else if button(
                        ui,
                        "Arm input mapper",
                        "Arm mapper",
                        session_active
                            && applied_ocr_ready
                            && ready
                            && editable
                            && !busy
                            && !self.waiting()
                            && !self.cleanup_pending
                            && !self.capture_worker_busy
                            && !self.applied.nodes.is_empty()
                            && self.draft == self.applied
                            && validation.is_none(),
                        ButtonVariant::Outline,
                        &mut controls,
                    ) && let Some(command_id) = self.command_id()
                    {
                        self.pending = Some(Pending::Arm {
                            command_id,
                            expected_revision: self.revision,
                        });
                        action = Some(PageAction::Arm {
                            command_id,
                            expected_revision: self.revision,
                        });
                    }
                },
            );
        }
        if self.view != View::Graph {
            self.selected_nodes.clear();
            self.canvas_select_mode = false;
            self.marquee = None;
        }
        let can_edit = editable && !busy && !self.waiting();
        let can_author = can_edit && !self.cleanup_pending;
        if self.canvas_drag.is_some()
            && (!can_author
                || self.view != View::Graph
                || !self.canvas
                || !ui.input(|input| {
                    input.focused
                        && (input.pointer.button_down(egui::PointerButton::Primary)
                            || input.pointer.button_released(egui::PointerButton::Primary))
                }))
        {
            self.canvas_drag = None;
        }
        if matches!(self.view, View::Graph | View::Inspector) {
            self.handle_authoring_keys(ui.ctx(), can_author);
        }
        if self.view == View::Graph && visible_width(ui) >= 760.0 {
            let available = ui.available_size();
            ui.horizontal_top(|ui| {
                ui.allocate_ui_with_layout(
                    Vec2::new(260.0, available.y),
                    egui::Layout::top_down_justified(egui::Align::Min),
                    |ui| self.node_palette(ui, seats, active_users, can_edit, false, &mut controls),
                );
                ui.separator();
                ui.allocate_ui_with_layout(
                    Vec2::new(ui.available_width(), available.y),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        egui::ScrollArea::vertical()
                            .id_salt("mapper-workspace")
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                self.authoring_notices(ui, validation.as_deref());
                                self.graph(ui, seats, active_users, can_edit, &mut controls);
                            });
                    },
                );
            });
        } else if self.view == View::Nodes {
            ui.with_layout(egui::Layout::top_down_justified(egui::Align::Min), |ui| {
                self.node_palette(ui, seats, active_users, can_edit, true, &mut controls);
            });
        } else {
            let mut body = egui::ScrollArea::vertical()
                .id_salt("mapper-body")
                .auto_shrink([false, false]);
            if self.view == View::Browser && self.flow_browser.scroll_to_top {
                self.flow_browser.scroll_to_top = false;
                body = body.vertical_scroll_offset(0.0);
            }
            body.show(ui, |ui| {
                self.authoring_notices(ui, validation.as_deref());
                match self.view {
                    View::Graph => self.graph(ui, seats, active_users, can_edit, &mut controls),
                    View::Nodes => (),
                    View::Templates => {
                        self.templates(ui, seats, active_users, can_edit, &mut controls)
                    }
                    View::Inspector => {
                        self.inspector(ui, seats, active_users, can_edit, &mut controls)
                    }
                    View::Activity => self.activity(ui, &mut controls),
                    View::SavedFlows => self.saved_flows(ui, editable, busy, &mut controls),
                    View::Browser => self.flow_browser(ui, seats, active_users, &mut controls),
                }
            });
        }
        self.controls = flags;
        action
    }

    fn open_flow_browser(&mut self, source: BrowserSource) {
        self.flow_load_baseline = None;
        self.flow_browser.origin = self.view;
        self.flow_browser.source = source;
        self.flow_browser.offset = 0;
        self.flow_browser.invalidate();
        self.view = View::Browser;
    }

    fn close_flow_browser(&mut self) {
        if self.view == View::Browser {
            self.view = self.flow_browser.origin;
        }
        self.flow_browser.invalidate();
    }

    fn receive_browser_page(&mut self, request: u64, page: BrowsePage) {
        let browser = &mut self.flow_browser;
        if self.view != View::Browser
            || browser.source != BrowserSource::Saved
            || request != browser.generation
            || page.query != browser.query
            || page.offset != browser.offset
            || page.limit != MAX_BROWSE_ENTRIES
        {
            return;
        }
        browser.pending = false;
        browser.total = page.total;
        browser.cards = page
            .entries
            .into_iter()
            .take(MAX_BROWSE_ENTRIES)
            .map(|entry| BrowserCard {
                name: entry.name,
                summary: "Saved mapper flow".into(),
                template: None,
                preview: entry.preview,
                error: entry.error,
            })
            .collect();
    }

    fn request_browser_page(&mut self, ctx: &egui::Context, seats: &[Seat], active: &[bool]) {
        if self.flow_browser.requested {
            return;
        }
        if self.flow_browser.source == BrowserSource::Saved {
            if let Some(service) = &mut self.flow_store {
                self.flow_browser.requested = service.start(
                    FlowJob::Browse {
                        request: self.flow_browser.generation,
                        query: self.flow_browser.query.clone(),
                        offset: self.flow_browser.offset,
                    },
                    ctx,
                );
                if service.dead {
                    self.flow_browser.pending = false;
                    self.flow_browser.requested = true;
                    self.flow_browser.error = Some("Mapper flow storage is unavailable.".into());
                }
            } else {
                self.flow_browser.pending = false;
                self.flow_browser.requested = true;
                self.flow_browser.error = Some("Mapper flow storage is unavailable.".into());
            }
            return;
        }
        let templates = mapper::templates();
        let query = self.flow_browser.query.to_lowercase();
        let matches: Vec<_> = templates
            .iter()
            .enumerate()
            .filter(|(_, template)| {
                format!("{} {}", template.label, template.description)
                    .to_lowercase()
                    .contains(&query)
            })
            .collect();
        let eligible = active_seats(seats, active);
        self.flow_browser.total = matches.len();
        self.flow_browser.cards = matches
            .into_iter()
            .skip(self.flow_browser.offset)
            .take(MAX_BROWSE_ENTRIES)
            .map(|(index, template)| {
                let users: Vec<_> = eligible
                    .iter()
                    .filter(|(id, _)| {
                        !template.requires_keyboard
                            || seats
                                .iter()
                                .any(|seat| seat.identity() == *id && seat.keyboard.is_some())
                    })
                    .collect();
                let source = self
                    .template_source
                    .as_deref()
                    .filter(|id| users.iter().any(|(user, _)| user.as_str() == *id))
                    .or_else(|| users.first().map(|(id, _)| id.as_str()))
                    .unwrap_or("mapper-preview-source");
                let target = template.requires_target.then(|| {
                    self.template_target
                        .as_deref()
                        .filter(|id| users.iter().any(|(user, _)| user.as_str() == *id))
                        .or_else(|| users.first().map(|(id, _)| id.as_str()))
                        .unwrap_or("mapper-preview-target")
                });
                let result = mapper::instantiate_template(template.template, source, target)
                    .and_then(|definition| mapper_flow_store::preview(&definition));
                let (preview, error) = match result {
                    Ok(preview) => (Some(preview), None),
                    Err(error) => (None, Some(bounded(&format!("{error:#}")))),
                };
                BrowserCard {
                    name: template.label.into(),
                    summary: template.description.into(),
                    template: Some(index),
                    preview,
                    error,
                }
            })
            .collect();
        self.flow_browser.requested = true;
        self.flow_browser.pending = false;
    }

    fn choose_browser_card(&mut self, card: &BrowserCard) {
        if card.preview.is_none() || card.error.is_some() {
            return;
        }
        if let Some(index) = card.template {
            self.template_index = index;
            self.view = View::Templates;
        } else {
            self.flow_selected = Some(card.name.clone());
            self.flow_name = card.name.clone();
            self.view = View::SavedFlows;
        }
        self.flow_browser.invalidate();
    }

    fn flow_browser(
        &mut self,
        ui: &mut egui::Ui,
        seats: &[Seat],
        active: &[bool],
        controls: &mut Controls<'_>,
    ) {
        ui.spacing_mut().item_spacing.y = core::SPACE_1;
        ui.horizontal(|ui| {
            Heading::new("Flow Browser").heading().show(ui);
            if button(
                ui,
                "Close mapper flow browser",
                "Back",
                true,
                ButtonVariant::Ghost,
                controls,
            ) {
                self.close_flow_browser();
            }
        });
        if self.view != View::Browser {
            return;
        }
        ui.horizontal(|ui| {
            for (source, identity, label) in [
                (
                    BrowserSource::Templates,
                    "Browse mapper templates",
                    "Templates",
                ),
                (BrowserSource::Saved, "Browse saved mapper flows", "Saved"),
            ] {
                let selected = self.flow_browser.source == source;
                if button(
                    ui,
                    identity,
                    label,
                    true,
                    if selected {
                        ButtonVariant::Secondary
                    } else {
                        ButtonVariant::Ghost
                    },
                    controls,
                ) && !selected
                {
                    self.flow_browser.source = source;
                    self.flow_browser.offset = 0;
                    self.flow_browser.invalidate();
                }
            }
        });
        ui.horizontal(|ui| {
            ui.scope(|ui| {
                ui.set_max_width((visible_width(ui) - 100.0).max(40.0));
                let response = Input::new(&mut self.flow_browser.search)
                    .id_source("Mapper flow search")
                    .show(ui);
                controls.observe(ui, "Mapper flow search", &response);
            });
            if button(
                ui,
                "Search mapper flows",
                "Search",
                true,
                ButtonVariant::Outline,
                controls,
            ) {
                self.flow_browser.query = self.flow_browser.search.trim().to_owned();
                self.flow_browser.offset = 0;
                self.flow_browser.invalidate();
            }
        });
        self.request_browser_page(ui.ctx(), seats, active);
        ui.horizontal(|ui| {
            if button(
                ui,
                "Previous mapper flows",
                "Previous",
                self.flow_browser.offset > 0,
                ButtonVariant::Ghost,
                controls,
            ) {
                self.flow_browser.offset =
                    self.flow_browser.offset.saturating_sub(MAX_BROWSE_ENTRIES);
                self.flow_browser.invalidate();
            }
            if button(
                ui,
                "Next mapper flows",
                "Next",
                !self.flow_browser.pending
                    && self.flow_browser.offset + MAX_BROWSE_ENTRIES < self.flow_browser.total,
                ButtonVariant::Ghost,
                controls,
            ) {
                self.flow_browser.offset += MAX_BROWSE_ENTRIES;
                self.flow_browser.invalidate();
            }
            Text::new(if self.flow_browser.pending {
                "Loading…".into()
            } else {
                format!("{} flows", self.flow_browser.total)
            })
            .caption()
            .show(ui);
        });
        if let Some(error) = &self.flow_browser.error {
            Text::new(error).caption().wrap().show(ui);
        }
        if !self.flow_browser.pending
            && self.flow_browser.cards.is_empty()
            && self.flow_browser.error.is_none()
        {
            Text::new("No matching mapper flows.").caption().show(ui);
        }
        let width = ui.available_width();
        let columns = ((width + core::SPACE_2) / 300.0).floor().max(1.0) as usize;
        let padding = core::SPACE_1;
        let card_width = ((width - core::SPACE_2 * (columns - 1) as f32) / columns as f32
            - 2.0 * (padding + core::BORDER_THIN))
            .max(1.0);
        let cards = self.flow_browser.cards.clone();
        let mut chosen = None;
        egui::Grid::new("mapper_flow_browser_cards")
            .num_columns(columns)
            .spacing(Vec2::splat(core::SPACE_2))
            .show(ui, |ui| {
                for (index, card) in cards.iter().enumerate() {
                    Surface::new().pad(padding).show(ui, |ui| {
                        ui.vertical(|ui| {
                            ui.set_width(card_width);
                            Text::new(&card.name).caption().wrap().show(ui);
                            if let Some(preview) = &card.preview {
                                flow_preview(ui, preview);
                            }
                            Text::new(fitted_label(ui, &card.summary, 0.0))
                                .caption()
                                .muted()
                                .show(ui);
                            if let Some(preview) = &card.preview {
                                Text::new(format!(
                                    "Available · {} nodes · {} links · {} groups",
                                    preview.node_count, preview.edge_count, preview.group_count
                                ))
                                .caption()
                                .show(ui);
                            } else {
                                Text::new("Unavailable · Invalid flow").caption().show(ui);
                            }
                            if let Some(error) = &card.error {
                                Text::new(error).caption().wrap().show(ui);
                            }
                            let identity = if card.template.is_some() {
                                format!("Choose mapper template {}", card.name)
                            } else {
                                format!("Choose saved mapper flow {}", card.name)
                            };
                            if button(
                                ui,
                                &identity,
                                "Choose",
                                card.preview.is_some() && card.error.is_none(),
                                ButtonVariant::Secondary,
                                controls,
                            ) {
                                chosen = Some(index);
                            }
                        });
                    });
                    if (index + 1) % columns == 0 {
                        ui.end_row();
                    }
                }
            });
        if let Some(index) = chosen {
            self.choose_browser_card(&cards[index]);
        }
    }

    fn saved_flows(
        &mut self,
        ui: &mut egui::Ui,
        editable: bool,
        busy: bool,
        controls: &mut Controls<'_>,
    ) {
        if !self.flow_initialized
            && let Some(service) = &mut self.flow_store
        {
            self.flow_initialized = service.start(FlowJob::Refresh, ui.ctx());
        }
        let pending = self
            .flow_store
            .as_ref()
            .is_some_and(|service| service.pending);
        let safe_load = editable
            && !busy
            && !self.armed
            && !self.cleanup_pending
            && !self.waiting()
            && !self.parameter_dirty();
        ui.horizontal_wrapped(|ui| {
            Heading::new("Saved flows").heading().show(ui);
            if button(
                ui,
                "Browse mapper flows",
                "Browse flows",
                true,
                ButtonVariant::Outline,
                controls,
            ) {
                self.open_flow_browser(BrowserSource::Saved);
            }
            if button(
                ui,
                "Refresh mapper flows",
                "Refresh",
                !pending,
                ButtonVariant::Ghost,
                controls,
            ) && let Some(service) = &mut self.flow_store
            {
                let _ = service.start(FlowJob::Refresh, ui.ctx());
            }
        });
        if self.flow_store.is_none() {
            Text::new("Saved flow storage is unavailable in this configuration.")
                .wrap()
                .show(ui);
            return;
        }
        if self.flow_names.is_empty() {
            Text::new(if pending {
                "Reading saved flows…"
            } else {
                "No saved flows yet."
            })
            .caption()
            .wrap()
            .show(ui);
        } else {
            let mut index = self
                .flow_selected
                .as_ref()
                .and_then(|selected| self.flow_names.iter().position(|name| name == selected))
                .unwrap_or(0);
            if select(
                ui,
                "Select saved mapper flow",
                &mut index,
                &self.flow_names,
                true,
                controls,
            ) {
                self.flow_selected = self.flow_names.get(index).cloned();
            }
        }
        ui.horizontal_wrapped(|ui| {
            if button(
                ui,
                "Load mapper flow",
                "Load into draft",
                safe_load && !pending && self.flow_selected.is_some(),
                ButtonVariant::Secondary,
                controls,
            ) && let (Some(service), Some(name)) =
                (&mut self.flow_store, self.flow_selected.clone())
            {
                self.flow_load_baseline =
                    Some((self.draft.clone(), self.applied.clone(), self.revision));
                let _ = service.start(FlowJob::Load(name), ui.ctx());
            }
        });
        Text::new("Flow name").caption().show(ui);
        Input::new(&mut self.flow_name)
            .id_source("Mapper flow name")
            .show(ui);
        ui.horizontal_wrapped(|ui| {
            let replacing = self
                .flow_names
                .iter()
                .any(|name| name == self.flow_name.trim());
            if button(
                ui,
                "Save mapper flow",
                if replacing {
                    "Replace saved flow"
                } else {
                    "Save current draft"
                },
                editable
                    && !busy
                    && !pending
                    && !self.cleanup_pending
                    && !self.parameter_dirty()
                    && !self.flow_name.trim().is_empty(),
                ButtonVariant::Secondary,
                controls,
            ) && let Some(service) = &mut self.flow_store
            {
                self.flow_success = None;
                let flow = Flow::new(self.flow_name.trim().to_owned(), self.draft.clone());
                let _ = service.start(FlowJob::Save(flow), ui.ctx());
            }
        });
        Text::new("Transfer path").caption().show(ui);
        Input::new(&mut self.flow_path)
            .id_source("Mapper flow transfer path")
            .show(ui);
        ui.horizontal_wrapped(|ui| {
            if button(
                ui,
                "Import mapper flow",
                "Import to draft",
                safe_load && !pending && !self.flow_path.trim().is_empty(),
                ButtonVariant::Outline,
                controls,
            ) && let Some(service) = &mut self.flow_store
            {
                self.flow_success = None;
                self.flow_load_baseline =
                    Some((self.draft.clone(), self.applied.clone(), self.revision));
                let _ = service.start(
                    FlowJob::Import(PathBuf::from(self.flow_path.trim())),
                    ui.ctx(),
                );
            }
            if button(
                ui,
                "Export mapper flow",
                "Export current draft",
                editable
                    && !busy
                    && !pending
                    && !self.parameter_dirty()
                    && !self.flow_path.trim().is_empty(),
                ButtonVariant::Outline,
                controls,
            ) && let Some(service) = &mut self.flow_store
            {
                self.flow_success = None;
                let name = if self.flow_name.trim().is_empty() {
                    "Mapper flow"
                } else {
                    self.flow_name.trim()
                };
                let flow = Flow::new(name.to_owned(), self.draft.clone());
                let _ = service.start(
                    FlowJob::Export(PathBuf::from(self.flow_path.trim()), flow),
                    ui.ctx(),
                );
            }
        });
        if pending {
            Text::new("Flow operation in progress…").caption().show(ui);
        }
        if let Some(error) = &self.flow_error {
            Text::new(error.clone()).wrap().show(ui);
        }
        if let Some(success) = &self.flow_success {
            Text::new(success.clone()).caption().wrap().show(ui);
        }
        if self.parameter_dirty() {
            Text::new(
                "Update or discard node parameter edits before saving or exporting this draft.",
            )
            .caption()
            .wrap()
            .show(ui);
        }
        Text::new("Loading changes the editable draft only. Review it, then use Apply and Arm separately.").caption().wrap().show(ui);
    }

    fn templates(
        &mut self,
        ui: &mut egui::Ui,
        seats: &[Seat],
        active: &[bool],
        editable: bool,
        controls: &mut Controls<'_>,
    ) {
        let templates = mapper::templates();
        let compact = visible_width(ui) < 440.0;
        if compact {
            ui.spacing_mut().item_spacing.y = core::SPACE_1;
        }
        ui.horizontal(|ui| {
            if button(
                ui,
                "Browse mapper flows",
                "Browse flows",
                true,
                ButtonVariant::Outline,
                controls,
            ) {
                self.open_flow_browser(BrowserSource::Templates);
            }
            for (label, requires_target) in [("Route", true), ("Monitor", false)] {
                let selected = templates[self.template_index].requires_target == requires_target;
                if button(
                    ui,
                    &format!("Mapper template category {label}"),
                    label,
                    true,
                    if selected {
                        ButtonVariant::Secondary
                    } else {
                        ButtonVariant::Ghost
                    },
                    controls,
                ) && !selected
                    && let Some(index) = templates
                        .iter()
                        .position(|template| template.requires_target == requires_target)
                {
                    self.template_index = index;
                }
            }
        });
        let requires_target = templates[self.template_index].requires_target;
        ui.horizontal_wrapped(|ui| {
            for (index, template) in templates
                .iter()
                .enumerate()
                .filter(|(_, template)| template.requires_target == requires_target)
            {
                if button(
                    ui,
                    &format!("Mapper template {}", template.label),
                    template.label,
                    true,
                    if index == self.template_index {
                        ButtonVariant::Secondary
                    } else {
                        ButtonVariant::Ghost
                    },
                    controls,
                ) {
                    self.template_index = index;
                }
            }
        });
        let template = &templates[self.template_index];
        let mut eligible = active_seats(seats, active);
        if template.requires_keyboard {
            eligible.retain(|(identity, _)| {
                seats
                    .iter()
                    .any(|seat| seat.identity() == *identity && seat.keyboard.is_some())
            });
        }
        let can_bind = editable && !eligible.is_empty();
        let source = template_binding(
            ui,
            "Source user",
            "Mapper template source",
            &mut self.template_source,
            &eligible,
            can_bind,
            controls,
        );
        let target = if template.requires_target {
            template_binding(
                ui,
                "Destination user",
                "Mapper template destination",
                &mut self.template_target,
                &eligible,
                can_bind,
                controls,
            )
        } else {
            None
        };
        let note = if eligible.is_empty() {
            if template.requires_keyboard {
                "This flow needs a virtual mouse and keyboard pair."
            } else {
                "This flow needs a virtual mouse."
            }
        } else if self.parameter_dirty() {
            "Update or discard the current node edits first."
        } else {
            "Loads an editable draft. Apply and Arm are separate."
        };
        let bindings_ready = source.is_some() && (!template.requires_target || target.is_some());
        let label = if self.draft.nodes.is_empty() {
            "Load into draft"
        } else {
            "Replace draft"
        };
        if button(
            ui,
            "Load mapper template",
            label,
            editable && !self.parameter_dirty() && bindings_ready,
            ButtonVariant::Secondary,
            controls,
        ) && let Some(source) = source
        {
            self.load_template(template.template, source, target);
        }
        Text::new(template.description).caption().wrap().show(ui);
        if !template.requires_target {
            Text::new("Destination user · None for monitoring flows")
                .caption()
                .wrap()
                .show(ui);
        }
        Text::new(note).caption().wrap().show(ui);
    }

    fn load_template(&mut self, template: TemplateKind, source: &str, target: Option<&str>) {
        match mapper::instantiate_template(template, source, target) {
            Ok(definition) => {
                self.replace(definition);
                self.select_node(None);
                self.selected_edge = None;
                self.connection_sources.clear();
                self.view = View::Graph;
                self.canvas = true;
            }
            Err(error) => self.error = Some(bounded(&format!("{error:#}"))),
        }
    }

    fn reconcile_group_selection(&mut self) {
        if let Some(group) = self
            .draft
            .groups
            .iter()
            .find(|g| Some(&g.id) == self.selected_group.as_ref())
        {
            self.group_label = group.label.clone();
        } else {
            self.selected_group = None;
            self.group_label.clear();
        }
    }

    fn select_group(&mut self, id: &str) {
        if self.parameter_dirty() {
            return;
        }
        if let Some(group) = self.draft.groups.iter().find(|g| g.id == id) {
            self.selected_nodes = group.nodes.iter().cloned().collect();
            self.selected_group = Some(group.id.clone());
            self.group_label = group.label.clone();
            self.select_node(None);
            self.clear_canvas_gestures();
        }
    }

    fn can_group_selection(&self) -> bool {
        self.selected_nodes.len() >= 2
            && self.draft.groups.len() < mapper::MAX_GROUPS
            && self.selected_nodes.iter().all(|id| {
                self.draft.nodes.iter().any(|node| &node.id == id)
                    && !self
                        .draft
                        .groups
                        .iter()
                        .any(|group| group.nodes.contains(id))
            })
    }

    fn edit_group(&mut self, edit: GroupEdit) {
        match self.draft.edit_group(edit) {
            Ok(next) => self.replace(next),
            Err(error) => self.error = Some(bounded(&format!("{error:#}"))),
        }
    }

    fn group_selection(&mut self) {
        if !self.can_group_selection() {
            return;
        }
        let Some(id) = (1..=mapper::MAX_GROUPS + 1)
            .map(|n| format!("group-{n}"))
            .find(|id| !self.draft.groups.iter().any(|g| &g.id == id))
        else {
            return;
        };
        self.edit_group(GroupEdit::Create {
            group: NodeGroup {
                id: id.clone(),
                label: "Node group".into(),
                nodes: self.selected_nodes.iter().cloned().collect(),
                collapsed: false,
            },
        });
        self.select_group(&id);
    }

    fn group_controls(&mut self, ui: &mut egui::Ui, editable: bool, controls: &mut Controls<'_>) {
        let editable = editable && !self.cleanup_pending && !self.parameter_dirty();
        if button(
            ui,
            "Group selected mapper nodes",
            "Group selected",
            editable && self.can_group_selection(),
            ButtonVariant::Outline,
            controls,
        ) {
            self.group_selection();
        }
        for group in self.draft.groups.clone() {
            if button(
                ui,
                &format!("Select mapper group {}", group.id),
                &group.label,
                !self.parameter_dirty(),
                ButtonVariant::Ghost,
                controls,
            ) {
                self.select_group(&group.id);
            }
        }
        let Some(group) = self
            .draft
            .groups
            .iter()
            .find(|g| Some(&g.id) == self.selected_group.as_ref())
            .cloned()
        else {
            return;
        };
        let response = ui
            .add_enabled_ui(editable, |ui| {
                Input::new(&mut self.group_label)
                    .id_source("Mapper group label")
                    .show(ui)
            })
            .inner;
        controls.observe(ui, "Mapper group label", &response);
        let label_valid = !self.group_label.is_empty()
            && self.group_label.len() <= 128
            && self.group_label.trim() == self.group_label
            && !self.group_label.chars().any(char::is_control);
        ui.horizontal_wrapped(|ui| {
            if button(
                ui,
                "Rename mapper group",
                "Rename group",
                editable && label_valid && self.group_label != group.label,
                ButtonVariant::Outline,
                controls,
            ) {
                self.edit_group(GroupEdit::Rename {
                    group_id: group.id.clone(),
                    label: self.group_label.clone(),
                });
            }
            let (action, caption) = if group.collapsed {
                ("Expand mapper group", "Expand group")
            } else {
                ("Collapse mapper group", "Collapse group")
            };
            if button(
                ui,
                action,
                caption,
                editable,
                ButtonVariant::Outline,
                controls,
            ) {
                self.edit_group(GroupEdit::SetCollapsed {
                    group_id: group.id.clone(),
                    collapsed: !group.collapsed,
                });
            }
            if button(
                ui,
                "Ungroup mapper nodes",
                "Ungroup",
                editable,
                ButtonVariant::Outline,
                controls,
            ) {
                self.edit_group(GroupEdit::Remove {
                    group_id: group.id.clone(),
                });
            }
        });
        Text::new("Expand a group to connect its ports.")
            .caption()
            .wrap()
            .show(ui);
    }

    fn authoring_notices(&self, ui: &mut egui::Ui, validation: Option<&str>) {
        if self.cleanup_pending {
            Text::new("Mapper releases are pending. Arm and replacement remain blocked.")
                .wrap()
                .show(ui);
        }
        if let Some(error) = &self.error {
            Text::new(error).wrap().show(ui);
        }
        if let Some(error) = &self.worker_error
            && self.error.as_ref() != Some(error)
        {
            Text::new(error).wrap().show(ui);
        }
        if let Some(error) = validation {
            Text::new(format!("Validation: {error}")).wrap().show(ui);
        }
    }

    fn add_catalogue_node(&mut self, seats: &[Seat], active: &[bool]) {
        let Some(item) = self.catalogue.get(self.catalogue_index) else {
            return;
        };
        let mut kind = serde_json::to_value(&item.prototype).unwrap_or(Value::Null);
        assign_users(&mut kind, &active_seats(seats, active));
        match serde_json::from_value::<NodeKind>(kind) {
            Ok(kind) => {
                if let Some(id) = self.fresh_node_id() {
                    let mut next = self.draft.clone();
                    next.nodes.push(Node {
                        id: id.clone(),
                        kind,
                        position: None,
                    });
                    self.replace(next);
                    self.select_node(Some(&id));
                    self.view = View::Inspector;
                } else {
                    self.error = Some("A fresh stable node ID could not be allocated.".into());
                }
            }
            Err(error) => self.error = Some(bounded(&error.to_string())),
        }
    }

    fn node_palette(
        &mut self,
        ui: &mut egui::Ui,
        seats: &[Seat],
        active: &[bool],
        editable: bool,
        dedicated: bool,
        controls: &mut Controls<'_>,
    ) {
        ui.spacing_mut().item_spacing.y = core::SPACE_1;
        ui.horizontal(|ui| {
            Heading::new("Nodes").heading().show(ui);
            if dedicated
                && button(
                    ui,
                    "Back to mapper graph",
                    "Back to Graph",
                    true,
                    ButtonVariant::Ghost,
                    controls,
                )
            {
                self.view = View::Graph;
            }
        });
        if dedicated && self.view != View::Nodes {
            return;
        }
        let previous = self.node_search.clone();
        let response = Input::new(&mut self.node_search)
            .sm()
            .id_source("Mapper node catalogue")
            .placeholder("Search nodes")
            .show(ui);
        // Keep the catalogue's public identity on its searchable control.
        controls.observe(ui, "Mapper node catalogue", &response);
        if !controls.observe(ui, "Mapper node search", &response)
            || response.changed() && !controls.consume(ui.ctx(), "Mapper node search")
        {
            self.node_search = previous;
        } else if response.changed() {
            let query = std::mem::replace(&mut self.node_search, previous);
            self.stage_node_filters(&query, self.node_category);
        }
        let mut category = self
            .node_category
            .and_then(|value| NodeCategory::ALL.iter().position(|item| *item == value))
            .map_or(0, |index| index + 1);
        let labels = std::iter::once("All categories".to_owned())
            .chain(
                NodeCategory::ALL
                    .iter()
                    .map(|category| category.label().to_owned()),
            )
            .collect::<Vec<_>>();
        if select(
            ui,
            "Mapper node category",
            &mut category,
            &labels,
            true,
            controls,
        ) {
            let query = self.node_search.clone();
            self.stage_node_filters(
                &query,
                category
                    .checked_sub(1)
                    .and_then(|index| NodeCategory::ALL.get(index))
                    .copied(),
            );
        }
        ui.horizontal(|ui| {
            if button(
                ui,
                "Search mapper nodes",
                "Search",
                true,
                ButtonVariant::Outline,
                controls,
            ) {
                let query = self.node_search.clone();
                self.stage_node_filters(&query, self.node_category);
            }
            if button(
                ui,
                "Clear mapper node filters",
                "Clear filters",
                !self.node_search.is_empty()
                    || self.node_category.is_some()
                    || self.node_filter_error.is_some(),
                ButtonVariant::Ghost,
                controls,
            ) {
                self.stage_node_filters("", None);
            }
        });
        let matches =
            mapper::browse_catalogue(&self.node_search, self.node_category).unwrap_or_default();
        if self
            .catalogue
            .get(self.catalogue_index)
            .is_none_or(|selected| !matches.iter().any(|item| item.name == selected.name))
        {
            self.catalogue_index = usize::MAX;
        }
        let selected = self.catalogue.get(self.catalogue_index).cloned();
        let list_height =
            (ui.available_height() - core::Size::Sm.height() - core::SPACE_2).max(0.0);
        egui::ScrollArea::vertical()
            .id_salt("mapper-node-palette")
            .max_height(list_height)
            .min_scrolled_height(0.0)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if let Some(error) = &self.node_filter_error {
                    Text::new(error).caption().wrap().show(ui);
                }
                Text::new("Unavailable: LLM, hardware, feedback.")
                    .caption()
                    .muted()
                    .wrap()
                    .show(ui);
                if matches.is_empty() {
                    let response =
                        Text::new("No matching nodes. Change the search or clear filters.")
                            .wrap()
                            .show(ui);
                    controls.observe(ui, "Mapper node empty results", &response);
                }
                for item in &matches {
                    let chosen = selected
                        .as_ref()
                        .is_some_and(|selected| selected.name == item.name);
                    if button(
                        ui,
                        &format!("Choose mapper node {}", item.name),
                        &item.label,
                        true,
                        if chosen {
                            ButtonVariant::Secondary
                        } else {
                            ButtonVariant::Ghost
                        },
                        controls,
                    ) {
                        self.catalogue_index = self
                            .catalogue
                            .iter()
                            .position(|entry| entry.name == item.name)
                            .unwrap_or(usize::MAX);
                    }
                    if chosen {
                        Text::new(&item.description).caption().wrap().show(ui);
                    }
                }
            });
        let selected = self
            .catalogue
            .get(self.catalogue_index)
            .filter(|selected| matches.iter().any(|item| item.name == selected.name));
        let can_add = editable
            && !self.cleanup_pending
            && !self.parameter_dirty()
            && self.draft.nodes.len() < mapper::MAX_NODES
            && self.node_filter_error.is_none()
            && selected.is_some();
        if button(
            ui,
            "Add mapper node",
            "Add selected node",
            can_add,
            ButtonVariant::Secondary,
            controls,
        ) {
            self.add_catalogue_node(seats, active);
        }
    }

    fn graph(
        &mut self,
        ui: &mut egui::Ui,
        _seats: &[Seat],
        _active: &[bool],
        editable: bool,
        controls: &mut Controls<'_>,
    ) {
        Heading::new("Typed input graph").heading().show(ui);
        if !controls.flags.contains_key("Mapper node search")
            && button(
                ui,
                "Browse mapper nodes",
                "Browse nodes",
                true,
                ButtonVariant::Secondary,
                controls,
            )
        {
            self.cancel_canvas_interaction();
            self.view = View::Nodes;
        }
        ui.horizontal_wrapped(|ui| {
            if button(
                ui,
                "Undo mapper edit",
                "Undo",
                editable && !self.parameter_dirty() && !self.undo.is_empty(),
                ButtonVariant::Outline,
                controls,
            ) {
                self.history_step(false);
            }
            if button(
                ui,
                "Redo mapper edit",
                "Redo",
                editable && !self.parameter_dirty() && !self.redo.is_empty(),
                ButtonVariant::Outline,
                controls,
            ) {
                self.history_step(true);
            }
        });
        let mut canvas = self.canvas;
        let response = Checkbox::new(&mut canvas)
            .label("Show graph canvas")
            .show(ui);
        if controls.observe(ui, "Mapper canvas", &response)
            && response.changed()
            && controls.consume(ui.ctx(), "Mapper canvas")
        {
            self.canvas = canvas;
            if !canvas {
                self.cancel_canvas_interaction();
                self.selected_nodes.clear();
                self.canvas_select_mode = false;
            }
        }
        self.group_controls(ui, editable, controls);
        if self.canvas {
            Text::new(format!("{} node(s) selected", self.selected_nodes.len()))
                .caption()
                .show(ui);
            ui.horizontal_wrapped(|ui| {
                if button(
                    ui,
                    "Mapper canvas select nodes",
                    "Select nodes",
                    editable && !self.cleanup_pending && !self.parameter_dirty(),
                    if self.canvas_select_mode {
                        ButtonVariant::Secondary
                    } else {
                        ButtonVariant::Outline
                    },
                    controls,
                ) {
                    self.canvas_select_mode = !self.canvas_select_mode;
                }
                if button(
                    ui,
                    "Duplicate selected mapper nodes",
                    "Duplicate selected",
                    editable
                        && !self.cleanup_pending
                        && !self.parameter_dirty()
                        && !self.selected_nodes.is_empty()
                        && self.selected_nodes.iter().all(|id| {
                            self.draft
                                .nodes
                                .iter()
                                .any(|n| &n.id == id && !is_input_source(&n.kind))
                        })
                        && self
                            .draft
                            .nodes
                            .len()
                            .saturating_add(self.selected_nodes.len())
                            <= mapper::MAX_NODES
                        && self.draft.edges.len().saturating_add(
                            self.draft
                                .edges
                                .iter()
                                .filter(|e| {
                                    self.selected_nodes.contains(&e.from.node)
                                        && self.selected_nodes.contains(&e.to.node)
                                })
                                .count(),
                        ) <= mapper::MAX_EDGES,
                    ButtonVariant::Outline,
                    controls,
                ) {
                    self.duplicate_selected_nodes();
                }
                if button(
                    ui,
                    "Delete selected mapper nodes",
                    "Delete selected",
                    editable
                        && !self.cleanup_pending
                        && !self.parameter_dirty()
                        && !self.selected_nodes.is_empty(),
                    ButtonVariant::Outline,
                    controls,
                ) {
                    self.delete_selected_nodes();
                }
            });
            self.graph_canvas(
                ui,
                editable && !self.cleanup_pending && !self.parameter_dirty(),
                controls,
            );
        }
        for node in self.draft.nodes.clone() {
            if self
                .draft
                .groups
                .iter()
                .any(|g| g.collapsed && g.nodes.contains(&node.id))
            {
                continue;
            }
            Surface::new().pad(core::SPACE_2).show(ui, |ui| {
                let label = format!("{} · {}", node.id, kind_name(&node.kind));
                if button(
                    ui,
                    &format!("Select mapper node {}", node.id),
                    &label,
                    true,
                    if self.selected_node.as_deref() == Some(&node.id) {
                        ButtonVariant::Secondary
                    } else {
                        ButtonVariant::Ghost
                    },
                    controls,
                ) {
                    self.choose_node(&node.id);
                }
                let ports = node.kind.ports();
                Text::new(format!(
                    "{} input(s) · {} output(s)",
                    ports.inputs.len(),
                    ports.outputs.len()
                ))
                .caption()
                .show(ui);
            });
        }
        if self.draft.nodes.is_empty() {
            Text::new("Choose an input, processor or output from the catalogue to begin.")
                .wrap()
                .show(ui);
        }
        let connections = Collapsible::new("Connections")
            .id_source("mapper-connections")
            .show(ui, |ui| {
                for edge in self.draft.edges.clone() {
                    let label = edge_label(&edge);
                    if button(
                        ui,
                        &format!("Select mapper wire {label}"),
                        &label,
                        true,
                        ButtonVariant::Ghost,
                        controls,
                    ) {
                        self.selected_edge = Some(edge.clone());
                    }
                }
                if button(
                    ui,
                    "Disconnect mapper wire",
                    "Disconnect selected wire",
                    editable && self.selected_edge.is_some(),
                    ButtonVariant::Outline,
                    controls,
                ) && let Some(edge) = self.selected_edge.take()
                {
                    let mut next = self.draft.clone();
                    next.edges.retain(|value| value != &edge);
                    self.replace(next);
                }
            });
        controls.observe(ui, "Mapper connections", &connections);
        Text::new("Local OCR reads Latin text from explicitly selected allowed client windows. LLM, hardware and feedback nodes remain unavailable.").caption().wrap().show(ui);
    }

    fn inspector(
        &mut self,
        ui: &mut egui::Ui,
        seats: &[Seat],
        active: &[bool],
        editable: bool,
        controls: &mut Controls<'_>,
    ) {
        let Some(id) = self.selected_node.clone() else {
            Text::new("Select or add a node in Graph.").wrap().show(ui);
            return;
        };
        let Some(node) = self.draft.nodes.iter().find(|node| node.id == id).cloned() else {
            return;
        };
        Heading::new(format!("{} · {}", id, kind_name(&node.kind)))
            .heading()
            .show(ui);
        Text::new(
            "Stable node ID; parameter edits take effect only after Update node, then Apply.",
        )
        .caption()
        .wrap()
        .show(ui);
        if matches!(&node.kind, NodeKind::Pointer { .. }) {
            Text::new("Position is this actor's unscaled input trajectory: seeded once, integrating its own raw deltas, and reset by absolute MoveTo or retirement. Other actors and speed/clamp settings affect the shared rendered cursor, not this source position.")
                .caption().wrap().show(ui);
        }
        let help = match &node.kind {
            NodeKind::Ocr { .. } => Some(
                "A true trigger pulse reads Latin text from a fresh selected-client snapshot. Text is private to the actor. Empty recognition clears text and has_text; done pulses after each successful read. Gate done with a text predicate to request input. Local models are an early preview; OCR can misread text. Maximum region: 1,048,576 pixels, 4096 on either edge.",
            ),
            NodeKind::TextContains { .. } | NodeKind::TextCompare { .. } => Some(
                "Literal text matching, without regular expressions. Case-insensitive matching ignores ASCII A–Z case only. Configured text is shared graph configuration; recognized text stays actor-private. Connect the predicate Hold to a Pulse gate driven by OCR done. Empty OCR is an empty string, including for NotEquals; combine has_text with your predicate when recognition is required.",
            ),
            NodeKind::PixelColor { .. } => Some(
                "A true trigger pulse requests a fresh client-window pixel. Red, green, blue and alpha are scalar States; done pulses after an accepted observation. Capture permission is separate from input permission.",
            ),
            NodeKind::ImageMatch { .. } => Some(
                "A true trigger pulse searches a fresh selected-client snapshot. Matched is a Hold; score is zero when no match qualifies. Position is the last successful desktop center: gate actions with matched. Done also pulses on no match. Templates are shared graph configuration, saved with profiles and flows.",
            ),
            NodeKind::Remap { .. } => Some(
                "Map the input minimum and maximum to the output endpoints. Enable Clamp to keep values within the output range.",
            ),
            NodeKind::Smooth { .. } => Some(
                "Smooth Position or scalar State. Alpha is greater than zero and at most one. Higher values follow the input more closely.",
            ),
            NodeKind::Velocity => Some(
                "Report speed in desktop pixels per second between timed Position samples. The first live sample starts at zero.",
            ),
            NodeKind::Delay { .. } => Some(
                "Queue each signal for the configured delay. Disarm, Stop or actor retirement cancels pending signals. Scheduled recipients must stay unchanged.",
            ),
            NodeKind::Throttle { .. } => Some(
                "Pass the first signal in each interval. Hold releases pass immediately. Suppressed values are not replayed later.",
            ),
            NodeKind::Timer { .. } => Some(
                "The first pulse follows the interval. A low enable cancels future pulses. After the count finishes, lower enable before restarting.",
            ),
            NodeKind::Sink {
                channel: mapper::OutputChannel::TypeText,
                ..
            } => Some(
                "Type the latest Text only on a true trigger pulse. Up to 128 UTF-16 units share the wave's 128-message budget. A delivery error can leave a typed prefix; inspect before retrying.",
            ),
            NodeKind::Counter { .. } => Some(
                "Counts are separate for each actor. Enable the reset port to return to the initial count on a pulse.",
            ),
            NodeKind::Switch { .. } => Some(
                "Select off or on with a hold. Position and scalar state branches need a constant or initialized converter. Event branches forward current input.",
            ),
            _ => None,
        };
        if let Some(help) = help {
            let response = Text::new(help).caption().wrap().show(ui);
            if matches!(
                &node.kind,
                NodeKind::Sink {
                    channel: mapper::OutputChannel::TypeText,
                    ..
                }
            ) {
                controls.observe(ui, "Mapper Type text help", &response);
            }
        }
        let mut errors = Vec::new();
        let mut editor_schema = self.schema.clone();
        if !matches!(
            node.kind,
            NodeKind::Monitor { .. } | NodeKind::Constant { .. }
        ) && let Some(options) = editor_schema
            .pointer_mut("/$defs/SignalType/enum")
            .and_then(Value::as_array_mut)
        {
            options.retain(|value| value != "text");
        }
        edit_value(
            ui,
            &mut self.parameter_value,
            &editor_schema,
            &editor_schema,
            "",
            &active_seats(seats, active),
            editable,
            &mut self.parameter_text,
            &mut errors,
            controls,
            0,
        );
        let candidate = serde_json::from_value::<NodeKind>(self.parameter_value.clone());
        self.parameter_error = errors.first().cloned().or_else(|| {
            candidate
                .as_ref()
                .err()
                .map(|error| bounded(&error.to_string()))
        });
        if let Some(error) = &self.parameter_error {
            Text::new(error.clone()).wrap().show(ui);
        }
        if button(
            ui,
            "Update mapper node",
            "Update node",
            editable && self.parameter_error.is_none() && self.parameter_dirty(),
            ButtonVariant::Secondary,
            controls,
        ) && let Ok(kind) = candidate
        {
            let mut next = self.draft.clone();
            if let Some(node) = next.nodes.iter_mut().find(|node| node.id == id) {
                node.kind = kind;
            }
            self.replace(next);
            self.select_node(Some(&id));
        }
        if button(
            ui,
            "Duplicate mapper node",
            "Duplicate node",
            editable
                && !self.cleanup_pending
                && !self.parameter_dirty()
                && !is_input_source(&node.kind)
                && self.draft.nodes.len() < mapper::MAX_NODES,
            ButtonVariant::Outline,
            controls,
        ) {
            self.duplicate_node(&id);
        }
        if button(
            ui,
            "Discard mapper parameters",
            "Discard parameter edits",
            self.parameter_dirty(),
            ButtonVariant::Ghost,
            controls,
        ) {
            self.select_node(Some(&id));
        }
        Divider::horizontal().show(ui);
        Heading::new("Ports and wires").heading().show(ui);
        let ports = node.kind.ports();
        for output in ports.outputs {
            Text::new(format!(
                "Out {} · {}",
                output.name,
                signal_name(output.signal)
            ))
            .caption()
            .wrap()
            .show(ui);
        }
        for input in ports.inputs {
            let to = PortRef {
                node: id.clone(),
                port: input.name.clone(),
            };
            Text::new(format!("In {} · {}", input.name, signal_name(input.signal)))
                .wrap()
                .show(ui);
            if let Some(edge) = self.draft.edges.iter().find(|edge| edge.to == to).cloned() {
                Text::new(format!(
                    "Connected from {}.{}",
                    edge.from.node, edge.from.port
                ))
                .caption()
                .wrap()
                .show(ui);
                if button(
                    ui,
                    &format!("Disconnect mapper port {id}.{}", input.name),
                    "Disconnect",
                    editable,
                    ButtonVariant::Outline,
                    controls,
                ) {
                    let mut next = self.draft.clone();
                    next.edges.retain(|value| value != &edge);
                    self.replace(next);
                }
            } else {
                let choices: Vec<_> = self
                    .draft
                    .nodes
                    .iter()
                    .filter(|other| other.id != id)
                    .flat_map(|other| {
                        other
                            .kind
                            .ports()
                            .outputs
                            .into_iter()
                            .filter(|port| port.signal == input.signal)
                            .map(|port| PortRef {
                                node: other.id.clone(),
                                port: port.name,
                            })
                    })
                    .collect();
                let key = (id.clone(), input.name.clone());
                let mut index = self
                    .connection_sources
                    .get(&key)
                    .and_then(|selected| choices.iter().position(|port| port == selected))
                    .unwrap_or(0);
                let labels: Vec<_> = choices
                    .iter()
                    .map(|port| format!("{}.{}", port.node, port.port))
                    .collect();
                select(
                    ui,
                    &format!("Mapper source {id}.{}", input.name),
                    &mut index,
                    &labels,
                    editable && !choices.is_empty(),
                    controls,
                );
                if let Some(source) = choices.get(index) {
                    self.connection_sources.insert(key, source.clone());
                }
                if button(
                    ui,
                    &format!("Connect mapper port {id}.{}", input.name),
                    "Connect",
                    editable && !choices.is_empty() && self.draft.edges.len() < mapper::MAX_EDGES,
                    ButtonVariant::Secondary,
                    controls,
                ) && let Some(from) = choices.get(index)
                {
                    self.insert_canvas_edge(from, &to);
                }
                if choices.is_empty() {
                    Text::new("No compatible output port; add or configure another node.")
                        .caption()
                        .wrap()
                        .show(ui);
                }
            }
        }
        Divider::horizontal().show(ui);
        if button(
            ui,
            "Delete mapper node",
            "Delete node and its wires",
            editable,
            ButtonVariant::Outline,
            controls,
        ) {
            self.delete_node(&id);
        }
    }

    fn activity(&mut self, ui: &mut egui::Ui, controls: &mut Controls<'_>) {
        Heading::new("Mapper activity").heading().show(ui);
        self.visualizer_activity(ui);
        self.monitor_activity(ui);
        self.scope_activity(ui);
        Divider::horizontal().show(ui);
        Text::new(format!(
            "Applied revision {} · {} nodes · {} wires",
            self.revision,
            self.applied.nodes.len(),
            self.applied.edges.len()
        ))
        .wrap()
        .show(ui);
        Text::new(format!("Inputs processed: {}\nOutputs emitted: {}\nInputs rejected: {}\nActor contexts: {} / {}",
            self.processed_inputs, self.emitted_outputs, self.rejected_inputs, self.actor_contexts, mapper::MAX_ACTORS)).wrap().show(ui);
        Text::new(format!(
            "Pending timers: {}\nTimer pulses: {}",
            self.pending_timers, self.timer_pulses
        ))
        .wrap()
        .show(ui);
        Text::new(format!(
            "Queued delayed signals: {}\nDelayed signals processed: {}",
            self.pending_delays, self.delayed_events
        ))
        .wrap()
        .show(ui);
        Text::new(format!("Pending screen observations: {}\nScreen observations completed: {}\nScreen triggers dropped: {}", self.pending_observations, self.observation_completions, self.dropped_observations)).wrap().show(ui);
        if self.capture_worker_busy && !self.armed {
            Text::new("The previous capture worker is still finishing. Apply and Arm will return when it releases its slot.").caption().wrap().show(ui);
        }
        Text::new(if self.cleanup_pending {
            "Saved mapper release obligations remain pending."
        } else {
            "No mapper cleanup pending."
        })
        .wrap()
        .show(ui);
        if let Some(pending) = &self.pending {
            let (command, revision) = match pending {
                Pending::Apply {
                    expected_revision, ..
                } => ("Apply", expected_revision),
                Pending::Arm {
                    expected_revision, ..
                } => ("Arm", expected_revision),
            };
            Text::new(format!("Pending: {command} at revision {revision}"))
                .caption()
                .wrap()
                .show(ui);
        }
        if let Some(pending) = &self.disarm_pending {
            Text::new(format!(
                "Safety Disarm pending at revision {}",
                pending.expected_revision
            ))
            .caption()
            .wrap()
            .show(ui);
        }
        if button(
            ui,
            "Validate input mapper",
            "Validate draft",
            true,
            ButtonVariant::Outline,
            controls,
        ) {
            self.error = self.validation();
        }
        Text::new(self.validation().map_or_else(
            || "Structure validation passed. Apply also validates PNG templates and current runtime capability; validation emits no input.".into(),
            |error| format!("Draft validation: {error}"),
        ))
        .wrap()
        .show(ui);
        Text::new("Counters are bounded telemetry, not an input/text history. Arm is never saved or restored by navigation or profile load.").caption().wrap().show(ui);
    }

    fn monitor_activity(&self, ui: &mut egui::Ui) {
        let mut monitors = self
            .applied
            .nodes
            .iter()
            .filter_map(|node| match &node.kind {
                NodeKind::Monitor { signal } => Some((&node.id, *signal)),
                _ => None,
            })
            .peekable();
        if monitors.peek().is_none() {
            return;
        }
        Text::new("Signal monitors").show(ui);
        Text::new("Latest samples from manual input in this run.")
            .caption()
            .muted()
            .wrap()
            .show(ui);
        if !self.armed {
            Text::new("Arm the mapper to read manual input.")
                .wrap()
                .show(ui);
        } else if self.monitor_active() {
            ui.ctx().request_repaint_after(MONITOR_INTERVAL);
            for (id, signal) in monitors {
                let sample = self
                    .monitor_samples
                    .iter()
                    .find(|sample| &sample.node == id && sample.value.signal() == signal);
                let reading = sample.map_or_else(
                    || "No manual sample yet".into(),
                    |sample| monitor_reading(&sample.value),
                );
                Text::new(format!("{id} · {signal:?} · {reading}"))
                    .wrap()
                    .show(ui);
            }
        } else {
            Text::new("Monitor readings are paused.").wrap().show(ui);
        }
    }

    fn visualizer_activity(&self, ui: &mut egui::Ui) {
        let nodes: Vec<_> = self
            .applied
            .nodes
            .iter()
            .filter(|node| {
                matches!(
                    &node.kind,
                    NodeKind::Led
                        | NodeKind::Gauge { .. }
                        | NodeKind::XyPad { .. }
                        | NodeKind::PathTracer { .. }
                )
            })
            .collect();
        if nodes.is_empty() {
            return;
        }
        if self.monitor_active() {
            ui.ctx().request_repaint_after(MONITOR_INTERVAL);
        }
        let available = ui.available_width();
        let columns = if available >= 620.0 { 2 } else { 1 };
        let gap = core::SPACE_2;
        let padding = core::SPACE_2;
        let content_width = ((available - gap * (columns - 1) as f32) / columns as f32
            - (padding + core::BORDER_THIN) * 2.0)
            .max(1.0);
        egui::Grid::new("mapper_visualizer_cards")
            .num_columns(columns)
            .spacing(Vec2::splat(gap))
            .show(ui, |ui| {
                for (index, node) in nodes.iter().enumerate() {
                    let kind = &node.kind;
                    let signal = kind.latest_observer_signal();
                    let latest = signal.and_then(|expected| {
                        self.monitor_samples.iter().find(|sample| {
                            sample.node == node.id && sample.value.signal() == expected
                        })
                    });
                    let trace = kind.trace_observer_signal().and_then(|expected| {
                        self.scope_traces.iter().find(|trace| {
                            trace.node == node.id
                                && trace.samples.len() <= mapper::MAX_SCOPE_SAMPLES
                                && trace
                                    .samples
                                    .iter()
                                    .all(|sample| sample.value.signal() == expected)
                        })
                    });
                    let label = format!(
                        "{} · {}",
                        node.id,
                        match kind {
                            NodeKind::Led => "LED",
                            NodeKind::Gauge { .. } => "Gauge",
                            NodeKind::XyPad { .. } => "XY Pad",
                            _ => "Path Tracer",
                        }
                    );
                    let _card = Surface::new().pad(padding).show(ui, |ui| {
                        ui.vertical(|ui| {
                            ui.set_width(content_width);
                            Text::new(label).caption().wrap().show(ui);
                            if !self.armed {
                                Text::new("Arm the mapper to read manual input.")
                                    .caption()
                                    .muted()
                                    .show(ui);
                            } else if !self.monitor_active() {
                                Text::new("Display paused.").caption().muted().show(ui);
                            }
                            match kind {
                                NodeKind::Led => {
                                    let held = latest.and_then(|sample| match &sample.value {
                                        mapper::Value::Hold(v) => Some(*v),
                                        _ => None,
                                    });
                                    let (response, painter) = ui.allocate_painter(
                                        Vec2::new(ui.available_width(), 54.0),
                                        Sense::hover(),
                                    );
                                    let color = if held == Some(true) {
                                        core::GREEN_500
                                    } else {
                                        ui.visuals().widgets.inactive.bg_fill
                                    };
                                    painter.circle_filled(
                                        egui::pos2(
                                            response.rect.left() + 17.0,
                                            response.rect.center().y,
                                        ),
                                        9.0,
                                        color,
                                    );
                                    Text::new(match held {
                                        Some(true) => "ON · Held",
                                        Some(false) => "OFF · Released",
                                        None => "No matching sample",
                                    })
                                    .show(ui);
                                }
                                NodeKind::Gauge { min, max, .. } => {
                                    let value = latest.and_then(|sample| match &sample.value {
                                        mapper::Value::State(v) | mapper::Value::Impulse(v) => {
                                            Some(*v)
                                        }
                                        _ => None,
                                    });
                                    let ratio =
                                        value.map(|v| normalized(v, *min, *max)).unwrap_or(0.0);
                                    let (response, painter) = ui.allocate_painter(
                                        Vec2::new(ui.available_width(), 20.0),
                                        Sense::hover(),
                                    );
                                    painter.rect_filled(
                                        response.rect,
                                        3.0,
                                        ui.visuals().widgets.inactive.bg_fill,
                                    );
                                    painter.rect_filled(
                                        Rect::from_min_size(
                                            response.rect.min,
                                            Vec2::new(
                                                response.rect.width() * ratio,
                                                response.rect.height(),
                                            ),
                                        ),
                                        3.0,
                                        visualizer_accent(ui.visuals()),
                                    );
                                    Text::new(format!(
                                        "{}   range [{min:?}, {max:?}]",
                                        value
                                            .map_or_else(|| "Waiting".into(), |v| format!("{v:?}"))
                                    ))
                                    .caption()
                                    .show(ui);
                                }
                                NodeKind::XyPad { min, max, .. } => {
                                    let value = latest.and_then(|sample| match &sample.value {
                                        mapper::Value::Position(v) | mapper::Value::Delta(v) => {
                                            Some(*v)
                                        }
                                        _ => None,
                                    });
                                    let (response, painter) = ui.allocate_painter(
                                        Vec2::new(ui.available_width(), 90.0),
                                        Sense::hover(),
                                    );
                                    paint_xy(
                                        &painter,
                                        response.rect,
                                        value,
                                        *min,
                                        *max,
                                        ui.visuals(),
                                    );
                                    Text::new(value.map_or_else(
                                        || "Waiting for vector".into(),
                                        |[x, y]| format!("X {x:?} · Y {y:?}"),
                                    ))
                                    .caption()
                                    .show(ui);
                                }
                                NodeKind::PathTracer { min, max } => {
                                    let samples =
                                        trace.map_or(&[][..], |trace| trace.samples.as_slice());
                                    let (response, painter) = ui.allocate_painter(
                                        Vec2::new(ui.available_width(), 90.0),
                                        Sense::hover(),
                                    );
                                    paint_path(
                                        &painter,
                                        response.rect,
                                        samples,
                                        *min,
                                        *max,
                                        ui.visuals(),
                                    );
                                    let latest = samples.last().and_then(|s| match &s.value {
                                        mapper::Value::Position(v) => Some(*v),
                                        _ => None,
                                    });
                                    Text::new(latest.map_or_else(
                                        || "Waiting for position".into(),
                                        |[x, y]| format!("Latest X {x:?} · Y {y:?}"),
                                    ))
                                    .caption()
                                    .show(ui);
                                }
                                _ => {}
                            }
                        });
                    });
                    #[cfg(test)]
                    ui.ctx().data_mut(|data| {
                        data.insert_temp(
                            egui::Id::new(("mapper-visualizer-card", &node.id)),
                            _card.response.rect,
                        );
                    });
                    if columns == 1 || index % columns == columns - 1 {
                        ui.end_row();
                    }
                }
            });
    }

    fn scope_activity(&self, ui: &mut egui::Ui) {
        let scopes: Vec<_> = self
            .applied
            .nodes
            .iter()
            .filter_map(|node| match &node.kind {
                NodeKind::Scope { signal } => Some((node.id.as_str(), *signal)),
                _ => None,
            })
            .collect();
        if scopes.is_empty() {
            return;
        }
        Divider::horizontal().show(ui);
        Text::new("Signal scopes").show(ui);
        Text::new("Recent values from manual input in this run.")
            .caption()
            .muted()
            .wrap()
            .show(ui);
        if self.monitor_active() {
            ui.ctx().request_repaint_after(MONITOR_INTERVAL);
        }
        for (id, signal) in scopes {
            let trace = self.scope_traces.iter().find(|trace| trace.node == id);
            let samples = trace.map_or(&[][..], |trace| trace.samples.as_slice());
            let title = format!("{id} · {signal:?}");
            Surface::new().pad(core::SPACE_2).show(ui, |ui| {
                Text::new(title).caption().show(ui);
                if !self.armed {
                    Text::new("Arm the mapper to read manual input.")
                        .caption()
                        .muted()
                        .show(ui);
                } else if !self.monitor_active() {
                    Text::new("Scope is paused.").caption().muted().show(ui);
                } else if samples.is_empty() {
                    Text::new("Waiting for a matching signal.")
                        .caption()
                        .muted()
                        .show(ui);
                }
                let width = ui.available_width().max(1.0);
                let (response, painter) =
                    ui.allocate_painter(Vec2::new(width, 112.0), Sense::hover());
                paint_scope(&painter, response.rect, signal, samples, ui.visuals());
            });
        }
    }

    fn graph_canvas(&mut self, ui: &mut egui::Ui, editable: bool, controls: &mut Controls<'_>) {
        let editable = editable
            && ui.input(|input| input.focused)
            && !ui.ctx().text_edit_focused()
            && !egui::Popup::is_any_open(ui.ctx())
            && !ui.ctx().any_popup_open();
        if !editable {
            self.clear_canvas_gestures();
        } else if self.canvas_wire_source.is_some() || self.canvas_wire_drag.is_some() {
            let cancelled =
                ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
            if cancelled
                || (self.canvas_wire_drag.is_some()
                    && !ui.input(|input| {
                        input.pointer.button_down(egui::PointerButton::Primary)
                            || input.pointer.button_released(egui::PointerButton::Primary)
                    }))
            {
                self.clear_canvas_wire();
            }
        }
        let width = ui.available_width().max(1.0);
        let graph_tokens = ouroboros_ui::graph::GraphTokens::get(ui);
        let padding = core::SPACE_2;
        let size = Vec2::new(86.0_f32.min((width - 2.0 * padding).max(1.0)), 28.0);
        let handle_offset = 9.0;
        let port_spacing = graph_tokens.handle_hit_radius * 2.0;
        let margin = Vec2::new(
            size.x * 0.5 + handle_offset + graph_tokens.handle_hit_radius,
            (port_spacing + graph_tokens.handle_hit_radius).max(size.y * 0.5)
                + if self.draft.groups.is_empty() {
                    0.0
                } else {
                    24.0
                },
        );
        let columns = (((width - 2.0 * padding) + core::SPACE_2) / (margin.x * 2.0 + core::SPACE_2))
            .floor()
            .clamp(1.0, 3.0) as usize;
        let rows = self.draft.nodes.len().max(1).div_ceil(columns);
        let height = (rows as f32 * (margin.y * 2.0 + core::SPACE_2) + padding * 2.0).max(210.0);
        let (response, painter) = ui.allocate_painter(
            Vec2::new(width, height),
            if editable {
                Sense::click_and_drag()
            } else {
                Sense::click()
            },
        );
        let area = response.rect.shrink(padding);
        let canvas_origin = area.min + margin;
        let travel = (area.size() - margin * 2.0).max(Vec2::ZERO);
        let normalize = |point: egui::Pos2| {
            let delta = point - canvas_origin;
            [
                (delta.x / travel.x.max(1.0)).clamp(0.0, 1.0) as f64,
                (delta.y / travel.y.max(1.0)).clamp(0.0, 1.0) as f64,
            ]
        };
        let fraction = |index: usize, count: usize| {
            if count <= 1 {
                0.5
            } else {
                index as f64 / (count - 1) as f64
            }
        };
        let points: BTreeMap<_, _> = self
            .draft
            .nodes
            .iter()
            .enumerate()
            .map(|(index, node)| {
                let position = self
                    .canvas_drag
                    .as_ref()
                    .and_then(|drag| drag.position(&node.id))
                    .unwrap_or_else(|| {
                        node.position.unwrap_or([
                            fraction(index % columns, columns.min(self.draft.nodes.len())),
                            fraction(index / columns, rows),
                        ])
                    });
                let x = if position[0].is_finite() {
                    position[0].clamp(0.0, 1.0) as f32
                } else {
                    0.5
                };
                let y = if position[1].is_finite() {
                    position[1].clamp(0.0, 1.0) as f32
                } else {
                    0.5
                };
                (
                    node.id.clone(),
                    canvas_origin + Vec2::new(x * travel.x, y * travel.y),
                )
            })
            .collect();
        let collapsed: BTreeMap<_, _> = self
            .draft
            .groups
            .iter()
            .filter(|g| g.collapsed)
            .flat_map(|g| g.nodes.iter().map(move |id| (id.clone(), g.id.clone())))
            .collect();
        let group_rects: BTreeMap<_, _> = self
            .draft
            .groups
            .iter()
            .filter_map(|group| {
                let anchor = *points.get(group.nodes.first()?)?;
                let rect = if group.collapsed {
                    Rect::from_center_size(anchor, size)
                } else {
                    group
                        .nodes
                        .iter()
                        .filter_map(|id| points.get(id))
                        .map(|point| Rect::from_center_size(*point, size))
                        .reduce(|a, b| a.union(b))?
                        .expand(5.0)
                };
                Some((group.id.clone(), rect))
            })
            .collect();
        let mut handles = Vec::new();
        for node in &self.draft.nodes {
            if collapsed.contains_key(&node.id) {
                continue;
            }
            let center = points[&node.id];
            let ports = node.kind.ports();
            let handle_size = Vec2::splat(graph_tokens.handle_hit_radius * 2.0);
            for (output, list) in [(false, &ports.inputs), (true, &ports.outputs)] {
                for (index, port) in list.iter().enumerate() {
                    let count = list.len();
                    let y = center.y
                        + (index as f32 - (count.saturating_sub(1) as f32 / 2.0)) * port_spacing;
                    let x = center.x
                        + if output {
                            size.x * 0.5 + handle_offset
                        } else {
                            -size.x * 0.5 - handle_offset
                        };
                    handles.push(CanvasPortHandle {
                        reference: PortRef {
                            node: node.id.clone(),
                            port: port.name.clone(),
                        },
                        signal: port.signal,
                        rect: Rect::from_center_size(egui::pos2(x, y), handle_size),
                        output,
                    });
                }
            }
        }
        let handle_map: BTreeMap<_, _> = handles
            .iter()
            .map(|h| {
                (
                    (
                        h.output,
                        h.reference.node.as_str(),
                        h.reference.port.as_str(),
                    ),
                    (h.rect.center(), h.signal),
                )
            })
            .collect();
        for edge in &self.draft.edges {
            let signal = self
                .draft
                .nodes
                .iter()
                .find(|node| node.id == edge.from.node)
                .and_then(|node| {
                    node.kind
                        .ports()
                        .outputs
                        .into_iter()
                        .find(|p| p.name == edge.from.port)
                })
                .map(|port| port.signal)
                .unwrap_or(mapper::SignalType::Delta);
            let from_group = collapsed.get(&edge.from.node);
            let to_group = collapsed.get(&edge.to.node);
            if from_group.is_some() && from_group == to_group {
                continue;
            }
            let from = from_group
                .and_then(|id| group_rects.get(id))
                .map(|rect| (rect.right_center(), signal))
                .or_else(|| {
                    handle_map
                        .get(&(true, edge.from.node.as_str(), edge.from.port.as_str()))
                        .copied()
                });
            let to = to_group
                .and_then(|id| group_rects.get(id))
                .map(|rect| (rect.left_center(), signal))
                .or_else(|| {
                    handle_map
                        .get(&(false, edge.to.node.as_str(), edge.to.port.as_str()))
                        .copied()
                });
            if let (Some((from, signal)), Some((to, _))) = (from, to) {
                painter.line_segment(
                    [from, to],
                    Stroke::new(
                        graph_tokens.edge_width,
                        if self.selected_edge.as_ref() == Some(edge) {
                            graph_tokens.edge_selected
                        } else {
                            signal_color(signal)
                        },
                    ),
                );
            }
        }
        if let Some(from) = &self.canvas_wire_drag
            && let Some(pointer) = ui.input(|input| input.pointer.interact_pos())
            && let Some(handle) = handles.iter().find(|h| h.output && h.reference == *from)
        {
            painter.line_segment(
                [handle.rect.center(), pointer],
                Stroke::new(graph_tokens.edge_width, graph_tokens.edge_selected),
            );
        }
        let mut selected = None;
        let mut paint_order: Vec<_> = points
            .iter()
            .filter(|(id, _)| !collapsed.contains_key(*id))
            .collect();
        paint_order.sort_by_key(|(id, _)| self.selected_nodes.contains(*id));
        for (id, &point) in paint_order {
            let rect = Rect::from_center_size(point, size);
            painter.rect_filled(
                rect,
                core::RADIUS_SM,
                if self.selected_nodes.contains(id) || self.selected_node.as_deref() == Some(id) {
                    core::BLUE_400
                } else {
                    core::ZINC_700
                },
            );
            if self.selected_nodes.contains(id) {
                painter.rect_stroke(
                    rect.expand(2.0),
                    core::RADIUS_SM,
                    Stroke::new(core::BORDER_FOCUS, graph_tokens.edge_selected),
                    egui::StrokeKind::Outside,
                );
            }
            painter.text(
                point,
                Align2::CENTER_CENTER,
                id,
                core::Size::Sm.text_style().font_id(),
                Color32::WHITE,
            );
            let response = ui.interact(
                rect,
                ui.make_persistent_id(("mapper-canvas-node", id)),
                if editable {
                    Sense::click_and_drag()
                } else {
                    Sense::click()
                },
            );
            let label = format!("Mapper canvas node {id}");
            if controls.observe(ui, &label, &response)
                && response.clicked()
                && controls.consume(ui.ctx(), &label)
            {
                if ui.input(|input| input.modifiers.shift) || self.canvas_select_mode {
                    if editable {
                        self.clear_canvas_wire();
                        self.selected_edge = None;
                        self.selected_group = None;
                        if !self.selected_nodes.remove(id) {
                            self.selected_nodes.insert(id.clone());
                        }
                    }
                } else {
                    self.selected_nodes.clear();
                    selected = Some(id.clone());
                }
            }
            if editable
                && response.drag_started_by(egui::PointerButton::Primary)
                && let Some(pointer) = response.interact_pointer_pos()
            {
                self.clear_canvas_wire();
                self.selected_edge = None;
                let origins = if self.selected_nodes.contains(id) {
                    points
                        .iter()
                        .filter(|(node, _)| self.selected_nodes.contains(*node))
                        .map(|(node, origin)| (node.clone(), normalize(*origin)))
                        .collect()
                } else {
                    self.selected_nodes.clear();
                    self.select_node(Some(id));
                    BTreeMap::from([(id.clone(), normalize(point))])
                };
                let start = pointer - response.total_drag_delta().unwrap_or_default();
                self.canvas_drag = Some(CanvasDrag {
                    id: id.clone(),
                    grab_offset: start - point,
                    origins,
                    delta: [0.0, 0.0],
                });
            }
            if editable
                && (response.dragged() || response.drag_stopped())
                && let Some(drag) = self.canvas_drag.as_mut().filter(|drag| drag.id == *id)
                && let Some(pointer) = response.interact_pointer_pos()
            {
                drag.move_to(normalize(pointer - drag.grab_offset));
            }
            if response.drag_stopped_by(egui::PointerButton::Primary)
                && self.canvas_drag.as_ref().is_some_and(|drag| drag.id == *id)
                && let Some(drag) = self.canvas_drag.take()
                && drag.delta != [0.0, 0.0]
            {
                let mut after = self.draft.clone();
                for node in &mut after.nodes {
                    if let Some(position) = drag.position(&node.id) {
                        node.position = Some(position);
                    }
                }
                self.replace(after);
            }
        }
        if editable {
            if response.drag_started_by(egui::PointerButton::Primary)
                && let Some(pointer) = response.interact_pointer_pos()
            {
                let press = pointer - response.total_drag_delta().unwrap_or_default();
                if area.contains(press)
                    && ui.clip_rect().contains(press)
                    && !points
                        .values()
                        .any(|point| Rect::from_center_size(*point, size).contains(press))
                    && !handles.iter().any(|handle| handle.rect.contains(press))
                    && !group_rects
                        .values()
                        .any(|rect| rect.expand(22.0).contains(press))
                {
                    self.clear_canvas_wire();
                    self.selected_edge = None;
                    self.marquee = Some((press, pointer));
                }
            }
            if (response.dragged_by(egui::PointerButton::Primary)
                || response.drag_stopped_by(egui::PointerButton::Primary))
                && let (Some((start, _)), Some(pointer)) =
                    (self.marquee, response.interact_pointer_pos())
            {
                self.marquee = Some((start, pointer));
            }
            if let Some((start, end)) = self.marquee {
                let rect = Rect::from_two_pos(start, end)
                    .intersect(area)
                    .intersect(ui.clip_rect());
                painter.rect_filled(rect, 0.0, graph_tokens.marquee_fill);
                painter.rect_stroke(
                    rect,
                    0.0,
                    Stroke::new(core::BORDER_THIN, graph_tokens.marquee_border),
                    egui::StrokeKind::Inside,
                );
                if response.drag_stopped_by(egui::PointerButton::Primary) {
                    self.selected_group = None;
                    self.selected_nodes = points
                        .iter()
                        .filter(|(id, _)| !collapsed.contains_key(*id))
                        .filter(|(_, point)| rect.intersects(Rect::from_center_size(**point, size)))
                        .map(|(id, _)| id.clone())
                        .collect();
                    self.marquee = None;
                }
            }
        }
        let mut port_clicked = false;
        let mut wire_release = None;
        for handle in &handles {
            let label = format!(
                "Mapper canvas {} {}.{}",
                if handle.output { "output" } else { "input" },
                handle.reference.node,
                handle.reference.port
            );
            painter.circle_filled(
                handle.rect.center(),
                graph_tokens.handle_radius,
                signal_color(handle.signal),
            );
            let active =
                self.canvas_wire_source.as_ref() == Some(&handle.reference) && handle.output;
            let outline = if active {
                Stroke::new(core::BORDER_FOCUS, graph_tokens.edge_selected)
            } else {
                Stroke::new(core::BORDER_THIN, graph_tokens.handle_border)
            };
            painter.circle_stroke(handle.rect.center(), graph_tokens.handle_radius, outline);
            let glyph = if handle.output { ">" } else { "<" };
            painter.text(
                handle.rect.center(),
                Align2::CENTER_CENTER,
                glyph,
                core::Size::Sm.text_style().font_id(),
                core::ZINC_900,
            );
            let hresponse = ui.interact(
                handle.rect,
                ui.make_persistent_id((
                    "mapper-canvas-handle",
                    &handle.reference.node,
                    &handle.reference.port,
                    handle.output,
                )),
                if editable && handle.output {
                    Sense::click_and_drag()
                } else if editable {
                    Sense::click()
                } else {
                    Sense::hover()
                },
            );
            let signal = format!("{} · {}", handle.reference.port, signal_name(handle.signal));
            let _ = hresponse.clone().on_hover_text(signal);
            let observed = controls.observe(ui, &label, &hresponse);
            if editable && handle.output && hresponse.drag_started_by(egui::PointerButton::Primary)
            {
                self.canvas_wire_drag = Some(handle.reference.clone());
                self.canvas_wire_source = None;
                self.selected_edge = None;
            }
            if editable
                && handle.output
                && hresponse.drag_stopped_by(egui::PointerButton::Primary)
                && self.canvas_wire_drag.as_ref() == Some(&handle.reference)
            {
                wire_release = self
                    .canvas_wire_drag
                    .take()
                    .zip(hresponse.interact_pointer_pos());
            }
            if editable
                && handle.output
                && observed
                && hresponse.clicked()
                && controls.consume(ui.ctx(), &label)
            {
                self.canvas_wire_source = Some(handle.reference.clone());
                self.canvas_wire_drag = None;
                self.selected_edge = None;
                port_clicked = true;
            }
            if editable
                && observed
                && !handle.output
                && hresponse.clicked()
                && controls.consume(ui.ctx(), &label)
            {
                port_clicked = true;
                if let Some(edge) = self.draft.edges.iter().find(|e| e.to == handle.reference) {
                    self.selected_edge = Some(edge.clone());
                    self.canvas_wire_source = None;
                } else if let Some(source) = self.canvas_wire_source.take() {
                    self.insert_canvas_edge(&source, &handle.reference);
                }
            }
        }
        for group in self.draft.groups.clone() {
            let Some(&rect) = group_rects.get(&group.id) else {
                continue;
            };
            let selected_group = self.selected_group.as_ref() == Some(&group.id);
            painter.rect_stroke(
                rect,
                core::RADIUS_SM,
                Stroke::new(
                    core::BORDER_THIN,
                    if selected_group {
                        graph_tokens.edge_selected
                    } else {
                        core::ZINC_700
                    },
                ),
                egui::StrokeKind::Outside,
            );
            let heading = if group.collapsed {
                rect
            } else {
                Rect::from_min_size(
                    rect.min - egui::vec2(0.0, 22.0),
                    egui::vec2(rect.width(), 20.0),
                )
            };
            painter.rect_filled(
                heading,
                core::RADIUS_SM,
                if selected_group {
                    core::BLUE_400
                } else {
                    core::ZINC_700
                },
            );
            let title = fitted_label(
                ui,
                &group.label,
                (ui.available_width() - heading.width()).max(0.0),
            );
            painter.text(
                heading.center(),
                Align2::CENTER_CENTER,
                title,
                core::Size::Sm.text_style().font_id(),
                Color32::WHITE,
            );
            let response = ui.interact(
                heading,
                ui.make_persistent_id(("mapper-canvas-group", &group.id)),
                if editable {
                    Sense::click_and_drag()
                } else {
                    Sense::click()
                },
            );
            let label = format!("Mapper canvas group {}", group.id);
            if controls.observe(ui, &label, &response)
                && response.clicked()
                && controls.consume(ui.ctx(), &label)
            {
                self.select_group(&group.id);
            }
            let Some(anchor) = group.nodes.first() else {
                continue;
            };
            let anchor_point = points[anchor];
            if editable
                && response.drag_started_by(egui::PointerButton::Primary)
                && let Some(pointer) = response.interact_pointer_pos()
            {
                self.select_group(&group.id);
                let start = pointer - response.total_drag_delta().unwrap_or_default();
                self.canvas_drag = Some(CanvasDrag {
                    id: anchor.clone(),
                    grab_offset: start - anchor_point,
                    origins: group
                        .nodes
                        .iter()
                        .filter_map(|id| points.get(id).map(|p| (id.clone(), normalize(*p))))
                        .collect(),
                    delta: [0.0, 0.0],
                });
            }
            if editable
                && (response.dragged() || response.drag_stopped())
                && let Some(drag) = self.canvas_drag.as_mut().filter(|d| &d.id == anchor)
                && let Some(pointer) = response.interact_pointer_pos()
            {
                drag.move_to(normalize(pointer - drag.grab_offset));
            }
            if response.drag_stopped_by(egui::PointerButton::Primary)
                && self.canvas_drag.as_ref().is_some_and(|d| &d.id == anchor)
                && let Some(drag) = self.canvas_drag.take()
                && drag.delta != [0.0, 0.0]
            {
                let mut after = self.draft.clone();
                for node in &mut after.nodes {
                    if let Some(position) = drag.position(&node.id) {
                        node.position = Some(position);
                    }
                }
                self.replace(after);
            }
        }
        if let Some((source, pointer)) = wire_release
            && area.contains(pointer)
            && ui.clip_rect().contains(pointer)
        {
            let mut targets = handles
                .iter()
                .filter(|h| !h.output && h.rect.contains(pointer));
            if let Some(target) = targets.next()
                && targets.next().is_none()
            {
                self.insert_canvas_edge(&source, &target.reference);
            }
        }
        if response.clicked() && self.canvas_wire_source.is_some() && !port_clicked {
            self.clear_canvas_wire();
        }
        if let Some(edge) = &self.selected_edge {
            let label = format!(
                "Disconnect mapper canvas wire {}.{} → {}.{}",
                edge.from.node, edge.from.port, edge.to.node, edge.to.port
            );
            if button(
                ui,
                "Disconnect mapper canvas wire",
                "Disconnect wire",
                editable,
                ButtonVariant::Outline,
                controls,
            ) {
                let selected = self.selected_edge.take().expect("selected wire");
                let mut next = self.draft.clone();
                next.edges.retain(|edge| edge != &selected);
                self.replace(next);
            }
            Text::new(label).caption().wrap().show(ui);
        }
        if let Some(id) = selected {
            self.cancel_canvas_interaction();
            self.choose_node(&id);
        }
    }
}

struct Controls<'a> {
    script: Option<&'a mut Script>,
    action_frame: &'a mut u64,
    frame_actions: &'a mut BTreeSet<String>,
    flags: &'a mut BTreeMap<String, ControlFlags>,
}
impl Controls<'_> {
    fn observe(&mut self, ui: &egui::Ui, label: &str, response: &egui::Response) -> bool {
        #[cfg(test)]
        ui.ctx().data_mut(|data| {
            data.insert_temp(
                egui::Id::new(("mapper-response", label)),
                (response.rect, response.interact_rect),
            );
            data.insert_temp(egui::Id::new(("mapper-clip", label)), ui.clip_rect())
        });
        self.flags.insert(
            label.to_owned(),
            ControlFlags {
                frame: ui.ctx().cumulative_frame_nr(),
                enabled: response.enabled() && response.sense.senses_click(),
                visible: ui.is_rect_visible(response.rect),
                whole_visible: response.interact_rect.contains_rect(response.rect)
                    && ui.clip_rect().contains_rect(response.rect),
            },
        );
        self.script
            .as_deref_mut()
            .is_none_or(|script| script.observe(label, response))
    }
    fn consume(&mut self, ctx: &egui::Context, label: &str) -> bool {
        let frame = ctx.cumulative_frame_nr();
        if *self.action_frame != frame {
            *self.action_frame = frame;
            self.frame_actions.clear();
        }
        self.frame_actions.insert(label.to_owned())
    }
}

fn button(
    ui: &mut egui::Ui,
    identity: &str,
    label: &str,
    enabled: bool,
    variant: ButtonVariant,
    controls: &mut Controls<'_>,
) -> bool {
    let display = fitted_label(ui, label, 2.0 * core::Size::Sm.pad_x());
    let response = ui
        .push_id(identity, |ui| {
            Button::new(display)
                .sm()
                .id_source(identity)
                .variant(variant)
                .enabled(enabled)
                .show(ui)
        })
        .inner
        .on_hover_text(label);
    controls.observe(ui, identity, &response)
        && response.clicked()
        && controls.consume(ui.ctx(), identity)
}

fn select(
    ui: &mut egui::Ui,
    identity: &str,
    index: &mut usize,
    labels: &[String],
    enabled: bool,
    controls: &mut Controls<'_>,
) -> bool {
    let previous = *index;
    let display: Vec<_> = labels
        .iter()
        .map(|label| fitted_label(ui, label, 2.0 * core::Size::Sm.pad_x() + 32.0))
        .collect();
    let response = ui
        .push_id(identity, |ui| {
            ui.add_enabled_ui(enabled, |ui| {
                Select::new(index)
                    .sm()
                    .options(display)
                    .placeholder("Choose…")
                    .show(ui)
            })
            .inner
        })
        .inner
        .on_hover_text(
            labels
                .get(*index)
                .map(String::as_str)
                .unwrap_or("Choose a value"),
        );
    let accepted = controls.observe(ui, identity, &response);
    if response.changed() && accepted && controls.consume(ui.ctx(), identity) {
        true
    } else {
        *index = previous;
        false
    }
}

fn visible_width(ui: &egui::Ui) -> f32 {
    ui.available_width()
        .min(ui.clip_rect().right() - ui.next_widget_position().x)
}

fn fitted_label(ui: &egui::Ui, label: &str, reserved: f32) -> String {
    let width = (visible_width(ui) - reserved).max(1.0);
    let fits = |text: &str| {
        let style = core::Size::Sm.text_style();
        let mut job = egui::text::LayoutJob::default();
        job.append(
            text,
            0.0,
            egui::text::TextFormat {
                font_id: style.font_id(),
                extra_letter_spacing: style.tracking,
                ..Default::default()
            },
        );
        ui.painter().layout_job(job).size().x <= width
    };
    if fits(label) {
        return label.to_owned();
    }
    let characters: Vec<_> = label.chars().take(256).collect();
    let mut low = 0;
    let mut high = characters.len();
    while low < high {
        let middle = low + (high - low).div_ceil(2);
        let candidate = characters[..middle].iter().collect::<String>() + "…";
        if fits(&candidate) {
            low = middle;
        } else {
            high = middle - 1;
        }
    }
    characters[..low].iter().collect::<String>() + "…"
}

fn bounded(text: &str) -> String {
    text.chars().take(ERROR_LIMIT).collect()
}
fn edge_label(edge: &Edge) -> String {
    format!(
        "{}.{} → {}.{}",
        edge.from.node, edge.from.port, edge.to.node, edge.to.port
    )
}
fn signal_name(signal: mapper::SignalType) -> String {
    serde_json::to_value(signal)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default()
}
fn signal_color(signal: mapper::SignalType) -> Color32 {
    match signal {
        mapper::SignalType::Position | mapper::SignalType::Delta => core::TEAL_300,
        mapper::SignalType::State | mapper::SignalType::Hold => core::BLUE_400,
        mapper::SignalType::Impulse | mapper::SignalType::Pulse => core::ZINC_300,
        mapper::SignalType::Text => core::AMBER_500,
    }
}

fn path_reaches(definition: &Definition, start: &str, target: &str) -> bool {
    let mut adjacency: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for edge in &definition.edges {
        adjacency
            .entry(edge.from.node.clone())
            .or_default()
            .push(edge.to.node.clone());
    }
    let mut pending = vec![start.to_owned()];
    let mut seen = BTreeSet::new();
    while let Some(node) = pending.pop() {
        if node == target {
            return true;
        }
        if seen.insert(node.clone()) {
            pending.extend(adjacency.get(&node).into_iter().flatten().cloned());
        }
    }
    false
}

fn monitor_reading(value: &mapper::Value) -> String {
    match value {
        mapper::Value::Position([x, y]) | mapper::Value::Delta([x, y]) => {
            format!("X {x:?}, Y {y:?}")
        }
        mapper::Value::State(value) | mapper::Value::Impulse(value) => format!("{value:?}"),
        mapper::Value::Hold(held) => if *held { "Held" } else { "Released" }.into(),
        mapper::Value::Pulse(fired) => if *fired { "Fired" } else { "Idle" }.into(),
        mapper::Value::Text(text) => text.clone(),
    }
}

fn normalized(value: f64, min: f64, max: f64) -> f32 {
    if !value.is_finite() || !min.is_finite() || !max.is_finite() || max <= min {
        return 0.0;
    }
    ((value.clamp(min, max) - min) / (max - min)) as f32
}

fn xy_point(rect: Rect, value: [f64; 2], min: [f64; 2], max: [f64; 2]) -> egui::Pos2 {
    egui::pos2(
        rect.left() + normalized(value[0], min[0], max[0]) * rect.width(),
        rect.top() + normalized(value[1], min[1], max[1]) * rect.height(),
    )
}

fn visualizer_accent(visuals: &egui::Visuals) -> Color32 {
    if visuals.dark_mode {
        core::TEAL_300
    } else {
        core::TEAL_600
    }
}

fn paint_xy(
    painter: &egui::Painter,
    rect: Rect,
    value: Option<[f64; 2]>,
    min: [f64; 2],
    max: [f64; 2],
    visuals: &egui::Visuals,
) {
    let plot = rect.shrink(5.0);
    if !plot.is_positive() {
        return;
    }
    painter.rect_stroke(
        plot,
        2.0,
        Stroke::new(1.0, visuals.widgets.noninteractive.bg_stroke.color),
        egui::StrokeKind::Inside,
    );
    painter.line_segment(
        [
            egui::pos2(plot.center().x, plot.top()),
            egui::pos2(plot.center().x, plot.bottom()),
        ],
        Stroke::new(1.0, visuals.widgets.noninteractive.bg_stroke.color),
    );
    painter.line_segment(
        [
            egui::pos2(plot.left(), plot.center().y),
            egui::pos2(plot.right(), plot.center().y),
        ],
        Stroke::new(1.0, visuals.widgets.noninteractive.bg_stroke.color),
    );
    if let Some(v) = value {
        painter.circle_filled(xy_point(plot, v, min, max), 5.0, visualizer_accent(visuals));
    }
}

fn paint_path(
    painter: &egui::Painter,
    rect: Rect,
    samples: &[mapper::ScopeSample],
    min: [f64; 2],
    max: [f64; 2],
    visuals: &egui::Visuals,
) {
    let plot = rect.shrink(5.0);
    if !plot.is_positive() {
        return;
    }
    painter.rect_stroke(
        plot,
        2.0,
        Stroke::new(1.0, visuals.widgets.noninteractive.bg_stroke.color),
        egui::StrokeKind::Inside,
    );
    let points: Vec<_> = samples
        .iter()
        .filter_map(|sample| match &sample.value {
            mapper::Value::Position(v) if v.iter().all(|n| n.is_finite()) => {
                Some(xy_point(plot, *v, min, max))
            }
            _ => None,
        })
        .collect();
    let latest = points.last().copied();
    if points.len() > 1 {
        painter.add(egui::Shape::line(
            points,
            Stroke::new(1.5, visualizer_accent(visuals)),
        ));
    }
    if let Some(point) = latest {
        painter.circle_filled(point, 4.0, visualizer_accent(visuals));
    }
}

fn paint_scope(
    painter: &egui::Painter,
    rect: Rect,
    signal: mapper::SignalType,
    samples: &[mapper::ScopeSample],
    visuals: &egui::Visuals,
) {
    if signal == mapper::SignalType::Text
        || samples
            .iter()
            .any(|sample| matches!(sample.value, mapper::Value::Text(_)))
    {
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            "Text is available in Monitor; Scope requires a numeric signal.",
            egui::FontId::proportional(12.0),
            visuals.text_color(),
        );
        return;
    }
    let plot = rect.shrink2(Vec2::new(30.0, 15.0));
    if !plot.is_positive() {
        return;
    }
    let clip = painter.with_clip_rect(rect);
    let foreground = visuals.text_color();
    let faint = visuals.widgets.noninteractive.bg_stroke.color;
    clip.rect_stroke(plot, 2.0, Stroke::new(1.0, faint), egui::StrokeKind::Inside);
    let (first, last) = samples
        .first()
        .zip(samples.last())
        .map_or((0_u64, 0_u64), |(first, last)| {
            (first.elapsed_us, last.elapsed_us)
        });
    let x_at = |time: u64| {
        let fraction = if first == last {
            0.5
        } else {
            (time.saturating_sub(first) as f64 / last.saturating_sub(first) as f64).clamp(0.0, 1.0)
        };
        plot.left() + plot.width() * fraction as f32
    };
    let channels: &[(&str, usize)] = match signal {
        mapper::SignalType::Position | mapper::SignalType::Delta => &[("X", 0), ("Y", 1)],
        _ => &[("Value", 0)],
    };
    let channel_color = |index| {
        if index == 0 {
            visuals.selection.bg_fill
        } else {
            core::AMBER_500
        }
    };
    let value_at = |sample: &mapper::ScopeSample, axis: usize| -> f64 {
        match &sample.value {
            mapper::Value::Position(values) | mapper::Value::Delta(values) => values[axis],
            mapper::Value::State(value) | mapper::Value::Impulse(value) => *value,
            mapper::Value::Hold(held) => {
                if *held {
                    1.0
                } else {
                    0.0
                }
            }
            mapper::Value::Pulse(fired) => {
                if *fired {
                    1.0
                } else {
                    0.0
                }
            }
            // Text samples are rejected before constructing this numeric projection.
            mapper::Value::Text(_) => unreachable!("Text is not a Scope signal"),
        }
        .clamp(-mapper::NUMERIC_LIMIT, mapper::NUMERIC_LIMIT)
    };
    let mut min = samples
        .iter()
        .flat_map(|sample| channels.iter().map(|(_, axis)| value_at(sample, *axis)))
        .fold(0.0_f64, f64::min);
    let mut max = samples
        .iter()
        .flat_map(|sample| channels.iter().map(|(_, axis)| value_at(sample, *axis)))
        .fold(0.0_f64, f64::max);
    if matches!(signal, mapper::SignalType::Hold | mapper::SignalType::Pulse) {
        min = 0.0;
        max = 1.0;
    } else if min == max {
        min -= 1.0;
        max += 1.0;
    }
    // Keep the graph legible and the normalization finite for malformed inputs.
    let span = (max - min).clamp(1.0e-9, mapper::NUMERIC_LIMIT * 2.0);
    let y_at = |value: f64| plot.bottom() - plot.height() * ((value - min) / span) as f32;
    for (channel_index, (label, axis)) in channels.iter().enumerate() {
        let color = channel_color(channel_index);
        clip.text(
            egui::pos2(rect.left() + 3.0, plot.top() + channel_index as f32 * 12.0),
            Align2::LEFT_TOP,
            *label,
            egui::FontId::proportional(10.0),
            color,
        );
        if samples.is_empty() {
            continue;
        }
        match signal {
            mapper::SignalType::Hold => {
                for pair in samples.windows(2) {
                    let x1 = x_at(pair[0].elapsed_us);
                    let x2 = x_at(pair[1].elapsed_us);
                    let y1 = y_at(value_at(&pair[0], *axis));
                    let y2 = y_at(value_at(&pair[1], *axis));
                    clip.line_segment(
                        [egui::pos2(x1, y1), egui::pos2(x2, y1)],
                        Stroke::new(1.5, color),
                    );
                    clip.line_segment(
                        [egui::pos2(x2, y1), egui::pos2(x2, y2)],
                        Stroke::new(1.5, color),
                    );
                }
                if samples.len() == 1 {
                    let sample = &samples[0];
                    clip.circle_filled(
                        egui::pos2(x_at(sample.elapsed_us), y_at(value_at(sample, *axis))),
                        2.5,
                        color,
                    );
                }
            }
            mapper::SignalType::Impulse | mapper::SignalType::Pulse => {
                let baseline = y_at(0.0);
                for sample in samples {
                    let value = value_at(sample, *axis);
                    if value != 0.0 {
                        let x = x_at(sample.elapsed_us);
                        clip.line_segment(
                            [egui::pos2(x, baseline), egui::pos2(x, y_at(value))],
                            Stroke::new(1.5, color),
                        );
                        clip.circle_filled(egui::pos2(x, y_at(value)), 2.0, color);
                    }
                }
            }
            _ => {
                let points: Vec<_> = samples
                    .iter()
                    .map(|sample| {
                        egui::pos2(x_at(sample.elapsed_us), y_at(value_at(sample, *axis)))
                    })
                    .collect();
                if points.len() == 1 {
                    clip.circle_filled(points[0], 2.5, color);
                } else if points.len() > 1 {
                    clip.add(egui::Shape::line(points, Stroke::new(1.5, color)));
                }
            }
        }
    }
    clip.text(
        egui::pos2(plot.left(), rect.bottom() - 2.0),
        Align2::LEFT_BOTTOM,
        format!("{:.1} ms", first as f64 / 1000.0),
        egui::FontId::proportional(9.0),
        foreground,
    );
    clip.text(
        egui::pos2(plot.right(), rect.bottom() - 2.0),
        Align2::RIGHT_BOTTOM,
        format!("{:.1} ms", last as f64 / 1000.0),
        egui::FontId::proportional(9.0),
        foreground,
    );
}

fn is_input_source(kind: &NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Pointer { .. }
            | NodeKind::Wheel { .. }
            | NodeKind::Key { .. }
            | NodeKind::Button { .. }
    )
}

fn kind_name(kind: &NodeKind) -> String {
    if matches!(
        kind,
        NodeKind::Sink {
            channel: mapper::OutputChannel::TypeText,
            ..
        }
    ) {
        return "type text".into();
    }
    serde_json::to_value(kind)
        .ok()
        .and_then(|value| value.get("type").and_then(Value::as_str).map(str::to_owned))
        .unwrap_or_else(|| "node".into())
        .replace('_', " ")
}

fn active_seats(seats: &[Seat], active: &[bool]) -> Vec<(String, String)> {
    seats
        .iter()
        .enumerate()
        .filter(|(index, seat)| {
            active.get(*index).copied().unwrap_or(false)
                && mux_core::emulated::parse(&seat.mouse)
                    .ok()
                    .flatten()
                    .is_some_and(|device| device.kind == mux_core::DeviceKind::Mouse)
                && seat.keyboard.as_deref().is_none_or(|path| {
                    mux_core::emulated::parse(path)
                        .ok()
                        .flatten()
                        .is_some_and(|device| device.kind == mux_core::DeviceKind::Keyboard)
                })
        })
        .map(|(_, seat)| {
            (
                seat.identity(),
                format!("{} · {}", seat.name, seat.identity()),
            )
        })
        .collect()
}

// An original schematic of the validated structural preview; no user bindings or parameters.
fn flow_preview(ui: &mut egui::Ui, preview: &Preview) {
    let (response, painter) =
        ui.allocate_painter(Vec2::new(ui.available_width(), 84.0), Sense::hover());
    if preview.nodes.is_empty() {
        painter.text(
            response.rect.center(),
            Align2::CENTER_CENTER,
            "Empty flow",
            core::Size::Sm.text_style().font_id(),
            core::ZINC_400,
        );
        return;
    }
    let boxed = preview.nodes.len() <= 8;
    let half_node = if boxed {
        Vec2::new(28.0, 9.0)
    } else {
        Vec2::splat(3.0)
    };
    let rect = response
        .rect
        .shrink2(half_node + Vec2::splat(core::SPACE_1));
    let coordinates: Vec<_> = preview
        .nodes
        .iter()
        .map(|node| {
            node.position.map(|value| {
                if value.is_finite() {
                    value.clamp(0.0, 1.0)
                } else {
                    0.5
                }
            })
        })
        .collect();
    let min_x = coordinates
        .iter()
        .map(|point| point[0])
        .fold(f64::INFINITY, f64::min);
    let min_y = coordinates
        .iter()
        .map(|point| point[1])
        .fold(f64::INFINITY, f64::min);
    let max_x = coordinates
        .iter()
        .map(|point| point[0])
        .fold(f64::NEG_INFINITY, f64::max);
    let max_y = coordinates
        .iter()
        .map(|point| point[1])
        .fold(f64::NEG_INFINITY, f64::max);
    let axis = |value: f64, min: f64, max: f64| {
        if max - min > f64::EPSILON {
            ((value - min) / (max - min)) as f32
        } else {
            0.5
        }
    };
    let positions: Vec<_> = coordinates
        .iter()
        .map(|point| {
            egui::pos2(
                rect.left() + axis(point[0], min_x, max_x) * rect.width(),
                rect.top() + axis(point[1], min_y, max_y) * rect.height(),
            )
        })
        .collect();
    for &[source, target] in &preview.edges {
        if let (Some(&source), Some(&target)) = (positions.get(source), positions.get(target)) {
            let delta = target - source;
            if delta.length_sq() > 1.0 {
                let direction = delta.normalized();
                let radius =
                    1.0 / (direction.x.abs() / half_node.x).max(direction.y.abs() / half_node.y);
                let start = source + direction * radius;
                let end = target - direction * radius;
                if (end - start).dot(direction) > 1.0 {
                    let stroke = Stroke::new(core::BORDER_THIN, core::TEAL_600);
                    let head = (end - start).length().min(6.0);
                    let sideways = Vec2::new(-direction.y, direction.x) * (head * 0.5);
                    let neck = end - direction * head;
                    painter.line_segment([start, end], stroke);
                    painter.line_segment([neck + sideways, end], stroke);
                    painter.line_segment([neck - sideways, end], stroke);
                }
            }
        }
    }
    for (node, position) in preview.nodes.iter().zip(positions) {
        if boxed {
            let node_rect = Rect::from_center_size(position, half_node * 2.0);
            painter.rect_filled(node_rect, core::RADIUS_SM, core::ZINC_800);
            painter.rect_stroke(
                node_rect,
                core::RADIUS_SM,
                Stroke::new(core::BORDER_THIN, core::ZINC_500),
                egui::StrokeKind::Inside,
            );
            let label = match node.kind.as_str() {
                "sink" => "Output",
                "position_to_delta" => "Δ",
                "delta_to_position" => "Position",
                "dead_zone_delta" => "Dead zone",
                "path_tracer" => "Trail",
                "impulse_to_pulse" => "Pulse",
                kind => kind.split('_').next().unwrap_or(kind),
            };
            let short: String = label.chars().take(8).collect();
            painter.text(
                position,
                Align2::CENTER_CENTER,
                short,
                core::Size::Sm.text_style().font_id(),
                core::ZINC_200,
            );
        } else {
            painter.circle_filled(position, half_node.x, core::ZINC_400);
        }
    }
}

fn template_binding<'a>(
    ui: &mut egui::Ui,
    label: &str,
    identity: &str,
    selected: &mut Option<String>,
    users: &'a [(String, String)],
    editable: bool,
    controls: &mut Controls<'_>,
) -> Option<&'a str> {
    if selected.is_none() {
        *selected = users.first().map(|(id, _)| id.clone());
    }
    let mut index = users
        .iter()
        .position(|(id, _)| selected.as_ref() == Some(id))
        .unwrap_or(usize::MAX);
    let labels = users
        .iter()
        .map(|(_, label)| label.clone())
        .collect::<Vec<_>>();
    ui.horizontal(|ui| {
        Text::new(label).caption().show(ui);
        if select(ui, identity, &mut index, &labels, editable, controls) {
            *selected = users.get(index).map(|(id, _)| id.clone());
        }
    });
    users.get(index).map(|(id, _)| id.as_str())
}

fn png_template_summary(encoded: &str) -> anyhow::Result<String> {
    use base64::Engine as _;
    anyhow::ensure!(
        encoded.len() <= crate::screen_analysis::MAX_TEMPLATE_BASE64_BYTES,
        "Template exceeds the encoded size limit"
    );
    let prefix = encoded
        .as_bytes()
        .get(..44)
        .ok_or_else(|| anyhow::anyhow!("Choose a PNG template"))?;
    let bytes = base64::engine::general_purpose::STANDARD.decode(prefix)?;
    anyhow::ensure!(
        bytes.len() >= 24 && &bytes[..8] == b"\x89PNG\r\n\x1a\n" && &bytes[12..16] == b"IHDR",
        "Template must have a PNG header"
    );
    let width = u32::from_be_bytes(bytes[16..20].try_into()?);
    let height = u32::from_be_bytes(bytes[20..24].try_into()?);
    anyhow::ensure!(
        width > 0 && height > 0 && width <= 128 && height <= 128,
        "Template must be at most 128 × 128"
    );
    Ok(format!(
        "PNG template · {width} × {height} · {} encoded bytes",
        encoded.len()
    ))
}

fn assign_users(value: &mut Value, seats: &[(String, String)]) {
    if let Value::Object(fields) = value {
        for key in ["user", "capture_user"] {
            if let Some(user) = fields.get_mut(key)
                && let Some((id, _)) = seats.first()
            {
                *user = Value::String(id.clone());
            }
        }
        for field in fields.values_mut() {
            assign_users(field, seats);
        }
    }
}

fn resolved<'a>(schema: &'a Value, root: &'a Value) -> &'a Value {
    let mut current = schema;
    for _ in 0..FORM_DEPTH {
        let Some(reference) = current
            .get("$ref")
            .and_then(Value::as_str)
            .and_then(|value| value.strip_prefix('#'))
        else {
            break;
        };
        let Some(next) = root.pointer(reference) else {
            break;
        };
        current = next;
    }
    current
}

fn variants<'a>(schema: &'a Value, root: &'a Value) -> Vec<&'a Value> {
    let schema = resolved(schema, root);
    schema
        .get("oneOf")
        .or_else(|| schema.get("anyOf"))
        .and_then(Value::as_array)
        .map(|values| values.iter().map(|value| resolved(value, root)).collect())
        .unwrap_or_default()
}

fn tag(schema: &Value) -> Option<&str> {
    let field = schema.get("properties")?.get("type")?;
    field
        .get("const")
        .and_then(Value::as_str)
        .or_else(|| field.get("enum")?.as_array()?.first()?.as_str())
}

fn schema_default(schema: &Value, root: &Value, depth: usize) -> Value {
    if depth > FORM_DEPTH {
        return Value::Null;
    }
    let schema = resolved(schema, root);
    if let Some(value) = schema.get("default").or_else(|| schema.get("const")) {
        return value.clone();
    }
    if let Some(value) = schema
        .get("enum")
        .and_then(Value::as_array)
        .and_then(|values| values.first())
    {
        return value.clone();
    }
    if let Some(first) = variants(schema, root).first() {
        return schema_default(first, root, depth + 1);
    }
    if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
        return Value::Object(
            properties
                .iter()
                .map(|(key, field)| (key.clone(), schema_default(field, root, depth + 1)))
                .collect(),
        );
    }
    match schema.get("type").and_then(Value::as_str) {
        Some("boolean") => Value::Bool(false),
        Some("string") => Value::String(String::new()),
        Some("integer") => schema
            .get("minimum")
            .filter(|value| value.is_i64() || value.is_u64())
            .cloned()
            .unwrap_or_else(|| serde_json::json!(0)),
        Some("number") => {
            serde_json::json!(schema.get("minimum").and_then(Value::as_f64).unwrap_or(0.0))
        }
        Some("array") => {
            if let Some(items) = schema.get("prefixItems").and_then(Value::as_array) {
                Value::Array(
                    items
                        .iter()
                        .map(|item| schema_default(item, root, depth + 1))
                        .collect(),
                )
            } else {
                let count = schema
                    .get("minItems")
                    .and_then(Value::as_u64)
                    .unwrap_or(0)
                    .min(8);
                Value::Array(
                    (0..count)
                        .map(|_| schema_default(&schema["items"], root, depth + 1))
                        .collect(),
                )
            }
        }
        _ => Value::Null,
    }
}

#[allow(clippy::too_many_arguments)]
fn edit_value(
    ui: &mut egui::Ui,
    value: &mut Value,
    schema: &Value,
    root: &Value,
    path: &str,
    seats: &[(String, String)],
    editable: bool,
    texts: &mut BTreeMap<String, String>,
    errors: &mut Vec<String>,
    controls: &mut Controls<'_>,
    depth: usize,
) {
    if depth > FORM_DEPTH {
        errors.push("Parameter nesting exceeded the editor bound.".into());
        return;
    }
    if path == "template_png_base64" {
        match value
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Template must be a PNG string"))
            .and_then(png_template_summary)
        {
            Ok(summary) => {
                Text::new(summary).wrap().show(ui);
            }
            Err(error) => errors.push(format!("{error:#}")),
        }
        Text::new("Use Screen tools to capture and deliberately copy a template into this graph. The image becomes shared configuration after Update and Apply.").caption().wrap().show(ui);
        return;
    }
    if path == "region" {
        let mut enabled = !value.is_null();
        let response = ui
            .add_enabled_ui(editable, |ui| {
                Checkbox::new(&mut enabled)
                    .label("Limit search region")
                    .show(ui)
            })
            .inner;
        if controls.observe(ui, "Mapper search region enabled", &response)
            && response.changed()
            && controls.consume(ui.ctx(), "Mapper search region enabled")
        {
            *value = if enabled {
                serde_json::json!({"x":0,"y":0,"width":64,"height":64})
            } else {
                Value::Null
            };
            for key in ["region.x", "region.y", "region.width", "region.height"] {
                texts.remove(key);
            }
        }
        if value.is_null() {
            Text::new("Search the selected client area, within the work budget.")
                .caption()
                .wrap()
                .show(ui);
            return;
        }
        if let Some(fields) = value.as_object_mut() {
            for key in ["x", "y", "width", "height"] {
                if let Some(field) = fields.get_mut(key) {
                    Text::new(key).caption().show(ui);
                    edit_value(
                        ui,
                        field,
                        &Value::Null,
                        root,
                        &format!("region.{key}"),
                        seats,
                        editable,
                        texts,
                        errors,
                        controls,
                        depth + 1,
                    );
                }
            }
        } else {
            errors.push("Search region must be a rectangle".into());
        }
        return;
    }
    let schema = resolved(schema, root);
    let alternatives = variants(schema, root);
    if !alternatives.is_empty() {
        let current = value.get("type").and_then(Value::as_str);
        let mut index = alternatives
            .iter()
            .position(|schema| tag(schema) == current)
            .unwrap_or(0);
        let labels: Vec<_> = alternatives
            .iter()
            .enumerate()
            .map(|(index, schema)| {
                tag(schema)
                    .map(|name| name.replace('_', " "))
                    .unwrap_or_else(|| format!("Variant {}", index + 1))
            })
            .collect();
        // Node kind itself comes from the complete core catalogue; nested variants remain editable.
        if !path.is_empty()
            && select(
                ui,
                &format!("Mapper parameter {path} type"),
                &mut index,
                &labels,
                editable,
                controls,
            )
        {
            *value = schema_default(alternatives[index], root, depth + 1);
            assign_users(value, seats);
            texts.retain(|key, _| !key.starts_with(path));
        }
        edit_value(
            ui,
            value,
            alternatives[index],
            root,
            path,
            seats,
            editable,
            texts,
            errors,
            controls,
            depth + 1,
        );
        return;
    }
    match value {
        Value::Object(fields) => {
            for (key, field) in fields {
                if key == "type" {
                    continue;
                }
                let next = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
                };
                let field_schema = schema
                    .get("properties")
                    .and_then(|fields| fields.get(key))
                    .unwrap_or(&Value::Null);
                Text::new(match key.as_str() {
                    "capture_user" => "Capture user".into(),
                    "template_png_base64" => "Template image".into(),
                    "min_score" => "Minimum similarity".into(),
                    _ => key.replace('_', " "),
                })
                .caption()
                .show(ui);
                if key == "user" || key == "capture_user" {
                    let mut index = seats
                        .iter()
                        .position(|(id, _)| field.as_str() == Some(id))
                        .unwrap_or(usize::MAX);
                    let labels: Vec<_> = seats.iter().map(|(_, label)| label.clone()).collect();
                    if select(
                        ui,
                        &format!("Mapper parameter {next}"),
                        &mut index,
                        &labels,
                        editable && !seats.is_empty(),
                        controls,
                    ) && let Some((id, _)) = seats.get(index)
                    {
                        *field = Value::String(id.clone());
                    }
                    if !seats.iter().any(|(id, _)| field.as_str() == Some(id)) {
                        errors.push(format!("{next}: choose a current active virtual user."));
                    }
                } else {
                    edit_value(
                        ui,
                        field,
                        field_schema,
                        root,
                        &next,
                        seats,
                        editable,
                        texts,
                        errors,
                        controls,
                        depth + 1,
                    );
                }
            }
        }
        Value::Array(values) => {
            for (index, field) in values.iter_mut().enumerate().take(8) {
                let next = format!("{path}.{index}");
                let field_schema = schema
                    .get("prefixItems")
                    .and_then(Value::as_array)
                    .and_then(|items| items.get(index))
                    .or_else(|| schema.get("items"))
                    .unwrap_or(&Value::Null);
                edit_value(
                    ui,
                    field,
                    field_schema,
                    root,
                    &next,
                    seats,
                    editable,
                    texts,
                    errors,
                    controls,
                    depth + 1,
                );
            }
        }
        Value::Bool(current) => {
            let mut draft = *current;
            let response = ui
                .add_enabled_ui(editable, |ui| {
                    Checkbox::new(&mut draft).label(path).show(ui)
                })
                .inner;
            if controls.observe(ui, &format!("Mapper parameter {path}"), &response)
                && response.changed()
                && controls.consume(ui.ctx(), &format!("Mapper parameter {path}"))
            {
                *current = draft;
            }
        }
        Value::String(current) => {
            let options = resolved(schema, root).get("enum").and_then(Value::as_array);
            if let Some(options) = options {
                let labels: Vec<_> = options
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect();
                let mut index = labels
                    .iter()
                    .position(|value| value == current)
                    .unwrap_or(0);
                if select(
                    ui,
                    &format!("Mapper parameter {path}"),
                    &mut index,
                    &labels,
                    editable,
                    controls,
                ) && let Some(selected) = labels.get(index)
                {
                    current.clone_from(selected);
                }
            } else {
                let mut draft = current.clone();
                let response = ui
                    .add_enabled_ui(editable, |ui| {
                        Input::new(&mut draft)
                            .sm()
                            .id_source(("mapper-parameter", path))
                            .show(ui)
                    })
                    .inner;
                if controls.observe(ui, &format!("Mapper parameter {path}"), &response)
                    && response.changed()
                    && controls.consume(ui.ctx(), &format!("Mapper parameter {path}"))
                {
                    *current = draft;
                }
            }
        }
        Value::Number(_) => {
            let text = texts
                .entry(path.to_owned())
                .or_insert_with(|| value.to_string());
            let previous = text.clone();
            let response = ui
                .add_enabled_ui(editable, |ui| {
                    Input::new(text)
                        .sm()
                        .id_source(("mapper-number", path))
                        .show(ui)
                })
                .inner;
            if !controls.observe(ui, &format!("Mapper parameter {path}"), &response)
                || (response.changed()
                    && !controls.consume(ui.ctx(), &format!("Mapper parameter {path}")))
            {
                *text = previous;
            }
            match serde_json::from_str::<Value>(text) {
                Ok(number)
                    if number.is_number()
                        && number.as_f64().is_some_and(|value| {
                            value.is_finite() && value.abs() <= mapper::NUMERIC_LIMIT
                        }) =>
                {
                    *value = number
                }
                _ => errors.push(format!(
                    "{path}: enter a finite number in the mapper domain."
                )),
            }
        }
        Value::Null => errors.push(format!(
            "{path}: this parameter requires a supported value."
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn graph() -> Definition {
        Definition {
            nodes: vec![
                Node {
                    id: "pointer".into(),
                    kind: NodeKind::Pointer {
                        user: "virtual-user".into(),
                        disposition: mapper::Disposition::Consume,
                    },
                    position: None,
                },
                Node {
                    id: "output".into(),
                    kind: NodeKind::Sink {
                        user: "virtual-user".into(),
                        channel: mapper::OutputChannel::Delta,
                    },
                    position: None,
                },
            ],
            edges: vec![Edge {
                from: PortRef {
                    node: "pointer".into(),
                    port: "delta".into(),
                },
                to: PortRef {
                    node: "output".into(),
                    port: "value".into(),
                },
            }],
            ..Default::default()
        }
    }

    fn frame(
        ctx: &egui::Context,
        size: Vec2,
        events: Vec<egui::Event>,
        page: &mut MapperPage,
        busy: bool,
    ) -> Option<PageAction> {
        let seat: Seat = serde_json::from_value(serde_json::json!({ "id": "virtual-user", "name": "A representative virtual user with a long descriptive name", "mouse": "virtual://mouse/1", "keyboard": "virtual://keyboard/1" })).unwrap();
        let mut action = None;
        let mut action_frame = 0;
        let mut actions = BTreeSet::new();
        let modifiers = events
            .iter()
            .find_map(|event| match event {
                egui::Event::PointerButton { modifiers, .. } => Some(*modifiers),
                _ => None,
            })
            .unwrap_or_default();
        let _ = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, size)),
                modifiers,
                events,
                ..Default::default()
            },
            |root| {
                egui::CentralPanel::default().show_inside(root, |ui| {
                    Button::new("Back to Home").sm().show(ui);
                    ui.add_space(core::SPACE_2);
                    action = page.show(
                        ui,
                        std::slice::from_ref(&seat),
                        &[true],
                        true,
                        true,
                        busy,
                        true,
                        None,
                        &mut action_frame,
                        &mut actions,
                    );
                });
            },
        );
        action
    }

    fn rectangle(ctx: &egui::Context, label: &str, screen: Rect) -> Rect {
        let (rect, interact) = ctx
            .data(|data| data.get_temp::<(Rect, Rect)>(egui::Id::new(("mapper-response", label))))
            .unwrap();
        assert!(
            screen.contains_rect(rect),
            "{label} left the viewport: {rect:?}"
        );
        assert!(
            interact.contains_rect(rect),
            "{label} is clipped: {rect:?} in {interact:?}"
        );
        rect
    }

    fn click(position: egui::Pos2, pressed: bool) -> Vec<egui::Event> {
        vec![
            egui::Event::PointerMoved(position),
            egui::Event::PointerButton {
                pos: position,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            },
        ]
    }

    fn group_frame(
        ctx: &egui::Context,
        width: f32,
        events: Vec<egui::Event>,
        page: &mut MapperPage,
    ) -> egui::FullOutput {
        let mut action_frame = 0;
        let mut frame_actions = BTreeSet::new();
        let mut flags = BTreeMap::new();
        let output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(width, 900.0),
                )),
                events,
                ..Default::default()
            },
            |root| {
                egui::CentralPanel::default().show_inside(root, |ui| {
                    let mut controls = Controls {
                        script: None,
                        action_frame: &mut action_frame,
                        frame_actions: &mut frame_actions,
                        flags: &mut flags,
                    };
                    page.group_controls(ui, true, &mut controls);
                    page.graph_canvas(ui, true, &mut controls);
                });
            },
        );
        page.controls = flags;
        output
    }

    fn click_group_control(ctx: &egui::Context, width: f32, page: &mut MapperPage, label: &str) {
        group_frame(ctx, width, vec![], page);
        let rect = rectangle(
            ctx,
            label,
            Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, 900.0)),
        );
        assert!(page.controls[label].enabled, "{label} must be enabled");
        group_frame(ctx, width, click(rect.center(), true), page);
        group_frame(ctx, width, click(rect.center(), false), page);
    }

    #[test]
    fn collapsed_group_card_receives_click_and_drag_above_overlapping_nodes_and_ports() {
        for width in [320.0, 680.0] {
            let ctx = egui::Context::default();
            ouroboros_ui::Theme::install(&ctx, ouroboros_ui::Mode::Dark);
            let mut definition = graph();
            definition.nodes[0].position = Some([0.5, 0.5]);
            definition.nodes[1].position = Some([0.8, 0.6]);
            definition.nodes.push(Node {
                id: "processor".into(),
                kind: NodeKind::ScaleDelta { factor: 2.0 },
                position: Some([0.5, 0.5]),
            });
            definition.edges[0].from.node = "processor".into();
            definition.edges[0].from.port = "value".into();
            definition.edges.push(Edge {
                from: PortRef {
                    node: "pointer".into(),
                    port: "delta".into(),
                },
                to: PortRef {
                    node: "processor".into(),
                    port: "value".into(),
                },
            });
            definition.groups.push(NodeGroup {
                id: "group-1".into(),
                label: "Overlapping group".into(),
                nodes: vec!["processor".into(), "output".into()],
                collapsed: true,
            });
            let mut page = MapperPage {
                draft: definition.clone(),
                applied: definition.clone(),
                ..Default::default()
            };
            click_group_control(&ctx, width, &mut page, "Mapper canvas group group-1");
            assert_eq!(page.selected_group.as_deref(), Some("group-1"));
            assert_eq!(
                page.selected_nodes,
                BTreeSet::from(["processor".into(), "output".into()])
            );
            assert!(page.selected_node.is_none() && page.canvas_wire_source.is_none());

            let screen = Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, 900.0));
            let card = rectangle(&ctx, "Mapper canvas group group-1", screen);
            let start = card.center();
            let end = start + egui::vec2(12.0, 0.0);
            group_frame(&ctx, width, click(start, true), &mut page);
            group_frame(&ctx, width, vec![egui::Event::PointerMoved(end)], &mut page);
            group_frame(&ctx, width, click(end, false), &mut page);
            assert_eq!(page.draft.nodes[0].position, definition.nodes[0].position);
            assert_ne!(page.draft.nodes[1].position, definition.nodes[1].position);
            assert_ne!(page.draft.nodes[2].position, definition.nodes[2].position);
            assert_eq!(page.draft.edges, definition.edges);
            assert_eq!(page.undo.len(), 1);
            page.history_step(false);
            assert_eq!(page.draft, definition);
        }
    }

    #[test]
    fn named_groups_are_undoable_and_collapsed_cards_hide_ports_and_preserve_external_edges() {
        for width in [320.0, 680.0] {
            let ctx = egui::Context::default();
            ouroboros_ui::Theme::install(&ctx, ouroboros_ui::Mode::Dark);
            let mut definition = graph();
            definition.nodes[0].position = Some([0.0, 0.5]);
            definition.nodes[1].position = Some([1.0, 0.5]);
            definition.nodes.push(Node {
                id: "processor".into(),
                kind: NodeKind::ScaleDelta { factor: 2.0 },
                position: Some([0.4, 0.5]),
            });
            definition.edges[0].from = PortRef {
                node: "processor".into(),
                port: "value".into(),
            };
            definition.edges.push(Edge {
                from: PortRef {
                    node: "pointer".into(),
                    port: "delta".into(),
                },
                to: PortRef {
                    node: "processor".into(),
                    port: "value".into(),
                },
            });
            let mut page = MapperPage {
                draft: definition.clone(),
                applied: definition.clone(),
                selected_nodes: ["pointer".into(), "processor".into()].into_iter().collect(),
                ..Default::default()
            };
            click_group_control(&ctx, width, &mut page, "Group selected mapper nodes");
            assert_eq!(page.draft.groups.len(), 1);
            assert_eq!(page.snapshot().selected_group.as_deref(), Some("group-1"));
            assert_eq!(page.undo.len(), 1);
            page.apply_script_fields(&BTreeMap::from([(
                "mapper_group_label".into(),
                "Aim controls".into(),
            )]));
            click_group_control(&ctx, width, &mut page, "Rename mapper group");
            assert_eq!(page.draft.groups[0].label, "Aim controls");
            let expanded = page.draft.clone();
            click_group_control(&ctx, width, &mut page, "Collapse mapper group");
            assert!(page.draft.groups[0].collapsed);
            assert_eq!(page.draft.nodes, definition.nodes);
            assert_eq!(page.draft.edges, definition.edges);
            let output = group_frame(&ctx, width, vec![], &mut page);
            for label in [
                "Mapper canvas node pointer",
                "Mapper canvas node processor",
                "Mapper canvas output pointer.delta",
                "Mapper canvas input processor.value",
            ] {
                assert!(
                    !page.controls.contains_key(label),
                    "hidden member exposes {label}"
                );
            }
            let screen = Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, 900.0));
            let card = rectangle(&ctx, "Mapper canvas group group-1", screen);
            let sink = rectangle(&ctx, "Mapper canvas input output.value", screen);
            assert!(output.shapes.iter().any(|shape| matches!(&shape.shape,
                egui::Shape::LineSegment { points, .. } if *points == [card.right_center(), sink.center()])),
                "external wire must terminate at the collapsed card");
            click_group_control(&ctx, width, &mut page, "Mapper canvas group group-1");
            assert_eq!(page.selected_nodes.len(), 2);
            click_group_control(&ctx, width, &mut page, "Expand mapper group");
            assert_eq!(page.draft, expanded);
            group_frame(&ctx, width, vec![], &mut page);
            assert!(
                page.controls
                    .contains_key("Mapper canvas output pointer.delta")
            );
            click_group_control(&ctx, width, &mut page, "Ungroup mapper nodes");
            assert!(page.draft.groups.is_empty());
            page.history_step(false);
            assert_eq!(page.draft, expanded);
            assert_eq!(page.applied, definition);
            assert!(page.pending.is_none());
            // Deletion prunes membership in the same history entry; Undo restores it.
            page.selected_nodes = BTreeSet::from(["pointer".into()]);
            page.delete_selected_nodes();
            assert_eq!(page.draft.groups[0].nodes, ["processor"]);
            page.selected_nodes = BTreeSet::from(["processor".into()]);
            page.delete_selected_nodes();
            assert!(page.draft.groups.is_empty());
            page.history_step(false);
            assert_eq!(page.draft.groups[0].nodes, ["processor"]);
        }
    }

    #[test]
    fn selected_group_duplicate_and_delete_are_atomic_single_history_edits() {
        let mut definition = graph();
        definition.nodes.push(Node {
            id: "processor".into(),
            kind: NodeKind::ScaleDelta { factor: 2.0 },
            position: Some([0.4, 0.5]),
        });
        definition.edges.push(Edge {
            from: PortRef {
                node: "pointer".into(),
                port: "delta".into(),
            },
            to: PortRef {
                node: "processor".into(),
                port: "value".into(),
            },
        });
        definition.edges.push(Edge {
            from: PortRef {
                node: "processor".into(),
                port: "value".into(),
            },
            to: PortRef {
                node: "output".into(),
                port: "value".into(),
            },
        });
        let mut page = MapperPage {
            draft: definition.clone(),
            selected_nodes: ["processor".to_string(), "output".to_string()]
                .into_iter()
                .collect(),
            ..Default::default()
        };
        page.duplicate_selected_nodes();
        assert_eq!(page.draft.nodes.len(), 5);
        assert_eq!(page.draft.edges.len(), 4);
        assert_eq!(page.undo.len(), 1);
        let copies: BTreeSet<_> = page.selected_nodes.clone();
        assert_eq!(copies.len(), 2);
        assert!(
            page.draft
                .edges
                .iter()
                .any(|edge| copies.contains(&edge.from.node) && copies.contains(&edge.to.node))
        );
        page.history_step(false);
        assert_eq!(page.draft, definition);
        assert!(page.selected_nodes.is_empty());
        page.selected_nodes = ["processor".to_string(), "output".to_string()]
            .into_iter()
            .collect();
        page.delete_selected_nodes();
        assert_eq!(page.draft.nodes.len(), 1);
        assert!(page.draft.edges.is_empty());
        assert_eq!(page.undo.len(), 1);
        page.history_step(false);
        assert_eq!(page.draft, definition);
    }

    #[test]
    fn group_duplicate_rejections_do_not_advance_ids_or_mutate_draft() {
        let definition = graph();
        let mut page = MapperPage {
            draft: definition.clone(),
            selected_nodes: ["pointer".to_string()].into_iter().collect(),
            ..Default::default()
        };
        let counter = page.next_node;
        page.duplicate_selected_nodes();
        assert_eq!(page.draft, definition);
        assert_eq!(page.next_node, counter);
        assert!(page.undo.is_empty());
    }

    #[test]
    fn canvas_marquee_and_shift_click_select_a_group_that_moves_and_undoes_together() {
        let ctx = egui::Context::default();
        ouroboros_ui::Theme::install(&ctx, ouroboros_ui::Mode::Dark);
        let size = egui::vec2(680.0, 620.0);
        let screen = Rect::from_min_size(egui::Pos2::ZERO, size);
        let mut definition = graph();
        definition.nodes[0].position = Some([0.32, 0.5]);
        definition.nodes[1].position = Some([0.65, 0.5]);
        let mut page = MapperPage {
            draft: definition.clone(),
            canvas: true,
            ..Default::default()
        };
        let _ = frame(&ctx, size, vec![], &mut page, false);
        let left = rectangle(&ctx, "Mapper canvas node pointer", screen);
        let right = rectangle(&ctx, "Mapper canvas node output", screen);

        let start = left.left_top() - egui::vec2(14.0, 14.0);
        let end = right.right_bottom() + egui::vec2(14.0, 14.0);
        let _ = frame(&ctx, size, click(start, true), &mut page, false);
        let _ = frame(
            &ctx,
            size,
            vec![egui::Event::PointerMoved(end)],
            &mut page,
            false,
        );
        assert_eq!(page.draft, definition);
        let _ = frame(&ctx, size, click(end, false), &mut page, false);
        assert_eq!(page.snapshot().canvas_selection, ["output", "pointer"]);

        let shift_click = |pressed| {
            let mut events = click(left.center(), pressed);
            if let egui::Event::PointerButton { modifiers, .. } = &mut events[1] {
                *modifiers = egui::Modifiers::SHIFT;
            }
            events
        };
        let _ = frame(&ctx, size, shift_click(true), &mut page, false);
        let _ = frame(&ctx, size, shift_click(false), &mut page, false);
        assert_eq!(page.snapshot().canvas_selection, ["output"]);
        let _ = frame(&ctx, size, shift_click(true), &mut page, false);
        let _ = frame(&ctx, size, shift_click(false), &mut page, false);
        assert_eq!(page.snapshot().canvas_selection, ["output", "pointer"]);

        let travel = (right.center().x - left.center().x) / 0.33;
        let end = left.center() + egui::vec2(travel * 0.6, 24.0);
        let _ = frame(&ctx, size, click(left.center(), true), &mut page, false);
        let _ = frame(
            &ctx,
            size,
            vec![egui::Event::PointerMoved(end)],
            &mut page,
            false,
        );
        assert_eq!(page.draft, definition);
        let _ = frame(&ctx, size, click(end, false), &mut page, false);
        let first = page.draft.nodes[0].position.unwrap();
        let second = page.draft.nodes[1].position.unwrap();
        assert!(
            (second[0] - 1.0).abs() < 0.001,
            "right node must stop at the edge"
        );
        assert!((second[0] - first[0] - 0.33).abs() < 0.001);
        assert!((second[1] - first[1]).abs() < 0.001 && first[1] > 0.5);
        assert_eq!(page.draft.edges, definition.edges);
        assert_eq!(page.undo.len(), 1);
        page.history_step(false);
        assert_eq!(page.draft, definition);
    }

    #[test]
    fn visualizer_cards_fit_wide_columns_and_stack_compactly() {
        for width in [320.0, 680.0] {
            let ctx = egui::Context::default();
            ouroboros_ui::Theme::install(&ctx, ouroboros_ui::Mode::Dark);
            let mut page = MapperPage::default();
            page.applied.nodes = [
                ("led", NodeKind::Led),
                (
                    "gauge",
                    NodeKind::Gauge {
                        signal: mapper::ScalarType::State,
                        min: 0.0,
                        max: 100.0,
                    },
                ),
                (
                    "xy",
                    NodeKind::XyPad {
                        signal: mapper::VectorType::Position,
                        min: [0.0, 0.0],
                        max: [100.0, 100.0],
                    },
                ),
                (
                    "path",
                    NodeKind::PathTracer {
                        min: [0.0, 0.0],
                        max: [100.0, 100.0],
                    },
                ),
            ]
            .into_iter()
            .map(|(id, kind)| Node {
                id: id.into(),
                kind,
                position: None,
            })
            .collect();
            let input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(width, 620.0),
                )),
                ..Default::default()
            };
            let _ = ctx.run_ui(input, |ui| {
                egui::CentralPanel::default().show_inside(ui, |ui| page.visualizer_activity(ui));
            });
            let rects = ["led", "gauge", "xy", "path"].map(|id| {
                ctx.data(|data| {
                    data.get_temp::<Rect>(egui::Id::new(("mapper-visualizer-card", id)))
                })
                .expect("visualizer card rendered")
            });
            assert!(
                rects
                    .iter()
                    .all(|rect| rect.left() >= 0.0 && rect.right() <= width)
            );
            if width == 680.0 {
                assert!(rects[0].right() < rects[1].left());
                assert!(rects[2].right() < rects[3].left());
            } else {
                assert!(
                    rects
                        .windows(2)
                        .all(|pair| pair[0].bottom() <= pair[1].top())
                );
            }
        }
    }

    #[test]
    fn vector_visualizers_follow_desktop_axes_and_clamp_only_the_display() {
        let rect = Rect::from_min_size(egui::pos2(10.0, 20.0), egui::vec2(100.0, 200.0));
        let min = [-100.0, -50.0];
        let max = [100.0, 50.0];
        assert_eq!(xy_point(rect, min, min, max), rect.left_top());
        assert_eq!(xy_point(rect, max, min, max), rect.right_bottom());
        assert_eq!(xy_point(rect, [0.0, 0.0], min, max), rect.center());
        assert_eq!(xy_point(rect, [200.0, -70.0], min, max), rect.right_top());
    }

    #[test]
    fn canvas_drag_commits_one_undo_and_preserves_canvas_selection() {
        let ctx = egui::Context::default();
        ouroboros_ui::Theme::install(&ctx, ouroboros_ui::Mode::Dark);
        let size = egui::vec2(680.0, 620.0);
        let mut page = MapperPage {
            draft: graph(),
            canvas: true,
            ..Default::default()
        };
        let _ = frame(&ctx, size, vec![], &mut page, false);
        let screen = Rect::from_min_size(egui::Pos2::ZERO, size);
        let start = rectangle(&ctx, "Mapper canvas node output", screen).center();
        let _ = frame(&ctx, size, click(start, true), &mut page, false);
        let target = start + egui::vec2(-55.0, 35.0);
        let _ = frame(
            &ctx,
            size,
            vec![egui::Event::PointerMoved(target)],
            &mut page,
            false,
        );
        assert_eq!(page.undo.len(), 0);
        assert_eq!(
            page.draft,
            graph(),
            "drag preview must not mutate the authoritative draft"
        );
        let _ = frame(&ctx, size, click(target, false), &mut page, false);
        assert_eq!(page.undo.len(), 1);
        let _ = frame(&ctx, size, vec![], &mut page, false);
        let end = rectangle(&ctx, "Mapper canvas node output", screen).center();
        assert!(
            (end - target).length() < 1.0,
            "node did not follow the pointer"
        );
        assert_eq!(page.selected_node.as_deref(), Some("output"));
        page.history_step(false);
        assert_eq!(page.draft, graph());
        let _ = frame(&ctx, size, vec![], &mut page, false);
        let start = rectangle(&ctx, "Mapper canvas node output", screen).center();
        let _ = frame(&ctx, size, click(start, true), &mut page, false);
        let _ = frame(
            &ctx,
            size,
            vec![egui::Event::PointerMoved(target)],
            &mut page,
            false,
        );
        assert!(page.canvas_drag.is_some());
        let _ = frame(&ctx, size, click(target, false), &mut page, true);
        assert!(page.canvas_drag.is_none());
        assert!(page.undo.is_empty());
        assert_eq!(page.draft, graph());
    }

    #[test]
    fn canvas_ports_connect_by_drag_and_click_with_one_history_entry() {
        let ctx = egui::Context::default();
        ouroboros_ui::Theme::install(&ctx, ouroboros_ui::Mode::Dark);
        let size = egui::vec2(320.0, 620.0);
        let screen = Rect::from_min_size(egui::Pos2::ZERO, size);
        let mut empty = graph();
        empty.edges.clear();
        let mut page = MapperPage {
            draft: empty.clone(),
            canvas: true,
            ..Default::default()
        };
        let _ = frame(&ctx, size, vec![], &mut page, false);
        let out = rectangle(&ctx, "Mapper canvas output pointer.delta", screen).center();
        let input = rectangle(&ctx, "Mapper canvas input output.value", screen).center();
        let _ = frame(&ctx, size, click(out, true), &mut page, false);
        let _ = frame(&ctx, size, click(out, false), &mut page, false);
        let _ = frame(&ctx, size, click(input, true), &mut page, false);
        assert!(page.draft.edges.is_empty());
        let _ = frame(&ctx, size, click(input, false), &mut page, false);
        assert_eq!(page.draft.edges.len(), 1);
        assert_eq!(
            page.draft.edges[0].from,
            PortRef {
                node: "pointer".into(),
                port: "delta".into()
            }
        );
        assert_eq!(
            page.draft.edges[0].to,
            PortRef {
                node: "output".into(),
                port: "value".into()
            }
        );
        assert_eq!(page.undo.len(), 1);
        page.history_step(false);
        assert_eq!(page.draft, empty);

        let _ = frame(&ctx, size, vec![], &mut page, false);
        let out = rectangle(&ctx, "Mapper canvas output pointer.delta", screen).center();
        let input = rectangle(&ctx, "Mapper canvas input output.value", screen).center();
        let _ = frame(&ctx, size, click(out, true), &mut page, false);
        let _ = frame(
            &ctx,
            size,
            vec![egui::Event::PointerMoved(input)],
            &mut page,
            false,
        );
        assert_eq!(page.draft, empty);
        assert!(page.canvas_wire_drag.is_some());
        let _ = frame(&ctx, size, click(input, false), &mut page, false);
        assert_eq!(
            page.draft.edges,
            graph().edges,
            "primary drag inserts on release"
        );
        assert_eq!(page.undo.len(), 1);
        page.history_step(false);
        assert_eq!(page.draft, empty);
        let _ = frame(&ctx, size, click(out, true), &mut page, false);
        let _ = frame(&ctx, size, click(out, false), &mut page, false);
        assert!(page.canvas_wire_source.is_some());
        let _ = frame(&ctx, size, vec![], &mut page, true);
        assert!(page.canvas_wire_source.is_none());
        let _ = frame(&ctx, size, click(input, true), &mut page, false);
        let _ = frame(&ctx, size, click(input, false), &mut page, false);
        assert_eq!(page.draft, empty);
    }

    #[test]
    fn canvas_connection_rejects_cycles_and_wrong_typed_ports_without_mutation() {
        let mut page = MapperPage {
            draft: graph(),
            ..Default::default()
        };
        let old = page.draft.clone();
        assert!(!page.insert_canvas_edge(
            &PortRef {
                node: "pointer".into(),
                port: "delta".into()
            },
            &PortRef {
                node: "output".into(),
                port: "value".into()
            },
        ));
        assert!(!page.insert_canvas_edge(
            &PortRef {
                node: "output".into(),
                port: "value".into()
            },
            &PortRef {
                node: "pointer".into(),
                port: "delta".into()
            },
        ));
        assert_eq!(page.draft, old);
        let mut incomplete = graph();
        incomplete.edges.clear();
        incomplete.nodes.push(Node {
            id: "state-monitor".into(),
            kind: NodeKind::Monitor {
                signal: mapper::SignalType::State,
            },
            position: None,
        });
        page.draft = incomplete;
        assert!(!page.insert_canvas_edge(
            &PortRef {
                node: "pointer".into(),
                port: "delta".into()
            },
            &PortRef {
                node: "state-monitor".into(),
                port: "value".into()
            },
        ));
        assert!(!page.insert_canvas_edge(
            &PortRef {
                node: "pointer".into(),
                port: "delta".into()
            },
            &PortRef {
                node: "pointer".into(),
                port: "delta".into()
            },
        ));
        assert!(page.draft.edges.is_empty());
        let translate = NodeKind::Translate { offset: [0.0, 0.0] };
        page.draft = Definition {
            nodes: vec![
                Node {
                    id: "a".into(),
                    kind: translate.clone(),
                    position: None,
                },
                Node {
                    id: "b".into(),
                    kind: translate,
                    position: None,
                },
            ],
            edges: vec![Edge {
                from: PortRef {
                    node: "a".into(),
                    port: "value".into(),
                },
                to: PortRef {
                    node: "b".into(),
                    port: "value".into(),
                },
            }],
            ..Default::default()
        };
        assert!(!page.insert_canvas_edge(
            &PortRef {
                node: "b".into(),
                port: "value".into()
            },
            &PortRef {
                node: "a".into(),
                port: "value".into()
            },
        ));
        assert_eq!(page.draft.edges.len(), 1);
    }

    #[test]
    fn authoring_shortcuts_obey_busy_parameter_and_modifier_guards() {
        let ctx = egui::Context::default();
        ouroboros_ui::Theme::install(&ctx, ouroboros_ui::Mode::Dark);
        let size = egui::vec2(680.0, 620.0);
        let mut page = MapperPage {
            draft: graph(),
            ..Default::default()
        };
        page.select_node(Some("output"));
        let send = |key, modifiers, busy, page: &mut MapperPage| {
            let events = [true, false].map(|pressed| egui::Event::Key {
                key,
                physical_key: None,
                pressed,
                repeat: false,
                modifiers,
            });
            let _ = frame(&ctx, size, events.to_vec(), page, busy);
        };
        let windows_ctrl = egui::Modifiers::CTRL | egui::Modifiers::COMMAND;
        send(egui::Key::D, windows_ctrl, false, &mut page);
        let duplicated = page.draft.clone();
        let copy = duplicated.nodes.last().unwrap();
        assert_eq!(duplicated.nodes.len(), 3);
        assert_ne!(copy.id, "output");
        assert_eq!(copy.kind, graph().nodes[1].kind);
        assert!(copy.position.is_none());
        assert_eq!(duplicated.edges, graph().edges);
        send(egui::Key::Z, windows_ctrl, false, &mut page);
        assert_eq!(page.draft, graph());
        send(egui::Key::Y, windows_ctrl, false, &mut page);
        assert_eq!(page.draft, duplicated);
        page.select_node(Some("output"));
        send(egui::Key::Delete, egui::Modifiers::NONE, true, &mut page);
        assert_eq!(page.draft, duplicated);
        send(egui::Key::Delete, egui::Modifiers::CTRL, false, &mut page);
        assert_eq!(page.draft, duplicated);
        page.parameter_value["channel"] = serde_json::json!("other");
        send(egui::Key::Delete, egui::Modifiers::NONE, false, &mut page);
        assert_eq!(page.draft, duplicated);
        page.select_node(Some("output"));
        send(egui::Key::Delete, egui::Modifiers::NONE, false, &mut page);
        assert!(!page.draft.nodes.iter().any(|node| node.id == "output"));
        assert!(page.draft.edges.is_empty());
    }

    #[test]
    fn reserved_apply_and_disarm_are_real_clickable_controls_at_both_sizes() {
        for size in [egui::vec2(320.0, 360.0), egui::vec2(680.0, 620.0)] {
            let ctx = egui::Context::default();
            ouroboros_ui::Theme::install(&ctx, ouroboros_ui::Mode::Dark);
            let mut page = MapperPage {
                draft: graph(),
                error: Some("A retained explanation ".repeat(100)),
                ..Default::default()
            };
            page.select_node(Some("output"));
            page.view = View::Inspector;
            assert!(page.validation().is_none());
            let _ = frame(&ctx, size, vec![], &mut page, false);
            let screen = Rect::from_min_size(egui::Pos2::ZERO, size);
            let apply = rectangle(&ctx, "Apply input mapper", screen);
            rectangle(&ctx, "Mapper Activity", screen);
            assert!(page.snapshot().controls["Apply input mapper"].whole_visible);
            let _ = frame(&ctx, size, click(apply.center(), true), &mut page, false);
            let command = frame(&ctx, size, click(apply.center(), false), &mut page, false);
            assert!(matches!(
                command,
                Some(PageAction::Apply {
                    expected_revision: 0,
                    ..
                })
            ));
            assert!(page.pending.is_some());
            assert!(!page.armed);
            page.pending = None;
            page.applied = page.draft.clone();
            page.armed = true;
            page.cleanup_pending = true;
            page.view = View::Activity;
            let _ = frame(&ctx, size, vec![], &mut page, true);
            let disarm = rectangle(&ctx, "Disarm input mapper", screen);
            let _ = frame(&ctx, size, click(disarm.center(), true), &mut page, true);
            assert!(matches!(
                frame(&ctx, size, click(disarm.center(), false), &mut page, true),
                Some(PageAction::Disarm {
                    expected_revision: 0,
                    ..
                })
            ));
        }
    }

    fn click_mapper_control(ctx: &egui::Context, size: Vec2, page: &mut MapperPage, label: &str) {
        assert!(frame(ctx, size, vec![], page, false).is_none());
        let rect = scroll_to_mapper_control(ctx, size, page, label);
        assert!(frame(ctx, size, click(rect.center(), true), page, false).is_none());
        assert!(frame(ctx, size, click(rect.center(), false), page, false).is_none());
        assert!(frame(ctx, size, vec![], page, false).is_none());
    }

    fn open_node_palette(ctx: &egui::Context, size: Vec2, page: &mut MapperPage) {
        assert!(frame(ctx, size, vec![], page, false).is_none());
        if page.view == View::Graph && size.x < 780.0 {
            click_mapper_control(ctx, size, page, "Browse mapper nodes");
            assert_eq!(page.view, View::Nodes);
        }
        assert!(page.snapshot().palette.is_some());
    }

    fn type_node_search(ctx: &egui::Context, size: Vec2, page: &mut MapperPage, text: &str) {
        click_mapper_control(ctx, size, page, "Mapper node search");
        assert!(frame(ctx, size, vec![egui::Event::Text(text.into())], page, false).is_none());
        // Release text focus before choosing catalogue entries or using Undo.
        assert!(
            frame(
                ctx,
                size,
                vec![egui::Event::Key {
                    key: egui::Key::Escape,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                }],
                page,
                false
            )
            .is_none()
        );
    }

    #[test]
    fn image_template_copy_stages_an_undoable_inspector_edit_at_both_sizes() {
        use base64::Engine as _;
        let mut png = std::io::Cursor::new(Vec::new());
        image::write_buffer_with_format(
            &mut png,
            &[11, 37, 73, 255, 83, 9, 5, 255],
            2,
            1,
            image::ColorType::Rgba8,
            image::ImageFormat::Png,
        )
        .unwrap();
        let template = base64::engine::general_purpose::STANDARD.encode(png.into_inner());
        assert!(png_template_summary(&template).unwrap().contains("2 × 1"));
        for size in [egui::vec2(320.0, 360.0), egui::vec2(680.0, 620.0)] {
            let ctx = egui::Context::default();
            ouroboros_ui::Theme::install(&ctx, ouroboros_ui::Mode::Dark);
            let mut page = MapperPage::default();
            open_node_palette(&ctx, size, &mut page);
            page.catalogue_index = page
                .catalogue
                .iter()
                .position(|entry| entry.name == "image_match")
                .unwrap();
            click_mapper_control(&ctx, size, &mut page, "Add mapper node");
            assert_eq!(page.view, View::Inspector);
            assert!(page.can_stage_screen_template());
            let original = page.draft.clone();
            let undo = page.undo.len();
            page.stage_screen_template(&template).unwrap();
            assert_eq!(page.draft, original);
            assert_eq!(page.undo.len(), undo);
            assert!(page.parameter_dirty());
            let _ = frame(&ctx, size, vec![], &mut page, false);
            assert!(page.parameter_error.is_none(), "{:?}", page.parameter_error);
            scroll_to_mapper_control(&ctx, size, &mut page, "Mapper parameter capture_user");
            click_mapper_control(&ctx, size, &mut page, "Update mapper node");
            assert_eq!(page.undo.len(), undo + 1);
            let NodeKind::ImageMatch {
                template_png_base64,
                ..
            } = &page.draft.nodes[0].kind
            else {
                panic!("image node")
            };
            assert_eq!(template_png_base64, &template);
            assert!(!page.armed && page.pending.is_none());
            page.history_step(false);
            assert_eq!(page.draft, original);
        }
    }
    #[test]
    fn retiring_capture_disables_apply_and_arm_until_the_worker_drains() {
        let ctx = egui::Context::default();
        ouroboros_ui::Theme::install(&ctx, ouroboros_ui::Mode::Dark);
        let size = egui::vec2(680.0, 620.0);
        let definition = mapper::instantiate_template(
            TemplateKind::KeyRemap,
            "virtual-user",
            Some("virtual-user"),
        )
        .unwrap();
        let mut page = MapperPage {
            draft: definition.clone(),
            applied: definition,
            ..Default::default()
        };
        let _ = frame(&ctx, size, vec![], &mut page, false);
        assert!(page.controls["Arm input mapper"].enabled);
        page.capture_worker_busy = true;
        let _ = frame(&ctx, size, vec![], &mut page, false);
        assert!(!page.controls["Arm input mapper"].enabled);
        page.draft.nodes[0].position = Some([123.0, 45.0]);
        let _ = frame(&ctx, size, vec![], &mut page, false);
        assert!(!page.controls["Apply input mapper"].enabled);
        page.capture_worker_busy = false;
        let _ = frame(&ctx, size, vec![], &mut page, false);
        assert!(page.controls["Apply input mapper"].enabled);
    }

    #[test]
    fn searchable_palette_selects_then_adds_one_undoable_draft_at_all_sizes() {
        for size in [
            egui::vec2(320.0, 360.0),
            egui::vec2(680.0, 620.0),
            egui::vec2(1000.0, 720.0),
        ] {
            let ctx = egui::Context::default();
            ouroboros_ui::Theme::install(&ctx, ouroboros_ui::Mode::Dark);
            let mut page = MapperPage::default();
            assert!(page.canvas);
            open_node_palette(&ctx, size, &mut page);
            let screen = Rect::from_min_size(egui::Pos2::ZERO, size);
            for label in [
                "Mapper node search",
                "Mapper node category",
                "Add mapper node",
            ] {
                rectangle(&ctx, label, screen);
            }
            assert!(!page.controls["Add mapper node"].enabled);
            type_node_search(&ctx, size, &mut page, "normalize");
            let palette = page.snapshot().palette.unwrap();
            assert_eq!(palette.matches, ["normalize_vector"]);
            assert!(palette.selected.is_none());
            click_mapper_control(&ctx, size, &mut page, "Choose mapper node normalize_vector");
            assert_eq!(
                page.snapshot().palette.unwrap().selected.as_deref(),
                Some("normalize_vector")
            );
            assert!(
                page.draft.nodes.is_empty()
                    && page.undo.is_empty()
                    && page.pending.is_none()
                    && !page.armed
            );
            click_mapper_control(&ctx, size, &mut page, "Add mapper node");
            assert_eq!(page.view, View::Inspector);
            assert_eq!(page.draft.nodes.len(), 1);
            assert!(matches!(
                page.draft.nodes[0].kind,
                NodeKind::NormalizeVector {
                    signal: mapper::VectorType::Delta
                }
            ));
            assert_eq!(page.undo.len(), 1);
            assert_eq!(page.applied, Definition::default());
            assert!(page.pending.is_none() && !page.armed);
            click_mapper_control(&ctx, size, &mut page, "Mapper Graph");
            click_mapper_control(&ctx, size, &mut page, "Undo mapper edit");
            assert!(page.draft.nodes.is_empty());
            assert!(page.pending.is_none() && !page.armed);
        }
    }

    #[test]
    fn palette_filters_clear_stale_choices_and_keep_add_reserved_with_no_results() {
        for size in [
            egui::vec2(320.0, 360.0),
            egui::vec2(680.0, 620.0),
            egui::vec2(1000.0, 720.0),
        ] {
            let ctx = egui::Context::default();
            ouroboros_ui::Theme::install(&ctx, ouroboros_ui::Mode::Dark);
            let mut page = MapperPage::default();
            open_node_palette(&ctx, size, &mut page);
            click_mapper_control(&ctx, size, &mut page, "Choose mapper node pointer");
            type_node_search(&ctx, size, &mut page, "nothing-matches-this-node");
            let palette = page.snapshot().palette.unwrap();
            assert!(palette.matches.is_empty() && palette.selected.is_none());
            let screen = Rect::from_min_size(egui::Pos2::ZERO, size);
            rectangle(&ctx, "Add mapper node", screen);
            assert!(!page.controls["Add mapper node"].enabled);
            click_mapper_control(&ctx, size, &mut page, "Add mapper node");
            assert!(page.draft.nodes.is_empty());
            click_mapper_control(&ctx, size, &mut page, "Clear mapper node filters");
            assert!(page.node_search.is_empty() && page.node_category.is_none());
            assert!(page.snapshot().palette.unwrap().selected.is_none());
            type_node_search(&ctx, size, &mut page, "pointer");
            click_mapper_control(&ctx, size, &mut page, "Choose mapper node pointer");
            click_mapper_control(&ctx, size, &mut page, "Add mapper node");
            assert!(
                matches!(&page.draft.nodes[0].kind, NodeKind::Pointer { user, .. } if user == "virtual-user")
            );
            assert_eq!(page.undo.len(), 1);
            assert!(page.pending.is_none() && !page.armed);
        }
    }

    #[test]
    fn dedicated_palette_uses_the_compact_app_chrome_budget_for_results_and_add() {
        // Reserve another 60px beyond the helper's Back to Home row. This
        // models the native 320x360 app's title, navigation divider and footer.
        let size = egui::vec2(320.0, 300.0);
        let screen = Rect::from_min_size(egui::Pos2::ZERO, size);
        let ctx = egui::Context::default();
        ouroboros_ui::Theme::install(&ctx, ouroboros_ui::Mode::Dark);
        let mut page = MapperPage::default();
        open_node_palette(&ctx, size, &mut page);
        assert_eq!(page.view, View::Nodes);
        assert!(!page.controls.contains_key("Mapper Graph"));
        assert!(!page.controls.contains_key("Apply input mapper"));
        assert!(!page.controls.contains_key("Arm input mapper"));
        for label in [
            "Back to mapper graph",
            "Mapper node search",
            "Mapper node category",
            "Add mapper node",
        ] {
            rectangle(&ctx, label, screen);
        }
        type_node_search(&ctx, size, &mut page, "no-such-node-8364");
        assert!(frame(&ctx, size, vec![], &mut page, false).is_none());
        rectangle(&ctx, "Mapper node empty results", screen);
        rectangle(&ctx, "Add mapper node", screen);
        assert!(!page.controls["Add mapper node"].enabled);
        click_mapper_control(&ctx, size, &mut page, "Clear mapper node filters");
        type_node_search(&ctx, size, &mut page, "pointer");
        let choice = scroll_to_mapper_control(&ctx, size, &mut page, "Choose mapper node pointer");
        assert!(screen.contains_rect(choice));
        click_mapper_control(&ctx, size, &mut page, "Choose mapper node pointer");
        assert!(page.draft.nodes.is_empty() && page.undo.is_empty());
        rectangle(&ctx, "Add mapper node", screen);
        click_mapper_control(&ctx, size, &mut page, "Add mapper node");
        assert_eq!(page.view, View::Inspector);
        assert_eq!(page.draft.nodes.len(), 1);
        assert_eq!(page.undo.len(), 1);
        assert!(page.pending.is_none() && !page.armed);
        page.view = View::Nodes;
        page.armed = true;
        assert!(frame(&ctx, size, vec![], &mut page, false).is_none());
        rectangle(&ctx, "Disarm input mapper", screen);
        rectangle(&ctx, "Back to mapper graph", screen);
        assert!(!page.controls.contains_key("Mapper Graph"));
        assert!(!page.controls.contains_key("Arm input mapper"));
        assert!(page.armed && page.pending.is_none());
    }

    #[test]
    fn palette_metadata_stages_only_visible_valid_filters_and_add_obeys_edit_gates() {
        let size = egui::vec2(680.0, 620.0);
        let ctx = egui::Context::default();
        ouroboros_ui::Theme::install(&ctx, ouroboros_ui::Mode::Dark);
        let mut page = MapperPage::default();
        let filters = BTreeMap::from([
            ("mapper_node_search".into(), "normalize".into()),
            ("mapper_node_category".into(), "vector".into()),
        ]);
        page.apply_script_fields(&filters);
        assert!(page.node_search.is_empty() && page.node_category.is_none());
        open_node_palette(&ctx, size, &mut page);
        page.apply_script_fields(&filters);
        click_mapper_control(&ctx, size, &mut page, "Search mapper nodes");
        let palette = page.snapshot().palette.unwrap();
        assert_eq!(palette.category, Some(NodeCategory::Vector));
        assert_eq!(palette.matches, ["normalize_vector"]);
        for query in [
            "é".repeat(65),
            "bad\nquery".into(),
            "bad\u{0085}query".into(),
        ] {
            page.apply_script_fields(&BTreeMap::from([("mapper_node_search".into(), query)]));
            assert_eq!(page.node_search, "normalize");
            assert!(page.node_filter_error.is_some());
        }
        page.apply_script_fields(&BTreeMap::from([(
            "mapper_node_category".into(),
            "unknown".into(),
        )]));
        assert_eq!(page.node_category, Some(NodeCategory::Vector));
        click_mapper_control(&ctx, size, &mut page, "Search mapper nodes");
        click_mapper_control(&ctx, size, &mut page, "Choose mapper node normalize_vector");
        let before = page.draft.clone();
        assert!(frame(&ctx, size, vec![], &mut page, true).is_none());
        assert!(!page.controls["Add mapper node"].enabled);
        page.pending = Some(Pending::Arm {
            command_id: 1,
            expected_revision: 0,
        });
        assert!(frame(&ctx, size, vec![], &mut page, false).is_none());
        assert!(!page.controls["Add mapper node"].enabled);
        page.pending = None;
        page.cleanup_pending = true;
        assert!(frame(&ctx, size, vec![], &mut page, false).is_none());
        assert!(!page.controls["Add mapper node"].enabled);
        page.cleanup_pending = false;
        page.draft = graph();
        page.select_node(Some("output"));
        page.parameter_value["channel"] = serde_json::json!("changed");
        assert!(frame(&ctx, size, vec![], &mut page, false).is_none());
        assert!(!page.controls["Add mapper node"].enabled);
        assert!(page.undo.is_empty() && page.pending.is_none() && !page.armed);
        page.parameter_value = serde_json::to_value(&page.draft.nodes[1].kind).unwrap();
        page.draft = before;
        click_mapper_control(&ctx, size, &mut page, "Back to mapper graph");
        assert_eq!(page.view, View::Graph);
        assert!(page.pending.is_none() && !page.armed && page.draft.nodes.is_empty());
    }

    #[test]
    fn accepted_worker_recovery_clears_only_worker_diagnostics() {
        let mut page = MapperPage::default();
        let fault = mapper_runtime::State {
            sequence: 10,
            error: Some("Mapper delay stopped: delay recipient changed".into()),
            ..Default::default()
        };
        page.receive_external(&fault);
        assert_eq!(page.snapshot().error, fault.error);
        assert_eq!(page.worker_error_text(), fault.error.as_deref());
        let healthy = mapper_runtime::State {
            sequence: 11,
            run: 2,
            armed: true,
            ..Default::default()
        };
        page.receive_external(&healthy);
        assert!(page.snapshot().error.is_none() && page.worker_error_text().is_none());
        page.receive_external(&fault);
        assert!(
            page.snapshot().error.is_none(),
            "stale fault cannot replace recovered authority"
        );

        let mut page = MapperPage::default();
        page.receive_external(&fault);
        // Identical text still belongs to a local command when explicitly set locally.
        page.error = fault.error.clone();
        page.receive_external(&healthy);
        assert_eq!(page.snapshot().error, fault.error);
        assert!(page.worker_error_text().is_none());
        page.error = Some("A local command was not acknowledged; inspect before retrying.".into());
        page.receive_external(&mapper_runtime::State {
            sequence: 12,
            ..healthy
        });
        assert!(page.snapshot().error.unwrap().contains("not acknowledged"));
    }

    #[test]
    fn timing_nodes_use_real_ouroboros_editors_and_publish_queue_counts() {
        for size in [egui::vec2(320.0, 360.0), egui::vec2(680.0, 620.0)] {
            for (name, duration) in [("delay", "delay_ms"), ("throttle", "interval_ms")] {
                let ctx = egui::Context::default();
                ouroboros_ui::Theme::install(&ctx, ouroboros_ui::Mode::Dark);
                let mut page = MapperPage::default();
                let _ = frame(&ctx, size, vec![], &mut page, false);
                open_node_palette(&ctx, size, &mut page);
                type_node_search(&ctx, size, &mut page, name);
                click_mapper_control(&ctx, size, &mut page, &format!("Choose mapper node {name}"));
                assert!(page.draft.nodes.is_empty() && page.undo.is_empty());
                click_mapper_control(&ctx, size, &mut page, "Add mapper node");
                assert_eq!(page.view, View::Inspector);
                let _ = frame(&ctx, size, vec![], &mut page, false);
                scroll_to_mapper_control(&ctx, size, &mut page, "Mapper parameter signal");
                scroll_to_mapper_control(
                    &ctx,
                    size,
                    &mut page,
                    &format!("Mapper parameter {duration}"),
                );
                assert_eq!(page.draft.nodes[0].kind.category(), NodeCategory::Routing);
                assert_eq!(
                    page.draft.nodes[0].kind.ports().inputs[0].signal,
                    page.draft.nodes[0].kind.ports().outputs[0].signal
                );
                page.apply_script_fields(&BTreeMap::from([(
                    format!("mapper_parameter.{duration}"),
                    "1234".into(),
                )]));
                let _ = frame(&ctx, size, vec![], &mut page, false);
                click_mapper_control(&ctx, size, &mut page, "Update mapper node");
                assert_eq!(
                    serde_json::to_value(&page.draft.nodes[0].kind).unwrap()[duration],
                    1234
                );
                assert_eq!(page.undo.len(), 2);
                assert!(
                    page.pending.is_none() && !page.armed && page.applied == Definition::default()
                );
            }
        }
        let mut page = MapperPage::default();
        page.receive_external(&mapper_runtime::State {
            sequence: 1,
            pending_delays: 7,
            delayed_events: 9,
            ..Default::default()
        });
        assert_eq!(page.snapshot().pending_delays, 7);
        assert_eq!(page.snapshot().delayed_events, 9);
    }

    #[test]
    fn text_predicate_literals_stage_update_and_undo_without_applying_or_arming() {
        let size = egui::vec2(680.0, 620.0);
        for (name, field) in [("text_contains", "needle"), ("text_compare", "expected")] {
            let ctx = egui::Context::default();
            ouroboros_ui::Theme::install(&ctx, ouroboros_ui::Mode::Dark);
            let mut page = MapperPage::default();
            let _ = frame(&ctx, size, vec![], &mut page, false);
            open_node_palette(&ctx, size, &mut page);
            type_node_search(&ctx, size, &mut page, name);
            click_mapper_control(&ctx, size, &mut page, &format!("Choose mapper node {name}"));
            click_mapper_control(&ctx, size, &mut page, "Add mapper node");
            let original = page.draft.clone();
            page.apply_script_fields(&BTreeMap::from([(
                format!("mapper_parameter.{field}"),
                "Ready 7319".into(),
            )]));
            assert_eq!(page.draft, original);
            let _ = frame(&ctx, size, vec![], &mut page, false);
            scroll_to_mapper_control(&ctx, size, &mut page, &format!("Mapper parameter {field}"));
            click_mapper_control(&ctx, size, &mut page, "Update mapper node");
            assert_eq!(
                serde_json::to_value(&page.draft.nodes[0].kind).unwrap()[field],
                "Ready 7319"
            );
            assert_eq!(page.undo.len(), 2);
            page.history_step(false);
            assert_eq!(page.draft, original);
            assert!(!page.armed && page.pending.is_none() && page.applied == Definition::default());
        }
    }
    #[test]
    fn scalar_and_vector_math_catalogue_nodes_expose_their_ouroboros_parameters() {
        for size in [egui::vec2(320.0, 360.0), egui::vec2(680.0, 620.0)] {
            for name in ["unary_math", "vector_length", "normalize_vector"] {
                let ctx = egui::Context::default();
                ouroboros_ui::Theme::install(&ctx, ouroboros_ui::Mode::Dark);
                let mut page = MapperPage::default();
                open_node_palette(&ctx, size, &mut page);
                page.catalogue_index = page
                    .catalogue
                    .iter()
                    .position(|entry| entry.name == name)
                    .unwrap();
                let expected = page.catalogue[page.catalogue_index].prototype.clone();
                assert!(frame(&ctx, size, vec![], &mut page, false).is_none());
                let add = scroll_to_mapper_control(&ctx, size, &mut page, "Add mapper node");
                assert!(frame(&ctx, size, click(add.center(), true), &mut page, false).is_none());
                assert!(frame(&ctx, size, click(add.center(), false), &mut page, false).is_none());
                assert_eq!(page.view, View::Inspector);
                assert!(frame(&ctx, size, vec![], &mut page, false).is_none());
                assert_eq!(page.draft.nodes[0].kind, expected);
                scroll_to_mapper_control(&ctx, size, &mut page, "Mapper parameter signal");
                if name == "unary_math" {
                    scroll_to_mapper_control(&ctx, size, &mut page, "Mapper parameter operation");
                    assert!(matches!(
                        page.draft.nodes[0].kind,
                        NodeKind::UnaryMath {
                            operation: mapper::UnaryOperation::Abs,
                            signal: mapper::ScalarType::State,
                        }
                    ));
                }
                assert_eq!(page.draft.nodes[0].kind.ports().inputs.len(), 1);
                assert_eq!(page.draft.nodes[0].kind.ports().outputs.len(), 1);
                assert_eq!(page.undo.len(), 1);
                assert_eq!(page.applied, Definition::default());
                assert!(page.pending.is_none() && !page.armed);
            }
        }
    }

    #[test]
    fn flow_controls_use_catalogue_prototypes_and_actual_ouroboros_editors() {
        let size = egui::vec2(680.0, 620.0);
        let screen = Rect::from_min_size(egui::Pos2::ZERO, size);
        let ctx = egui::Context::default();
        ouroboros_ui::Theme::install(&ctx, ouroboros_ui::Mode::Dark);
        let mut page = MapperPage::default();
        for name in ["counter", "switch", "timer"] {
            page.view = View::Graph;
            open_node_palette(&ctx, size, &mut page);
            page.catalogue_index = page
                .catalogue
                .iter()
                .position(|item| item.name == name)
                .unwrap();
            let _ = frame(&ctx, size, vec![], &mut page, false);
            rectangle(&ctx, "Mapper node catalogue", screen);
            let add = rectangle(&ctx, "Add mapper node", screen);
            let _ = frame(&ctx, size, click(add.center(), true), &mut page, false);
            let _ = frame(&ctx, size, click(add.center(), false), &mut page, false);
            assert_eq!(kind_name(&page.draft.nodes.last().unwrap().kind), name);
            let _ = frame(&ctx, size, vec![], &mut page, false);
            if name == "counter" {
                rectangle(&ctx, "Mapper parameter initial", screen);
                let reset = rectangle(&ctx, "Mapper parameter with_reset", screen);
                let _ = frame(&ctx, size, click(reset.center(), true), &mut page, false);
                let _ = frame(&ctx, size, click(reset.center(), false), &mut page, false);
                let update = rectangle(&ctx, "Update mapper node", screen);
                let _ = frame(&ctx, size, click(update.center(), true), &mut page, false);
                let _ = frame(&ctx, size, click(update.center(), false), &mut page, false);
                assert!(matches!(
                    page.draft.nodes[0].kind,
                    NodeKind::Counter {
                        initial: 0,
                        with_reset: true
                    }
                ));
                assert!(
                    page.draft.nodes[0]
                        .kind
                        .ports()
                        .inputs
                        .iter()
                        .any(|port| port.name == "reset")
                );
            } else if name == "switch" {
                rectangle(&ctx, "Mapper parameter signal", screen);
                assert_eq!(page.draft.nodes[1].kind.ports().inputs.len(), 3);
            } else {
                rectangle(&ctx, "Mapper parameter interval_ms", screen);
                rectangle(&ctx, "Mapper parameter count", screen);
                assert_eq!(page.draft.nodes[2].kind.ports().inputs[0].name, "enabled");
            }
        }
    }

    #[test]
    fn blocked_flow_save_keeps_maintenance_excluded_after_gui_owner_drops() {
        use crate::config_maintenance;
        use std::sync::{
            Arc,
            atomic::{AtomicU64, Ordering},
        };

        static NEXT: AtomicU64 = AtomicU64::new(1);
        let temporary = std::env::temp_dir();
        let root = temporary.join(format!(
            "protoinputmultiplex-flow-worker-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
        ));
        let data = root.join("data");
        let backup = root.join("backup");
        std::fs::create_dir_all(&data).unwrap();
        let guards = crate::gui::RuntimeGuards::for_test(&data).unwrap();
        let retained = Arc::downgrade(&guards);
        let store = Store::new(&data);
        let worker_store = store.clone();
        let (started, operation_started) = mpsc::sync_channel(1);
        let (release, resume) = mpsc::sync_channel(1);
        let service = FlowService::with_operation(Arc::clone(&guards), move |job| {
            let FlowJob::Save(flow) = job else {
                anyhow::bail!("unexpected fixture operation");
            };
            started.send(())?;
            resume.recv()?;
            worker_store.save(&flow)?;
            Ok(FlowResult::Saved)
        });
        let mut page = MapperPage {
            flow_store: Some(service),
            ..MapperPage::default()
        };
        let expected = Flow::new("Pending flow".into(), graph());
        assert!(
            page.flow_store
                .as_mut()
                .unwrap()
                .start(FlowJob::Save(expected.clone()), &egui::Context::default(),)
        );
        operation_started
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
        drop(guards);

        let (dropped, page_dropped) = mpsc::sync_channel(1);
        let dropper = std::thread::spawn(move || {
            drop(page);
            let _ = dropped.send(());
        });
        let closed = page_dropped.recv_timeout(Duration::from_secs(2));
        if closed.is_err() {
            let _ = release.send(());
        }
        assert!(closed.is_ok(), "GUI teardown waited for blocked storage");
        dropper.join().unwrap();

        assert!(
            retained.upgrade().is_some(),
            "blocked storage lost its runtime guards"
        );
        assert!(
            config_maintenance::run(config_maintenance::Command::Backup {
                data_dir: data.clone(),
                backup_dir: backup.clone(),
            })
            .is_err(),
            "maintenance acquired the blocked storage root",
        );
        assert!(!backup.exists());

        release.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while retained.upgrade().is_some() {
            assert!(
                Instant::now() < deadline,
                "finished storage worker retained runtime guards"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(store.load(&expected.name).unwrap(), expected);
        config_maintenance::run(config_maintenance::Command::Backup {
            data_dir: data,
            backup_dir: backup.clone(),
        })
        .unwrap();
        assert!(backup.is_dir());

        assert!(root.is_absolute() && root.starts_with(&temporary));
        std::fs::remove_dir_all(root).unwrap();
    }

    fn scroll_to_mapper_control(
        ctx: &egui::Context,
        size: Vec2,
        page: &mut MapperPage,
        label: &str,
    ) -> Rect {
        let screen = Rect::from_min_size(egui::Pos2::ZERO, size);
        for _ in 0..40 {
            if page
                .controls
                .get(label)
                .is_some_and(|control| control.whole_visible)
            {
                return rectangle(ctx, label, screen);
            }
            let target = ctx
                .data(|data| {
                    data.get_temp::<(Rect, Rect)>(egui::Id::new(("mapper-response", label)))
                })
                .unwrap()
                .0;
            let clip = ctx
                .data(|data| data.get_temp::<Rect>(egui::Id::new(("mapper-clip", label))))
                .expect("rendered mapper control clip");
            let delta = if target.center().y < clip.center().y {
                40.0
            } else {
                -40.0
            };
            assert!(
                frame(
                    ctx,
                    size,
                    vec![
                        egui::Event::PointerMoved(clip.center()),
                        egui::Event::MouseWheel {
                            unit: egui::MouseWheelUnit::Point,
                            phase: egui::TouchPhase::Move,
                            delta: egui::vec2(0.0, delta),
                            modifiers: egui::Modifiers::default(),
                        },
                    ],
                    page,
                    false
                )
                .is_none()
            );
            // Let egui's app-local smooth scrolling settle before qualifying the control.
            for _ in 0..4 {
                assert!(frame(ctx, size, vec![], page, false).is_none());
            }
        }
        panic!("{label} did not become wholly visible after scrolling the mapper body");
    }

    #[test]
    fn browser_cards_choose_only_and_remain_reachable_at_both_sizes() {
        for size in [egui::vec2(320.0, 360.0), egui::vec2(680.0, 620.0)] {
            let screen = Rect::from_min_size(egui::Pos2::ZERO, size);
            let ctx = egui::Context::default();
            ouroboros_ui::Theme::install(&ctx, ouroboros_ui::Mode::Dark);
            let mut page = MapperPage {
                view: View::Templates,
                ..MapperPage::default()
            };
            let original = page.snapshot();
            let _ = frame(&ctx, size, vec![], &mut page, false);
            let entry = rectangle(&ctx, "Browse mapper flows", screen);
            let _ = frame(&ctx, size, click(entry.center(), true), &mut page, false);
            assert!(frame(&ctx, size, click(entry.center(), false), &mut page, false).is_none());
            assert_eq!(page.view, View::Browser);
            let _ = frame(&ctx, size, vec![], &mut page, false);
            assert!(!page.controls.contains_key("Apply input mapper"));
            assert!(!page.controls.contains_key("Arm input mapper"));
            for name in [
                "Mapper Graph",
                "Mapper Activity",
                "Mapper Templates",
                "Close mapper flow browser",
                "Browse mapper templates",
                "Browse saved mapper flows",
                "Search mapper flows",
                "Mapper flow search",
            ] {
                rectangle(&ctx, name, screen);
            }
            let (chosen_index, chosen_template) = mapper::templates()
                .iter()
                .enumerate()
                .find(|(_, template)| template.template == TemplateKind::PointerTrail)
                .unwrap();
            assert_ne!(chosen_index, page.template_index);
            page.apply_script_fields(&BTreeMap::from([(
                "mapper_flow_search".into(),
                chosen_template.label.into(),
            )]));
            assert!(page.flow_browser.query.is_empty());
            let search = rectangle(&ctx, "Search mapper flows", screen);
            let _ = frame(&ctx, size, click(search.center(), true), &mut page, false);
            assert!(frame(&ctx, size, click(search.center(), false), &mut page, false).is_none());
            let _ = frame(&ctx, size, vec![], &mut page, false);
            assert_eq!(page.flow_browser.query, chosen_template.label);
            assert_eq!(page.flow_browser.total, 1);
            let choice = scroll_to_mapper_control(
                &ctx,
                size,
                &mut page,
                &format!("Choose mapper template {}", chosen_template.label),
            );
            let _ = frame(&ctx, size, click(choice.center(), true), &mut page, false);
            assert!(frame(&ctx, size, click(choice.center(), false), &mut page, false).is_none());
            assert_eq!(page.view, View::Templates);
            assert_eq!(page.template_index, chosen_index);
            assert_eq!(page.draft, original.draft);
            assert_eq!(page.applied, original.applied);
            assert!(page.pending.is_none() && !page.armed && page.undo.is_empty());

            page.view = View::SavedFlows;
            page.flow_initialized = true;
            page.flow_browser.search.clear();
            page.flow_browser.query.clear();
            page.open_flow_browser(BrowserSource::Saved);
            let generation = page.flow_browser.generation;
            page.flow_browser.requested = true;
            page.receive_browser_page(
                generation,
                BrowsePage {
                    query: String::new(),
                    offset: 0,
                    limit: MAX_BROWSE_ENTRIES,
                    total: 1,
                    entries: vec![mapper_flow_store::BrowseEntry {
                        name: "Original saved flow".into(),
                        preview: Some(mapper_flow_store::preview(&graph()).unwrap()),
                        error: None,
                    }],
                },
            );
            let _ = frame(&ctx, size, vec![], &mut page, false);
            for name in [
                "Close mapper flow browser",
                "Mapper flow search",
                "Search mapper flows",
            ] {
                rectangle(&ctx, name, screen);
            }
            let choice = scroll_to_mapper_control(
                &ctx,
                size,
                &mut page,
                "Choose saved mapper flow Original saved flow",
            );
            let _ = frame(&ctx, size, click(choice.center(), true), &mut page, false);
            assert!(frame(&ctx, size, click(choice.center(), false), &mut page, false).is_none());
            assert_eq!(page.view, View::SavedFlows);
            assert_eq!(page.flow_selected.as_deref(), Some("Original saved flow"));
            assert_eq!(page.draft, original.draft);
            assert_eq!(page.applied, original.applied);
            assert!(page.pending.is_none() && !page.armed && page.undo.is_empty());
        }
    }

    #[test]
    fn browser_discards_pending_pages_after_query_source_offset_and_close_changes() {
        let mut page = MapperPage {
            view: View::SavedFlows,
            ..MapperPage::default()
        };
        page.open_flow_browser(BrowserSource::Saved);
        let stale = page.flow_browser.generation;
        page.flow_browser.query = "updated".into();
        page.flow_browser.offset = MAX_BROWSE_ENTRIES;
        page.flow_browser.invalidate();
        let make_page = |query: &str, offset| BrowsePage {
            query: query.into(),
            offset,
            limit: MAX_BROWSE_ENTRIES,
            total: 1,
            entries: vec![mapper_flow_store::BrowseEntry {
                name: "Stale page".into(),
                preview: Some(mapper_flow_store::preview(&graph()).unwrap()),
                error: None,
            }],
        };
        page.receive_browser_page(stale, make_page("", 0));
        assert!(page.flow_browser.cards.is_empty() && page.flow_browser.pending);
        let current = page.flow_browser.generation;
        page.receive_browser_page(current, make_page("updated", 0));
        assert!(page.flow_browser.cards.is_empty());
        page.flow_browser.source = BrowserSource::Templates;
        page.flow_browser.invalidate();
        page.receive_browser_page(current, make_page("updated", MAX_BROWSE_ENTRIES));
        assert!(page.flow_browser.cards.is_empty());
        page.flow_browser.source = BrowserSource::Saved;
        page.flow_browser.invalidate();
        let closing = page.flow_browser.generation;
        page.close_flow_browser();
        page.receive_browser_page(closing, make_page("updated", MAX_BROWSE_ENTRIES));
        assert!(page.flow_browser.cards.is_empty());
        assert_eq!(page.view, View::SavedFlows);
        assert!(page.snapshot().browser.is_none());
        assert_eq!(page.draft, Definition::default());
    }

    #[test]
    fn initial_saved_flow_refresh_retries_after_a_stale_browser_request_finishes() {
        let size = egui::vec2(680.0, 620.0);
        let ctx = egui::Context::default();
        ouroboros_ui::Theme::install(&ctx, ouroboros_ui::Mode::Dark);
        let (sender, jobs) = mpsc::sync_channel(1);
        let (replies, receiver) = mpsc::channel();
        let mut page = MapperPage {
            view: View::SavedFlows,
            flow_store: Some(FlowService {
                sender,
                receiver,
                pending: true,
                dead: false,
                reported_dead: false,
            }),
            ..MapperPage::default()
        };
        assert!(frame(&ctx, size, vec![], &mut page, false).is_none());
        assert!(!page.flow_initialized);
        assert!(matches!(jobs.try_recv(), Err(TryRecvError::Empty)));

        assert!(
            replies
                .send(FlowResult::Browsed {
                    request: 0,
                    page: BrowsePage {
                        query: String::new(),
                        offset: 0,
                        limit: MAX_BROWSE_ENTRIES,
                        total: 0,
                        entries: Vec::new(),
                    },
                })
                .is_ok()
        );
        assert!(frame(&ctx, size, vec![], &mut page, false).is_none());
        assert!(page.flow_initialized);
        assert!(matches!(jobs.try_recv().unwrap().0, FlowJob::Refresh));
        assert_eq!(page.view, View::SavedFlows);

        assert!(
            replies
                .send(FlowResult::Names(vec!["Saved graph".into()]))
                .is_ok()
        );
        assert!(frame(&ctx, size, vec![], &mut page, false).is_none());
        assert_eq!(page.flow_names, ["Saved graph"]);
        assert_eq!(page.flow_selected.as_deref(), Some("Saved graph"));
        assert_eq!(page.draft, Definition::default());
        assert_eq!(page.applied, Definition::default());
    }

    #[test]
    fn saved_flow_read_cannot_replace_a_draft_after_leaving_mapper() {
        let size = egui::vec2(680.0, 620.0);
        let ctx = egui::Context::default();
        ouroboros_ui::Theme::install(&ctx, ouroboros_ui::Mode::Dark);
        let (sender, _jobs) = mpsc::sync_channel(1);
        let (replies, receiver) = mpsc::channel();
        let mut page = MapperPage {
            view: View::SavedFlows,
            flow_store: Some(FlowService {
                sender,
                receiver,
                pending: true,
                dead: false,
                reported_dead: false,
            }),
            flow_initialized: true,
            ..MapperPage::default()
        };
        page.flow_load_baseline = Some((page.draft.clone(), page.applied.clone(), page.revision));

        page.leave_page();
        assert!(
            replies
                .send(FlowResult::Loaded(Flow::new("Saved graph".into(), graph())))
                .is_ok()
        );
        assert!(frame(&ctx, size, vec![], &mut page, false).is_none());

        assert!(page.draft.nodes.is_empty());
        assert!(page.applied.nodes.is_empty());
        assert!(
            page.flow_error
                .as_deref()
                .unwrap()
                .contains("context changed")
        );
        assert_eq!(page.view, View::SavedFlows);
    }

    #[test]
    fn template_controls_create_an_undoable_draft_at_both_sizes() {
        for size in [egui::vec2(320.0, 360.0), egui::vec2(680.0, 620.0)] {
            let screen = Rect::from_min_size(egui::Pos2::ZERO, size);
            let ctx = egui::Context::default();
            ouroboros_ui::Theme::install(&ctx, ouroboros_ui::Mode::Dark);
            let mut page = MapperPage::default();
            assert!(frame(&ctx, size, vec![], &mut page, false).is_none());
            let templates = rectangle(&ctx, "Mapper Templates", screen);
            let _ = frame(
                &ctx,
                size,
                click(templates.center(), true),
                &mut page,
                false,
            );
            assert!(
                frame(
                    &ctx,
                    size,
                    click(templates.center(), false),
                    &mut page,
                    false
                )
                .is_none()
            );
            assert_eq!(page.view, View::Templates);
            assert!(!page.controls.contains_key("Apply input mapper"));
            assert!(!page.controls.contains_key("Arm input mapper"));
            page.armed = true;
            assert!(frame(&ctx, size, vec![], &mut page, false).is_none());
            rectangle(&ctx, "Disarm input mapper", screen);
            assert!(!page.controls.contains_key("Apply input mapper"));
            page.armed = false;
            let _ = frame(&ctx, size, vec![], &mut page, false);
            let label = mapper::templates()
                .iter()
                .find(|item| item.template == TemplateKind::KeyRemap)
                .unwrap()
                .label;
            let option = rectangle(&ctx, &format!("Mapper template {label}"), screen);
            let _ = frame(&ctx, size, click(option.center(), true), &mut page, false);
            let _ = frame(&ctx, size, click(option.center(), false), &mut page, false);
            let _ = frame(&ctx, size, vec![], &mut page, false);
            rectangle(&ctx, "Mapper template source", screen);
            rectangle(&ctx, "Mapper template destination", screen);
            let load = rectangle(&ctx, "Load mapper template", screen);
            let _ = frame(&ctx, size, click(load.center(), true), &mut page, false);
            assert!(frame(&ctx, size, click(load.center(), false), &mut page, false).is_none());
            assert_eq!(page.view, View::Graph);
            let expected = mapper::instantiate_template(
                TemplateKind::KeyRemap,
                "virtual-user",
                Some("virtual-user"),
            )
            .unwrap();
            assert_eq!(page.draft, expected);
            assert_eq!(page.applied, Definition::default());
            assert!(!page.armed && page.pending.is_none());
            let _ = frame(&ctx, size, vec![], &mut page, false);
            let undo = rectangle(&ctx, "Undo mapper edit", screen);
            let _ = frame(&ctx, size, click(undo.center(), true), &mut page, false);
            assert!(frame(&ctx, size, click(undo.center(), false), &mut page, false).is_none());
            assert_eq!(page.draft, Definition::default());

            for name in [
                "Mapper Templates",
                "Mapper template category Monitor",
                "Mapper template Pointer trail",
            ] {
                let _ = frame(&ctx, size, vec![], &mut page, false);
                let control = rectangle(&ctx, name, screen);
                let _ = frame(&ctx, size, click(control.center(), true), &mut page, false);
                assert!(
                    frame(&ctx, size, click(control.center(), false), &mut page, false).is_none()
                );
            }
            let _ = frame(&ctx, size, vec![], &mut page, false);
            rectangle(&ctx, "Mapper template source", screen);
            assert!(!page.controls.contains_key("Mapper template destination"));
            let load = rectangle(&ctx, "Load mapper template", screen);
            let _ = frame(&ctx, size, click(load.center(), true), &mut page, false);
            assert!(frame(&ctx, size, click(load.center(), false), &mut page, false).is_none());
            assert_eq!(page.view, View::Graph);
            let expected =
                mapper::instantiate_template(TemplateKind::PointerTrail, "virtual-user", None)
                    .unwrap();
            assert_eq!(page.draft, expected);
            assert_eq!(page.applied, Definition::default());
            assert!(!page.armed && page.pending.is_none());
        }
    }

    #[test]
    fn monitor_reads_are_throttled_and_bound_to_the_current_run() {
        let mut definition = graph();
        definition.nodes.push(Node {
            id: "monitor".into(),
            kind: NodeKind::Monitor {
                signal: mapper::SignalType::Delta,
            },
            position: None,
        });
        definition.nodes.push(Node {
            id: "scope".into(),
            kind: NodeKind::Scope {
                signal: mapper::SignalType::Delta,
            },
            position: None,
        });
        definition.edges.push(Edge {
            from: definition.edges[0].from.clone(),
            to: PortRef {
                node: "monitor".into(),
                port: "value".into(),
            },
        });
        definition.edges.push(Edge {
            from: definition.edges[0].from.clone(),
            to: PortRef {
                node: "scope".into(),
                port: "value".into(),
            },
        });
        let mut page = MapperPage::default();
        let mut state = mapper_runtime::State {
            definition,
            revision: 2,
            run: 3,
            sequence: 1,
            armed: true,
            ..Default::default()
        };
        let now = Instant::now();
        page.receive_external(&state);
        assert!(page.monitor_request(now).is_none());

        page.view = View::Activity;
        let first = page.monitor_request(now).unwrap();
        let later = now + MONITOR_INTERVAL;
        assert!(page.monitor_request(later).is_none());

        let mut receipt = mapper_runtime::MonitorReceipt {
            request_id: first + 1,
            snapshot: mapper_runtime::MonitorSnapshot {
                revision: state.revision,
                run: state.run,
                samples: vec![mapper::MonitorSample {
                    node: "monitor".into(),
                    value: mapper::Value::Delta([17.25, -4.5]),
                    samples: 1,
                }],
                traces: vec![mapper::ScopeTrace {
                    node: "scope".into(),
                    samples: vec![mapper::ScopeSample {
                        elapsed_us: 125_000,
                        value: mapper::Value::Delta([17.25, -4.5]),
                    }],
                }],
            },
        };
        page.receive_monitors(&receipt);
        assert!(page.monitor_samples.is_empty());
        assert!(page.monitor_request(later).is_none());

        receipt.request_id = first;
        page.receive_monitors(&receipt);
        assert_eq!(page.monitor_samples, receipt.snapshot.samples);
        assert_eq!(page.scope_traces, receipt.snapshot.traces);
        assert!(page.monitor_request(now).is_none());
        assert!(
            !serde_json::to_string(&page.snapshot())
                .unwrap()
                .contains("17.25")
        );

        let next = page.monitor_request(later).unwrap();
        state.sequence += 1;
        state.run += 1;
        page.receive_external(&state);
        assert!(page.monitor_samples.is_empty());
        assert!(page.scope_traces.is_empty());
        assert!(page.monitor_request(later + MONITOR_INTERVAL).is_none());

        receipt.request_id = next;
        page.receive_monitors(&receipt);
        assert!(page.monitor_samples.is_empty());
        let current = page.monitor_request(later + MONITOR_INTERVAL).unwrap();
        receipt.request_id = current;
        receipt.snapshot.run = state.run;
        page.receive_monitors(&receipt);
        assert_eq!(page.monitor_samples, receipt.snapshot.samples);
        assert_eq!(page.scope_traces, receipt.snapshot.traces);

        state.sequence += 1;
        state.armed = false;
        page.receive_external(&state);
        assert!(page.monitor_samples.is_empty());
        assert!(page.monitor_request(later + MONITOR_INTERVAL * 2).is_none());
    }

    #[test]
    fn publication_before_apply_reply_commits_once_and_preserves_later_dirty_work() {
        let mut page = MapperPage::default();
        let definition = graph();
        page.draft = definition.clone();
        page.pending = Some(Pending::Apply {
            command_id: 1,
            definition: definition.clone(),
            expected_revision: 0,
        });
        let state = mapper_runtime::State {
            acknowledged: true,
            reply_to: Some(1),
            sequence: 5,
            definition: definition.clone(),
            revision: 1,
            ..Default::default()
        };
        let mut cancelled = MapperPage {
            draft: definition.clone(),
            ..Default::default()
        };
        cancelled.pending = Some(Pending::Apply {
            command_id: 3,
            definition: definition.clone(),
            expected_revision: 0,
        });
        cancelled.receive_external(&mapper_runtime::State {
            acknowledged: false,
            reply_to: None,
            ..state.clone()
        });
        assert_eq!(cancelled.draft, cancelled.applied);
        cancelled.request_failed(3, "Routing is stopping.");
        cancelled.session_stopped(&Definition::default());
        assert_eq!(cancelled.draft, definition);
        assert_eq!(cancelled.applied, Definition::default());
        assert!(cancelled.has_profile_draft());
        assert!(
            cancelled
                .receive_reply(
                    3,
                    &mapper_runtime::State {
                        reply_to: Some(3),
                        ..state.clone()
                    }
                )
                .is_none()
        );
        page.receive_external(&mapper_runtime::State {
            acknowledged: false,
            reply_to: None,
            ..state.clone()
        });
        assert!(
            matches!(&page.pending, Some(Pending::Apply { command_id: 1, definition: candidate, .. }) if candidate == &definition)
        );
        assert_eq!(page.receive_reply(1, &state), Some(definition.clone()));
        assert_eq!(page.receive_reply(1, &state), None);
        assert!(!page.armed);
        let mut dirty = definition.clone();
        dirty.nodes[0].position = Some([0.3, 0.4]);
        page.replace(dirty.clone());
        page.receive_external(&mapper_runtime::State {
            revision: 2,
            sequence: 6,
            reply_to: None,
            ..state.clone()
        });
        assert_eq!(page.draft, dirty);
        assert!(page.pending.is_none());
        assert!(!page.armed);
        page.pending = Some(Pending::Apply {
            command_id: 2,
            definition: definition.clone(),
            expected_revision: 2,
        });
        assert!(page.receive_reply(1, &state).is_none()); // An old command cannot consume new pending work.
        assert!(page.pending.is_some());
        assert_eq!(page.draft, dirty);
        page.select_node(Some("pointer"));
        page.parameter_value["disposition"] = serde_json::json!("passthrough");
        let parameters = page.parameter_value.clone();
        let next_command = page.next_command;
        page.armed = true;
        page.cleanup_pending = true;
        page.session_stopped(&definition);
        assert_eq!(page.draft, dirty);
        assert_eq!(page.parameter_value, parameters);
        assert_eq!(page.selected_node.as_deref(), Some("pointer"));
        assert!(page.has_profile_draft());
        assert!(page.pending.is_none() && page.disarm_pending.is_none());
        assert!(!page.armed && !page.cleanup_pending);
        assert_eq!((page.revision, page.sequence), (0, 0));
        assert_eq!(page.next_command, next_command);
    }

    #[test]
    fn safety_disarm_preserves_primary_identity_and_late_reply_cannot_regress_authority() {
        for applying in [true, false] {
            let ctx = egui::Context::default();
            ouroboros_ui::Theme::install(&ctx, ouroboros_ui::Mode::Dark);
            let definition = graph();
            let mut page = MapperPage {
                draft: definition.clone(),
                next_command: 2,
                ..Default::default()
            };
            page.pending = Some(if applying {
                Pending::Apply {
                    command_id: 1,
                    definition: definition.clone(),
                    expected_revision: 0,
                }
            } else {
                page.applied = definition.clone();
                Pending::Arm {
                    command_id: 1,
                    expected_revision: 0,
                }
            });
            let size = egui::vec2(320.0, 360.0);
            let _ = frame(&ctx, size, vec![], &mut page, true);
            let disarm = rectangle(
                &ctx,
                "Disarm input mapper",
                Rect::from_min_size(egui::Pos2::ZERO, size),
            );
            let _ = frame(&ctx, size, click(disarm.center(), true), &mut page, true);
            assert!(matches!(
                frame(&ctx, size, click(disarm.center(), false), &mut page, true),
                Some(PageAction::Disarm { command_id: 2, .. })
            ));
            assert_eq!(page.pending.as_ref().unwrap().command_id(), 1);
            assert_eq!(page.disarm_pending.as_ref().unwrap().command_id, 2);
            let latest = mapper_runtime::State {
                acknowledged: !applying,
                reply_to: Some(2),
                sequence: 12,
                revision: u64::from(applying),
                definition: definition.clone(),
                error: applying.then(|| "Disarm revision changed".into()),
                ..Default::default()
            };
            assert!(page.receive_reply(2, &latest).is_none());
            assert_eq!(page.pending.as_ref().unwrap().command_id(), 1);
            assert!(page.disarm_pending.is_none());
            let earlier = mapper_runtime::State {
                acknowledged: true,
                reply_to: Some(1),
                sequence: 11,
                revision: u64::from(applying),
                definition: definition.clone(),
                armed: !applying,
                ..Default::default()
            };
            assert_eq!(
                page.receive_reply(1, &earlier),
                applying.then_some(definition.clone())
            );
            assert!(page.receive_reply(1, &earlier).is_none());
            assert!(page.pending.is_none());
            assert_eq!(page.sequence, 12);
            assert!(!page.armed);
            page.reset_profile(&definition);
            assert!(page.receive_reply(1, &earlier).is_none());
            assert_eq!(page.next_command, 3);
        }
    }
    #[test]
    fn type_text_authoring_exposes_two_ports_and_keeps_apply_and_arm_explicit() {
        for size in [egui::vec2(320.0, 360.0), egui::vec2(680.0, 620.0)] {
            let ctx = egui::Context::default();
            ouroboros_ui::Theme::install(&ctx, ouroboros_ui::Mode::Dark);
            let node = |id: &str, kind| Node {
                id: id.into(),
                kind,
                position: None,
            };
            let mut page = MapperPage {
                draft: Definition {
                    nodes: vec![
                        node(
                            "wheel",
                            NodeKind::Wheel {
                                user: "virtual-user".into(),
                                horizontal: false,
                                disposition: mapper::Disposition::Passthrough,
                            },
                        ),
                        node("pulse", NodeKind::ImpulseToPulse),
                        node(
                            "literal",
                            NodeKind::Constant {
                                value: mapper::Value::Text("MUX 7319 🙂".into()),
                            },
                        ),
                    ],
                    edges: vec![Edge {
                        from: PortRef {
                            node: "wheel".into(),
                            port: "value".into(),
                        },
                        to: PortRef {
                            node: "pulse".into(),
                            port: "value".into(),
                        },
                    }],
                    ..Default::default()
                },
                ..Default::default()
            };
            let original = page.draft.clone();
            open_node_palette(&ctx, size, &mut page);
            type_node_search(&ctx, size, &mut page, "Type text");
            click_mapper_control(&ctx, size, &mut page, "Choose mapper node type_text");
            assert_eq!(
                page.draft, original,
                "choosing the palette entry must not add or arm it"
            );
            assert!(page.undo.is_empty() && page.pending.is_none() && !page.armed);
            click_mapper_control(&ctx, size, &mut page, "Add mapper node");
            assert_eq!(page.view, View::Inspector);
            assert_eq!(page.undo.len(), 1);
            let id = page.selected_node.clone().unwrap();
            assert_eq!(
                page.draft.nodes.last().unwrap().kind,
                NodeKind::Sink {
                    user: "virtual-user".into(),
                    channel: mapper::OutputChannel::TypeText
                }
            );
            scroll_to_mapper_control(&ctx, size, &mut page, "Mapper Type text help");
            scroll_to_mapper_control(&ctx, size, &mut page, "Mapper parameter user");
            scroll_to_mapper_control(&ctx, size, &mut page, "Mapper parameter channel type");
            for port in ["text", "trigger"] {
                scroll_to_mapper_control(
                    &ctx,
                    size,
                    &mut page,
                    &format!("Mapper source {id}.{port}"),
                );
                click_mapper_control(
                    &ctx,
                    size,
                    &mut page,
                    &format!("Connect mapper port {id}.{port}"),
                );
            }
            assert!(page.draft.compile().is_ok());
            assert_eq!(
                page.draft
                    .edges
                    .iter()
                    .filter(|edge| edge.to.node == id)
                    .count(),
                2
            );
            assert!(
                page.draft
                    .edges
                    .iter()
                    .any(|edge| edge.from.node == "literal"
                        && edge.to.node == id
                        && edge.to.port == "text")
            );
            assert!(page.draft.edges.iter().any(|edge| edge.from.node == "pulse"
                && edge.to.node == id
                && edge.to.port == "trigger"));
            assert_eq!(page.undo.len(), 3);
            assert_eq!(page.applied, Definition::default());
            assert!(page.pending.is_none() && !page.armed);
            click_mapper_control(&ctx, size, &mut page, "Mapper Graph");
            for _ in 0..3 {
                click_mapper_control(&ctx, size, &mut page, "Undo mapper edit");
            }
            assert_eq!(page.draft, original);
            assert!(page.pending.is_none() && !page.armed);
        }
    }
}
