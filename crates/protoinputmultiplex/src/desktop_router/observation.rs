//! Private, bounded desktop observation. No SDK authority or target operations.
use mux_core::{
    application_event::{Point, ScreenState, Transition, WheelAxis},
    desktop::{CursorSettings, CursorShape, MouseButton},
    emulated::Packet,
};
use serde::{Deserialize, Serialize};
use std::{collections::{BTreeMap, BTreeSet, VecDeque}, sync::{Arc, Mutex, atomic::{AtomicU64, Ordering}}};

pub const MAX_BINDINGS: usize = 64;
pub const MAX_RECORDS: usize = 4096;
pub const MAX_DRAIN: usize = 128;
pub const MAX_DELIVERY_JSON_BYTES: usize = 64 * 1024;
const MAX_BYTES: usize = 2 * 1024 * 1024;
pub const GENERATION_ENV: &str = "PIM_DESKTOP_OBSERVATION_GENERATION";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct BindingTag { pub routing_generation: u64, pub binding_generation: u64, pub token: u64 }
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ScanPrefix { None, E0, E1 }
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct WindowsVirtualKey { pub virtual_key: u16, pub scan_code: u8, pub prefix: ScanPrefix }
impl WindowsVirtualKey {
    pub fn from_down(packet: Packet) -> Option<Self> {
        if packet.kind != 1 || packet.flags & 1 != 0 || packet.validate().is_err() { return None; }
        Some(Self {
            virtual_key: u16::try_from(packet.data).ok()?,
            scan_code: u8::try_from(packet.x).ok()?,
            prefix: match packet.flags { 0 => ScanPrefix::None, 2 => ScanPrefix::E0, 4 => ScanPrefix::E1, _ => return None },
        })
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ConfiguredCursor {
    pub shape: CursorShape, pub size: f32, pub color: [u8; 3], pub visible: bool, pub follow_application: bool,
}
impl From<&CursorSettings> for ConfiguredCursor {
    fn from(value: &CursorSettings) -> Self {
        Self { shape: value.shape, size: value.size, color: value.color, visible: value.visible, follow_application: value.follow_application }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Admission { LogicalOnly, NativeSubmissionAccepted, SharedNativeHold }
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum Change {
    Motion { position: Point },
    Button { button: MouseButton, transition: Transition },
    Key { key: WindowsVirtualKey, transition: Transition },
    Wheel { axis: WheelAxis, units: i32 },
    TextUnit { unit: u16 },
    Cursor { cursor: ConfiguredCursor },
}
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Record { pub tag: BindingTag, pub sequence: u64, pub screen_revision: u64, pub admission: Admission, pub change: Change }
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ButtonCount { pub button: MouseButton, pub count: u8 }
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HeldState { pub buttons: Vec<ButtonCount>, pub keys: Vec<WindowsVirtualKey> }
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Baseline {
    pub request: u64, pub tag: BindingTag, pub next_sequence: u64, pub name: String,
    pub screens: ScreenState, pub position: Point, pub cursor: ConfiguredCursor,
    pub buttons: Vec<ButtonCount>, pub keys: Vec<WindowsVirtualKey>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LossReason { StopIntent, Eof, Overflow, Sequence, Generation, Uncertain, Removed, Screen, Unbound }
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BindRejection { StaleGeneration, MissingSeat, NotVirtual, Gesture, UncertainHoldOrRelease, PendingMotion, InvalidBaseline, Screen, Limit, Unsupported }
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Command {
    Bind { request: u64, tag: BindingTag, seat_identity: String },
    Unbind { tag: BindingTag },
}
pub enum Operation { Command(Command), Drain, Fence(LossReason) }
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Delivery {
    Bound(Baseline),
    Rejected { request: u64, tag: BindingTag, reason: BindRejection },
    Record(Record),
    Lost { tag: Option<BindingTag>, reason: LossReason },
}
pub fn button_from_down(flags: u32) -> Option<MouseButton> {
    match flags { 1 => Some(MouseButton::Primary), 4 => Some(MouseButton::Secondary), 16 => Some(MouseButton::Middle), 64 => Some(MouseButton::X1), 256 => Some(MouseButton::X2), _ => None }
}
pub fn wheel(packet: Packet) -> Option<Change> {
    let axis = match packet.flags { 0x400 => WheelAxis::Vertical, 0x800 => WheelAxis::Horizontal, _ => return None };
    let value = u16::try_from(packet.data).ok()?;
    let units = i32::from(i16::from_le_bytes(value.to_le_bytes()));
    (units != 0).then_some(Change::Wheel { axis, units })
}
pub fn screens() -> Result<ScreenState, BindRejection> {
    let desktop = mux_windows::monitors::physical_desktop_bounds().map_err(|_| BindRejection::Screen)?;
    let mut bounds = mux_windows::monitors::inventory().map_err(|_| BindRejection::Screen)?.into_iter().map(|m| m.bounds).collect::<Vec<_>>();
    bounds.sort_by_key(|b| (b.left, b.top, b.right, b.bottom));
    let screens = ScreenState { revision: 1, desktop, screens: bounds };
    screens.validate().map_err(|_| BindRejection::Screen)?;
    Ok(screens)
}

struct Binding { user: usize, next: u64 }
#[derive(Default)]
pub struct Journal {
    bindings: BTreeMap<BindingTag, Binding>, records: VecDeque<Record>, losses: VecDeque<Delivery>,
    screens: Option<ScreenState>, stopped: bool,
}
impl Journal {
    pub fn bind(&mut self, tag: BindingTag, user: usize, screens: &ScreenState) -> Result<(), BindRejection> {
        if self.stopped { return Err(BindRejection::UncertainHoldOrRelease); }
        if self.bindings.len() + self.losses.len() >= MAX_BINDINGS || self.bindings.contains_key(&tag) || self.bindings.values().any(|b| b.user == user) { return Err(BindRejection::Limit); }
        if self.screens.as_ref().is_some_and(|s| s != screens) { return Err(BindRejection::Screen); }
        self.screens = Some(screens.clone());
        self.bindings.insert(tag, Binding { user, next: 1 });
        Ok(())
    }
    pub fn active(&self) -> bool { !self.bindings.is_empty() }
    pub fn observing_user(&self, user: usize) -> bool { self.bindings.values().any(|b| b.user == user) }
    pub fn screen_matches(&self, screens: &ScreenState) -> bool { self.screens.as_ref().is_none_or(|s| s == screens) }
    pub fn unbind(&mut self, tag: BindingTag, reason: LossReason) {
        if self.bindings.remove(&tag).is_some() {
            self.records.retain(|r| r.tag != tag);
            self.losses.push_back(Delivery::Lost { tag: Some(tag), reason });
        }
    }
    pub fn fence_user(&mut self, user: usize, reason: LossReason) {
        let tags = self.bindings.iter().filter_map(|(tag, b)| (b.user == user).then_some(*tag)).collect::<Vec<_>>();
        for tag in tags { self.unbind(tag, reason); }
    }
    pub fn fence(&mut self, reason: LossReason) {
        if self.stopped { return; }
        self.stopped = true;
        self.bindings.clear(); self.records.clear(); self.losses.clear();
        self.losses.push_back(Delivery::Lost { tag: None, reason });
    }
    pub fn record(&mut self, user: usize, change: Change, admission: Admission) {
        if self.stopped { return; }
        let Some(tag) = self.bindings.iter().find_map(|(tag, b)| (b.user == user).then_some(*tag)) else { return; };
        if self.records.len() >= MAX_RECORDS || (self.records.len() + 1).saturating_mul(std::mem::size_of::<Record>()) > MAX_BYTES {
            self.fence(LossReason::Overflow); return;
        }
        let binding = self.bindings.get_mut(&tag).unwrap();
        let sequence = binding.next;
        let Some(next) = sequence.checked_add(1) else { self.fence(LossReason::Sequence); return; };
        binding.next = next;
        self.records.push_back(Record { tag, sequence, screen_revision: 1, admission, change });
    }
    pub fn drain(&mut self) -> Vec<Delivery> {
        let mut out = Vec::new();
        while out.len() < MAX_DRAIN {
            if let Some(loss) = self.losses.pop_front() { out.push(loss); }
            else if let Some(record) = self.records.pop_front() { out.push(Delivery::Record(record)); }
            else { break; }
        }
        out
    }
}

// A separate bounded inbox; reader-observed loss does not wait behind GUI records.
struct Pending { request: u64, next: u64, bound: bool }
struct InboxState {
    generation: u64, next: u64, stopped: Option<LossReason>,
    pending: BTreeMap<BindingTag, Pending>, outstanding: BTreeSet<BindingTag>, cancel_sent: BTreeSet<BindingTag>, queue: VecDeque<Delivery>, losses: VecDeque<Delivery>, bytes: usize,
}
#[derive(Clone)]
pub struct Inbox(Arc<Mutex<InboxState>>);
impl Inbox {
    pub fn new() -> anyhow::Result<Self> {
        static GENERATION: AtomicU64 = AtomicU64::new(1);
        let generation = GENERATION.fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_add(1)).map_err(|_| anyhow::anyhow!("observation routing generation exhausted"))?;
        Ok(Self(Arc::new(Mutex::new(InboxState { generation, next: 1, stopped: None, pending: BTreeMap::new(), outstanding: BTreeSet::new(), cancel_sent: BTreeSet::new(), queue: VecDeque::new(), losses: VecDeque::new(), bytes: 0 }))))
    }
    pub fn generation(&self) -> u64 { self.0.lock().unwrap().generation }
    pub fn begin(&self, request: u64) -> Result<BindingTag, BindRejection> {
        let mut s = self.0.lock().unwrap();
        if request == 0 || s.stopped.is_some() { return Err(BindRejection::StaleGeneration); }
        if s.pending.len() + s.losses.len() >= MAX_BINDINGS || s.outstanding.len() >= MAX_BINDINGS || s.pending.values().any(|p| p.request == request) { return Err(BindRejection::Limit); }
        let token = s.next;
        s.next = token.checked_add(1).ok_or(BindRejection::Limit)?;
        let tag = BindingTag { routing_generation: s.generation, binding_generation: token, token };
        s.pending.insert(tag, Pending { request, next: 1, bound: false });
        s.outstanding.insert(tag);
        Ok(tag)
    }
    pub fn live(&self, tag: BindingTag) -> bool { let s = self.0.lock().unwrap(); s.stopped.is_none() && s.pending.contains_key(&tag) }
    pub fn cancel(&self, tag: BindingTag, reason: LossReason) -> bool {
        let mut s = self.0.lock().unwrap();
        if s.pending.remove(&tag).is_some() {
            s.queue.retain(|d| delivery_tag(d) != Some(tag));
            s.bytes = s.queue.iter().map(delivery_cost).sum();
            s.losses.push_back(Delivery::Lost { tag: Some(tag), reason });
        }
        s.outstanding.contains(&tag) && s.cancel_sent.insert(tag)
    }
    pub fn fence(&self, reason: LossReason) {
        let mut s = self.0.lock().unwrap();
        if s.stopped.is_none() {
            s.stopped = Some(reason); s.pending.clear(); s.queue.clear(); s.bytes = 0; s.losses.clear();
            s.losses.push_back(Delivery::Lost { tag: None, reason });
        }
    }
    pub fn receive(&self, delivery: Delivery) {
        if let Delivery::Lost { tag, reason } = delivery { match tag { Some(tag) => { let mut s = self.0.lock().unwrap(); s.outstanding.remove(&tag); s.cancel_sent.remove(&tag); drop(s); self.cancel(tag, reason); }, None => self.fence(reason) }; return; }
        let mut s = self.0.lock().unwrap();
        if s.stopped.is_some() { return; }
        let Some(tag) = delivery_tag(&delivery) else { return; };
        if tag.routing_generation != s.generation { drop(s); self.fence(LossReason::Generation); return; }
        // A late cancelled Baseline/record may never revive its private mapping.
        let Some(pending) = s.pending.get_mut(&tag) else { if matches!(&delivery, Delivery::Rejected { .. }) { s.outstanding.remove(&tag); s.cancel_sent.remove(&tag); } return; };
        let valid = match &delivery {
            Delivery::Bound(b) => !pending.bound && b.request == pending.request && b.next_sequence == 1 && b.name.len() <= 128 && b.buttons.len() <= 5 && b.keys.len() <= 247 && b.screens.validate().is_ok(),
            Delivery::Rejected { request, .. } => !pending.bound && *request == pending.request,
            Delivery::Record(r) => pending.bound && r.sequence == pending.next && r.screen_revision == 1,
            Delivery::Lost { .. } => false,
        };
        if !valid { drop(s); self.fence(LossReason::Sequence); return; }
        match &delivery {
            Delivery::Bound(_) => pending.bound = true,
            Delivery::Record(r) => { let Some(next) = r.sequence.checked_add(1) else { drop(s); self.fence(LossReason::Sequence); return; }; pending.next = next; }
            Delivery::Rejected { .. } => { s.pending.remove(&tag); s.outstanding.remove(&tag); s.cancel_sent.remove(&tag); }
            Delivery::Lost { .. } => unreachable!(),
        }
        let cost = delivery_cost(&delivery);
        if s.queue.len() >= MAX_RECORDS || s.bytes.saturating_add(cost) > MAX_BYTES { drop(s); self.fence(LossReason::Overflow); return; }
        s.bytes += cost; s.queue.push_back(delivery);
    }
    pub fn take(&self) -> Vec<Delivery> {
        let mut s = self.0.lock().unwrap(); let mut out = Vec::new();
        while out.len() < MAX_DRAIN {
            if let Some(loss) = s.losses.pop_front() { out.push(loss); }
            else if let Some(d) = s.queue.pop_front() { s.bytes -= delivery_cost(&d); out.push(d); }
            else { break; }
        }
        out
    }
}
fn delivery_tag(d: &Delivery) -> Option<BindingTag> { match d { Delivery::Bound(b) => Some(b.tag), Delivery::Rejected { tag, .. } => Some(*tag), Delivery::Record(r) => Some(r.tag), Delivery::Lost { tag, .. } => *tag } }
fn delivery_cost(d: &Delivery) -> usize { std::mem::size_of::<Delivery>() + match d { Delivery::Bound(b) => b.name.len() + b.buttons.len() * std::mem::size_of::<ButtonCount>() + b.keys.len() * std::mem::size_of::<WindowsVirtualKey>() + b.screens.screens.len() * std::mem::size_of::<mux_core::desktop::Bounds>(), _ => 0 } }

impl super::DesktopRouter {
    pub(crate) fn desktop_observation(&mut self, operation: Operation) -> Vec<Delivery> {
        match operation {
            Operation::Drain => self.observation.drain(),
            Operation::Fence(reason) => { self.observation.fence(reason); self.observation.drain() }
            Operation::Command(Command::Unbind { tag }) => { self.observation.unbind(tag, LossReason::Unbound); self.observation.drain() }
            Operation::Command(Command::Bind { request, tag, seat_identity }) => {
                let baseline = self.observation_baseline(request, tag, &seat_identity);
                vec![match baseline { Ok(b) => Delivery::Bound(b), Err(reason) => Delivery::Rejected { request, tag, reason } }]
            }
        }
    }
    fn observation_baseline(&mut self, request: u64, tag: BindingTag, identity: &str) -> Result<Baseline, BindRejection> {
        if request == 0 || tag.routing_generation == 0 || tag.routing_generation != self.observation_generation || tag.token == 0 || tag.binding_generation == 0 { return Err(BindRejection::StaleGeneration); }
        let user = self.seat_ids.iter().position(|id| id == identity).ok_or(BindRejection::MissingSeat)?;
        if !self.users[user].active { return Err(BindRejection::MissingSeat); }
        let seat = &self.seats[user];
        let virtual_device = |device: u32| device & mux_core::emulated::MASK == mux_core::emulated::BASE;
        if !virtual_device(seat.mouse) || !seat.keyboard.is_none_or(virtual_device) { return Err(BindRejection::NotVirtual); }
        if self.users[user].gesture.is_some() { return Err(BindRejection::Gesture); }
        if self.users[user].observation_uncertain || self.user_pauses[user].error.is_some() { return Err(BindRejection::UncertainHoldOrRelease); }
        if self.backend.has_pending_motion(seat.mouse).map_err(|_| BindRejection::UncertainHoldOrRelease)? { return Err(BindRejection::PendingMotion); }
        let screens = screens()?;
        let position = Point { x: self.users[user].cursor.x, y: self.users[user].cursor.y };
        if !screens.desktop.contains(position.x, position.y) || seat.name.is_empty() || seat.name.len() > 128 || seat.name.trim() != seat.name || seat.name.chars().any(char::is_control) { return Err(BindRejection::InvalidBaseline); }
        let held = self.observation_held(user)?;
        let baseline = Baseline { request, tag, next_sequence: 1, name: seat.name.clone(), screens: screens.clone(), position, cursor: (&self.users[user].settings).into(), buttons: held.buttons, keys: held.keys };
        self.observation.bind(tag, user, &screens)?;
        Ok(baseline)
    }
    pub(super) fn observation_held(&self, user: usize) -> Result<HeldState, BindRejection> {
        let seat = self.seats.get(user).ok_or(BindRejection::MissingSeat)?;
        held_snapshot(user, seat, &self.held)
    }
    pub(super) fn observation_before(&mut self, user: usize) -> Option<HeldState> {
        if !self.observation.observing_user(user) { return None; }
        match self.observation_held(user) { Ok(before) => Some(before), Err(_) => { self.observation_uncertain(user); None } }
    }
    pub(super) fn observation_uncertain(&mut self, user: usize) {
        if let Some(state) = self.users.get_mut(user) { state.observation_uncertain = true; }
        self.observation.fence_user(user, LossReason::Uncertain);
    }
    pub(super) fn observation_uncertain_all(&mut self) {
        for user in 0..self.users.len() { self.observation_uncertain(user); }
    }
    pub(super) fn observation_flush_motion(&mut self) -> anyhow::Result<()> {
        let result = self.backend.flush_motion();
        if result.is_err() { self.observation_uncertain_all(); }
        result
    }
    pub(super) fn observation_record(&mut self, user: usize, change: Change, admission: Admission) {
        if let Change::Motion { position } = change
            && self.observation.screens.as_ref().is_some_and(|s| !s.desktop.contains(position.x, position.y)) {
            self.observation_uncertain(user); return;
        }
        self.observation.record(user, change, admission);
    }
    pub(super) fn observation_motion(&mut self, user: usize) {
        self.observation_record(user, Change::Motion { position: Point { x: self.users[user].cursor.x, y: self.users[user].cursor.y } }, Admission::LogicalOnly);
    }
    pub(super) fn observation_hold_changes(&mut self, user: usize, before: HeldState, admission: Admission, additions: bool) {
        let Ok(after) = self.observation_held(user) else { self.observation_uncertain(user); return; };
        for change in held_changes(&before, &after, additions) { self.observation_record(user, change, admission); }
    }
    pub(super) fn observation_check_screens(&mut self) {
        if self.observation.active() {
            match screens() {
                Ok(current) if self.observation.screen_matches(&current) => {}
                _ => self.observation.fence(LossReason::Screen),
            }
        }
    }
}

fn held_snapshot(user: usize, seat: &mux_core::ResolvedSeat, held_inputs: &BTreeMap<(u32, u32), super::Held>) -> Result<HeldState, BindRejection> {
        let mut state = HeldState::default();
        for ((device, token), held) in held_inputs.iter().filter(|(_, h)| h.user == user) {
            let p = held.packet;
            if *device != p.device || p.validate().is_err() { return Err(BindRejection::InvalidBaseline); }
            if p.kind == 1 {
                if Some(*device) != seat.keyboard || *token != p.data { return Err(BindRejection::InvalidBaseline); }
                let key = WindowsVirtualKey::from_down(p).ok_or(BindRejection::InvalidBaseline)?;
                if state.keys.iter().any(|k| k.virtual_key == key.virtual_key) { return Err(BindRejection::InvalidBaseline); }
                state.keys.push(key);
            } else if p.kind == 0 {
                if *device != seat.mouse || button_from_down(*token).is_none() { return Err(BindRejection::InvalidBaseline); }
                let button = button_from_down(p.flags).ok_or(BindRejection::InvalidBaseline)?;
                if let Some(count) = state.buttons.iter_mut().find(|b| b.button == button) { count.count = count.count.checked_add(1).ok_or(BindRejection::Limit)?; }
                else { state.buttons.push(ButtonCount { button, count: 1 }); }
            } else { return Err(BindRejection::InvalidBaseline); }
        }
        if state.keys.len() > 247 || state.buttons.len() > 5 || state.buttons.iter().any(|b| b.count > 5) { return Err(BindRejection::Limit); }
        state.keys.sort();
        state.buttons.sort_by_key(|b| match b.button { MouseButton::Primary => 0, MouseButton::Secondary => 1, MouseButton::Middle => 2, MouseButton::X1 => 3, MouseButton::X2 => 4 });
        Ok(state)
}
fn held_changes(before: &HeldState, after: &HeldState, additions: bool) -> Vec<Change> {
        let (from, to, transition) = if additions { (before, after, Transition::Down) } else { (after, before, Transition::Up) };
        let mut changes = Vec::new();
        for button in &to.buttons { if !from.buttons.iter().any(|b| b.button == button.button) { changes.push(Change::Button { button: button.button, transition }); } }
        for key in &to.keys { if !from.keys.contains(key) { changes.push(Change::Key { key: *key, transition }); } }
        changes
}

#[cfg(test)]
mod tests {
    use super::*;
    fn tag(n: u64) -> BindingTag { BindingTag { routing_generation: 1, binding_generation: n, token: n } }
    fn screen() -> ScreenState { let bounds = mux_core::desktop::Bounds { left: -100, top: 0, right: 100, bottom: 100 }; ScreenState { revision: 1, desktop: bounds, screens: vec![bounds] } }
    #[test]
    fn exact_native_identity_and_signed_transformed_wheel() {
        let device = mux_core::emulated::BASE | 3;
        let a = WindowsVirtualKey::from_down(Packet::key_scanned(device, 0xa3, 0x1d, 2, false)).unwrap();
        let b = WindowsVirtualKey::from_down(Packet::key_scanned(device, 0x11, 0x1d, 0, false)).unwrap();
        assert_ne!(a, b); assert_eq!(a.prefix, ScanPrefix::E0);
        assert!(WindowsVirtualKey::from_down(Packet::key_scanned(device, 0xa3, 0x1d, 6, false)).is_none());
        assert!(matches!(wheel(Packet::horizontal_wheel(device - 1, -120)), Some(Change::Wheel { axis: WheelAxis::Horizontal, units: -120 })));
    }
    #[test]
    fn loss_precedes_records_and_cannot_be_cleared_by_rebind() {
        let mut j = Journal::default(); j.bind(tag(1), 0, &screen()).unwrap();
        for _ in 0..=MAX_RECORDS { j.record(0, Change::TextUnit { unit: 0xd800 }, Admission::NativeSubmissionAccepted); }
        assert_eq!(j.drain(), vec![Delivery::Lost { tag: None, reason: LossReason::Overflow }]);
        assert_eq!(j.bind(tag(2), 0, &screen()), Err(BindRejection::UncertainHoldOrRelease));
    }
    #[test]
    fn cancelled_pending_baseline_never_resurrects() {
        let inbox = Inbox::new().unwrap(); let tag = inbox.begin(1).unwrap(); inbox.cancel(tag, LossReason::Unbound);
        inbox.receive(Delivery::Bound(Baseline { request: 1, tag, next_sequence: 1, name: "A".into(), screens: screen(), position: Point { x: 0, y: 1 }, cursor: (&CursorSettings::default()).into(), buttons: vec![], keys: vec![] }));
        assert!(!inbox.live(tag)); assert_eq!(inbox.take(), vec![Delivery::Lost { tag: Some(tag), reason: LossReason::Unbound }]);
    }
    #[test]
    fn ordinary_held_bootstrap_preserves_saved_mapping_and_exact_key() {
        let mouse = mux_core::emulated::BASE | 2; let keyboard = mouse | 1;
        let seat = mux_core::ResolvedSeat { name: "A".into(), mouse, keyboard: Some(keyboard), target_pid: 10 };
        let key = Packet::key_scanned(keyboard, 0xa3, 0x1d, 2, false);
        let mut held = BTreeMap::new();
        // Source primary was mapped to secondary. The baseline follows saved output.
        held.insert((mouse, 1), super::super::Held { user: 0, packet: Packet::button(mouse, 1, false), target: 0, window: mux_core::routing::Window { pid: 10, hwnd: 100 } });
        held.insert((keyboard, 0xa3), super::super::Held { user: 0, packet: key, target: 0, window: mux_core::routing::Window { pid: 10, hwnd: 100 } });
        let baseline = held_snapshot(0, &seat, &held).unwrap();
        assert_eq!(baseline.buttons, vec![ButtonCount { button: MouseButton::Secondary, count: 1 }]);
        assert_eq!(baseline.keys, vec![WindowsVirtualKey { virtual_key: 0xa3, scan_code: 0x1d, prefix: ScanPrefix::E0 }]);
        let mut journal = Journal::default(); journal.bind(tag(1), 0, &screen()).unwrap();
        assert!(journal.drain().is_empty(), "bootstrap never synthesizes DOWN");
        held.remove(&(keyboard, 0xa3));
        let after = held_snapshot(0, &seat, &held).unwrap();
        assert_eq!(held_changes(&baseline, &after, false), vec![Change::Key { key: WindowsVirtualKey::from_down(key).unwrap(), transition: Transition::Up }]);
        // A token inconsistent with its saved DOWN cannot be partially imported.
        held.get_mut(&(mouse, 1)).unwrap().packet.device = keyboard;
        assert_eq!(held_snapshot(0, &seat, &held), Err(BindRejection::InvalidBaseline));
    }
    #[test]
    fn cancelled_commands_keep_a_bounded_issued_slot_until_worker_ack() {
        let inbox = Inbox::new().unwrap();
        for request in 1..=MAX_BINDINGS as u64 { let tag = inbox.begin(request).unwrap(); inbox.cancel(tag, LossReason::Unbound); inbox.take(); }
        assert_eq!(inbox.begin(1000), Err(BindRejection::Limit));
        let tag = BindingTag { routing_generation: inbox.generation(), binding_generation: 1, token: 1 };
        inbox.receive(Delivery::Lost { tag: Some(tag), reason: LossReason::Unbound });
        assert!(inbox.begin(1000).is_ok());
    }
    #[test]
    fn shared_native_key_has_two_seat_baselines_and_one_suppressed_release() {
        let mouse_a = mux_core::emulated::BASE | 2; let mouse_b = mux_core::emulated::BASE | 4;
        let seat = |mouse| mux_core::ResolvedSeat { name: "A".into(), mouse, keyboard: Some(mouse | 1), target_pid: 10 };
        let a = seat(mouse_a); let b = seat(mouse_b);
        let mut held = BTreeMap::new();
        for (user, keyboard) in [(0, mouse_a | 1), (1, mouse_b | 1)] {
            held.insert((keyboard, 0x41), super::super::Held { user, packet: Packet::key_scanned(keyboard, 0x41, 0x1e, 0, false), target: 0, window: mux_core::routing::Window { pid: 10, hwnd: 100 } });
        }
        let baseline_a = held_snapshot(0, &a, &held).unwrap(); let baseline_b = held_snapshot(1, &b, &held).unwrap();
        assert_eq!(baseline_a.keys, baseline_b.keys);
        super::super::release_held_token(&mut held, (mouse_a | 1, 0x41), |_| true, |_| panic!("peer still owns native key; no native UP")).unwrap();
        assert!(held_snapshot(0, &a, &held).unwrap().keys.is_empty());
        assert_eq!(held_snapshot(1, &b, &held).unwrap(), baseline_b);
        assert_eq!(held_changes(&baseline_a, &held_snapshot(0, &a, &held).unwrap(), false).len(), 1);
    }
    #[test]
    fn loss_still_allows_one_effect_free_worker_retirement() {
        let inbox = Inbox::new().unwrap(); let tag = inbox.begin(1).unwrap();
        inbox.fence(LossReason::Uncertain);
        assert!(!inbox.live(tag)); assert!(inbox.cancel(tag, LossReason::Unbound));
        assert!(!inbox.cancel(tag, LossReason::Unbound));
        assert_eq!(inbox.begin(2), Err(BindRejection::StaleGeneration));
    }
    #[test]
    fn sequence_loss_clears_a_queued_baseline_before_gui_can_fold_it() {
        let inbox = Inbox::new().unwrap(); let tag = inbox.begin(1).unwrap();
        inbox.receive(Delivery::Bound(Baseline { request: 1, tag, next_sequence: 1, name: "A".into(), screens: screen(), position: Point { x: 0, y: 1 }, cursor: (&CursorSettings::default()).into(), buttons: vec![], keys: vec![] }));
        inbox.receive(Delivery::Record(Record { tag, sequence: 2, screen_revision: 1, admission: Admission::LogicalOnly, change: Change::TextUnit { unit: 0xdc00 } }));
        assert!(!inbox.live(tag));
        assert_eq!(inbox.take(), vec![Delivery::Lost { tag: None, reason: LossReason::Sequence }]);
    }
}
