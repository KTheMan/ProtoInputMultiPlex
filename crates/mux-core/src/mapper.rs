//! Portable, bounded virtual-input signal graphs.
//!
//! Numeric values and parameters lie in [-1e9, 1e9]. This is a signal domain,
//! not an integer delivery domain: callers retain per-sink fractional remainders
//! and range-check quantization. One `State` belongs to exactly one actor and one
//! compiled graph. Admission, permissions, arming and native holds are external.
use anyhow::{Context, Result, bail, ensure};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::{collections::VecDeque, sync::Arc};

pub const MAX_NODES: usize = 256;
pub const MAX_GROUPS: usize = 64;
pub const MAX_EDGES: usize = 1024;
pub const MAX_JSON_BYTES: usize = 1024 * 1024;
pub const MAX_EFFECTS: usize = 128;
pub const MAX_ACTORS: usize = 64;
pub const MAX_TIMERS: usize = 16;
pub const MAX_DELAYS: usize = 16;
pub const MAX_PENDING_DELAYS_PER_ACTOR: usize = 128;
pub const MAX_PENDING_DELAYS_SESSION: usize = 256;
pub const MAX_MONITORS: usize = 16;
pub const MAX_SCOPE_SAMPLES: usize = 64;
pub const MAX_OBSERVATIONS: usize = 8;
pub const MAX_PENDING_OBSERVATIONS_PER_ACTOR: usize = 8;
pub const MAX_PENDING_OBSERVATIONS_SESSION: usize = 32;
/// Maximum UTF-8 bytes in a private text value (including OCR results).
pub const MAX_TEXT_BYTES: usize = 4096;
pub const MAX_TYPE_TEXT_UTF16_UNITS: usize = MAX_EFFECTS;
pub const MAX_TEXT_LITERAL_BYTES: usize = 256;
pub const NUMERIC_LIMIT: f64 = 1_000_000_000.0;
const DEFAULT_TIMER_INTERVAL_MS: u32 = 500;
const DEFAULT_TIMER_COUNT: u32 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SignalType {
    Position,
    Delta,
    State,
    Impulse,
    Hold,
    Pulse,
    Text,
}
impl SignalType {
    fn transient(self) -> bool {
        matches!(self, Self::Delta | Self::Impulse | Self::Pulse)
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum Value {
    Position([f64; 2]),
    Delta([f64; 2]),
    State(f64),
    Impulse(f64),
    Hold(bool),
    Pulse(bool),
    Text(String),
}
impl Value {
    pub fn signal(&self) -> SignalType {
        match self {
            Self::Position(_) => SignalType::Position,
            Self::Delta(_) => SignalType::Delta,
            Self::State(_) => SignalType::State,
            Self::Impulse(_) => SignalType::Impulse,
            Self::Hold(_) => SignalType::Hold,
            Self::Pulse(_) => SignalType::Pulse,
            Self::Text(_) => SignalType::Text,
        }
    }
    fn validate(&self) -> Result<()> {
        match self {
            Self::Position(v) | Self::Delta(v) => vector(*v),
            Self::State(v) | Self::Impulse(v) => number(*v),
            Self::Hold(_) | Self::Pulse(_) => Ok(()),
            Self::Text(value) => text_value(value, MAX_TEXT_BYTES),
        }
    }
    fn active(&self) -> bool {
        match self {
            Self::Delta(v) => *v != [0.0, 0.0],
            Self::Impulse(v) => *v != 0.0,
            Self::Pulse(v) => *v,
            _ => true,
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum Disposition {
    #[default]
    Passthrough,
    Consume,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ScalarType {
    State,
    Impulse,
}
impl ScalarType {
    fn signal(self) -> SignalType {
        match self {
            Self::State => SignalType::State,
            Self::Impulse => SignalType::Impulse,
        }
    }
    fn value(self, v: f64) -> Value {
        match self {
            Self::State => Value::State(v),
            Self::Impulse => Value::Impulse(v),
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum VectorType {
    Position,
    Delta,
}
impl VectorType {
    fn signal(self) -> SignalType {
        match self {
            Self::Position => SignalType::Position,
            Self::Delta => SignalType::Delta,
        }
    }
    fn scalar(self) -> ScalarType {
        match self {
            Self::Position => ScalarType::State,
            Self::Delta => ScalarType::Impulse,
        }
    }
    fn value(self, v: [f64; 2]) -> Value {
        match self {
            Self::Position => Value::Position(v),
            Self::Delta => Value::Delta(v),
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum NodeCategory {
    Sources,
    Constants,
    Vector,
    Scalar,
    Binary,
    Routing,
    Conversion,
    Observers,
    Outputs,
}
impl NodeCategory {
    pub const ALL: [Self; 9] = [
        Self::Sources,
        Self::Constants,
        Self::Vector,
        Self::Scalar,
        Self::Binary,
        Self::Routing,
        Self::Conversion,
        Self::Observers,
        Self::Outputs,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Sources => "Sources",
            Self::Constants => "Constants",
            Self::Vector => "Vector",
            Self::Scalar => "Scalar",
            Self::Binary => "Binary",
            Self::Routing => "Routing",
            Self::Conversion => "Conversion",
            Self::Observers => "Observers",
            Self::Outputs => "Outputs",
        }
    }

    pub const fn description(self) -> &'static str {
        match self {
            Self::Sources => "Virtual pointer, wheel, key and button inputs",
            Self::Constants => "Fixed values for graph branches",
            Self::Vector => "Position and relative vector processing",
            Self::Scalar => "Numeric scalar processing",
            Self::Binary => "Boolean comparisons and logic",
            Self::Routing => "Select, gate and schedule signals",
            Self::Conversion => "Convert between signal units and forms",
            Self::Observers => "Inspect values without producing input",
            Self::Outputs => "Send values to selected virtual users",
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Arithmetic {
    Add,
    Subtract,
    Multiply,
    Divide,
    Min,
    Max,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum UnaryOperation {
    Abs,
    Negate,
    Sqrt,
    Square,
    Round,
    Floor,
    Ceil,
    Sin,
    Cos,
    Sign,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Comparison {
    Less,
    LessEqual,
    Equal,
    GreaterEqual,
    Greater,
}
/// Literal comparison only. Case-insensitive matching folds ASCII A-Z; it
/// performs no Unicode normalization, locale mapping, regex or numeric parsing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TextComparison {
    Equals,
    NotEquals,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Logic {
    And,
    Or,
    Not,
}
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Source {
    Pointer { user: String },
    Wheel { user: String, horizontal: bool },
    Key { user: String, key: u16 },
    Button { user: String, button: u8 },
}
impl Source {
    pub fn user(&self) -> &str {
        match self {
            Self::Pointer { user }
            | Self::Wheel { user, .. }
            | Self::Key { user, .. }
            | Self::Button { user, .. } => user,
        }
    }
    fn validate(&self) -> Result<()> {
        id(self.user())?;
        match self {
            Self::Key { key, .. } => key_code(*key),
            Self::Button { button, .. } => button_code(*button),
            _ => Ok(()),
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum OutputChannel {
    Position,
    Delta,
    Wheel { horizontal: bool },
    KeyHold { key: u16 },
    KeyPulse { key: u16 },
    ButtonHold { button: u8 },
    ButtonPulse { button: u8 },
    TypeText,
}
impl OutputChannel {
    /// Structural cost for source fan-out validation. Fixed channels use their
    /// complete native transition cost; TypeText contributes a minimum of one.
    /// Every admitted wave must also use `value_effect_cost` for its actual text
    /// payload before any native effect is delivered.
    pub fn effect_cost(&self) -> usize {
        if matches!(self, Self::KeyPulse { .. } | Self::ButtonPulse { .. }) {
            2
        } else {
            1
        }
    }
    /// Actual native message cost for a typed payload. Empty TypeText is a no-op.
    pub fn value_effect_cost(&self, value: &Value) -> Result<usize> {
        ensure!(
            self.signal() == value.signal(),
            "incompatible mapper output value"
        );
        value.validate()?;
        if let (Self::TypeText, Value::Text(text)) = (self, value) {
            type_text_cost(text)
        } else {
            Ok(self.effect_cost())
        }
    }
    pub fn signal(&self) -> SignalType {
        match self {
            Self::Position => SignalType::Position,
            Self::Delta => SignalType::Delta,
            Self::Wheel { .. } => SignalType::Impulse,
            Self::KeyHold { .. } | Self::ButtonHold { .. } => SignalType::Hold,
            Self::KeyPulse { .. } | Self::ButtonPulse { .. } => SignalType::Pulse,
            Self::TypeText => SignalType::Text,
        }
    }
    fn validate(&self) -> Result<()> {
        match self {
            Self::KeyHold { key } | Self::KeyPulse { key } => key_code(*key),
            Self::ButtonHold { button } | Self::ButtonPulse { button } => button_code(*button),
            _ => Ok(()),
        }
    }
    fn duplicates(&self, user: &str, source: &Source) -> bool {
        if user != source.user() {
            return false;
        }
        match (self, source) {
            (Self::Position | Self::Delta, Source::Pointer { .. }) => true,
            (Self::Wheel { horizontal: a }, Source::Wheel { horizontal: b, .. }) => a == b,
            (Self::KeyHold { key: a } | Self::KeyPulse { key: a }, Source::Key { key: b, .. }) => {
                a == b
            }
            (
                Self::ButtonHold { button: a } | Self::ButtonPulse { button: a },
                Source::Button { button: b, .. },
            ) => a == b,
            _ => false,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum NodeKind {
    Pointer {
        user: String,
        #[serde(default)]
        disposition: Disposition,
    },
    Wheel {
        user: String,
        horizontal: bool,
        #[serde(default)]
        disposition: Disposition,
    },
    Key {
        user: String,
        key: u16,
        #[serde(default)]
        disposition: Disposition,
    },
    Button {
        user: String,
        button: u8,
        #[serde(default)]
        disposition: Disposition,
    },
    Constant {
        value: Value,
    },
    Translate {
        offset: [f64; 2],
    },
    ClampPosition {
        min: [f64; 2],
        max: [f64; 2],
    },
    PositionToDelta,
    DeltaToPosition {
        initial: [f64; 2],
    },
    ScaleDelta {
        factor: f64,
    },
    AddDelta,
    RotateDelta {
        radians: f64,
    },
    DeadZoneDelta {
        radius: f64,
    },
    ScalarMath {
        signal: ScalarType,
        operation: Arithmetic,
    },
    UnaryMath {
        signal: ScalarType,
        operation: UnaryOperation,
    },
    VectorLength {
        signal: VectorType,
    },
    NormalizeVector {
        signal: VectorType,
    },
    ScaleScalar {
        signal: ScalarType,
        factor: f64,
    },
    ClampScalar {
        signal: ScalarType,
        min: f64,
        max: f64,
    },
    Remap {
        signal: ScalarType,
        input_min: f64,
        input_max: f64,
        output_min: f64,
        output_max: f64,
        clamp: bool,
    },
    Smooth {
        signal: SignalType,
        alpha: f64,
    },
    Velocity,
    Components {
        signal: VectorType,
    },
    Compose {
        signal: VectorType,
    },
    Compare {
        signal: ScalarType,
        operation: Comparison,
    },
    Logic {
        operation: Logic,
    },
    Gate {
        signal: SignalType,
    },
    ImpulseToPulse,
    RisingEdge,
    Toggle,
    Accumulate {
        initial: f64,
    },
    Counter {
        initial: u32,
        #[serde(default)]
        with_reset: bool,
    },
    Switch {
        signal: SignalType,
    },
    Timer {
        interval_ms: u32,
        count: u32,
    },
    Delay {
        signal: SignalType,
        delay_ms: u32,
    },
    Throttle {
        signal: SignalType,
        interval_ms: u32,
    },
    PixelColor {
        capture_user: String,
        x: u32,
        y: u32,
    },
    ImageMatch {
        capture_user: String,
        template_png_base64: String,
        region: Option<crate::screen::Region>,
        min_score: f64,
    },
    Ocr {
        capture_user: String,
        region: Option<crate::screen::Region>,
    },
    TextContains {
        needle: String,
        case_sensitive: bool,
    },
    TextCompare {
        expected: String,
        operation: TextComparison,
        case_sensitive: bool,
    },
    Monitor {
        signal: SignalType,
    },
    Led,
    Gauge {
        signal: ScalarType,
        min: f64,
        max: f64,
    },
    XyPad {
        signal: VectorType,
        min: [f64; 2],
        max: [f64; 2],
    },
    Scope {
        signal: SignalType,
    },
    PathTracer {
        min: [f64; 2],
        max: [f64; 2],
    },
    Sink {
        user: String,
        channel: OutputChannel,
    },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Port {
    pub name: String,
    pub signal: SignalType,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Ports {
    pub inputs: Vec<Port>,
    pub outputs: Vec<Port>,
}
impl NodeKind {
    pub fn category(&self) -> NodeCategory {
        match self {
            Self::Pointer { .. } | Self::Wheel { .. } | Self::Key { .. } | Self::Button { .. } => {
                NodeCategory::Sources
            }
            Self::Constant { .. } => NodeCategory::Constants,
            Self::Translate { .. }
            | Self::ClampPosition { .. }
            | Self::ScaleDelta { .. }
            | Self::AddDelta
            | Self::RotateDelta { .. }
            | Self::DeadZoneDelta { .. }
            | Self::VectorLength { .. }
            | Self::NormalizeVector { .. }
            | Self::Smooth { .. }
            | Self::Velocity => NodeCategory::Vector,
            Self::ScalarMath { .. }
            | Self::UnaryMath { .. }
            | Self::ScaleScalar { .. }
            | Self::ClampScalar { .. }
            | Self::Remap { .. }
            | Self::Accumulate { .. }
            | Self::Counter { .. } => NodeCategory::Scalar,
            Self::Compare { .. }
            | Self::TextContains { .. }
            | Self::TextCompare { .. }
            | Self::Logic { .. }
            | Self::Toggle => NodeCategory::Binary,
            Self::Gate { .. }
            | Self::Switch { .. }
            | Self::Timer { .. }
            | Self::Delay { .. }
            | Self::Throttle { .. } => NodeCategory::Routing,
            Self::PositionToDelta
            | Self::DeltaToPosition { .. }
            | Self::Components { .. }
            | Self::Compose { .. }
            | Self::ImpulseToPulse
            | Self::RisingEdge => NodeCategory::Conversion,
            Self::Monitor { .. }
            | Self::PixelColor { .. }
            | Self::ImageMatch { .. }
            | Self::Ocr { .. }
            | Self::Led
            | Self::Gauge { .. }
            | Self::XyPad { .. }
            | Self::Scope { .. }
            | Self::PathTracer { .. } => NodeCategory::Observers,
            Self::Sink { .. } => NodeCategory::Outputs,
        }
    }

    /// Signal consumed by a latest-value visualizer.
    pub fn latest_observer_signal(&self) -> Option<SignalType> {
        match self {
            Self::Monitor { signal } => Some(*signal),
            Self::Led => Some(SignalType::Hold),
            Self::Gauge { signal, .. } => Some(signal.signal()),
            Self::XyPad { signal, .. } => Some(signal.signal()),
            _ => None,
        }
    }
    /// Signal consumed by a time-series visualizer.
    pub fn trace_observer_signal(&self) -> Option<SignalType> {
        match self {
            Self::Scope { signal } => Some(*signal),
            Self::PathTracer { .. } => Some(SignalType::Position),
            _ => None,
        }
    }
    pub fn source(&self) -> Option<(Source, Disposition)> {
        match self {
            Self::Pointer { user, disposition } => {
                Some((Source::Pointer { user: user.clone() }, *disposition))
            }
            Self::Wheel {
                user,
                horizontal,
                disposition,
            } => Some((
                Source::Wheel {
                    user: user.clone(),
                    horizontal: *horizontal,
                },
                *disposition,
            )),
            Self::Key {
                user,
                key,
                disposition,
            } => Some((
                Source::Key {
                    user: user.clone(),
                    key: *key,
                },
                *disposition,
            )),
            Self::Button {
                user,
                button,
                disposition,
            } => Some((
                Source::Button {
                    user: user.clone(),
                    button: *button,
                },
                *disposition,
            )),
            _ => None,
        }
    }
    pub fn ports(&self) -> Ports {
        use SignalType::{Delta, Hold, Impulse, Position, Pulse, Text};
        let p = |name: &str, signal| Port {
            name: name.into(),
            signal,
        };
        let (inputs, outputs) = match self {
            Self::Pointer { .. } => (vec![], vec![p("position", Position), p("delta", Delta)]),
            Self::Wheel { .. } => (vec![], vec![p("value", Impulse)]),
            Self::Key { .. } | Self::Button { .. } => (vec![], vec![p("value", Hold)]),
            Self::Constant { value } => (vec![], vec![p("value", value.signal())]),
            Self::Translate { .. } | Self::ClampPosition { .. } => {
                (vec![p("value", Position)], vec![p("value", Position)])
            }
            Self::PositionToDelta => (vec![p("value", Position)], vec![p("value", Delta)]),
            Self::DeltaToPosition { .. } => (vec![p("value", Delta)], vec![p("value", Position)]),
            Self::ScaleDelta { .. } | Self::RotateDelta { .. } | Self::DeadZoneDelta { .. } => {
                (vec![p("value", Delta)], vec![p("value", Delta)])
            }
            Self::AddDelta => (vec![p("a", Delta), p("b", Delta)], vec![p("value", Delta)]),
            Self::ScalarMath { signal, .. } => (
                vec![p("a", signal.signal()), p("b", signal.signal())],
                vec![p("value", signal.signal())],
            ),
            Self::UnaryMath { signal, .. } => (
                vec![p("value", signal.signal())],
                vec![p("value", signal.signal())],
            ),
            Self::VectorLength { signal } => (
                vec![p("value", signal.signal())],
                vec![p("value", signal.scalar().signal())],
            ),
            Self::NormalizeVector { signal } => (
                vec![p("value", signal.signal())],
                vec![p("value", signal.signal())],
            ),
            Self::ScaleScalar { signal, .. }
            | Self::ClampScalar { signal, .. }
            | Self::Remap { signal, .. } => (
                vec![p("value", signal.signal())],
                vec![p("value", signal.signal())],
            ),
            Self::Smooth { signal, .. } => (vec![p("value", *signal)], vec![p("value", *signal)]),
            Self::Velocity => (
                vec![p("value", Position)],
                vec![p("value", SignalType::State)],
            ),
            Self::Components { signal } => (
                vec![p("value", signal.signal())],
                vec![
                    p("x", signal.scalar().signal()),
                    p("y", signal.scalar().signal()),
                ],
            ),
            Self::Compose { signal } => (
                vec![
                    p("x", signal.scalar().signal()),
                    p("y", signal.scalar().signal()),
                ],
                vec![p("value", signal.signal())],
            ),
            Self::Compare { signal, .. } => (
                vec![p("a", signal.signal()), p("b", signal.signal())],
                vec![p(
                    "value",
                    if *signal == ScalarType::State {
                        Hold
                    } else {
                        Pulse
                    },
                )],
            ),
            Self::Logic { operation } => (
                if *operation == Logic::Not {
                    vec![p("a", Hold)]
                } else {
                    vec![p("a", Hold), p("b", Hold)]
                },
                vec![p("value", Hold)],
            ),
            Self::Gate { signal } => (
                vec![p("value", *signal), p("open", Hold)],
                vec![p("value", *signal)],
            ),
            Self::ImpulseToPulse => (vec![p("value", Impulse)], vec![p("value", Pulse)]),
            Self::RisingEdge => (vec![p("value", Hold)], vec![p("value", Pulse)]),
            Self::Toggle => (vec![p("value", Pulse)], vec![p("value", Hold)]),
            Self::Accumulate { .. } => (
                vec![p("value", Impulse)],
                vec![p("value", SignalType::State)],
            ),
            Self::Counter { with_reset, .. } => {
                let mut inputs = vec![p("count", Pulse)];
                if *with_reset {
                    inputs.push(p("reset", Pulse));
                }
                (inputs, vec![p("value", SignalType::State)])
            }
            Self::Switch { signal } => (
                vec![p("off", *signal), p("on", *signal), p("select", Hold)],
                vec![p("value", *signal)],
            ),
            Self::Timer { .. } => (vec![p("enabled", Hold)], vec![p("value", Pulse)]),
            Self::Delay { signal, .. } | Self::Throttle { signal, .. } => {
                (vec![p("value", *signal)], vec![p("value", *signal)])
            }
            Self::PixelColor { .. } | Self::ImageMatch { .. } => (
                vec![p("trigger", Pulse)],
                if matches!(self, Self::PixelColor { .. }) {
                    vec![
                        p("red", SignalType::State),
                        p("green", SignalType::State),
                        p("blue", SignalType::State),
                        p("alpha", SignalType::State),
                        p("done", Pulse),
                    ]
                } else {
                    vec![
                        p("matched", Hold),
                        p("score", SignalType::State),
                        p("position", Position),
                        p("done", Pulse),
                    ]
                },
            ),
            Self::Ocr { .. } => (
                vec![p("trigger", Pulse)],
                vec![p("text", Text), p("has_text", Hold), p("done", Pulse)],
            ),
            Self::TextContains { .. } | Self::TextCompare { .. } => {
                (vec![p("value", Text)], vec![p("value", Hold)])
            }
            Self::Monitor { signal } | Self::Scope { signal } => {
                (vec![p("value", *signal)], vec![])
            }
            Self::Led => (vec![p("value", Hold)], vec![]),
            Self::Gauge { signal, .. } => (vec![p("value", signal.signal())], vec![]),
            Self::XyPad { signal, .. } => (vec![p("value", signal.signal())], vec![]),
            Self::PathTracer { .. } => (vec![p("value", Position)], vec![]),
            Self::Sink {
                channel: OutputChannel::TypeText,
                ..
            } => (vec![p("text", Text), p("trigger", Pulse)], vec![]),
            Self::Sink { channel, .. } => (vec![p("value", channel.signal())], vec![]),
        };
        Ports { inputs, outputs }
    }
    fn validate(&self) -> Result<()> {
        if let Some((source, _)) = self.source() {
            return source.validate();
        }
        // Text operations require explicit semantics. Cached text is not a
        // transient event, so a typing sink has a separate live Pulse trigger.
        if matches!(
            self,
            Self::Gate {
                signal: SignalType::Text
            } | Self::Switch {
                signal: SignalType::Text
            } | Self::Delay {
                signal: SignalType::Text,
                ..
            } | Self::Throttle {
                signal: SignalType::Text,
                ..
            } | Self::Scope {
                signal: SignalType::Text
            }
        ) {
            bail!("this mapper operation does not support text");
        }
        match self {
            Self::Constant { value } => {
                value.validate()?;
                ensure!(
                    !value.signal().transient(),
                    "constants must be position, state, hold or text"
                );
            }
            Self::Translate { offset } => vector(*offset)?,
            Self::ClampPosition { min, max } => {
                vector(*min)?;
                vector(*max)?;
                ensure!(
                    min[0] <= max[0] && min[1] <= max[1],
                    "invalid position clamp bounds"
                );
            }
            Self::DeltaToPosition { initial } => vector(*initial)?,
            Self::ScaleDelta { factor } | Self::ScaleScalar { factor, .. } => number(*factor)?,
            Self::RotateDelta { radians } => number(*radians)?,
            Self::DeadZoneDelta { radius } => {
                number(*radius)?;
                ensure!(*radius >= 0.0, "negative dead zone");
            }
            Self::ClampScalar { min, max, .. } => {
                number(*min)?;
                number(*max)?;
                ensure!(min <= max, "invalid scalar clamp bounds");
            }
            Self::Remap {
                input_min,
                input_max,
                output_min,
                output_max,
                ..
            } => {
                for bound in [input_min, input_max, output_min, output_max] {
                    number(*bound)?;
                }
                ensure!(
                    input_min < input_max,
                    "remap input_min must be less than input_max"
                );
            }
            Self::Smooth { signal, alpha } => {
                ensure!(
                    matches!(signal, SignalType::Position | SignalType::State),
                    "Smooth supports only position or state"
                );
                ensure!(
                    alpha.is_finite() && *alpha > 0.0 && *alpha <= 1.0,
                    "Smooth alpha must be in (0, 1]"
                );
            }
            Self::Gauge { min, max, .. } => visual_range(*min, *max)?,
            Self::XyPad { min, max, .. } | Self::PathTracer { min, max } => {
                visual_vector_range(*min, *max)?;
            }
            Self::Accumulate { initial } => number(*initial)?,
            Self::Counter { initial, .. } => number(f64::from(*initial))?,
            Self::Timer { interval_ms, count } => {
                ensure!(
                    (50..=60_000).contains(interval_ms),
                    "Timer interval must be 50–60000 ms"
                );
                ensure!(
                    (1..=1000).contains(count),
                    "Timer count must be 1–1000 pulses"
                );
            }
            Self::Delay { delay_ms, .. } => {
                ensure!((1..=60_000).contains(delay_ms), "Delay must be 1–60000 ms");
            }
            Self::Throttle { interval_ms, .. } => {
                ensure!(
                    (1..=60_000).contains(interval_ms),
                    "Throttle interval must be 1–60000 ms"
                );
            }
            Self::TextContains { needle, .. } => {
                text_value(needle, MAX_TEXT_LITERAL_BYTES)?;
                ensure!(!needle.is_empty(), "text contains needle must be nonempty");
            }
            Self::TextCompare { expected, .. } => text_value(expected, MAX_TEXT_LITERAL_BYTES)?,
            Self::Ocr {
                capture_user,
                region,
            } => {
                id(capture_user)?;
                if let Some(region) = region {
                    ensure!(
                        region.width > 0 && region.height > 0,
                        "OCR region dimensions must be positive"
                    );
                    region
                        .x
                        .checked_add(region.width)
                        .context("OCR region x overflow")?;
                    region
                        .y
                        .checked_add(region.height)
                        .context("OCR region y overflow")?;
                }
            }
            Self::PixelColor { capture_user, .. } => id(capture_user)?,
            Self::ImageMatch {
                capture_user,
                template_png_base64,
                region,
                min_score,
            } => {
                id(capture_user)?;
                ensure!(
                    !template_png_base64.is_empty()
                        && template_png_base64.len() <= 256 * 1024
                        && template_png_base64
                            .bytes()
                            .all(|byte| { byte.is_ascii_alphanumeric() || b"+/=".contains(&byte) }),
                    "ImageMatch template must be nonempty base64 within 256 KiB"
                );
                ensure!(
                    min_score.is_finite() && (0.0..=1.0).contains(min_score),
                    "ImageMatch minimum score must be finite and in [0, 1]"
                );
                if let Some(region) = region {
                    ensure!(
                        region.width > 0 && region.height > 0,
                        "ImageMatch region dimensions must be positive"
                    );
                    region
                        .x
                        .checked_add(region.width)
                        .context("ImageMatch region x overflow")?;
                    region
                        .y
                        .checked_add(region.height)
                        .context("ImageMatch region y overflow")?;
                }
            }
            Self::Sink { user, channel } => {
                id(user)?;
                channel.validate()?;
            }
            _ => {}
        }
        Ok(())
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Node {
    pub id: String,
    pub kind: NodeKind,
    #[serde(default)]
    pub position: Option<[f64; 2]>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PortRef {
    pub node: String,
    pub port: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Edge {
    pub from: PortRef,
    pub to: PortRef,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NodeGroup {
    pub id: String,
    pub label: String,
    pub nodes: Vec<String>,
    #[serde(default)]
    pub collapsed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum GroupEdit {
    Create { group: NodeGroup },
    Rename { group_id: String, label: String },
    SetCollapsed { group_id: String, collapsed: bool },
    Remove { group_id: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Definition {
    pub version: u32,
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub groups: Vec<NodeGroup>,
}
impl Default for Definition {
    fn default() -> Self {
        Self {
            version: 1,
            nodes: vec![],
            edges: vec![],
            groups: vec![],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TemplateKind {
    MirrorPointer,
    KeyRemap,
    TimedKeyPulse,
    SmoothPointer,
    SpeedMonitor,
    PointerTrail,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
pub struct TemplateDescriptor {
    pub template: TemplateKind,
    pub label: &'static str,
    pub description: &'static str,
    pub requires_keyboard: bool,
    pub requires_target: bool,
}

pub fn templates() -> &'static [TemplateDescriptor] {
    &[
        TemplateDescriptor {
            template: TemplateKind::MirrorPointer,
            label: "Mirror pointer",
            description: "Consume relative pointer movement and reverse its horizontal direction.",
            requires_keyboard: false,
            requires_target: true,
        },
        TemplateDescriptor {
            template: TemplateKind::KeyRemap,
            label: "Remap a key",
            description: "Consume F7 and hold F10 until F7 is released.",
            requires_keyboard: true,
            requires_target: true,
        },
        TemplateDescriptor {
            template: TemplateKind::TimedKeyPulse,
            label: "Repeat a key on a timer",
            description: "Consume F7 and tap F10 every 500 ms, up to three times while F7 is held.",
            requires_keyboard: true,
            requires_target: true,
        },
        TemplateDescriptor {
            template: TemplateKind::SmoothPointer,
            label: "Smooth pointer",
            description: "Consume pointer positions, smooth them, and send the result to a virtual user.",
            requires_keyboard: false,
            requires_target: true,
        },
        TemplateDescriptor {
            template: TemplateKind::SpeedMonitor,
            label: "Speed monitor",
            description: "Measure pointer speed up to 2000 units per second without consuming input.",
            requires_keyboard: false,
            requires_target: false,
        },
        TemplateDescriptor {
            template: TemplateKind::PointerTrail,
            label: "Pointer trail",
            description: "Show pointer positions in an XY pad and path tracer without consuming input.",
            requires_keyboard: false,
            requires_target: false,
        },
    ]
}

/// Build and validate a draft bound to stable virtual user IDs.
pub fn instantiate_template(
    template: TemplateKind,
    source_user: &str,
    target_user: Option<&str>,
) -> Result<Definition> {
    id(source_user).context("invalid source user for mapper template")?;
    let requires_target = matches!(
        template,
        TemplateKind::MirrorPointer
            | TemplateKind::KeyRemap
            | TemplateKind::TimedKeyPulse
            | TemplateKind::SmoothPointer
    );
    let target_user = match (requires_target, target_user) {
        (true, Some(user)) => {
            id(user).context("invalid target user for mapper template")?;
            Some(user)
        }
        (true, None) => bail!("mapper template requires a target user"),
        (false, Some(_)) => bail!("passive mapper template does not accept a target user"),
        (false, None) => None,
    };

    const F7: u16 = 0x76;
    const F10: u16 = 0x79;
    let node = |id: &str, kind, position| Node {
        id: id.into(),
        kind,
        position: Some(position),
    };
    let edge = |from: &str, output: &str, to: &str, input: &str| Edge {
        from: PortRef {
            node: from.into(),
            port: output.into(),
        },
        to: PortRef {
            node: to.into(),
            port: input.into(),
        },
    };
    let source_key = || NodeKind::Key {
        user: source_user.into(),
        key: F7,
        disposition: Disposition::Consume,
    };
    let sink = |channel| NodeKind::Sink {
        user: target_user
            .expect("target validated for live-output template")
            .into(),
        channel,
    };

    let (nodes, edges) = match template {
        TemplateKind::MirrorPointer => (
            vec![
                node(
                    "source",
                    NodeKind::Pointer {
                        user: source_user.into(),
                        disposition: Disposition::Consume,
                    },
                    [0.05, 0.25],
                ),
                node(
                    "split",
                    NodeKind::Components {
                        signal: VectorType::Delta,
                    },
                    [0.5, 0.25],
                ),
                node(
                    "flip-x",
                    NodeKind::ScaleScalar {
                        signal: ScalarType::Impulse,
                        factor: -1.0,
                    },
                    [0.5, 0.75],
                ),
                node(
                    "combine",
                    NodeKind::Compose {
                        signal: VectorType::Delta,
                    },
                    [0.95, 0.25],
                ),
                node("output", sink(OutputChannel::Delta), [0.95, 0.75]),
            ],
            vec![
                edge("source", "delta", "split", "value"),
                edge("split", "x", "flip-x", "value"),
                edge("split", "y", "combine", "y"),
                edge("flip-x", "value", "combine", "x"),
                edge("combine", "value", "output", "value"),
            ],
        ),
        TemplateKind::KeyRemap => (
            vec![
                node("source", source_key(), [0.2, 0.5]),
                node(
                    "output",
                    sink(OutputChannel::KeyHold { key: F10 }),
                    [0.8, 0.5],
                ),
            ],
            vec![edge("source", "value", "output", "value")],
        ),
        TemplateKind::SmoothPointer => (
            vec![
                node(
                    "source",
                    NodeKind::Pointer {
                        user: source_user.into(),
                        disposition: Disposition::Consume,
                    },
                    [0.05, 0.5],
                ),
                node(
                    "smooth",
                    NodeKind::Smooth {
                        signal: SignalType::Position,
                        alpha: 0.5,
                    },
                    [0.5, 0.5],
                ),
                node("output", sink(OutputChannel::Position), [0.95, 0.5]),
            ],
            vec![
                edge("source", "position", "smooth", "value"),
                edge("smooth", "value", "output", "value"),
            ],
        ),
        TemplateKind::SpeedMonitor => (
            vec![
                node(
                    "source",
                    NodeKind::Pointer {
                        user: source_user.into(),
                        disposition: Disposition::Passthrough,
                    },
                    [0.05, 0.5],
                ),
                node("velocity", NodeKind::Velocity, [0.5, 0.5]),
                node(
                    "gauge",
                    NodeKind::Gauge {
                        signal: ScalarType::State,
                        min: 0.0,
                        max: 2000.0,
                    },
                    [0.95, 0.5],
                ),
            ],
            vec![
                edge("source", "position", "velocity", "value"),
                edge("velocity", "value", "gauge", "value"),
            ],
        ),
        TemplateKind::PointerTrail => (
            vec![
                node(
                    "source",
                    NodeKind::Pointer {
                        user: source_user.into(),
                        disposition: Disposition::Passthrough,
                    },
                    [0.05, 0.5],
                ),
                node(
                    "xy-pad",
                    NodeKind::XyPad {
                        signal: VectorType::Position,
                        min: [0.0, 0.0],
                        max: [1920.0, 1080.0],
                    },
                    [0.95, 0.25],
                ),
                node(
                    "path-tracer",
                    NodeKind::PathTracer {
                        min: [0.0, 0.0],
                        max: [1920.0, 1080.0],
                    },
                    [0.95, 0.75],
                ),
            ],
            vec![
                edge("source", "position", "xy-pad", "value"),
                edge("source", "position", "path-tracer", "value"),
            ],
        ),
        TemplateKind::TimedKeyPulse => (
            vec![
                node("source", source_key(), [0.05, 0.5]),
                node(
                    "timer",
                    NodeKind::Timer {
                        interval_ms: DEFAULT_TIMER_INTERVAL_MS,
                        count: DEFAULT_TIMER_COUNT,
                    },
                    [0.5, 0.5],
                ),
                node(
                    "output",
                    sink(OutputChannel::KeyPulse { key: F10 }),
                    [0.95, 0.5],
                ),
            ],
            vec![
                edge("source", "value", "timer", "enabled"),
                edge("timer", "value", "output", "value"),
            ],
        ),
    };
    let draft = Definition {
        nodes,
        edges,
        ..Definition::default()
    };
    draft.validate()?;
    Ok(draft)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct NodeDescriptor {
    pub name: String,
    pub label: String,
    pub description: String,
    pub category: NodeCategory,
    pub prototype: NodeKind,
    pub ports: Ports,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ObservationKind {
    PixelColor,
    ImageMatch,
    Ocr,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ObservationDescriptor {
    pub node_index: usize,
    pub node_id: String,
    pub capture_user: String,
    pub kind: ObservationKind,
    pub source_users: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PendingObservation {
    pub id: u64,
    pub node_index: usize,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ObservationMatch {
    pub desktop_center: [f64; 2],
    pub score: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ObservationResult {
    PixelColor { rgba: [u8; 4] },
    ImageMatch { matched: Option<ObservationMatch> },
    Ocr { text: String },
}

/// Complete parameter-bearing prototypes; editors can inspect/edit the real
/// serialized fields without inventing a second, reduced node model.
pub fn catalogue() -> Vec<NodeDescriptor> {
    use NodeKind::*;
    let user = "user-id".to_string();
    let prototypes = vec![
        (
            "pointer",
            "Pointer",
            "Virtual pointer position and relative delta",
            Pointer {
                user: user.clone(),
                disposition: Disposition::Passthrough,
            },
        ),
        (
            "wheel",
            "Wheel",
            "One-wave vertical or horizontal wheel impulse",
            Wheel {
                user: user.clone(),
                horizontal: false,
                disposition: Disposition::Passthrough,
            },
        ),
        (
            "key",
            "Key",
            "Virtual key hold",
            Key {
                user: user.clone(),
                key: 65,
                disposition: Disposition::Passthrough,
            },
        ),
        (
            "button",
            "Mouse button",
            "Virtual mouse-button hold",
            Button {
                user: user.clone(),
                button: 1,
                disposition: Disposition::Passthrough,
            },
        ),
        (
            "constant",
            "Constant",
            "Persistent position, scalar state or binary hold",
            Constant {
                value: Value::State(1.0),
            },
        ),
        (
            "translate",
            "Translate position",
            "Add a fixed position offset",
            Translate { offset: [0.0, 0.0] },
        ),
        (
            "clamp_position",
            "Clamp position",
            "Bound absolute position",
            ClampPosition {
                min: [0.0, 0.0],
                max: [1920.0, 1080.0],
            },
        ),
        (
            "position_to_delta",
            "Position to delta",
            "Difference from the previous position; first sample produces no movement",
            PositionToDelta,
        ),
        (
            "delta_to_position",
            "Delta to position",
            "Accumulate relative movement from an explicit initial position",
            DeltaToPosition {
                initial: [0.0, 0.0],
            },
        ),
        (
            "scale_delta",
            "Scale delta",
            "Multiply relative movement",
            ScaleDelta { factor: 1.0 },
        ),
        (
            "add_delta",
            "Add deltas",
            "Add only deltas present in the current wave",
            AddDelta,
        ),
        (
            "rotate_delta",
            "Rotate delta",
            "Rotate relative movement by radians",
            RotateDelta { radians: 0.0 },
        ),
        (
            "dead_zone_delta",
            "Delta dead zone",
            "Suppress relative vectors within the given radius",
            DeadZoneDelta { radius: 0.0 },
        ),
        (
            "scalar_math",
            "Scalar arithmetic",
            "Arithmetic with exact state or impulse units",
            ScalarMath {
                signal: ScalarType::State,
                operation: Arithmetic::Add,
            },
        ),
        (
            "unary_math",
            "Scalar function",
            "Apply a unary numeric function to state or impulse values",
            UnaryMath {
                signal: ScalarType::State,
                operation: UnaryOperation::Abs,
            },
        ),
        (
            "vector_length",
            "Vector magnitude",
            "Measure position as state or delta as impulse",
            VectorLength {
                signal: VectorType::Delta,
            },
        ),
        (
            "normalize_vector",
            "Normalize vector",
            "Scale a nonzero vector to unit length while preserving its signal type",
            NormalizeVector {
                signal: VectorType::Delta,
            },
        ),
        (
            "scale_scalar",
            "Scale scalar",
            "Multiply a state or impulse by a fixed factor",
            ScaleScalar {
                signal: ScalarType::Impulse,
                factor: 1.0,
            },
        ),
        (
            "clamp_scalar",
            "Clamp scalar",
            "Bound state or impulse values",
            ClampScalar {
                signal: ScalarType::State,
                min: -1.0,
                max: 1.0,
            },
        ),
        (
            "remap",
            "Remap",
            "Map a scalar input range to an output range",
            Remap {
                signal: ScalarType::State,
                input_min: 0.0,
                input_max: 1.0,
                output_min: 0.0,
                output_max: 100.0,
                clamp: true,
            },
        ),
        (
            "smooth",
            "Smooth",
            "Apply an exponential moving average",
            Smooth {
                signal: SignalType::Position,
                alpha: 0.5,
            },
        ),
        (
            "velocity",
            "Velocity",
            "Measure position speed in desktop pixels per second",
            Velocity,
        ),
        (
            "components",
            "Vector components",
            "Position becomes state components; delta becomes impulses",
            Components {
                signal: VectorType::Delta,
            },
        ),
        (
            "compose",
            "Compose vector",
            "Combine components with matching units",
            Compose {
                signal: VectorType::Delta,
            },
        ),
        (
            "compare",
            "Compare",
            "State comparison yields a hold; impulse comparison yields a pulse",
            Compare {
                signal: ScalarType::State,
                operation: Comparison::Greater,
            },
        ),
        (
            "logic",
            "Binary logic",
            "And, Or or Not on holds",
            Logic {
                operation: super::mapper::Logic::And,
            },
        ),
        (
            "gate",
            "Typed gate",
            "Closing a hold gate releases; opening never replays old impulses",
            Gate {
                signal: SignalType::Delta,
            },
        ),
        (
            "impulse_to_pulse",
            "Impulse to pulse",
            "Nonzero impulse produces one binary pulse",
            ImpulseToPulse,
        ),
        (
            "rising_edge",
            "Rising edge",
            "False-to-true hold transition produces one pulse",
            RisingEdge,
        ),
        (
            "toggle",
            "Toggle",
            "Each pulse toggles a persistent hold",
            Toggle,
        ),
        (
            "accumulate",
            "Accumulate scalar",
            "Accumulate current-wave impulses into state",
            Accumulate { initial: 0.0 },
        ),
        (
            "counter",
            "Counter",
            "Count pulses as scalar state. An enabled reset port takes priority over count.",
            Counter {
                initial: 0,
                with_reset: false,
            },
        ),
        (
            "switch",
            "Typed switch",
            "A hold selects one matching input. Position and scalar state branches need an initial value.",
            Switch {
                signal: SignalType::Hold,
            },
        ),
        (
            "timer",
            "Timer",
            "Generate a finite series of pulses while enabled by live virtual input.",
            Timer {
                interval_ms: DEFAULT_TIMER_INTERVAL_MS,
                count: DEFAULT_TIMER_COUNT,
            },
        ),
        (
            "delay",
            "Delay",
            "Queue a typed value for one bounded interval",
            Delay {
                signal: SignalType::State,
                delay_ms: 250,
            },
        ),
        (
            "throttle",
            "Throttle",
            "Pass the first typed value in each interval and drop later values",
            Throttle {
                signal: SignalType::Impulse,
                interval_ms: 100,
            },
        ),
        (
            "pixel_color",
            "Pixel color",
            "Read one pixel from the selected screen after a live trigger",
            PixelColor {
                capture_user: user.clone(),
                x: 0,
                y: 0,
            },
        ),
        (
            "image_match",
            "Image match",
            "Find a bounded PNG template after a live trigger",
            ImageMatch {
                capture_user: user.clone(),
                template_png_base64:
                    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4////fwAJ+wP9KobjigAAAABJRU5ErkJggg=="
                        .into(),
                region: None,
                min_score: 0.95,
            },
        ),
        (
            "ocr",
            "Extract text (Latin)",
            "Read bounded text from the selected screen after a live trigger",
            Ocr { capture_user: user.clone(), region: None },
        ),
        (
            "text_contains",
            "Text contains",
            "Find a literal substring; insensitive mode folds only ASCII A-Z",
            TextContains { needle: "Ready".into(), case_sensitive: true },
        ),
        (
            "text_compare",
            "Compare text",
            "Compare literal text; insensitive mode folds only ASCII A-Z",
            TextCompare { expected: String::new(), operation: TextComparison::Equals, case_sensitive: true },
        ),
        (
            "monitor",
            "Monitor",
            "Observe the latest value and received sample count without sending input.",
            Monitor {
                signal: SignalType::Hold,
            },
        ),
        (
            "led",
            "LED",
            "Show the latest held state without sending input",
            Led,
        ),
        (
            "gauge",
            "Gauge",
            "Show the latest scalar value within a finite display range",
            Gauge {
                signal: ScalarType::State,
                min: 0.0,
                max: 100.0,
            },
        ),
        (
            "xy_pad",
            "XY pad",
            "Show the latest two-axis value within finite display bounds",
            XyPad {
                signal: VectorType::Position,
                min: [0.0, 0.0],
                max: [1920.0, 1080.0],
            },
        ),
        (
            "scope",
            "Scope",
            "Record the latest 64 typed samples with elapsed microsecond timestamps.",
            Scope {
                signal: SignalType::State,
            },
        ),
        (
            "path_tracer",
            "Path tracer",
            "Record the latest 64 position samples with elapsed microsecond timestamps",
            PathTracer {
                min: [0.0, 0.0],
                max: [1920.0, 1080.0],
            },
        ),
        (
            "type_text",
            "Type text",
            "Type the latest text into the selected user only on a true trigger pulse",
            Sink {
                user: user.clone(),
                channel: OutputChannel::TypeText,
            },
        ),
        (
            "sink",
            "User output",
            "Explicit selected virtual user and semantic output channel",
            Sink {
                user,
                channel: OutputChannel::Delta,
            },
        ),
    ];
    prototypes
        .into_iter()
        .map(|(name, label, description, prototype)| NodeDescriptor {
            name: name.into(),
            label: label.into(),
            description: description.into(),
            category: prototype.category(),
            ports: prototype.ports(),
            prototype,
        })
        .collect()
}

/// Browse the full prototype catalogue without changing graph state.
/// Query length is bounded by UTF-8 byte count; matching is Unicode lowercase
/// substring matching over stable name, node label, description and category.
pub fn browse_catalogue(
    query: &str,
    category: Option<NodeCategory>,
) -> Result<Vec<NodeDescriptor>> {
    ensure!(
        query.len() <= 128,
        "catalogue query exceeds 128 UTF-8 bytes"
    );
    ensure!(
        !query.chars().any(char::is_control),
        "catalogue query cannot contain control characters"
    );
    let query = query.trim().to_lowercase();
    Ok(catalogue()
        .into_iter()
        .filter(|descriptor| category.is_none_or(|wanted| descriptor.category == wanted))
        .filter(|descriptor| {
            query.is_empty()
                || [
                    descriptor.name.as_str(),
                    descriptor.label.as_str(),
                    descriptor.description.as_str(),
                    descriptor.category.label(),
                ]
                .into_iter()
                .any(|field| field.to_lowercase().contains(&query))
        })
        .collect())
}

fn number(v: f64) -> Result<()> {
    ensure!(
        v.is_finite() && v.abs() <= NUMERIC_LIMIT,
        "numeric value outside finite mapper domain"
    );
    Ok(())
}
fn visual_range(min: f64, max: f64) -> Result<()> {
    number(min)?;
    number(max)?;
    ensure!(min < max, "visualizer range must have min < max");
    Ok(())
}
fn visual_vector_range(min: [f64; 2], max: [f64; 2]) -> Result<()> {
    for axis in 0..2 {
        visual_range(min[axis], max[axis])?;
    }
    Ok(())
}
fn vector(v: [f64; 2]) -> Result<()> {
    number(v[0])?;
    number(v[1])
}
fn text_value(value: &str, max_bytes: usize) -> Result<()> {
    ensure!(
        value.len() <= max_bytes,
        "mapper text exceeds its UTF-8 byte limit"
    );
    ensure!(
        value
            .chars()
            .all(|ch| !ch.is_control() || matches!(ch, '\n' | '\r' | '\t')),
        "mapper text contains unsupported control characters"
    );
    Ok(())
}

/// Validate a typing payload without allocating or contacting an input backend.
/// Generic Text remains broader: native text delivery permits LF and tab, but CR
/// and other control characters cannot be sent. No normalization or truncation.
pub fn type_text_cost(text: &str) -> Result<usize> {
    text_value(text, MAX_TEXT_BYTES)?;
    ensure!(
        text.chars()
            .all(|ch| !ch.is_control() || matches!(ch, '\n' | '\t')),
        "mapper typing text contains an unsupported control character"
    );
    let units = text.encode_utf16().count();
    ensure!(
        units <= MAX_TYPE_TEXT_UTF16_UNITS,
        "mapper typing text exceeds its UTF-16 unit limit"
    );
    Ok(units)
}

fn id(v: &str) -> Result<()> {
    ensure!(
        !v.is_empty()
            && v.len() <= 128
            && v.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b)),
        "invalid stable mapper ID"
    );
    Ok(())
}
fn key_code(v: u16) -> Result<()> {
    ensure!((1..=255).contains(&v), "invalid virtual key");
    Ok(())
}
fn button_code(v: u8) -> Result<()> {
    ensure!((1..=5).contains(&v), "invalid mouse button");
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SinkDescriptor {
    pub sink: String,
    pub user: String,
    pub channel: OutputChannel,
    pub source_users: BTreeSet<String>,
}
#[derive(Debug, Clone)]
pub struct Compiled {
    definition: Definition,
    order: Vec<usize>,
    inputs: Vec<BTreeMap<String, (usize, String)>>,
    sources: BTreeMap<Source, (usize, Disposition)>,
    ancestry: Vec<BTreeSet<Source>>,
    timer_sinks: BTreeMap<usize, Vec<usize>>,
    delay_sinks: BTreeMap<usize, Vec<usize>>,
    observation_sinks: BTreeMap<usize, Vec<usize>>,
    seed: Seed,
}
impl Definition {
    /// Return a copy with validated group metadata, including for incomplete editor drafts.
    pub fn edit_group(&self, edit: GroupEdit) -> Result<Self> {
        let mut next = self.clone();
        match edit {
            GroupEdit::Create { group } => {
                ensure!(
                    !next.groups.iter().any(|g| g.id == group.id),
                    "duplicate mapper group ID"
                );
                next.groups.push(group);
            }
            GroupEdit::Rename { group_id, label } => {
                let group = next
                    .groups
                    .iter_mut()
                    .find(|g| g.id == group_id)
                    .context("unknown mapper group")?;
                group.label = label;
            }
            GroupEdit::SetCollapsed {
                group_id,
                collapsed,
            } => {
                let group = next
                    .groups
                    .iter_mut()
                    .find(|g| g.id == group_id)
                    .context("unknown mapper group")?;
                group.collapsed = collapsed;
            }
            GroupEdit::Remove { group_id } => {
                let index = next
                    .groups
                    .iter()
                    .position(|g| g.id == group_id)
                    .context("unknown mapper group")?;
                next.groups.remove(index);
            }
        }
        ensure!(
            serde_json::to_vec(&next)?.len() <= MAX_JSON_BYTES,
            "mapper JSON exceeds 1 MiB"
        );
        next.validate_group_metadata()?;
        Ok(next)
    }
    fn validate_group_metadata(&self) -> Result<()> {
        let node_ids: BTreeSet<_> = self.nodes.iter().map(|node| node.id.as_str()).collect();
        ensure!(self.groups.len() <= MAX_GROUPS, "mapper exceeds 64 groups");
        let mut group_ids = BTreeSet::new();
        let mut grouped_nodes = BTreeSet::new();
        for group in &self.groups {
            id(&group.id)?;
            ensure!(
                group_ids.insert(group.id.as_str()),
                "duplicate mapper group ID"
            );
            ensure!(
                !group.label.trim().is_empty()
                    && group.label.len() <= 128
                    && !group.label.chars().any(char::is_control),
                "invalid mapper group label"
            );
            ensure!(
                !group.nodes.is_empty() && group.nodes.len() <= MAX_NODES,
                "mapper groups must contain 1–256 nodes"
            );
            let mut members = BTreeSet::new();
            for member in &group.nodes {
                ensure!(
                    node_ids.contains(member.as_str()),
                    "unknown mapper group node"
                );
                ensure!(
                    members.insert(member.as_str()),
                    "duplicate node in mapper group"
                );
                ensure!(
                    grouped_nodes.insert(member.as_str()),
                    "mapper node belongs to multiple groups"
                );
            }
        }
        Ok(())
    }

    pub fn validate(&self) -> Result<()> {
        self.compile().map(|_| ())
    }
    pub fn from_json(json: &str) -> Result<Self> {
        ensure!(json.len() <= MAX_JSON_BYTES, "mapper JSON exceeds 1 MiB");
        let definition: Self = serde_json::from_str(json).context("invalid mapper JSON")?;
        definition.validate()?;
        Ok(definition)
    }
    pub fn to_json(&self) -> Result<String> {
        self.validate()?;
        let json = serde_json::to_string_pretty(self)?;
        ensure!(json.len() <= MAX_JSON_BYTES, "mapper JSON exceeds 1 MiB");
        Ok(json)
    }
    pub fn compile(&self) -> Result<Compiled> {
        ensure!(self.version == 1, "unsupported mapper version");
        ensure!(self.nodes.len() <= MAX_NODES, "mapper exceeds 256 nodes");
        ensure!(self.edges.len() <= MAX_EDGES, "mapper exceeds 1024 edges");
        ensure!(
            serde_json::to_vec(self)?.len() <= MAX_JSON_BYTES,
            "mapper JSON exceeds 1 MiB"
        );
        let mut indices = BTreeMap::new();
        let mut sources = BTreeMap::new();
        let mut ports = Vec::with_capacity(self.nodes.len());
        for (index, node) in self.nodes.iter().enumerate() {
            id(&node.id)?;
            ensure!(
                indices.insert(node.id.as_str(), index).is_none(),
                "duplicate mapper node ID"
            );
            node.kind
                .validate()
                .with_context(|| format!("invalid mapper node {}", node.id))?;
            if let Some(position) = node.position {
                vector(position)?;
            }
            if let Some((source, disposition)) = node.kind.source() {
                ensure!(
                    sources.insert(source, (index, disposition)).is_none(),
                    "duplicate mapper source subscription"
                );
            }
            ports.push(node.kind.ports());
        }
        self.validate_group_metadata()?;
        let mut inputs = vec![BTreeMap::new(); self.nodes.len()];
        let mut next = vec![BTreeSet::new(); self.nodes.len()];
        let mut indegree = vec![0; self.nodes.len()];
        for edge in &self.edges {
            let from = *indices
                .get(edge.from.node.as_str())
                .context("unknown mapper source node")?;
            let to = *indices
                .get(edge.to.node.as_str())
                .context("unknown mapper destination node")?;
            let output = ports[from]
                .outputs
                .iter()
                .find(|p| p.name == edge.from.port)
                .context("unknown mapper output port")?;
            let input = ports[to]
                .inputs
                .iter()
                .find(|p| p.name == edge.to.port)
                .context("unknown mapper input port")?;
            ensure!(
                output.signal == input.signal,
                "incompatible semantic mapper port types"
            );
            ensure!(
                inputs[to]
                    .insert(edge.to.port.clone(), (from, edge.from.port.clone()))
                    .is_none(),
                "multiple producers for mapper input"
            );
            if next[from].insert(to) {
                indegree[to] += 1;
            }
        }
        for (index, node_ports) in ports.iter().enumerate() {
            ensure!(
                inputs[index].len() == node_ports.inputs.len(),
                "missing required input on mapper node {}",
                self.nodes[index].id
            );
        }
        let mut ready: BTreeSet<_> = self
            .nodes
            .iter()
            .enumerate()
            .filter(|(i, _)| indegree[*i] == 0)
            .map(|(i, node)| (node.id.clone(), i))
            .collect();
        let mut order = Vec::with_capacity(self.nodes.len());
        while let Some((_, index)) = ready.pop_first() {
            order.push(index);
            for &target in &next[index] {
                indegree[target] -= 1;
                if indegree[target] == 0 {
                    ready.insert((self.nodes[target].id.clone(), target));
                }
            }
        }
        ensure!(
            order.len() == self.nodes.len(),
            "mapper graph contains a cycle"
        );
        let mut ancestry = vec![BTreeSet::<Source>::new(); self.nodes.len()];
        let mut timer_lineage = vec![BTreeSet::<usize>::new(); self.nodes.len()];
        let mut delay_lineage = vec![BTreeSet::<usize>::new(); self.nodes.len()];
        let mut observation_lineage = vec![BTreeSet::<usize>::new(); self.nodes.len()];
        let mut timer_sinks = BTreeMap::<usize, Vec<usize>>::new();
        let mut delay_sinks = BTreeMap::<usize, Vec<usize>>::new();
        let mut observation_sinks = BTreeMap::<usize, Vec<usize>>::new();
        let mut observation_count = 0;
        let mut monitors = 0;
        for &index in &order {
            if let Some((source, _)) = self.nodes[index].kind.source() {
                ancestry[index].insert(source);
            }
            let inherited: Vec<_> = inputs[index]
                .values()
                .flat_map(|(parent, _)| ancestry[*parent].iter().cloned())
                .collect();
            ancestry[index].extend(inherited);
            let inherited_timers: Vec<_> = inputs[index]
                .values()
                .flat_map(|(parent, _)| timer_lineage[*parent].iter().copied())
                .collect();
            timer_lineage[index].extend(inherited_timers);
            let inherited_delays: Vec<_> = inputs[index]
                .values()
                .flat_map(|(parent, _)| delay_lineage[*parent].iter().copied())
                .collect();
            delay_lineage[index].extend(inherited_delays);
            let inherited_observations: Vec<_> = inputs[index]
                .values()
                .flat_map(|(parent, _)| observation_lineage[*parent].iter().copied())
                .collect();
            observation_lineage[index].extend(inherited_observations);
            if matches!(self.nodes[index].kind, NodeKind::Timer { .. }) {
                ensure!(timer_sinks.len() < MAX_TIMERS, "mapper exceeds 16 timers");
                ensure!(
                    !ancestry[index].is_empty(),
                    "Timer must depend on a virtual input source"
                );
                ensure!(
                    timer_lineage[index].is_empty(),
                    "Timer chains are unavailable"
                );
                timer_sinks.insert(index, Vec::new());
                timer_lineage[index].insert(index);
            }
            if matches!(self.nodes[index].kind, NodeKind::Delay { .. }) {
                ensure!(
                    delay_sinks.len() < MAX_DELAYS,
                    "mapper exceeds 16 Delay nodes"
                );
                ensure!(
                    !ancestry[index].is_empty(),
                    "Delay must depend on a virtual input source"
                );
                delay_sinks.insert(index, Vec::new());
                delay_lineage[index].insert(index);
            }
            if matches!(
                self.nodes[index].kind,
                NodeKind::PixelColor { .. } | NodeKind::ImageMatch { .. } | NodeKind::Ocr { .. }
            ) {
                observation_count += 1;
                ensure!(
                    observation_count <= MAX_OBSERVATIONS,
                    "mapper exceeds 8 screen observations"
                );
                ensure!(
                    !ancestry[index].is_empty(),
                    "screen observation must depend on a virtual input source"
                );
                observation_sinks.insert(index, Vec::new());
                observation_lineage[index].insert(index);
            }
            let kind = &self.nodes[index].kind;
            if kind.latest_observer_signal().is_some() || kind.trace_observer_signal().is_some() {
                monitors += 1;
                ensure!(
                    monitors <= MAX_MONITORS,
                    "mapper exceeds 16 monitors and scopes"
                );
                ensure!(
                    !ancestry[index].is_empty(),
                    "visualizer must depend on a virtual input source"
                );
            }
            if let NodeKind::Sink { user, channel } = &self.nodes[index].kind {
                ensure!(
                    !ancestry[index].is_empty(),
                    "mapper sink must depend on an input source"
                );
                for source in &ancestry[index] {
                    let (_, disposition) = sources.get(source).context("missing mapper source")?;
                    ensure!(
                        *disposition == Disposition::Consume || !channel.duplicates(user, source),
                        "passthrough mapper source also targets its original input channel"
                    );
                }
                for timer in &timer_lineage[index] {
                    timer_sinks
                        .get_mut(timer)
                        .expect("compiled timer")
                        .push(index);
                }
                for delay in &delay_lineage[index] {
                    delay_sinks
                        .get_mut(delay)
                        .expect("compiled Delay")
                        .push(index);
                }
                for observation in &observation_lineage[index] {
                    observation_sinks
                        .get_mut(observation)
                        .expect("compiled screen observation")
                        .push(index);
                }
            }
        }
        for source in sources.keys() {
            let cost: usize = self
                .nodes
                .iter()
                .enumerate()
                .filter_map(|(i, node)| {
                    if ancestry[i].contains(source)
                        && let NodeKind::Sink { channel, .. } = &node.kind
                    {
                        return Some(channel.effect_cost());
                    }
                    None
                })
                .sum();
            ensure!(
                cost <= MAX_EFFECTS,
                "mapper source can exceed 128 weighted sink effects"
            );
        }
        let seed = initial_seed(self, &order, &inputs)?;
        Ok(Compiled {
            definition: self.clone(),
            order,
            inputs,
            sources,
            ancestry,
            timer_sinks,
            delay_sinks,
            observation_sinks,
            seed,
        })
    }
}
impl Compiled {
    pub fn definition(&self) -> &Definition {
        &self.definition
    }
    pub fn sources(&self) -> Vec<Source> {
        self.sources.keys().cloned().collect()
    }
    pub fn source_disposition(&self, source: &Source) -> Option<Disposition> {
        self.sources.get(source).map(|(_, d)| *d)
    }
    pub fn affected_outputs(&self, source: &Source) -> Vec<SinkDescriptor> {
        self.order
            .iter()
            .filter(|&&i| self.ancestry[i].contains(source))
            .filter_map(|&i| self.sink(i))
            .collect()
    }
    pub fn timers(&self) -> impl Iterator<Item = (usize, u32, u32)> + '_ {
        self.timer_sinks.keys().map(|&index| {
            let NodeKind::Timer { interval_ms, count } = self.definition.nodes[index].kind else {
                unreachable!("compiled timer index")
            };
            (index, interval_ms, count)
        })
    }
    pub fn timer_outputs(&self, index: usize) -> Vec<SinkDescriptor> {
        self.timer_sinks
            .get(&index)
            .into_iter()
            .flatten()
            .filter_map(|&sink| self.sink(sink))
            .collect()
    }
    pub fn delays(&self) -> impl Iterator<Item = (usize, u32)> + '_ {
        self.delay_sinks.keys().map(|&index| {
            let NodeKind::Delay { delay_ms, .. } = self.definition.nodes[index].kind else {
                unreachable!("compiled Delay index")
            };
            (index, delay_ms)
        })
    }
    pub fn delay_outputs(&self, index: usize) -> Vec<SinkDescriptor> {
        self.delay_sinks
            .get(&index)
            .into_iter()
            .flatten()
            .filter_map(|&sink| self.sink(sink))
            .collect()
    }
    pub fn observations(&self) -> Vec<ObservationDescriptor> {
        self.order
            .iter()
            .filter_map(|&node_index| {
                let node = &self.definition.nodes[node_index];
                let (capture_user, kind) = match &node.kind {
                    NodeKind::PixelColor { capture_user, .. } => {
                        (capture_user.clone(), ObservationKind::PixelColor)
                    }
                    NodeKind::ImageMatch { capture_user, .. } => {
                        (capture_user.clone(), ObservationKind::ImageMatch)
                    }
                    NodeKind::Ocr { capture_user, .. } => {
                        (capture_user.clone(), ObservationKind::Ocr)
                    }
                    _ => return None,
                };
                let source_users = self.ancestry[node_index]
                    .iter()
                    .map(|source| source.user().to_string())
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .collect();
                Some(ObservationDescriptor {
                    node_index,
                    node_id: node.id.clone(),
                    capture_user,
                    kind,
                    source_users,
                })
            })
            .collect()
    }
    pub fn observation_outputs(&self, index: usize) -> Vec<SinkDescriptor> {
        self.observation_sinks
            .get(&index)
            .into_iter()
            .flatten()
            .filter_map(|&sink| self.sink(sink))
            .collect()
    }
    fn sink(&self, index: usize) -> Option<SinkDescriptor> {
        if let NodeKind::Sink { user, channel } = &self.definition.nodes[index].kind {
            Some(SinkDescriptor {
                sink: self.definition.nodes[index].id.clone(),
                user: user.clone(),
                channel: channel.clone(),
                source_users: self.ancestry[index]
                    .iter()
                    .map(|s| s.user().to_string())
                    .collect(),
            })
        } else {
            None
        }
    }
}

/// An admitted original virtual input, supplied by the runtime. Physical input,
/// derived sink effects and platform packet encodings never enter implicitly.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum RuntimeInput {
    Pointer {
        user: String,
        position: [f64; 2],
        delta: [f64; 2],
    },
    Wheel {
        user: String,
        horizontal: bool,
        amount: f64,
    },
    Key {
        user: String,
        key: u16,
        held: bool,
    },
    Button {
        user: String,
        button: u8,
        held: bool,
    },
}
impl RuntimeInput {
    pub fn source(&self) -> Source {
        match self {
            Self::Pointer { user, .. } => Source::Pointer { user: user.clone() },
            Self::Wheel {
                user, horizontal, ..
            } => Source::Wheel {
                user: user.clone(),
                horizontal: *horizontal,
            },
            Self::Key { user, key, .. } => Source::Key {
                user: user.clone(),
                key: *key,
            },
            Self::Button { user, button, .. } => Source::Button {
                user: user.clone(),
                button: *button,
            },
        }
    }
    fn validate(&self) -> Result<()> {
        self.source().validate()?;
        match self {
            Self::Pointer {
                position, delta, ..
            } => {
                vector(*position)?;
                vector(*delta)
            }
            Self::Wheel { amount, .. } => number(*amount),
            _ => Ok(()),
        }
    }
    fn values(&self) -> Vec<(&'static str, Value)> {
        match self {
            Self::Pointer {
                position, delta, ..
            } => vec![
                ("position", Value::Position(*position)),
                ("delta", Value::Delta(*delta)),
            ],
            Self::Wheel { amount, .. } => vec![("value", Value::Impulse(*amount))],
            Self::Key { held, .. } | Self::Button { held, .. } => {
                vec![("value", Value::Hold(*held))]
            }
        }
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Emission {
    pub sink: String,
    pub user: String,
    pub channel: OutputChannel,
    pub value: Value,
    pub source_users: BTreeSet<String>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MonitorSample {
    pub node: String,
    pub value: Value,
    pub samples: u64,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScopeSample {
    pub elapsed_us: u64,
    pub value: Value,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScopeTrace {
    pub node: String,
    pub samples: Vec<ScopeSample>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PendingDelay {
    pub id: u64,
    pub node_index: usize,
    pub due_elapsed_us: Option<u64>,
}
#[derive(Debug, Clone)]
struct DelayJob {
    id: u64,
    node_index: usize,
    value: Value,
    delay_ms: u32,
    due_elapsed_us: Option<u64>,
}
#[derive(Debug, Clone)]
struct ObservationJob {
    id: u64,
    node_index: usize,
}
#[derive(Debug, Clone, Default)]
struct Memory {
    position: Option<[f64; 2]>,
    velocity_sample: Option<([f64; 2], u64)>,
    smooth: Option<Value>,
    scalar: f64,
    held: bool,
    throttle_at_us: Option<u64>,
}
#[derive(Debug, Clone)]
struct Seed {
    outputs: Vec<BTreeMap<String, Value>>,
    memory: Vec<Memory>,
}
/// Compute an effect-free persistent closure. This is not an input wave:
/// sources are not dirty, impulse/pulse operations do not run, and edge/baseline
/// converters do not observe invented samples. Static arithmetic errors reject
/// compilation instead of being hidden in an infallible State constructor.
fn initial_seed(
    definition: &Definition,
    order: &[usize],
    inputs: &[BTreeMap<String, (usize, String)>],
) -> Result<Seed> {
    let mut seed = Seed {
        outputs: vec![BTreeMap::new(); definition.nodes.len()],
        memory: vec![Memory::default(); definition.nodes.len()],
    };
    for (index, node) in definition.nodes.iter().enumerate() {
        let value = match &node.kind {
            NodeKind::Key { .. } | NodeKind::Button { .. } | NodeKind::Toggle => {
                Some(Value::Hold(false))
            }
            NodeKind::Constant { value } => Some(value.clone()),
            NodeKind::DeltaToPosition { initial } => {
                seed.memory[index].position = Some(*initial);
                Some(Value::Position(*initial))
            }
            NodeKind::Accumulate { initial } => {
                seed.memory[index].scalar = *initial;
                Some(Value::State(*initial))
            }
            NodeKind::Counter { initial, .. } => {
                seed.memory[index].scalar = f64::from(*initial);
                Some(Value::State(f64::from(*initial)))
            }
            _ => None,
        };
        if let Some(value) = value {
            seed.outputs[index].insert("value".into(), value);
        }
    }
    for &index in order {
        let node = &definition.nodes[index];
        let kind = &node.kind;
        if matches!(kind, NodeKind::Delay { .. } | NodeKind::Throttle { .. }) {
            continue;
        }
        if matches!(
            kind,
            NodeKind::Switch {
                signal: SignalType::Position | SignalType::State
            }
        ) {
            // Persistent numeric branches must always have a value. Otherwise a
            // branch change could leave the previously selected value latched.
            for branch in ["off", "on"] {
                let (parent, port) = &inputs[index][branch];
                ensure!(
                    seed.outputs[*parent].contains_key(port),
                    "mapper switch {} branch {branch} needs an initial value from {}.{}; use a constant or an initialized converter",
                    node.id,
                    definition.nodes[*parent].id,
                    port
                );
            }
        }
        let ports = kind.ports();
        if inputs[index].is_empty()
            || ports.outputs.is_empty()
            || ports.outputs.iter().any(|port| port.signal.transient())
            || ports.inputs.iter().any(|port| port.signal.transient())
        {
            continue;
        }
        let values = inputs[index]
            .iter()
            .filter_map(|(name, (parent, port))| {
                seed.outputs[*parent]
                    .get(port)
                    .map(|value| (name.as_str(), value.clone()))
            })
            .collect();
        let results = evaluate_node(
            kind,
            &values,
            &BTreeSet::new(),
            &mut seed.memory[index],
            None,
        )
        .with_context(|| format!("invalid initial mapper node {}", definition.nodes[index].id))?;
        for (port, value) in results {
            value.validate()?;
            seed.outputs[index].insert(port.into(), value);
        }
    }
    Ok(seed)
}
#[derive(Debug, Clone)]
pub struct State {
    definition: Arc<Definition>,
    outputs: Vec<BTreeMap<String, Value>>,
    memory: Vec<Memory>,
    seed: Seed,
    // Operator caches may be initialized without an input wave. Sink history
    // starts empty: a cached value is not proof of prior native delivery.
    emitted: Vec<Option<Value>>,
    monitors: BTreeMap<String, MonitorSample>,
    scopes: BTreeMap<String, Arc<VecDeque<ScopeSample>>>,
    pending_delays: Vec<DelayJob>,
    next_delay_id: u64,
    pending_observations: Vec<ObservationJob>,
    next_observation_id: u64,
    dropped_observations: u64,
}
impl State {
    pub fn new(compiled: &Compiled) -> Self {
        Self {
            definition: Arc::new(compiled.definition.clone()),
            outputs: compiled.seed.outputs.clone(),
            memory: compiled.seed.memory.clone(),
            seed: compiled.seed.clone(),
            emitted: vec![None; compiled.definition.nodes.len()],
            monitors: BTreeMap::new(),
            scopes: BTreeMap::new(),
            pending_delays: Vec::new(),
            next_delay_id: 1,
            pending_observations: Vec::new(),
            next_observation_id: 1,
            dropped_observations: 0,
        }
    }
    /// Latest committed sample for each observed monitor, in node ID order.
    pub fn monitor_samples(&self) -> Vec<MonitorSample> {
        self.monitors.values().cloned().collect()
    }
    /// Bounded scope samples in stable node ID order, each oldest to newest.
    pub fn scope_traces(&self) -> Vec<ScopeTrace> {
        self.scopes
            .iter()
            .map(|(node, samples)| ScopeTrace {
                node: node.clone(),
                samples: samples.iter().cloned().collect(),
            })
            .collect()
    }
    /// Retires all actor values and conversion memory, without emitting effects.
    /// The runtime must separately release saved native sink ownership first.
    pub fn clear(&mut self) {
        self.outputs.clone_from(&self.seed.outputs);
        self.memory.clone_from(&self.seed.memory);
        self.emitted.fill(None);
        self.monitors.clear();
        self.scopes.clear();
        self.pending_delays.clear();
        self.pending_observations.clear();
        self.dropped_observations = 0;
    }
    /// Caller preauthorizes `affected_outputs(input.source())` and their complete
    /// source ancestry before this call. Errors leave this actor unchanged.
    pub fn evaluate(&mut self, compiled: &Compiled, input: RuntimeInput) -> Result<Vec<Emission>> {
        self.evaluate_at(compiled, input, 0)
    }
    pub fn evaluate_at(
        &mut self,
        compiled: &Compiled,
        input: RuntimeInput,
        elapsed_us: u64,
    ) -> Result<Vec<Emission>> {
        ensure!(
            self.definition.as_ref() == &compiled.definition,
            "mapper state belongs to a different compiled graph"
        );
        input.validate()?;
        let Some(&(source_index, _)) = compiled.sources.get(&input.source()) else {
            return Ok(vec![]);
        };
        let mut next = self.clone();
        let effects = next.wave(compiled, source_index, input.values(), elapsed_us)?;
        *self = next;
        Ok(effects)
    }
    pub fn timer_enabled(&self, index: usize) -> bool {
        self.memory.get(index).is_some_and(|memory| memory.held)
    }

    pub fn pending_delays(&self) -> Vec<PendingDelay> {
        self.pending_delays
            .iter()
            .map(|job| PendingDelay {
                id: job.id,
                node_index: job.node_index,
                due_elapsed_us: job.due_elapsed_us,
            })
            .collect()
    }

    /// Metadata-only requests awaiting app-side screen work.
    pub fn pending_observations(&self) -> Vec<PendingObservation> {
        self.pending_observations
            .iter()
            .map(|job| PendingObservation {
                id: job.id,
                node_index: job.node_index,
            })
            .collect()
    }

    /// Number of screen requests dropped for this actor since its last clear.
    pub fn dropped_observations(&self) -> u64 {
        self.dropped_observations
    }

    /// Discard exact pending IDs after app-side admission cannot retain them.
    /// Validation is complete before queue or telemetry state is changed.
    pub fn discard_observations(&mut self, ids: &[u64]) -> Result<()> {
        let mut seen = BTreeSet::new();
        for &id in ids {
            ensure!(seen.insert(id), "duplicate screen observation ID");
            ensure!(
                self.pending_observations.iter().any(|job| job.id == id),
                "unknown screen observation ID"
            );
        }
        self.pending_observations
            .retain(|job| !seen.contains(&job.id));
        self.dropped_observations = self
            .dropped_observations
            .saturating_add(u64::try_from(ids.len()).unwrap_or(u64::MAX));
        Ok(())
    }

    /// Apply an asynchronous observation to the actor's current state. Callers
    /// must evaluate on the live actor state, never a trigger-time snapshot.
    pub fn complete_observation(
        &mut self,
        compiled: &Compiled,
        id: u64,
        result: ObservationResult,
        elapsed_us: u64,
    ) -> Result<Vec<Emission>> {
        ensure!(
            self.definition.as_ref() == &compiled.definition,
            "mapper state belongs to a different compiled graph"
        );
        let job = self
            .pending_observations
            .iter()
            .find(|job| job.id == id)
            .cloned()
            .context("unknown screen observation ID")?;
        let node = &compiled.definition.nodes[job.node_index];
        let input = match (&node.kind, result) {
            (NodeKind::PixelColor { .. }, ObservationResult::PixelColor { rgba }) => {
                vec![
                    ("red", Value::State(f64::from(rgba[0]))),
                    ("green", Value::State(f64::from(rgba[1]))),
                    ("blue", Value::State(f64::from(rgba[2]))),
                    ("alpha", Value::State(f64::from(rgba[3]))),
                    ("done", Value::Pulse(true)),
                ]
            }
            (NodeKind::ImageMatch { .. }, ObservationResult::ImageMatch { matched }) => {
                let mut values = match matched {
                    Some(found) => {
                        vector(found.desktop_center)?;
                        ensure!(
                            found.score.is_finite() && (0.0..=1.0).contains(&found.score),
                            "ImageMatch score must be finite and in [0, 1]"
                        );
                        vec![
                            ("matched", Value::Hold(true)),
                            ("score", Value::State(found.score)),
                            ("position", Value::Position(found.desktop_center)),
                        ]
                    }
                    None => vec![
                        ("matched", Value::Hold(false)),
                        ("score", Value::State(0.0)),
                    ],
                };
                values.push(("done", Value::Pulse(true)));
                values
            }
            (NodeKind::Ocr { .. }, ObservationResult::Ocr { text }) => {
                text_value(&text, MAX_TEXT_BYTES)?;
                vec![
                    ("has_text", Value::Hold(!text.is_empty())),
                    ("text", Value::Text(text)),
                    ("done", Value::Pulse(true)),
                ]
            }
            _ => bail!("screen observation result does not match its node kind"),
        };
        let mut next = self.clone();
        next.pending_observations.retain(|pending| pending.id != id);
        let effects = next.wave(compiled, job.node_index, input, elapsed_us)?;
        *self = next;
        Ok(effects)
    }

    pub fn next_delay_due(&self) -> Option<PendingDelay> {
        self.pending_delays
            .iter()
            .filter_map(|job| {
                job.due_elapsed_us.map(|due_elapsed_us| PendingDelay {
                    id: job.id,
                    node_index: job.node_index,
                    due_elapsed_us: Some(due_elapsed_us),
                })
            })
            .min_by_key(|job| (job.due_elapsed_us, job.id))
    }

    /// Anchor newly queued Delay waves only after the runtime accepts the
    /// original input wave. Validate every job before changing any deadline.
    pub fn anchor_delays(&mut self, ids: &[u64], accepted_elapsed_us: u64) -> Result<()> {
        let mut seen = BTreeSet::new();
        let mut deadlines = Vec::with_capacity(ids.len());
        for &id in ids {
            ensure!(seen.insert(id), "duplicate Delay job ID");
            let job = self
                .pending_delays
                .iter()
                .find(|job| job.id == id)
                .context("unknown Delay job ID")?;
            ensure!(
                job.due_elapsed_us.is_none(),
                "Delay job is already anchored"
            );
            let interval_us = u64::from(job.delay_ms) * 1_000;
            let due = accepted_elapsed_us
                .checked_add(interval_us)
                .context("Delay deadline overflow")?;
            deadlines.push((id, due));
        }
        for (id, due) in deadlines {
            self.pending_delays
                .iter_mut()
                .find(|job| job.id == id)
                .expect("validated Delay job")
                .due_elapsed_us = Some(due);
        }
        Ok(())
    }

    /// Evaluate the oldest due Delay job. Runtime callers use an actor clone
    /// and commit it only after native effects are accepted.
    pub fn evaluate_delay_at(
        &mut self,
        compiled: &Compiled,
        id: u64,
        elapsed_us: u64,
    ) -> Result<Vec<Emission>> {
        ensure!(
            self.definition.as_ref() == &compiled.definition,
            "mapper state belongs to a different compiled graph"
        );
        let job = self
            .pending_delays
            .iter()
            .find(|job| job.id == id)
            .cloned()
            .context("unknown Delay job ID")?;
        let due = job.due_elapsed_us.context("Delay job is not anchored")?;
        ensure!(due <= elapsed_us, "Delay job is not due");
        let oldest_due = self
            .pending_delays
            .iter()
            .filter_map(|pending| {
                pending
                    .due_elapsed_us
                    .filter(|&pending_due| pending_due <= elapsed_us)
                    .map(|pending_due| (pending_due, pending.id))
            })
            .min();
        ensure!(
            oldest_due == Some((due, id)),
            "Delay jobs must be evaluated in deadline order"
        );
        let mut next = self.clone();
        next.pending_delays.retain(|pending| pending.id != id);
        let effects = next.wave(
            compiled,
            job.node_index,
            vec![("value", job.value)],
            elapsed_us,
        )?;
        *self = next;
        Ok(effects)
    }

    /// Inject one worker-admitted timer pulse without re-entering virtual input.
    pub fn evaluate_timer(&mut self, compiled: &Compiled, index: usize) -> Result<Vec<Emission>> {
        self.evaluate_timer_at(compiled, index, 0)
    }
    pub fn evaluate_timer_at(
        &mut self,
        compiled: &Compiled,
        index: usize,
        elapsed_us: u64,
    ) -> Result<Vec<Emission>> {
        ensure!(
            self.definition.as_ref() == &compiled.definition,
            "mapper state belongs to a different compiled graph"
        );
        ensure!(
            compiled.timer_sinks.contains_key(&index) && self.timer_enabled(index),
            "Timer is not enabled"
        );
        let mut next = self.clone();
        let effects = next.wave(
            compiled,
            index,
            vec![("value", Value::Pulse(true))],
            elapsed_us,
        )?;
        *self = next;
        Ok(effects)
    }

    fn wave(
        &mut self,
        compiled: &Compiled,
        source_index: usize,
        input: Vec<(&'static str, Value)>,
        elapsed_us: u64,
    ) -> Result<Vec<Emission>> {
        for outputs in &mut self.outputs {
            outputs.retain(|_, v| !v.signal().transient());
        }
        let mut dirty = vec![BTreeSet::<String>::new(); self.outputs.len()];
        for (port, value) in input {
            self.put(source_index, port, value, &mut dirty, false)?;
        }
        let mut effects = Vec::new();
        let mut effect_cost = 0;
        for &index in &compiled.order {
            let bindings = &compiled.inputs[index];
            if bindings.is_empty()
                || !bindings
                    .values()
                    .any(|(parent, port)| dirty[*parent].contains(port))
            {
                continue;
            }
            let node = &compiled.definition.nodes[index];
            let kind = &node.kind;
            let values: BTreeMap<_, _> = bindings
                .iter()
                .filter_map(|(name, (parent, port))| {
                    self.outputs[*parent]
                        .get(port)
                        .map(|v| (name.as_str(), v.clone()))
                })
                .collect();
            if kind.latest_observer_signal().is_some() {
                if let Some(value) = values.get("value") {
                    let samples = self
                        .monitors
                        .get(&node.id)
                        .map_or(1, |sample| sample.samples.saturating_add(1));
                    self.monitors.insert(
                        node.id.clone(),
                        MonitorSample {
                            node: node.id.clone(),
                            value: value.clone(),
                            samples,
                        },
                    );
                }
                continue;
            }
            if kind.trace_observer_signal().is_some() {
                if let Some(value) = values.get("value") {
                    let samples = Arc::make_mut(self.scopes.entry(node.id.clone()).or_default());
                    let timestamp = samples
                        .back()
                        .map_or(elapsed_us, |last| elapsed_us.max(last.elapsed_us));
                    if samples.len() == MAX_SCOPE_SAMPLES {
                        samples.pop_front();
                    }
                    samples.push_back(ScopeSample {
                        elapsed_us: timestamp,
                        value: value.clone(),
                    });
                }
                continue;
            }
            if let NodeKind::Delay { delay_ms, .. } = kind {
                if let Some(value) = values
                    .get("value")
                    .filter(|value| !matches!(value, Value::Pulse(false)))
                {
                    ensure!(
                        self.pending_delays.len() < MAX_PENDING_DELAYS_PER_ACTOR,
                        "mapper actor exceeds 128 pending Delay waves"
                    );
                    let id = self.next_delay_id;
                    self.next_delay_id = self
                        .next_delay_id
                        .checked_add(1)
                        .context("Delay job ID exhausted")?;
                    self.pending_delays.push(DelayJob {
                        id,
                        node_index: index,
                        value: value.clone(),
                        delay_ms: *delay_ms,
                        due_elapsed_us: None,
                    });
                }
                continue;
            }
            if matches!(
                kind,
                NodeKind::PixelColor { .. } | NodeKind::ImageMatch { .. } | NodeKind::Ocr { .. }
            ) {
                if matches!(values.get("trigger"), Some(Value::Pulse(true))) {
                    if self.pending_observations.len() >= MAX_PENDING_OBSERVATIONS_PER_ACTOR
                        || self
                            .pending_observations
                            .iter()
                            .any(|pending| pending.node_index == index)
                    {
                        self.dropped_observations = self.dropped_observations.saturating_add(1);
                        continue;
                    }
                    let id = self.next_observation_id;
                    self.next_observation_id = self
                        .next_observation_id
                        .checked_add(1)
                        .context("screen observation ID exhausted")?;
                    self.pending_observations.push(ObservationJob {
                        id,
                        node_index: index,
                    });
                }
                continue;
            }
            let changed: BTreeSet<_> = bindings
                .iter()
                .filter(|(_, (parent, port))| dirty[*parent].contains(port))
                .map(|(name, _)| name.as_str())
                .collect();
            if let Some(sink) = compiled.sink(index) {
                let typing = sink.channel == OutputChannel::TypeText;
                let value = if typing {
                    // Text is cached state; only this wave's explicit true Pulse
                    // may consume it. Identical later triggers deliberately repeat.
                    if !changed.contains("trigger")
                        || !matches!(values.get("trigger"), Some(Value::Pulse(true)))
                    {
                        continue;
                    }
                    values
                        .get("text")
                        .context("mapper typing sink has no current text")?
                } else {
                    let Some(value) = values.get("value").filter(|v| v.active()) else {
                        continue;
                    };
                    value
                };
                if !typing
                    && !value.signal().transient()
                    && (self.emitted[index].as_ref() == Some(value)
                        || (self.emitted[index].is_none() && *value == Value::Hold(false)))
                {
                    continue;
                }
                let cost = sink.channel.value_effect_cost(value)?;
                if cost == 0 {
                    continue;
                }
                effect_cost += cost;
                ensure!(
                    effect_cost <= MAX_EFFECTS,
                    "mapper input wave exceeds 128 weighted sink effects"
                );
                effects.push(Emission {
                    sink: sink.sink,
                    user: sink.user,
                    channel: sink.channel,
                    value: value.clone(),
                    source_users: sink.source_users,
                });
                if !typing && !value.signal().transient() {
                    self.emitted[index] = Some(value.clone());
                }
                continue;
            }
            let results = evaluate_node(
                kind,
                &values,
                &changed,
                &mut self.memory[index],
                Some(elapsed_us),
            )
            .with_context(|| format!("mapper node {}", node.id))?;
            for (port, value) in results {
                // A live causal path reaches sinks even when an intermediate
                // result equals its effect-free seed or previous cache value.
                // Persistent repeats are suppressed at the sink, independently.
                self.put(index, port, value, &mut dirty, true)?;
            }
        }
        // Transients expire at the wave boundary, not merely when another input
        // happens to arrive. No caller can later inspect or replay a stale pulse.
        for outputs in &mut self.outputs {
            outputs.retain(|_, v| !v.signal().transient());
        }
        Ok(effects)
    }
    fn put(
        &mut self,
        index: usize,
        port: &str,
        value: Value,
        dirty: &mut [BTreeSet<String>],
        causal: bool,
    ) -> Result<()> {
        value.validate()?;
        if causal || value.signal().transient() || self.outputs[index].get(port) != Some(&value) {
            self.outputs[index].insert(port.into(), value);
            dirty[index].insert(port.into());
        }
        Ok(())
    }
}

fn evaluate_node(
    kind: &NodeKind,
    values: &BTreeMap<&str, Value>,
    changed: &BTreeSet<&str>,
    memory: &mut Memory,
    elapsed_us: Option<u64>,
) -> Result<Vec<(&'static str, Value)>> {
    let one = |v| Ok(vec![("value", v)]);
    let scalar = |name: &str, signal: ScalarType| -> Option<f64> {
        match values.get(name) {
            Some(Value::State(v) | Value::Impulse(v)) => Some(*v),
            None if signal == ScalarType::Impulse => Some(0.0),
            _ => None,
        }
    };
    let vector_value = |name: &str, signal: VectorType| -> Option<[f64; 2]> {
        match values.get(name) {
            Some(Value::Position(v) | Value::Delta(v)) => Some(*v),
            None if signal == VectorType::Delta => Some([0.0, 0.0]),
            _ => None,
        }
    };
    let held = |name: &str| -> Option<bool> {
        match values.get(name) {
            Some(Value::Hold(v)) => Some(*v),
            _ => None,
        }
    };
    use NodeKind::*;
    match kind {
        Translate { offset } => {
            if let Some(v) = vector_value("value", VectorType::Position) {
                one(Value::Position([v[0] + offset[0], v[1] + offset[1]]))
            } else {
                Ok(vec![])
            }
        }
        ClampPosition { min, max } => {
            if let Some(v) = vector_value("value", VectorType::Position) {
                one(Value::Position([
                    v[0].clamp(min[0], max[0]),
                    v[1].clamp(min[1], max[1]),
                ]))
            } else {
                Ok(vec![])
            }
        }
        PositionToDelta => {
            let Some(v) = vector_value("value", VectorType::Position) else {
                return Ok(vec![]);
            };
            let previous = memory.position.replace(v);
            if let Some(previous) = previous {
                one(Value::Delta([v[0] - previous[0], v[1] - previous[1]]))
            } else {
                Ok(vec![])
            }
        }
        DeltaToPosition { .. } => {
            let v = vector_value("value", VectorType::Delta).context("missing delta")?;
            let previous = memory.position.context("missing initial position")?;
            let result = [previous[0] + v[0], previous[1] + v[1]];
            memory.position = Some(result);
            one(Value::Position(result))
        }
        ScaleDelta { factor } => {
            let v = vector_value("value", VectorType::Delta).context("missing delta")?;
            one(Value::Delta([v[0] * factor, v[1] * factor]))
        }
        AddDelta => {
            let a = vector_value("a", VectorType::Delta).context("missing delta")?;
            let b = vector_value("b", VectorType::Delta).context("missing delta")?;
            one(Value::Delta([a[0] + b[0], a[1] + b[1]]))
        }
        RotateDelta { radians } => {
            let v = vector_value("value", VectorType::Delta).context("missing delta")?;
            let (s, c) = radians.sin_cos();
            one(Value::Delta([v[0] * c - v[1] * s, v[0] * s + v[1] * c]))
        }
        DeadZoneDelta { radius } => {
            let v = vector_value("value", VectorType::Delta).context("missing delta")?;
            one(Value::Delta(if v[0].hypot(v[1]) <= *radius {
                [0.0, 0.0]
            } else {
                v
            }))
        }
        ScalarMath { signal, operation } => {
            let (Some(a), Some(b)) = (scalar("a", *signal), scalar("b", *signal)) else {
                return Ok(vec![]);
            };
            let result = match operation {
                Arithmetic::Add => a + b,
                Arithmetic::Subtract => a - b,
                Arithmetic::Multiply => a * b,
                Arithmetic::Divide => {
                    ensure!(b != 0.0, "division by zero");
                    a / b
                }
                Arithmetic::Min => a.min(b),
                Arithmetic::Max => a.max(b),
            };
            one(signal.value(result))
        }
        UnaryMath { signal, operation } => {
            if let Some(value) = scalar("value", *signal) {
                let result = match operation {
                    UnaryOperation::Abs => value.abs(),
                    UnaryOperation::Negate => -value,
                    UnaryOperation::Sqrt => value.sqrt(),
                    UnaryOperation::Square => value * value,
                    UnaryOperation::Round => value.round(),
                    UnaryOperation::Floor => value.floor(),
                    UnaryOperation::Ceil => value.ceil(),
                    UnaryOperation::Sin => value.sin(),
                    UnaryOperation::Cos => value.cos(),
                    UnaryOperation::Sign => {
                        if value == 0.0 {
                            0.0
                        } else {
                            value.signum()
                        }
                    }
                };
                one(signal.value(result))
            } else {
                Ok(vec![])
            }
        }
        VectorLength { signal } => {
            if let Some(value) = vector_value("value", *signal) {
                one(signal.scalar().value(value[0].hypot(value[1])))
            } else {
                Ok(vec![])
            }
        }
        NormalizeVector { signal } => {
            if let Some(value) = vector_value("value", *signal) {
                let scale = value[0].abs().max(value[1].abs());
                let normalized = if scale == 0.0 {
                    [0.0, 0.0]
                } else {
                    let scaled = [value[0] / scale, value[1] / scale];
                    let length = scaled[0].hypot(scaled[1]);
                    [scaled[0] / length, scaled[1] / length]
                };
                one(signal.value(normalized))
            } else {
                Ok(vec![])
            }
        }
        ScaleScalar { signal, factor } => {
            if let Some(v) = scalar("value", *signal) {
                one(signal.value(v * factor))
            } else {
                Ok(vec![])
            }
        }
        ClampScalar { signal, min, max } => {
            if let Some(v) = scalar("value", *signal) {
                one(signal.value(v.clamp(*min, *max)))
            } else {
                Ok(vec![])
            }
        }
        Remap {
            signal,
            input_min,
            input_max,
            output_min,
            output_max,
            clamp,
        } => {
            if let Some(value) = scalar("value", *signal) {
                let x = if *clamp {
                    value.clamp(*input_min, *input_max)
                } else {
                    value
                };
                let mapped = if output_min == output_max {
                    *output_min
                } else {
                    (x - *input_min) / (*input_max - *input_min) * (*output_max - *output_min)
                        + *output_min
                };
                one(signal.value(mapped))
            } else {
                Ok(vec![])
            }
        }
        Smooth { signal, alpha } => {
            let input = values.get("value").filter(|v| v.signal() == *signal);
            let Some(input) = input else {
                return Ok(vec![]);
            };
            let smoothed = match (signal, input) {
                (SignalType::Position, Value::Position(current)) => {
                    let previous = match memory.smooth.as_ref() {
                        Some(Value::Position(v)) => Some(*v),
                        _ => None,
                    };
                    let result = previous.map_or(*current, |p| {
                        [
                            p[0] + *alpha * (current[0] - p[0]),
                            p[1] + *alpha * (current[1] - p[1]),
                        ]
                    });
                    Value::Position(result)
                }
                (SignalType::State, Value::State(current)) => {
                    let previous = match memory.smooth.as_ref() {
                        Some(Value::State(v)) => Some(*v),
                        _ => None,
                    };
                    Value::State(previous.map_or(*current, |p| p + *alpha * (*current - p)))
                }
                _ => return Ok(vec![]),
            };
            memory.smooth = Some(smoothed.clone());
            one(smoothed)
        }
        Velocity => {
            let Some(Value::Position(position)) = values.get("value") else {
                return Ok(vec![]);
            };
            let Some(now) = elapsed_us else {
                return Ok(vec![]);
            };
            let output = match memory.velocity_sample {
                None => {
                    memory.velocity_sample = Some((*position, now));
                    0.0
                }
                Some((previous, then)) if now > then => {
                    let distance = (position[0] - previous[0]).hypot(position[1] - previous[1]);
                    let speed = (distance * 1_000_000.0 / ((now - then) as f64)).min(NUMERIC_LIMIT);
                    memory.velocity_sample = Some((*position, now));
                    speed
                }
                Some(_) => return Ok(vec![]),
            };
            one(Value::State(output))
        }
        Components { signal } => {
            if let Some(v) = vector_value("value", *signal) {
                Ok(vec![
                    ("x", signal.scalar().value(v[0])),
                    ("y", signal.scalar().value(v[1])),
                ])
            } else {
                Ok(vec![])
            }
        }
        Compose { signal } => {
            if let (Some(x), Some(y)) = (scalar("x", signal.scalar()), scalar("y", signal.scalar()))
            {
                one(signal.value([x, y]))
            } else {
                Ok(vec![])
            }
        }
        Compare { signal, operation } => {
            let (Some(a), Some(b)) = (scalar("a", *signal), scalar("b", *signal)) else {
                return Ok(vec![]);
            };
            let result = match operation {
                Comparison::Less => a < b,
                Comparison::LessEqual => a <= b,
                Comparison::Equal => a == b,
                Comparison::GreaterEqual => a >= b,
                Comparison::Greater => a > b,
            };
            one(if *signal == ScalarType::State {
                Value::Hold(result)
            } else {
                Value::Pulse(result)
            })
        }
        TextContains {
            needle,
            case_sensitive,
        } => {
            let Some(Value::Text(text)) = values.get("value") else {
                return Ok(vec![]);
            };
            one(Value::Hold(if *case_sensitive {
                text.contains(needle)
            } else {
                text.to_ascii_lowercase()
                    .contains(&needle.to_ascii_lowercase())
            }))
        }
        TextCompare {
            expected,
            operation,
            case_sensitive,
        } => {
            let Some(Value::Text(text)) = values.get("value") else {
                return Ok(vec![]);
            };
            let equal = if *case_sensitive {
                text == expected
            } else {
                text.eq_ignore_ascii_case(expected)
            };
            one(Value::Hold(match operation {
                TextComparison::Equals => equal,
                TextComparison::NotEquals => !equal,
            }))
        }
        Logic { operation } => {
            let Some(a) = held("a") else {
                return Ok(vec![]);
            };
            let result = match operation {
                super::mapper::Logic::Not => !a,
                super::mapper::Logic::And => {
                    let Some(b) = held("b") else {
                        return Ok(vec![]);
                    };
                    a && b
                }
                super::mapper::Logic::Or => {
                    let Some(b) = held("b") else {
                        return Ok(vec![]);
                    };
                    a || b
                }
            };
            one(Value::Hold(result))
        }
        Gate { signal } => {
            let Some(open) = held("open") else {
                return Ok(vec![]);
            };
            if *signal == SignalType::Hold && !open {
                return one(Value::Hold(false));
            }
            if !open || (signal.transient() && !changed.contains("value")) {
                return Ok(vec![]);
            }
            if let Some(value) = values.get("value") {
                one(value.clone())
            } else {
                Ok(vec![])
            }
        }
        ImpulseToPulse => one(Value::Pulse(
            scalar("value", ScalarType::Impulse).unwrap_or(0.0) != 0.0,
        )),
        RisingEdge => {
            let Some(value) = held("value") else {
                return Ok(vec![]);
            };
            let rising = value && !memory.held;
            memory.held = value;
            one(Value::Pulse(rising))
        }
        Toggle => {
            if matches!(values.get("value"), Some(Value::Pulse(true))) {
                memory.held = !memory.held;
                one(Value::Hold(memory.held))
            } else {
                Ok(vec![])
            }
        }
        Accumulate { .. } => {
            memory.scalar += scalar("value", ScalarType::Impulse).unwrap_or(0.0);
            one(Value::State(memory.scalar))
        }
        Counter { initial, .. } => {
            if matches!(values.get("reset"), Some(Value::Pulse(true))) {
                memory.scalar = f64::from(*initial);
            } else if matches!(values.get("count"), Some(Value::Pulse(true))) {
                memory.scalar = (memory.scalar + 1.0).min(NUMERIC_LIMIT);
            } else {
                return Ok(vec![]);
            }
            one(Value::State(memory.scalar))
        }
        Switch { signal } => {
            let Some(select) = held("select") else {
                return Ok(vec![]);
            };
            let branch = if select { "on" } else { "off" };
            if !changed.contains(branch)
                && (signal.transient() || (!changed.is_empty() && !changed.contains("select")))
            {
                return Ok(vec![]);
            }
            match values.get(branch) {
                Some(value) => one(value.clone()),
                None if *signal == SignalType::Hold => one(Value::Hold(false)),
                None => Ok(vec![]),
            }
        }
        Timer { .. } => {
            memory.held = held("enabled").unwrap_or(false);
            Ok(vec![])
        }
        Throttle {
            signal,
            interval_ms,
        } => {
            let Some(value) = values
                .get("value")
                .filter(|value| value.signal() == *signal)
            else {
                return Ok(vec![]);
            };
            if matches!(value, Value::Hold(false)) {
                return one(value.clone());
            }
            if matches!(value, Value::Pulse(false)) {
                return Ok(vec![]);
            }
            let now = elapsed_us.context("Throttle requires an elapsed timestamp")?;
            let interval_us = u64::from(*interval_ms) * 1_000;
            if memory
                .throttle_at_us
                .is_none_or(|last| now.saturating_sub(last) >= interval_us)
            {
                memory.throttle_at_us = Some(now);
                one(value.clone())
            } else {
                Ok(vec![])
            }
        }
        Pointer { .. }
        | Wheel { .. }
        | Key { .. }
        | Button { .. }
        | Constant { .. }
        | Monitor { .. }
        | PixelColor { .. }
        | ImageMatch { .. }
        | Ocr { .. }
        | Scope { .. }
        | Led
        | Gauge { .. }
        | XyPad { .. }
        | PathTracer { .. }
        | Delay { .. }
        | Sink { .. } => bail!("node is not a processing operation"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: &str, kind: NodeKind) -> Node {
        Node {
            id: id.into(),
            kind,
            position: None,
        }
    }
    fn edge(from: &str, output: &str, to: &str, input: &str) -> Edge {
        Edge {
            from: PortRef {
                node: from.into(),
                port: output.into(),
            },
            to: PortRef {
                node: to.into(),
                port: input.into(),
            },
        }
    }
    fn definition(nodes: Vec<Node>, edges: Vec<Edge>) -> Definition {
        Definition {
            nodes,
            edges,
            ..Definition::default()
        }
    }
    fn pointer(id: &str, user: &str) -> Node {
        node(
            id,
            NodeKind::Pointer {
                user: user.into(),
                disposition: Disposition::Passthrough,
            },
        )
    }
    fn key(id: &str, user: &str, key: u16) -> Node {
        node(
            id,
            NodeKind::Key {
                user: user.into(),
                key,
                disposition: Disposition::Passthrough,
            },
        )
    }
    fn sink(id: &str, user: &str, channel: OutputChannel) -> Node {
        node(
            id,
            NodeKind::Sink {
                user: user.into(),
                channel,
            },
        )
    }
    fn motion(user: &str, delta: [f64; 2]) -> RuntimeInput {
        RuntimeInput::Pointer {
            user: user.into(),
            position: delta,
            delta,
        }
    }
    fn key_input(user: &str, key: u16, held: bool) -> RuntimeInput {
        RuntimeInput::Key {
            user: user.into(),
            key,
            held,
        }
    }
    fn wheel(user: &str, amount: f64) -> RuntimeInput {
        RuntimeInput::Wheel {
            user: user.into(),
            horizontal: false,
            amount,
        }
    }

    #[test]
    fn mapper_templates_round_trip_and_bind_stable_ids() {
        for template in templates() {
            let source = "Source.ID_1";
            let target = "Target.ID-2";
            let target_arg = template.requires_target.then_some(target);
            let draft = instantiate_template(template.template, source, target_arg).unwrap();
            assert!(draft.nodes.iter().all(|node| node.position.is_some()));
            assert_eq!(
                Definition::from_json(&draft.to_json().unwrap()).unwrap(),
                draft
            );
            let compiled = draft.compile().unwrap();
            let sources = compiled.sources();
            assert_eq!(sources.len(), 1);
            assert_eq!(sources[0].user(), source);
            assert_eq!(
                compiled.source_disposition(&sources[0]),
                Some(if template.requires_target {
                    Disposition::Consume
                } else {
                    Disposition::Passthrough
                })
            );
            let sinks = compiled.affected_outputs(&sources[0]);
            assert_eq!(sinks.len(), usize::from(template.requires_target));
            if template.requires_target {
                assert_eq!(sinks[0].user, target);
                assert!(instantiate_template(template.template, source, None).is_err());
            } else {
                assert!(instantiate_template(template.template, source, Some(target)).is_err());
            }
            for invalid in ["", " ", " source", "bad/user", &"a".repeat(129)] {
                assert!(instantiate_template(template.template, invalid, target_arg).is_err());
                if template.requires_target {
                    assert!(
                        instantiate_template(template.template, source, Some(invalid)).is_err()
                    );
                }
            }
        }
    }

    #[test]
    fn motion_templates_have_expected_live_and_passive_behavior() {
        let smooth_graph = instantiate_template(TemplateKind::SmoothPointer, "a", Some("b"))
            .unwrap()
            .compile()
            .unwrap();
        let mut smooth = State::new(&smooth_graph);
        let emitted = smooth
            .evaluate_at(&smooth_graph, motion("a", [10.0, 20.0]), 1)
            .unwrap();
        assert_eq!(emitted.len(), 1);
        assert_eq!(emitted[0].channel, OutputChannel::Position);
        assert_eq!(
            smooth_graph.source_disposition(&Source::Pointer { user: "a".into() }),
            Some(Disposition::Consume)
        );

        let speed_graph = instantiate_template(TemplateKind::SpeedMonitor, "a", None)
            .unwrap()
            .compile()
            .unwrap();
        let mut speed = State::new(&speed_graph);
        assert!(
            speed
                .evaluate_at(&speed_graph, motion("a", [0.0, 0.0]), 1_000)
                .unwrap()
                .is_empty()
        );
        assert!(
            speed
                .evaluate_at(&speed_graph, motion("a", [1.0, 0.0]), 2_000)
                .unwrap()
                .is_empty()
        );
        assert_eq!(speed.monitor_samples()[0].value, Value::State(1_000.0));
        assert_eq!(
            speed_graph.source_disposition(&Source::Pointer { user: "a".into() }),
            Some(Disposition::Passthrough)
        );

        let trail_graph = instantiate_template(TemplateKind::PointerTrail, "a", None)
            .unwrap()
            .compile()
            .unwrap();
        let mut trail = State::new(&trail_graph);
        assert!(
            trail
                .evaluate_at(&trail_graph, motion("a", [5.0, 7.0]), 1)
                .unwrap()
                .is_empty()
        );
        assert_eq!(trail.monitor_samples().len(), 1);
        assert_eq!(trail.scope_traces().len(), 1);
        assert_eq!(
            trail.scope_traces()[0].samples[0].value,
            Value::Position([5.0, 7.0])
        );
        assert_eq!(
            trail_graph.source_disposition(&Source::Pointer { user: "a".into() }),
            Some(Disposition::Passthrough)
        );
    }

    #[test]
    fn mirror_pointer_template_inverts_only_horizontal_movement() {
        let compiled = instantiate_template(TemplateKind::MirrorPointer, "same", Some("same"))
            .unwrap()
            .compile()
            .unwrap();
        let mut state = State::new(&compiled);
        for (delta, expected) in [
            ([2.25, -4.0], [-2.25, -4.0]),
            ([-2.0, 0.0], [2.0, 0.0]),
            ([0.0, 1.0], [0.0, 1.0]),
        ] {
            let emissions = state.evaluate(&compiled, motion("same", delta)).unwrap();
            assert_eq!(emissions.len(), 1);
            assert_eq!(emissions[0].channel, OutputChannel::Delta);
            assert_eq!(emissions[0].value, Value::Delta(expected));
        }
    }

    #[test]
    fn key_remap_template_preserves_press_and_release() {
        let compiled = instantiate_template(TemplateKind::KeyRemap, "a", Some("b"))
            .unwrap()
            .compile()
            .unwrap();
        let mut state = State::new(&compiled);
        for held in [true, false] {
            let emissions = state
                .evaluate(&compiled, key_input("a", 0x76, held))
                .unwrap();
            assert_eq!(emissions.len(), 1);
            assert_eq!(emissions[0].channel, OutputChannel::KeyHold { key: 0x79 });
            assert_eq!(emissions[0].value, Value::Hold(held));
        }
    }

    #[test]
    fn timed_key_template_only_pulses_on_a_timer_while_held() {
        let compiled = instantiate_template(TemplateKind::TimedKeyPulse, "a", Some("b"))
            .unwrap()
            .compile()
            .unwrap();
        let timers: Vec<_> = compiled.timers().collect();
        assert_eq!(timers.len(), 1);
        let (index, interval, count) = timers[0];
        assert_eq!((interval, count), (500, 3));

        let mut state = State::new(&compiled);
        assert!(state.evaluate_timer(&compiled, index).is_err());
        assert!(
            state
                .evaluate(&compiled, key_input("a", 0x76, true))
                .unwrap()
                .is_empty()
        );
        let emissions = state.evaluate_timer(&compiled, index).unwrap();
        assert_eq!(emissions.len(), 1);
        assert_eq!(emissions[0].channel, OutputChannel::KeyPulse { key: 0x79 });
        assert_eq!(emissions[0].value, Value::Pulse(true));
        assert!(
            state
                .evaluate(&compiled, key_input("a", 0x76, false))
                .unwrap()
                .is_empty()
        );
        assert!(state.evaluate_timer(&compiled, index).is_err());
    }

    #[test]
    fn semantic_types_and_explicit_source_ancestry_are_required() {
        let mut graph = definition(
            vec![pointer("p", "a"), sink("s", "b", OutputChannel::Delta)],
            vec![edge("p", "position", "s", "value")],
        );
        assert!(
            graph.compile().is_err(),
            "position must never silently become relative movement"
        );
        graph.edges[0].from.port = "delta".into();
        assert!(graph.compile().is_ok());
        graph.nodes[0] = node(
            "p",
            NodeKind::Constant {
                value: Value::Delta([1.0, 1.0]),
            },
        );
        assert!(
            graph.compile().is_err(),
            "constants cannot generate recurring impulses"
        );
        graph.nodes[0] = node(
            "p",
            NodeKind::Constant {
                value: Value::Position([1.0, 1.0]),
            },
        );
        graph.edges[0].from.port = "value".into();
        graph.nodes[1] = sink("s", "b", OutputChannel::Position);
        assert!(graph.compile().is_err(), "constants alone cannot actuate");
    }

    #[test]
    fn invalid_structure_and_resource_limits_fail_before_evaluation() {
        let valid = definition(
            vec![pointer("p", "a"), sink("s", "b", OutputChannel::Delta)],
            vec![edge("p", "delta", "s", "value")],
        );
        let mut graph = valid.clone();
        graph.nodes.push(graph.nodes[0].clone());
        assert!(graph.compile().is_err());
        let mut graph = valid.clone();
        graph.nodes.push(pointer("duplicate", "a"));
        assert!(graph.compile().is_err());
        let mut graph = valid.clone();
        graph.edges.push(graph.edges[0].clone());
        assert!(graph.compile().is_err());
        let mut graph = valid.clone();
        graph.edges.clear();
        assert!(graph.compile().is_err());
        let mut graph = valid.clone();
        graph.edges[0].from.node = "absent".into();
        assert!(graph.compile().is_err());
        let mut graph = valid.clone();
        graph.edges[0].from.port = "absent".into();
        assert!(graph.compile().is_err());
        let graph = definition(
            vec![
                node("a", NodeKind::ScaleDelta { factor: 1.0 }),
                node("b", NodeKind::ScaleDelta { factor: 1.0 }),
            ],
            vec![
                edge("a", "value", "b", "value"),
                edge("b", "value", "a", "value"),
            ],
        );
        assert!(graph.compile().is_err());
        let mut graph = valid.clone();
        graph.nodes[0].position = Some([f64::NAN, 0.0]);
        assert!(graph.compile().is_err());
        let mut graph = valid.clone();
        graph.nodes[0].kind = NodeKind::Wheel {
            user: "a".into(),
            horizontal: false,
            disposition: Disposition::Passthrough,
        };
        graph.edges[0].from.port = "value".into();
        assert!(graph.compile().is_err());
        let graph = Definition {
            nodes: (0..=MAX_NODES)
                .map(|i| {
                    node(
                        &format!("n{i}"),
                        NodeKind::Constant {
                            value: Value::State(0.0),
                        },
                    )
                })
                .collect(),
            ..Definition::default()
        };
        assert!(graph.compile().is_err());
        let mut graph = valid;
        graph.edges = vec![graph.edges[0].clone(); MAX_EDGES + 1];
        assert!(graph.compile().is_err());
        assert!(Definition::from_json(&" ".repeat(MAX_JSON_BYTES + 1)).is_err());
        assert!(
            Definition::from_json(r#"{"version":1,"nodes":[],"edges":[],"unknown":true}"#).is_err()
        );
    }

    #[test]
    fn passthrough_overlap_requires_explicit_consumption() {
        let mut graph = definition(
            vec![pointer("p", "a"), sink("s", "a", OutputChannel::Delta)],
            vec![edge("p", "delta", "s", "value")],
        );
        assert!(graph.compile().is_err());
        graph.nodes[0].kind = NodeKind::Pointer {
            user: "a".into(),
            disposition: Disposition::Consume,
        };
        let compiled = graph.compile().unwrap();
        assert_eq!(
            compiled.source_disposition(&Source::Pointer { user: "a".into() }),
            Some(Disposition::Consume)
        );
        let mut state = State::new(&compiled);
        assert_eq!(
            state
                .evaluate(&compiled, motion("a", [0.25, -0.5]))
                .unwrap()[0]
                .value,
            Value::Delta([0.25, -0.5])
        );
    }

    #[test]
    fn deterministic_diamond_emits_once_and_preserves_fractional_values() {
        let graph = definition(
            vec![
                pointer("p", "a"),
                node("left", NodeKind::ScaleDelta { factor: 0.5 }),
                node("right", NodeKind::ScaleDelta { factor: 0.25 }),
                node("sum", NodeKind::AddDelta),
                sink("sink-z", "b", OutputChannel::Delta),
                sink("sink-a", "c", OutputChannel::Delta),
            ],
            vec![
                edge("p", "delta", "left", "value"),
                edge("p", "delta", "right", "value"),
                edge("left", "value", "sum", "a"),
                edge("right", "value", "sum", "b"),
                edge("sum", "value", "sink-z", "value"),
                edge("sum", "value", "sink-a", "value"),
            ],
        );
        let compiled = graph.compile().unwrap();
        let mut shuffled = graph.clone();
        shuffled.nodes.reverse();
        shuffled.edges.reverse();
        let other = shuffled.compile().unwrap();
        let effects = State::new(&compiled)
            .evaluate(&compiled, motion("a", [1.0, 2.0]))
            .unwrap();
        assert_eq!(
            effects,
            State::new(&other)
                .evaluate(&other, motion("a", [1.0, 2.0]))
                .unwrap()
        );
        assert_eq!(effects.len(), 2);
        assert_eq!(effects[0].sink, "sink-a");
        assert_eq!(effects[0].value, Value::Delta([0.75, 1.5]));
    }

    fn gated_wheel() -> Compiled {
        definition(
            vec![
                node(
                    "wheel",
                    NodeKind::Wheel {
                        user: "a".into(),
                        horizontal: false,
                        disposition: Disposition::Passthrough,
                    },
                ),
                key("open", "b", 65),
                node(
                    "gate",
                    NodeKind::Gate {
                        signal: SignalType::Impulse,
                    },
                ),
                sink("out", "c", OutputChannel::Wheel { horizontal: false }),
            ],
            vec![
                edge("wheel", "value", "gate", "value"),
                edge("open", "value", "gate", "open"),
                edge("gate", "value", "out", "value"),
            ],
        )
        .compile()
        .unwrap()
    }
    #[test]
    fn opening_gate_never_replays_expired_wheel_and_contexts_do_not_leak() {
        let graph = gated_wheel();
        let mut a = State::new(&graph);
        let mut b = State::new(&graph);
        assert!(a.evaluate(&graph, wheel("a", 120.0)).unwrap().is_empty());
        assert!(
            a.evaluate(&graph, key_input("b", 65, true))
                .unwrap()
                .is_empty()
        );
        let effects = a.evaluate(&graph, wheel("a", 0.5)).unwrap();
        assert_eq!(effects[0].value, Value::Impulse(0.5));
        assert_eq!(
            effects[0].source_users,
            BTreeSet::from(["a".into(), "b".into()])
        );
        assert!(b.evaluate(&graph, wheel("a", 1.0)).unwrap().is_empty());
        assert!(
            a.evaluate(&graph, key_input("b", 65, false))
                .unwrap()
                .is_empty()
        );
        assert!(
            a.evaluate(&graph, key_input("b", 65, true))
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            graph.affected_outputs(&Source::Key {
                user: "b".into(),
                key: 65
            })[0]
                .source_users,
            BTreeSet::from(["a".into(), "b".into()])
        );
    }

    #[test]
    fn another_source_closing_hold_gate_produces_release() {
        let graph = definition(
            vec![
                key("held", "a", 65),
                key("open", "b", 66),
                node(
                    "gate",
                    NodeKind::Gate {
                        signal: SignalType::Hold,
                    },
                ),
                sink("out", "c", OutputChannel::KeyHold { key: 67 }),
            ],
            vec![
                edge("held", "value", "gate", "value"),
                edge("open", "value", "gate", "open"),
                edge("gate", "value", "out", "value"),
            ],
        )
        .compile()
        .unwrap();
        let mut state = State::new(&graph);
        assert!(
            state
                .evaluate(&graph, key_input("a", 65, true))
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            state.evaluate(&graph, key_input("b", 66, true)).unwrap()[0].value,
            Value::Hold(true)
        );
        let release = state.evaluate(&graph, key_input("b", 66, false)).unwrap();
        assert_eq!(release.len(), 1);
        assert_eq!(release[0].value, Value::Hold(false));
        assert_eq!(
            release[0].source_users,
            BTreeSet::from(["a".into(), "b".into()])
        );
        assert!(
            state
                .evaluate(&graph, key_input("b", 66, false))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn first_down_reaches_direct_and_nested_or_without_other_source_events() {
        let graph = definition(
            vec![
                key("a", "a", 65),
                key("b", "b", 66),
                key("c", "c", 67),
                node(
                    "and",
                    NodeKind::Logic {
                        operation: Logic::And,
                    },
                ),
                node(
                    "nested",
                    NodeKind::Logic {
                        operation: Logic::Or,
                    },
                ),
                node(
                    "direct",
                    NodeKind::Logic {
                        operation: Logic::Or,
                    },
                ),
                sink("nested-out", "out", OutputChannel::KeyHold { key: 68 }),
                sink("direct-out", "out", OutputChannel::KeyHold { key: 69 }),
            ],
            vec![
                edge("b", "value", "and", "a"),
                edge("c", "value", "and", "b"),
                edge("a", "value", "nested", "a"),
                edge("and", "value", "nested", "b"),
                edge("a", "value", "direct", "a"),
                edge("b", "value", "direct", "b"),
                edge("nested", "value", "nested-out", "value"),
                edge("direct", "value", "direct-out", "value"),
            ],
        )
        .compile()
        .unwrap();
        let mut state = State::new(&graph);
        assert!(
            state
                .evaluate(&graph, key_input("b", 66, false))
                .unwrap()
                .is_empty(),
            "neutral seeds must not be dirty"
        );
        let effects = state.evaluate(&graph, key_input("a", 65, true)).unwrap();
        assert_eq!(effects.len(), 2);
        assert!(
            effects
                .iter()
                .all(|effect| effect.value == Value::Hold(true))
        );
        state.clear();
        assert_eq!(
            state.evaluate(&graph, key_input("a", 65, true)).unwrap(),
            effects
        );
    }

    #[test]
    fn derived_constant_branch_is_ready_without_a_fake_input_wave() {
        let graph = definition(
            vec![
                node(
                    "two",
                    NodeKind::Constant {
                        value: Value::State(2.0),
                    },
                ),
                node(
                    "scale",
                    NodeKind::ScaleScalar {
                        signal: ScalarType::State,
                        factor: 3.0,
                    },
                ),
                node(
                    "four",
                    NodeKind::Constant {
                        value: Value::State(4.0),
                    },
                ),
                node(
                    "math",
                    NodeKind::ScalarMath {
                        signal: ScalarType::State,
                        operation: Arithmetic::Add,
                    },
                ),
                pointer("pointer", "a"),
                node(
                    "parts",
                    NodeKind::Components {
                        signal: VectorType::Position,
                    },
                ),
                node(
                    "compose",
                    NodeKind::Compose {
                        signal: VectorType::Position,
                    },
                ),
                key("open", "b", 65),
                node(
                    "gate",
                    NodeKind::Gate {
                        signal: SignalType::Position,
                    },
                ),
                sink("out", "c", OutputChannel::Position),
            ],
            vec![
                edge("two", "value", "scale", "value"),
                edge("scale", "value", "math", "a"),
                edge("four", "value", "math", "b"),
                edge("pointer", "position", "parts", "value"),
                edge("parts", "x", "compose", "x"),
                edge("math", "value", "compose", "y"),
                edge("compose", "value", "gate", "value"),
                edge("open", "value", "gate", "open"),
                edge("gate", "value", "out", "value"),
            ],
        )
        .compile()
        .unwrap();
        let mut state = State::new(&graph);
        assert!(
            state
                .evaluate(&graph, key_input("b", 65, true))
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            state.evaluate(&graph, motion("a", [3.0, 7.0])).unwrap()[0].value,
            Value::Position([3.0, 10.0])
        );
        state.clear();
        assert!(
            state
                .evaluate(&graph, motion("a", [9.0, 8.0]))
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            state.evaluate(&graph, key_input("b", 65, true)).unwrap()[0].value,
            Value::Position([9.0, 10.0])
        );
    }

    #[test]
    fn invalid_static_arithmetic_is_rejected_at_compile() {
        let mut graph = definition(
            vec![
                node(
                    "a",
                    NodeKind::Constant {
                        value: Value::State(1.0),
                    },
                ),
                node(
                    "b",
                    NodeKind::Constant {
                        value: Value::State(0.0),
                    },
                ),
                node(
                    "math",
                    NodeKind::ScalarMath {
                        signal: ScalarType::State,
                        operation: Arithmetic::Divide,
                    },
                ),
            ],
            vec![
                edge("a", "value", "math", "a"),
                edge("b", "value", "math", "b"),
            ],
        );
        assert!(
            graph.compile().is_err(),
            "infallible State construction cannot hide a static division error"
        );
        graph.nodes[0].kind = NodeKind::Constant {
            value: Value::State(NUMERIC_LIMIT),
        };
        graph.nodes[1].kind = NodeKind::Constant {
            value: Value::State(2.0),
        };
        graph.nodes[2].kind = NodeKind::ScalarMath {
            signal: ScalarType::State,
            operation: Arithmetic::Multiply,
        };
        assert!(
            graph.compile().is_err(),
            "static overflow must not be stored as an initial seed"
        );
        for value in [
            Value::Delta([1.0, 0.0]),
            Value::Impulse(1.0),
            Value::Pulse(true),
        ] {
            let graph = definition(vec![node("constant", NodeKind::Constant { value })], vec![]);
            assert!(
                graph.compile().is_err(),
                "transient constants have no implicit clock semantics"
            );
        }
    }

    #[test]
    fn independent_binary_sinks_and_edge_toggle_expiration() {
        let graph = definition(
            vec![
                key("key", "a", 65),
                node("edge", NodeKind::RisingEdge),
                node("toggle", NodeKind::Toggle),
                sink("pulse", "b", OutputChannel::KeyPulse { key: 66 }),
                sink("hold", "c", OutputChannel::ButtonHold { button: 1 }),
            ],
            vec![
                edge("key", "value", "edge", "value"),
                edge("edge", "value", "pulse", "value"),
                edge("edge", "value", "toggle", "value"),
                edge("toggle", "value", "hold", "value"),
            ],
        )
        .compile()
        .unwrap();
        let mut state = State::new(&graph);
        let first = state.evaluate(&graph, key_input("a", 65, true)).unwrap();
        assert_eq!(first.len(), 2);
        assert!(first.iter().any(|e| e.value == Value::Pulse(true)));
        assert!(first.iter().any(|e| e.value == Value::Hold(true)));
        assert!(
            state
                .evaluate(&graph, key_input("a", 65, true))
                .unwrap()
                .is_empty()
        );
        assert!(
            state
                .evaluate(&graph, key_input("a", 65, false))
                .unwrap()
                .is_empty()
        );
        let second = state.evaluate(&graph, key_input("a", 65, true)).unwrap();
        assert!(second.iter().any(|e| e.value == Value::Hold(false)));
        state.clear();
        assert!(
            state
                .evaluate(&graph, key_input("a", 65, true))
                .unwrap()
                .iter()
                .any(|e| e.value == Value::Hold(true))
        );
    }

    #[test]
    fn timer_needs_live_source_and_does_not_start_another_timer() {
        let mut definition = definition(
            vec![
                key("key", "a", 65),
                node(
                    "first",
                    NodeKind::Timer {
                        interval_ms: 100,
                        count: 2,
                    },
                ),
                node("toggle", NodeKind::Toggle),
                node(
                    "second",
                    NodeKind::Timer {
                        interval_ms: 100,
                        count: 2,
                    },
                ),
            ],
            vec![
                edge("key", "value", "first", "enabled"),
                edge("first", "value", "toggle", "value"),
                edge("toggle", "value", "second", "enabled"),
            ],
        );
        assert!(
            definition
                .compile()
                .err()
                .unwrap()
                .to_string()
                .contains("Timer chains")
        );
        definition.nodes.truncate(2);
        definition.edges.truncate(1);
        definition.nodes[0].kind = NodeKind::Constant {
            value: Value::Hold(true),
        };
        assert!(
            definition
                .compile()
                .err()
                .unwrap()
                .to_string()
                .contains("virtual input source")
        );
    }

    #[test]
    fn counter_is_actor_local_saturates_and_resets_before_count() {
        let initial = 999_999_999;
        let mut definition = definition(
            vec![
                key("count", "a", 65),
                key("reset", "a", 66),
                node("count-pulse", NodeKind::RisingEdge),
                node("reset-pulse", NodeKind::RisingEdge),
                node(
                    "counter",
                    NodeKind::Counter {
                        initial,
                        with_reset: true,
                    },
                ),
            ],
            vec![
                edge("count", "value", "count-pulse", "value"),
                edge("reset", "value", "reset-pulse", "value"),
                edge("count-pulse", "value", "counter", "count"),
                edge("reset-pulse", "value", "counter", "reset"),
            ],
        );
        let graph = definition.compile().unwrap();
        let initial = Value::State(f64::from(initial));
        let mut a = State::new(&graph);
        let b = State::new(&graph);
        for held in [true, true, false, true] {
            a.evaluate(&graph, key_input("a", 65, held)).unwrap();
            assert_eq!(a.outputs[4]["value"], Value::State(NUMERIC_LIMIT));
        }
        assert_eq!(b.outputs[4]["value"], initial);
        a.evaluate(&graph, key_input("a", 66, true)).unwrap();
        assert_eq!(a.outputs[4]["value"], initial);
        a.evaluate(&graph, key_input("a", 65, false)).unwrap();
        a.evaluate(&graph, key_input("a", 65, true)).unwrap();
        a.clear();
        assert_eq!(a.outputs[4]["value"], initial);

        definition.edges[3].from.node = "count-pulse".into();
        let graph = definition.compile().unwrap();
        let mut state = State::new(&graph);
        state.evaluate(&graph, key_input("a", 65, true)).unwrap();
        assert_eq!(state.outputs[4]["value"], initial);
    }

    #[test]
    fn transient_switch_only_forwards_fresh_selected_input() {
        let graph = definition(
            vec![
                node(
                    "a",
                    NodeKind::Wheel {
                        user: "a".into(),
                        horizontal: false,
                        disposition: Disposition::Passthrough,
                    },
                ),
                node(
                    "b",
                    NodeKind::Wheel {
                        user: "b".into(),
                        horizontal: false,
                        disposition: Disposition::Passthrough,
                    },
                ),
                key("select", "a", 65),
                node(
                    "switch",
                    NodeKind::Switch {
                        signal: SignalType::Impulse,
                    },
                ),
                sink("sink", "c", OutputChannel::Wheel { horizontal: false }),
            ],
            vec![
                edge("a", "value", "switch", "off"),
                edge("b", "value", "switch", "on"),
                edge("select", "value", "switch", "select"),
                edge("switch", "value", "sink", "value"),
            ],
        )
        .compile()
        .unwrap();
        let mut state = State::new(&graph);
        for (input, expected) in [
            (wheel("a", 120.0), Some(Value::Impulse(120.0))),
            (wheel("b", -120.0), None),
            (key_input("a", 65, true), None),
            (wheel("a", 120.0), None),
            (wheel("b", 240.0), Some(Value::Impulse(240.0))),
            (key_input("a", 65, false), None),
        ] {
            let values: Vec<_> = state
                .evaluate(&graph, input)
                .unwrap()
                .into_iter()
                .map(|effect| effect.value)
                .collect();
            assert_eq!(values, expected.into_iter().collect::<Vec<_>>());
        }
    }

    #[test]
    fn persistent_switch_requires_branch_seeds_and_routes_only_the_selected_value() {
        let mut definition = definition(
            vec![
                pointer("pointer", "a"),
                key("select", "a", 65),
                node(
                    "zero",
                    NodeKind::Constant {
                        value: Value::Position([10.0, 10.0]),
                    },
                ),
                node(
                    "switch",
                    NodeKind::Switch {
                        signal: SignalType::Position,
                    },
                ),
                sink("sink", "b", OutputChannel::Position),
            ],
            vec![
                edge("zero", "value", "switch", "off"),
                edge("pointer", "position", "switch", "on"),
                edge("select", "value", "switch", "select"),
                edge("switch", "value", "sink", "value"),
            ],
        );
        let error = definition.compile().err().unwrap();
        assert!(
            error
                .to_string()
                .contains("branch on needs an initial value")
        );
        definition.nodes.push(node(
            "seeded",
            NodeKind::DeltaToPosition {
                initial: [20.0, 30.0],
            },
        ));
        definition.edges[1].from.node = "seeded".into();
        definition.edges[1].from.port = "value".into();
        definition
            .edges
            .push(edge("pointer", "delta", "seeded", "value"));
        let graph = definition.compile().unwrap();
        let mut state = State::new(&graph);
        assert!(
            state
                .evaluate(&graph, motion("a", [2.0, 3.0]))
                .unwrap()
                .is_empty()
        );
        let selected = state.evaluate(&graph, key_input("a", 65, true)).unwrap();
        assert_eq!(selected[0].value, Value::Position([22.0, 33.0]));
        let deselected = state.evaluate(&graph, key_input("a", 65, false)).unwrap();
        assert_eq!(deselected[0].value, Value::Position([10.0, 10.0]));
    }

    #[test]
    fn position_delta_conversion_has_explicit_initialization() {
        let graph = definition(
            vec![
                pointer("p", "a"),
                node("difference", NodeKind::PositionToDelta),
                node(
                    "integral",
                    NodeKind::DeltaToPosition {
                        initial: [10.0, 20.0],
                    },
                ),
                sink("out", "b", OutputChannel::Position),
            ],
            vec![
                edge("p", "position", "difference", "value"),
                edge("difference", "value", "integral", "value"),
                edge("integral", "value", "out", "value"),
            ],
        )
        .compile()
        .unwrap();
        let mut state = State::new(&graph);
        assert!(
            state
                .evaluate(&graph, motion("a", [1.0, 2.0]))
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            state.evaluate(&graph, motion("a", [1.5, 1.0])).unwrap()[0].value,
            Value::Position([10.5, 19.0])
        );
    }

    #[test]
    fn scalar_accumulator_rolls_back_on_invalid_wave() {
        let graph = definition(
            vec![
                node(
                    "wheel",
                    NodeKind::Wheel {
                        user: "a".into(),
                        horizontal: false,
                        disposition: Disposition::Passthrough,
                    },
                ),
                node("sum", NodeKind::Accumulate { initial: 0.0 }),
                node(
                    "vec",
                    NodeKind::Compose {
                        signal: VectorType::Position,
                    },
                ),
                node(
                    "zero",
                    NodeKind::Constant {
                        value: Value::State(0.0),
                    },
                ),
                sink("out", "b", OutputChannel::Position),
            ],
            vec![
                edge("wheel", "value", "sum", "value"),
                edge("sum", "value", "vec", "x"),
                edge("zero", "value", "vec", "y"),
                edge("vec", "value", "out", "value"),
            ],
        )
        .compile()
        .unwrap();
        let mut state = State::new(&graph);
        assert_eq!(
            state.evaluate(&graph, wheel("a", NUMERIC_LIMIT)).unwrap()[0].value,
            Value::Position([NUMERIC_LIMIT, 0.0])
        );
        assert!(state.evaluate(&graph, wheel("a", 1.0)).is_err());
        assert!(state.evaluate(&graph, wheel("a", f64::INFINITY)).is_err());
        assert_eq!(
            state.evaluate(&graph, wheel("a", -1.0)).unwrap()[0].value,
            Value::Position([NUMERIC_LIMIT - 1.0, 0.0])
        );
    }

    #[test]
    fn vector_processing_preserves_units_and_fractional_components() {
        let graph = definition(
            vec![
                pointer("p", "a"),
                node(
                    "rotate",
                    NodeKind::RotateDelta {
                        radians: std::f64::consts::FRAC_PI_2,
                    },
                ),
                node(
                    "parts",
                    NodeKind::Components {
                        signal: VectorType::Delta,
                    },
                ),
                node(
                    "scale",
                    NodeKind::ScaleScalar {
                        signal: ScalarType::Impulse,
                        factor: 0.5,
                    },
                ),
                node(
                    "compose",
                    NodeKind::Compose {
                        signal: VectorType::Delta,
                    },
                ),
                node("dead", NodeKind::DeadZoneDelta { radius: 0.25 }),
                sink("out", "b", OutputChannel::Delta),
            ],
            vec![
                edge("p", "delta", "rotate", "value"),
                edge("rotate", "value", "parts", "value"),
                edge("parts", "x", "scale", "value"),
                edge("scale", "value", "compose", "x"),
                edge("parts", "y", "compose", "y"),
                edge("compose", "value", "dead", "value"),
                edge("dead", "value", "out", "value"),
            ],
        )
        .compile()
        .unwrap();
        let mut state = State::new(&graph);
        let effect = state.evaluate(&graph, motion("a", [2.0, 4.0])).unwrap();
        let Value::Delta(v) = &effect[0].value else {
            panic!("wrong signal units")
        };
        assert!((v[0] + 2.0).abs() < 1e-12 && (v[1] - 2.0).abs() < 1e-12);
        assert!(
            state
                .evaluate(&graph, motion("a", [0.1, 0.1]))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn scalar_math_comparison_and_logic_drive_explicit_hold() {
        let graph = definition(
            vec![
                node(
                    "wheel",
                    NodeKind::Wheel {
                        user: "a".into(),
                        horizontal: false,
                        disposition: Disposition::Passthrough,
                    },
                ),
                node("sum", NodeKind::Accumulate { initial: 0.0 }),
                node(
                    "clamp",
                    NodeKind::ClampScalar {
                        signal: ScalarType::State,
                        min: -10.0,
                        max: 10.0,
                    },
                ),
                node(
                    "two",
                    NodeKind::Constant {
                        value: Value::State(2.0),
                    },
                ),
                node(
                    "math",
                    NodeKind::ScalarMath {
                        signal: ScalarType::State,
                        operation: Arithmetic::Divide,
                    },
                ),
                node(
                    "compare",
                    NodeKind::Compare {
                        signal: ScalarType::State,
                        operation: Comparison::Greater,
                    },
                ),
                node(
                    "zero",
                    NodeKind::Constant {
                        value: Value::State(0.0),
                    },
                ),
                node(
                    "not",
                    NodeKind::Logic {
                        operation: Logic::Not,
                    },
                ),
                sink("out", "b", OutputChannel::ButtonHold { button: 1 }),
            ],
            vec![
                edge("wheel", "value", "sum", "value"),
                edge("sum", "value", "clamp", "value"),
                edge("clamp", "value", "math", "a"),
                edge("two", "value", "math", "b"),
                edge("math", "value", "compare", "a"),
                edge("zero", "value", "compare", "b"),
                edge("compare", "value", "not", "a"),
                edge("not", "value", "out", "value"),
            ],
        )
        .compile()
        .unwrap();
        let mut state = State::new(&graph);
        assert_eq!(
            state.evaluate(&graph, wheel("a", -1.0)).unwrap()[0].value,
            Value::Hold(true)
        );
        assert_eq!(
            state.evaluate(&graph, wheel("a", 2.0)).unwrap()[0].value,
            Value::Hold(false)
        );
        assert!(
            state.evaluate(&graph, wheel("a", 1.0)).unwrap().is_empty(),
            "unchanged boolean output must not repeat DOWN/UP"
        );
    }

    #[test]
    fn wheel_pulses_are_explicit_nonzero_and_expire() {
        let graph = definition(
            vec![
                node(
                    "wheel",
                    NodeKind::Wheel {
                        user: "a".into(),
                        horizontal: false,
                        disposition: Disposition::Passthrough,
                    },
                ),
                node("pulse", NodeKind::ImpulseToPulse),
                sink("out", "b", OutputChannel::KeyPulse { key: 65 }),
            ],
            vec![
                edge("wheel", "value", "pulse", "value"),
                edge("pulse", "value", "out", "value"),
            ],
        )
        .compile()
        .unwrap();
        let mut state = State::new(&graph);
        assert!(state.evaluate(&graph, wheel("a", 0.0)).unwrap().is_empty());
        assert_eq!(
            state.evaluate(&graph, wheel("a", 0.25)).unwrap()[0].value,
            Value::Pulse(true)
        );
        assert_eq!(
            state.evaluate(&graph, wheel("a", -0.25)).unwrap()[0].value,
            Value::Pulse(true)
        );
        assert!(
            state
                .evaluate(&graph, key_input("a", 65, true))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn source_effect_cap_counts_pulse_down_and_up_before_admission() {
        let mut nodes = vec![key("key", "a", 65)];
        let mut edges = vec![];
        for i in 0..=MAX_EFFECTS {
            let name = format!("s{i:03}");
            nodes.push(sink(&name, "b", OutputChannel::KeyHold { key: 66 }));
            edges.push(edge("key", "value", &name, "value"));
        }
        assert!(definition(nodes, edges).compile().is_err());
        let mut nodes = vec![key("key", "a", 65), node("rise", NodeKind::RisingEdge)];
        let mut edges = vec![edge("key", "value", "rise", "value")];
        for i in 0..=MAX_EFFECTS / 2 {
            let name = format!("s{i:03}");
            nodes.push(sink(&name, "b", OutputChannel::KeyPulse { key: 66 }));
            edges.push(edge("rise", "value", &name, "value"));
        }
        assert!(definition(nodes.clone(), edges.clone()).compile().is_err());
        nodes.pop();
        edges.pop();
        let graph = definition(nodes, edges).compile().unwrap();
        let effects = State::new(&graph)
            .evaluate(&graph, key_input("a", 65, true))
            .unwrap();
        assert_eq!(
            effects
                .iter()
                .map(|e| e.channel.effect_cost())
                .sum::<usize>(),
            MAX_EFFECTS
        );
    }

    #[test]
    fn unrelated_inputs_do_not_replay_persistent_sinks_or_old_impulses() {
        let graph = definition(
            vec![
                key("key", "a", 65),
                node(
                    "wheel",
                    NodeKind::Wheel {
                        user: "a".into(),
                        horizontal: false,
                        disposition: Disposition::Passthrough,
                    },
                ),
                sink("hold", "b", OutputChannel::KeyHold { key: 66 }),
                sink("wheel-out", "b", OutputChannel::Wheel { horizontal: false }),
            ],
            vec![
                edge("key", "value", "hold", "value"),
                edge("wheel", "value", "wheel-out", "value"),
            ],
        )
        .compile()
        .unwrap();
        let mut state = State::new(&graph);
        assert_eq!(
            state
                .evaluate(&graph, key_input("a", 65, true))
                .unwrap()
                .len(),
            1
        );
        let effects = state.evaluate(&graph, wheel("a", 3.0)).unwrap();
        assert_eq!(effects.len(), 1);
        assert_eq!(effects[0].sink, "wheel-out");
        assert!(
            state
                .evaluate(&graph, key_input("a", 65, true))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn monitors_require_a_typed_input_from_virtual_source_ancestry() {
        let monitor = node(
            "monitor",
            NodeKind::Monitor {
                signal: SignalType::Hold,
            },
        );
        assert_eq!(
            monitor.kind.ports(),
            Ports {
                inputs: vec![Port {
                    name: "value".into(),
                    signal: SignalType::Hold,
                }],
                outputs: vec![],
            }
        );

        let constant = node(
            "constant",
            NodeKind::Constant {
                value: Value::Hold(true),
            },
        );
        let source = pointer("pointer", "a");
        let invalid_edges = [
            (
                edge("constant", "value", "monitor", "value"),
                "virtual input source",
            ),
            (
                edge("pointer", "delta", "monitor", "value"),
                "incompatible semantic mapper port types",
            ),
            (
                edge("monitor", "value", "pointer", "value"),
                "unknown mapper output port",
            ),
        ];
        for (edge, expected_error) in invalid_edges {
            let error = definition(
                vec![monitor.clone(), constant.clone(), source.clone()],
                vec![edge],
            )
            .compile()
            .unwrap_err();
            assert!(error.to_string().contains(expected_error));
        }

        let mut nodes = vec![key("key", "a", 65)];
        let mut edges = Vec::new();
        for index in 0..MAX_MONITORS {
            let id = format!("monitor-{index}");
            nodes.push(node(&id, monitor.kind.clone()));
            edges.push(edge("key", "value", &id, "value"));
        }
        assert!(definition(nodes.clone(), edges.clone()).validate().is_ok());
        nodes.push(monitor);
        edges.push(edge("key", "value", "monitor", "value"));
        assert!(
            definition(nodes, edges)
                .compile()
                .unwrap_err()
                .to_string()
                .contains("mapper exceeds 16 monitors")
        );
    }

    #[test]
    fn monitors_record_only_committed_values_and_clear_per_actor() {
        let graph = definition(
            vec![
                key("key", "a", 65),
                node(
                    "z-hold",
                    NodeKind::Monitor {
                        signal: SignalType::Hold,
                    },
                ),
                pointer("pointer", "a"),
                node(
                    "a-delta",
                    NodeKind::Monitor {
                        signal: SignalType::Delta,
                    },
                ),
                node("z-scale", NodeKind::ScaleDelta { factor: 2.0 }),
            ],
            vec![
                edge("key", "value", "z-hold", "value"),
                edge("pointer", "delta", "a-delta", "value"),
                edge("pointer", "delta", "z-scale", "value"),
            ],
        )
        .compile()
        .unwrap();
        let mut state = State::new(&graph);
        let other_actor = State::new(&graph);
        assert!(state.monitor_samples().is_empty());
        let press = key_input("a", 65, true);
        assert!(graph.affected_outputs(&press.source()).is_empty());
        for input in [press, motion("a", [0.0, 0.0]), key_input("a", 65, false)] {
            assert!(state.evaluate(&graph, input).unwrap().is_empty());
        }
        assert!(
            state
                .evaluate(&graph, motion("a", [NUMERIC_LIMIT, 0.0]))
                .is_err()
        );
        assert_eq!(
            state.monitor_samples(),
            vec![
                MonitorSample {
                    node: "a-delta".into(),
                    value: Value::Delta([0.0, 0.0]),
                    samples: 1,
                },
                MonitorSample {
                    node: "z-hold".into(),
                    value: Value::Hold(false),
                    samples: 2,
                },
            ]
        );
        assert!(other_actor.monitor_samples().is_empty());
        state.clear();
        assert!(state.monitor_samples().is_empty());
        assert!(
            state
                .evaluate(&graph, key_input("a", 65, true))
                .unwrap()
                .is_empty()
        );
        assert_eq!(state.monitor_samples()[0].samples, 1);
    }

    #[test]
    fn monitors_observe_committed_timer_pulses_without_emissions() {
        let graph = definition(
            vec![
                key("key", "a", 65),
                node(
                    "timer",
                    NodeKind::Timer {
                        interval_ms: 100,
                        count: 2,
                    },
                ),
                node(
                    "a-monitor",
                    NodeKind::Monitor {
                        signal: SignalType::Pulse,
                    },
                ),
                node(
                    "counter",
                    NodeKind::Counter {
                        initial: 499_999_999,
                        with_reset: false,
                    },
                ),
                node(
                    "scale",
                    NodeKind::ScaleScalar {
                        signal: ScalarType::State,
                        factor: 2.0,
                    },
                ),
            ],
            vec![
                edge("key", "value", "timer", "enabled"),
                edge("timer", "value", "a-monitor", "value"),
                edge("timer", "value", "counter", "count"),
                edge("counter", "value", "scale", "value"),
            ],
        )
        .compile()
        .unwrap();
        let (index, _, _) = graph.timers().next().unwrap();
        let mut state = State::new(&graph);
        assert!(graph.timer_outputs(index).is_empty());
        assert!(state.evaluate_timer(&graph, index).is_err());
        assert!(
            state
                .evaluate(&graph, key_input("a", 65, true))
                .unwrap()
                .is_empty()
        );
        assert!(state.monitor_samples().is_empty());
        assert!(state.evaluate_timer(&graph, index).unwrap().is_empty());
        assert!(state.evaluate_timer(&graph, index).is_err());
        assert_eq!(
            state.monitor_samples(),
            vec![MonitorSample {
                node: "a-monitor".into(),
                value: Value::Pulse(true),
                samples: 1,
            }]
        );
    }

    #[test]
    fn scope_traces_record_bounded_committed_samples_per_actor() {
        let graph = definition(
            vec![
                node(
                    "wheel",
                    NodeKind::Wheel {
                        user: "a".into(),
                        horizontal: false,
                        disposition: Disposition::Passthrough,
                    },
                ),
                node(
                    "scope",
                    NodeKind::Scope {
                        signal: SignalType::Impulse,
                    },
                ),
                node(
                    "z-sum",
                    NodeKind::Accumulate {
                        initial: NUMERIC_LIMIT,
                    },
                ),
            ],
            vec![
                edge("wheel", "value", "scope", "value"),
                edge("wheel", "value", "z-sum", "value"),
            ],
        )
        .compile()
        .unwrap();
        let mut state = State::new(&graph);
        let untouched_actor = State::new(&graph);
        for i in 0..(MAX_SCOPE_SAMPLES as u64 + 2) {
            state.evaluate_at(&graph, wheel("a", 0.0), i * 10).unwrap();
        }
        let trace = &state.scope_traces()[0];
        assert_eq!(trace.node, "scope");
        assert_eq!(trace.samples.len(), MAX_SCOPE_SAMPLES);
        assert_eq!(trace.samples.first().unwrap().elapsed_us, 20);
        assert_eq!(
            trace.samples.last().unwrap().elapsed_us,
            (MAX_SCOPE_SAMPLES as u64 + 1) * 10
        );
        assert_eq!(trace.samples.first().unwrap().value, Value::Impulse(0.0));
        assert!(untouched_actor.scope_traces().is_empty());
        let before_failure = state.scope_traces();
        assert!(state.evaluate_at(&graph, wheel("a", 1.0), 999).is_err());
        assert_eq!(state.scope_traces(), before_failure);
        assert_eq!(state.scope_traces()[0].samples.len(), MAX_SCOPE_SAMPLES);
        state.clear();
        assert!(state.scope_traces().is_empty());
    }

    #[test]
    fn passive_visualizers_validate_typed_bounds_and_record_real_values() {
        let led = NodeKind::Led;
        let gauge = NodeKind::Gauge {
            signal: ScalarType::State,
            min: 0.0,
            max: 100.0,
        };
        let xy = NodeKind::XyPad {
            signal: VectorType::Position,
            min: [0.0, 0.0],
            max: [1920.0, 1080.0],
        };
        let path = NodeKind::PathTracer {
            min: [0.0, 0.0],
            max: [1920.0, 1080.0],
        };
        assert_eq!(led.latest_observer_signal(), Some(SignalType::Hold));
        assert_eq!(gauge.latest_observer_signal(), Some(SignalType::State));
        assert_eq!(xy.latest_observer_signal(), Some(SignalType::Position));
        assert_eq!(path.trace_observer_signal(), Some(SignalType::Position));
        for kind in [&led, &gauge, &xy, &path] {
            assert!(kind.ports().outputs.is_empty());
            assert_eq!(kind.ports().inputs[0].name, "value");
            kind.validate().unwrap();
        }
        assert!(
            NodeKind::Gauge {
                signal: ScalarType::State,
                min: 1.0,
                max: 1.0
            }
            .validate()
            .is_err()
        );
        assert!(
            NodeKind::XyPad {
                signal: VectorType::Position,
                min: [0.0, 0.0],
                max: [1.0, f64::INFINITY]
            }
            .validate()
            .is_err()
        );
        assert!(
            NodeKind::PathTracer {
                min: [0.0, 0.0],
                max: [NUMERIC_LIMIT * 2.0, 10.0]
            }
            .validate()
            .is_err()
        );

        let graph = definition(
            vec![
                key("key", "a", 65),
                node("led", led),
                node(
                    "wheel",
                    NodeKind::Wheel {
                        user: "a".into(),
                        horizontal: false,
                        disposition: Disposition::Passthrough,
                    },
                ),
                node("sum", NodeKind::Accumulate { initial: 0.0 }),
                node("gauge", gauge),
                pointer("pointer", "a"),
                node("xy", xy),
                node("path", path),
            ],
            vec![
                edge("key", "value", "led", "value"),
                edge("wheel", "value", "sum", "value"),
                edge("sum", "value", "gauge", "value"),
                edge("pointer", "position", "xy", "value"),
                edge("pointer", "position", "path", "value"),
            ],
        )
        .compile()
        .unwrap();
        let mut state = State::new(&graph);
        state
            .evaluate_at(&graph, key_input("a", 65, true), 10)
            .unwrap();
        state.evaluate_at(&graph, wheel("a", 7.0), 20).unwrap();
        state
            .evaluate_at(&graph, motion("a", [320.0, 240.0]), 30)
            .unwrap();
        let samples = state.monitor_samples();
        assert_eq!(
            samples
                .iter()
                .find(|sample| sample.node == "led")
                .unwrap()
                .value,
            Value::Hold(true)
        );
        assert_eq!(
            samples
                .iter()
                .find(|sample| sample.node == "gauge")
                .unwrap()
                .value,
            Value::State(7.0)
        );
        assert_eq!(
            samples
                .iter()
                .find(|sample| sample.node == "xy")
                .unwrap()
                .value,
            Value::Position([320.0, 240.0])
        );
        assert_eq!(
            state.scope_traces()[0].samples[0].value,
            Value::Position([320.0, 240.0])
        );
    }

    #[test]
    fn catalogue_ports_and_json_round_trip_are_complete() {
        let catalogue = catalogue();
        let mut names = BTreeSet::new();
        for descriptor in catalogue {
            assert!(names.insert(descriptor.name));
            assert_eq!(descriptor.ports, descriptor.prototype.ports());
            descriptor.prototype.validate().unwrap();
            let json = serde_json::to_string(&descriptor.prototype).unwrap();
            assert_eq!(
                serde_json::from_str::<NodeKind>(&json).unwrap(),
                descriptor.prototype
            );
        }
        let graph = definition(
            vec![pointer("p", "a"), sink("s", "b", OutputChannel::Delta)],
            vec![edge("p", "delta", "s", "value")],
        );
        assert_eq!(
            Definition::from_json(&graph.to_json().unwrap()).unwrap(),
            graph
        );
    }

    #[test]
    fn category_browse_is_complete_stable_and_bounded() {
        let all = catalogue();
        let browsed = browse_catalogue("", None).unwrap();
        assert_eq!(
            browsed, all,
            "browsing must preserve prototypes, ports and order"
        );
        let mut names = BTreeSet::new();
        for descriptor in &all {
            assert!(names.insert(descriptor.name.as_str()));
            assert_eq!(descriptor.category, descriptor.prototype.category());
            assert_eq!(descriptor.ports, descriptor.prototype.ports());
            assert!(!descriptor.category.label().is_empty());
            assert!(!descriptor.category.description().is_empty());
        }
        for category in NodeCategory::ALL {
            let in_category = browse_catalogue("", Some(category)).unwrap();
            assert!(!in_category.is_empty(), "every category has a prototype");
            assert!(in_category.iter().all(|node| node.category == category));
            let serialized = serde_json::to_string(&category).unwrap();
            assert_eq!(
                serde_json::from_str::<NodeCategory>(&serialized).unwrap(),
                category
            );
        }
        for (name, expected) in [
            ("pointer", NodeCategory::Sources),
            ("constant", NodeCategory::Constants),
            ("normalize_vector", NodeCategory::Vector),
            ("scalar_math", NodeCategory::Scalar),
            ("compare", NodeCategory::Binary),
            ("switch", NodeCategory::Routing),
            ("position_to_delta", NodeCategory::Conversion),
            ("monitor", NodeCategory::Observers),
            ("sink", NodeCategory::Outputs),
        ] {
            assert_eq!(
                all.iter().find(|node| node.name == name).unwrap().category,
                expected
            );
        }
        assert_eq!(
            serde_json::to_string(&NodeCategory::Sources).unwrap(),
            "\"sources\""
        );

        let filtered = browse_catalogue("  wHeEl  ", Some(NodeCategory::Sources)).unwrap();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].name, "wheel");
        assert!(
            browse_catalogue("wheel", Some(NodeCategory::Outputs))
                .unwrap()
                .is_empty()
        );
        assert!(
            browse_catalogue("no matching catalogue entry", None)
                .unwrap()
                .is_empty()
        );
        assert!(browse_catalogue("鼠", None).unwrap().is_empty());
        assert!(
            browse_catalogue(&"é".repeat(64), None).is_ok(),
            "128 UTF-8 bytes are allowed"
        );
        assert!(
            browse_catalogue(&"é".repeat(65), None).is_err(),
            "the limit counts UTF-8 bytes"
        );
        assert!(browse_catalogue(&"x".repeat(129), None).is_err());
        assert!(browse_catalogue("pointer\n", None).is_err());
        assert!(browse_catalogue("a\u{0085}b", None).is_err());
    }

    #[test]
    fn unary_and_vector_math_are_typed_bounded_and_round_trip() {
        let cases = [
            (UnaryOperation::Abs, -2.5, 2.5),
            (UnaryOperation::Negate, -2.5, 2.5),
            (UnaryOperation::Sqrt, 9.0, 3.0),
            (UnaryOperation::Square, -3.0, 9.0),
            (UnaryOperation::Round, 1.6, 2.0),
            (UnaryOperation::Floor, 1.6, 1.0),
            (UnaryOperation::Ceil, 1.1, 2.0),
            (UnaryOperation::Sin, std::f64::consts::FRAC_PI_2, 1.0),
            (UnaryOperation::Cos, 0.0, 1.0),
            (UnaryOperation::Sign, -2.5, -1.0),
            (UnaryOperation::Sign, 2.5, 1.0),
        ];
        for (operation, input, expected) in cases {
            let node = NodeKind::UnaryMath {
                signal: ScalarType::State,
                operation,
            };
            assert_eq!(node.ports().inputs[0].signal, SignalType::State);
            assert_eq!(node.ports().outputs[0].signal, SignalType::State);
            let output = evaluate_node(
                &node,
                &BTreeMap::from([("value", Value::State(input))]),
                &BTreeSet::from(["value"]),
                &mut Memory::default(),
                None,
            )
            .unwrap();
            assert_eq!(output[0].1, Value::State(expected));

            let node = NodeKind::UnaryMath {
                signal: ScalarType::Impulse,
                operation,
            };
            assert_eq!(node.ports().inputs[0].signal, SignalType::Impulse);
            assert_eq!(node.ports().outputs[0].signal, SignalType::Impulse);
            let input = if operation == UnaryOperation::Sqrt {
                9.0
            } else {
                input
            };
            let output = evaluate_node(
                &node,
                &BTreeMap::from([("value", Value::Impulse(input))]),
                &BTreeSet::from(["value"]),
                &mut Memory::default(),
                None,
            )
            .unwrap();
            assert_eq!(output[0].1, Value::Impulse(expected));
        }
        for zero in [0.0, -0.0] {
            let sign = NodeKind::UnaryMath {
                signal: ScalarType::State,
                operation: UnaryOperation::Sign,
            };
            assert_eq!(
                evaluate_node(
                    &sign,
                    &BTreeMap::from([("value", Value::State(zero))]),
                    &BTreeSet::from(["value"]),
                    &mut Memory::default(),
                    None
                )
                .unwrap()[0]
                    .1,
                Value::State(0.0)
            );
        }
        let magnitude = NodeKind::VectorLength {
            signal: VectorType::Delta,
        };
        let normalize = NodeKind::NormalizeVector {
            signal: VectorType::Delta,
        };
        assert_eq!(magnitude.ports().outputs[0].signal, SignalType::Impulse);
        assert_eq!(normalize.ports().outputs[0].signal, SignalType::Delta);
        let eval = |node: &NodeKind, v| {
            evaluate_node(
                node,
                &BTreeMap::from([("value", Value::Delta(v))]),
                &BTreeSet::from(["value"]),
                &mut Memory::default(),
                None,
            )
            .unwrap()[0]
                .1
                .clone()
        };
        assert_eq!(eval(&normalize, [0.0, 0.0]), Value::Delta([0.0, 0.0]));
        assert_eq!(eval(&normalize, [1.0e-300, 0.0]), Value::Delta([1.0, 0.0]));
        let tiny_diagonal = eval(&normalize, [f64::from_bits(1), f64::from_bits(1)]);
        if let Value::Delta([x, y]) = tiny_diagonal {
            assert!((x - std::f64::consts::FRAC_1_SQRT_2).abs() < 1.0e-15);
            assert!((y - std::f64::consts::FRAC_1_SQRT_2).abs() < 1.0e-15);
        } else {
            panic!("normalization must preserve the vector signal type");
        }
        let Value::Delta(large_normalized) = eval(&normalize, [NUMERIC_LIMIT, NUMERIC_LIMIT])
        else {
            panic!("normalization must preserve delta semantics");
        };
        for component in large_normalized {
            assert!((component - std::f64::consts::FRAC_1_SQRT_2).abs() < 1.0e-15);
        }
        assert!(
            eval(&magnitude, [NUMERIC_LIMIT, NUMERIC_LIMIT])
                .validate()
                .is_err()
        );
        assert!(
            evaluate_node(
                &NodeKind::UnaryMath {
                    signal: ScalarType::State,
                    operation: UnaryOperation::Sqrt
                },
                &BTreeMap::from([("value", Value::State(-1.0))]),
                &BTreeSet::from(["value"]),
                &mut Memory::default(),
                None
            )
            .unwrap()[0]
                .1
                .validate()
                .is_err()
        );
        assert!(
            evaluate_node(
                &NodeKind::UnaryMath {
                    signal: ScalarType::State,
                    operation: UnaryOperation::Square
                },
                &BTreeMap::from([("value", Value::State(NUMERIC_LIMIT))]),
                &BTreeSet::from(["value"]),
                &mut Memory::default(),
                None
            )
            .unwrap()[0]
                .1
                .validate()
                .is_err()
        );

        let math_nodes = [
            NodeKind::UnaryMath {
                signal: ScalarType::State,
                operation: UnaryOperation::Abs,
            },
            magnitude,
            normalize,
        ];
        for node in math_nodes {
            let json = serde_json::to_string(&node).unwrap();
            assert_eq!(serde_json::from_str::<NodeKind>(&json).unwrap(), node);
            node.validate().unwrap();
        }
        let schema = schemars::schema_for!(UnaryOperation);
        let schema_text = serde_json::to_string(&schema).unwrap();
        for operation in [
            "abs", "negate", "sqrt", "square", "round", "floor", "ceil", "sin", "cos", "sign",
        ] {
            assert!(schema_text.contains(operation));
        }
        let catalogue = catalogue();
        for (name, label) in [
            ("unary_math", "Scalar function"),
            ("vector_length", "Vector magnitude"),
            ("normalize_vector", "Normalize vector"),
        ] {
            let descriptor = catalogue.iter().find(|entry| entry.name == name).unwrap();
            assert_eq!(descriptor.label, label);
            assert_eq!(descriptor.ports, descriptor.prototype.ports());
        }
    }

    #[test]
    fn invalid_vector_magnitude_rolls_back_actor_state_and_observers() {
        let graph = definition(
            vec![
                pointer("pointer", "a"),
                node(
                    "position_watch",
                    NodeKind::Monitor {
                        signal: SignalType::Position,
                    },
                ),
                node(
                    "magnitude",
                    NodeKind::VectorLength {
                        signal: VectorType::Position,
                    },
                ),
                node(
                    "magnitude_watch",
                    NodeKind::Monitor {
                        signal: SignalType::State,
                    },
                ),
            ],
            vec![
                edge("pointer", "position", "position_watch", "value"),
                edge("pointer", "position", "magnitude", "value"),
                edge("magnitude", "value", "magnitude_watch", "value"),
            ],
        )
        .compile()
        .unwrap();
        let mut actor = State::new(&graph);
        let other_actor = State::new(&graph);
        assert!(
            actor
                .evaluate(&graph, motion("a", [NUMERIC_LIMIT, NUMERIC_LIMIT]))
                .is_err()
        );
        assert!(actor.monitor_samples().is_empty());
        assert!(actor.evaluate(&graph, motion("a", [3.0, 4.0])).is_ok());
        let samples: BTreeMap<_, _> = actor
            .monitor_samples()
            .into_iter()
            .map(|sample| (sample.node, sample.value))
            .collect();
        assert_eq!(samples.len(), 2);
        assert_eq!(samples["position_watch"], Value::Position([3.0, 4.0]));
        assert_eq!(samples["magnitude_watch"], Value::State(5.0));
        assert!(other_actor.monitor_samples().is_empty());
    }

    #[test]
    fn remap_and_smooth_compute_real_values_and_validate_parameters() {
        let remap = NodeKind::Remap {
            signal: ScalarType::State,
            input_min: 0.0,
            input_max: 10.0,
            output_min: 100.0,
            output_max: 0.0,
            clamp: false,
        };
        assert!(remap.validate().is_ok());
        assert!(
            NodeKind::Remap {
                signal: ScalarType::State,
                input_min: 1.0,
                input_max: 1.0,
                output_min: 0.0,
                output_max: 1.0,
                clamp: false
            }
            .validate()
            .is_err()
        );
        let input = BTreeMap::from([("value", Value::State(2.5))]);
        let mut memory = Memory::default();
        assert_eq!(
            evaluate_node(
                &remap,
                &input,
                &BTreeSet::from(["value"]),
                &mut memory,
                None
            )
            .unwrap()[0]
                .1,
            Value::State(75.0)
        );
        let constant = NodeKind::Remap {
            signal: ScalarType::State,
            input_min: 0.0,
            input_max: 1.0e-300,
            output_min: 7.0,
            output_max: 7.0,
            clamp: false,
        };
        let tiny = BTreeMap::from([("value", Value::State(1.0))]);
        assert_eq!(
            evaluate_node(
                &constant,
                &tiny,
                &BTreeSet::from(["value"]),
                &mut memory,
                None
            )
            .unwrap()[0]
                .1,
            Value::State(7.0)
        );
        let smooth = NodeKind::Smooth {
            signal: SignalType::State,
            alpha: 0.5,
        };
        let mut memory = Memory::default();
        assert_eq!(
            evaluate_node(
                &smooth,
                &input,
                &BTreeSet::from(["value"]),
                &mut memory,
                None
            )
            .unwrap()[0]
                .1,
            Value::State(2.5)
        );
        let input = BTreeMap::from([("value", Value::State(4.5))]);
        assert_eq!(
            evaluate_node(
                &smooth,
                &input,
                &BTreeSet::from(["value"]),
                &mut memory,
                Some(1)
            )
            .unwrap()[0]
                .1,
            Value::State(3.5)
        );
        assert!(
            NodeKind::Smooth {
                signal: SignalType::Impulse,
                alpha: 0.5
            }
            .validate()
            .is_err()
        );
        assert!(
            NodeKind::Smooth {
                signal: SignalType::Position,
                alpha: 0.0
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn velocity_uses_positive_elapsed_time_and_is_actor_local() {
        let graph = definition(
            vec![
                pointer("pointer", "a"),
                node("velocity", NodeKind::Velocity),
                node(
                    "watch",
                    NodeKind::Monitor {
                        signal: SignalType::State,
                    },
                ),
            ],
            vec![
                edge("pointer", "position", "velocity", "value"),
                edge("velocity", "value", "watch", "value"),
            ],
        )
        .compile()
        .unwrap();
        let mut actor = State::new(&graph);
        let untouched = State::new(&graph);
        actor
            .evaluate_at(&graph, motion("a", [10.0, 10.0]), 100)
            .unwrap();
        assert_eq!(actor.monitor_samples()[0].value, Value::State(0.0));
        actor
            .evaluate_at(&graph, motion("a", [20.0, 10.0]), 100)
            .unwrap();
        assert_eq!(
            actor.monitor_samples()[0].samples,
            1,
            "same-time samples emit nothing"
        );
        actor
            .evaluate_at(&graph, motion("a", [30.0, 10.0]), 50)
            .unwrap();
        assert_eq!(
            actor.monitor_samples()[0].samples,
            1,
            "backward time emits nothing"
        );
        actor
            .evaluate_at(&graph, motion("a", [40.0, 10.0]), 200)
            .unwrap();
        assert_eq!(actor.monitor_samples()[0].value, Value::State(300_000.0));
        assert!(untouched.monitor_samples().is_empty());
        let mut spike = State::new(&graph);
        spike
            .evaluate_at(&graph, motion("a", [0.0, 0.0]), 1)
            .unwrap();
        spike
            .evaluate_at(&graph, motion("a", [1.0e9, 1.0e9]), 2)
            .unwrap();
        assert_eq!(
            spike.monitor_samples()[0].value,
            Value::State(NUMERIC_LIMIT)
        );
    }

    #[test]
    fn groups_can_organize_a_draft_before_its_inputs_are_wired() {
        let draft = definition(
            vec![
                key("key", "a", 65),
                sink("sink", "b", OutputChannel::KeyHold { key: 66 }),
            ],
            vec![],
        );
        assert!(draft.validate().is_err());
        let grouped = draft
            .edit_group(GroupEdit::Create {
                group: NodeGroup {
                    id: "work".into(),
                    label: "Work in progress".into(),
                    nodes: vec!["key".into(), "sink".into()],
                    collapsed: false,
                },
            })
            .unwrap();
        let renamed = grouped
            .edit_group(GroupEdit::Rename {
                group_id: "work".into(),
                label: "Incomplete controls".into(),
            })
            .unwrap();
        let collapsed = renamed
            .edit_group(GroupEdit::SetCollapsed {
                group_id: "work".into(),
                collapsed: true,
            })
            .unwrap();
        assert!(collapsed.groups[0].collapsed);
        assert_eq!(collapsed.nodes, draft.nodes);
        assert_eq!(collapsed.edges, draft.edges);
        assert!(collapsed.validate().is_err());
        assert!(draft.groups.is_empty());
    }

    #[test]
    fn node_groups_round_trip_validate_and_do_not_change_execution() {
        let mut graph = definition(
            vec![
                key("key", "a", 65),
                sink("sink", "b", OutputChannel::KeyHold { key: 66 }),
            ],
            vec![edge("key", "value", "sink", "value")],
        );
        let legacy = graph.to_json().unwrap();
        assert!(!legacy.contains("\"groups\""));
        assert_eq!(Definition::from_json(&legacy).unwrap(), graph);

        graph.groups = vec![NodeGroup {
            id: "controls".into(),
            label: "Controls".into(),
            nodes: vec!["key".into(), "sink".into()],
            collapsed: true,
        }];
        let encoded = graph.to_json().unwrap();
        assert_eq!(Definition::from_json(&encoded).unwrap(), graph);
        let mut expanded = graph.clone();
        expanded.groups[0].collapsed = false;
        let ungrouped = definition(graph.nodes.clone(), graph.edges.clone());
        let evaluate = |definition: &Definition| {
            let compiled = definition.compile().unwrap();
            State::new(&compiled)
                .evaluate(&compiled, key_input("a", 65, true))
                .unwrap()
        };
        assert_eq!(evaluate(&graph), evaluate(&expanded));
        assert_eq!(evaluate(&graph), evaluate(&ungrouped));

        let mut rejected = graph.clone();
        rejected.groups[0].label = " \n ".into();
        assert!(rejected.validate().is_err());
        rejected = graph.clone();
        rejected.groups[0].label = "bad\u{7}".into();
        assert!(rejected.validate().is_err());
        rejected = graph.clone();
        rejected.groups[0].label = "x".repeat(129);
        assert!(rejected.validate().is_err());
        rejected = graph.clone();
        rejected.groups[0].id = "invalid id".into();
        assert!(rejected.validate().is_err());
        rejected = graph.clone();
        rejected.groups[0].nodes.push("missing".into());
        assert!(rejected.validate().is_err());
        rejected = graph.clone();
        rejected.groups[0].nodes.push("key".into());
        assert!(rejected.validate().is_err());
        rejected = graph.clone();
        rejected.groups.push(NodeGroup {
            id: "other".into(),
            label: "Other".into(),
            nodes: vec!["key".into()],
            collapsed: false,
        });
        assert!(rejected.validate().is_err());
        rejected = graph.clone();
        rejected.groups[0].nodes.clear();
        assert!(rejected.validate().is_err());
        rejected = graph.clone();
        rejected.groups = (0..=MAX_GROUPS)
            .map(|i| NodeGroup {
                id: format!("g{i}"),
                label: "Group".into(),
                nodes: vec![if i == MAX_GROUPS { "sink" } else { "key" }.into()],
                collapsed: false,
            })
            .collect();
        assert!(rejected.validate().is_err());

        let before = graph.clone();
        assert!(
            graph
                .edit_group(GroupEdit::Rename {
                    group_id: "missing".into(),
                    label: "Valid".into()
                })
                .is_err()
        );
        assert_eq!(graph, before);
        assert!(
            graph
                .edit_group(GroupEdit::Rename {
                    group_id: "controls".into(),
                    label: " \t ".into()
                })
                .is_err()
        );
        assert_eq!(graph, before);
        assert!(
            graph
                .edit_group(GroupEdit::Create {
                    group: graph.groups[0].clone()
                })
                .is_err()
        );
        assert_eq!(graph, before);
        let changed = graph
            .edit_group(GroupEdit::SetCollapsed {
                group_id: "controls".into(),
                collapsed: false,
            })
            .unwrap();
        assert!(!changed.groups[0].collapsed);
        assert!(graph.groups[0].collapsed);
    }

    #[test]
    fn delay_and_throttle_are_typed_bounded_routing_nodes() {
        for signal in [
            SignalType::Position,
            SignalType::Delta,
            SignalType::State,
            SignalType::Impulse,
            SignalType::Hold,
            SignalType::Pulse,
        ] {
            for kind in [
                NodeKind::Delay {
                    signal,
                    delay_ms: 1,
                },
                NodeKind::Throttle {
                    signal,
                    interval_ms: 1,
                },
            ] {
                kind.validate().unwrap();
                assert_eq!(kind.ports().inputs[0].signal, signal);
                assert_eq!(kind.ports().outputs[0].signal, signal);
                let json = serde_json::to_string(&kind).unwrap();
                assert_eq!(serde_json::from_str::<NodeKind>(&json).unwrap(), kind);
                assert_eq!(kind.category(), NodeCategory::Routing);
            }
        }
        for (signal, value) in [
            (SignalType::Position, Value::Position([0.0, 0.0])),
            (SignalType::Delta, Value::Delta([0.0, 0.0])),
            (SignalType::State, Value::State(0.0)),
            (SignalType::Impulse, Value::Impulse(0.0)),
            (SignalType::Hold, Value::Hold(false)),
            (SignalType::Pulse, Value::Pulse(true)),
        ] {
            let throttle = NodeKind::Throttle {
                signal,
                interval_ms: 1,
            };
            let values = BTreeMap::from([("value", value.clone())]);
            let mut memory = Memory::default();
            let first = evaluate_node(
                &throttle,
                &values,
                &BTreeSet::from(["value"]),
                &mut memory,
                Some(10),
            )
            .unwrap();
            assert_eq!(first.first().map(|(_, value)| value), Some(&value));
            if value == Value::Hold(false) {
                assert_eq!(memory.throttle_at_us, None, "false Hold bypasses quota");
                assert_eq!(
                    evaluate_node(
                        &throttle,
                        &values,
                        &BTreeSet::from(["value"]),
                        &mut memory,
                        Some(11)
                    )
                    .unwrap()[0]
                        .1,
                    value
                );
            } else {
                assert_eq!(memory.throttle_at_us, Some(10));
                assert!(
                    evaluate_node(
                        &throttle,
                        &values,
                        &BTreeSet::from(["value"]),
                        &mut memory,
                        Some(11)
                    )
                    .unwrap()
                    .is_empty()
                );
            }
        }
        let pulse_throttle = NodeKind::Throttle {
            signal: SignalType::Pulse,
            interval_ms: 1,
        };
        let mut pulse_memory = Memory::default();
        assert!(
            evaluate_node(
                &pulse_throttle,
                &BTreeMap::from([("value", Value::Pulse(false))]),
                &BTreeSet::from(["value"]),
                &mut pulse_memory,
                Some(10),
            )
            .unwrap()
            .is_empty()
        );
        assert_eq!(pulse_memory.throttle_at_us, None);
        assert_eq!(
            evaluate_node(
                &pulse_throttle,
                &BTreeMap::from([("value", Value::Pulse(true))]),
                &BTreeSet::from(["value"]),
                &mut pulse_memory,
                Some(11),
            )
            .unwrap()[0]
                .1,
            Value::Pulse(true)
        );
        assert!(
            NodeKind::Delay {
                signal: SignalType::State,
                delay_ms: 0
            }
            .validate()
            .is_err()
        );
        assert!(
            NodeKind::Delay {
                signal: SignalType::State,
                delay_ms: 60_001
            }
            .validate()
            .is_err()
        );
        assert!(
            NodeKind::Throttle {
                signal: SignalType::State,
                interval_ms: 0
            }
            .validate()
            .is_err()
        );
        assert!(
            NodeKind::Throttle {
                signal: SignalType::State,
                interval_ms: 60_001
            }
            .validate()
            .is_err()
        );
        let nodes = catalogue();
        for name in ["delay", "throttle"] {
            let descriptor = nodes.iter().find(|item| item.name == name).unwrap();
            assert_eq!(descriptor.category, NodeCategory::Routing);
            assert_eq!(descriptor.ports, descriptor.prototype.ports());
        }

        let graph = definition(
            vec![
                key("source", "a", 65),
                node(
                    "timer",
                    NodeKind::Timer {
                        interval_ms: 100,
                        count: 1,
                    },
                ),
                node(
                    "delay",
                    NodeKind::Delay {
                        signal: SignalType::Pulse,
                        delay_ms: 1,
                    },
                ),
                sink("output", "b", OutputChannel::KeyPulse { key: 66 }),
            ],
            vec![
                edge("source", "value", "timer", "enabled"),
                edge("timer", "value", "delay", "value"),
                edge("delay", "value", "output", "value"),
            ],
        )
        .compile()
        .unwrap();
        let (timer_index, _, _) = graph.timers().next().unwrap();
        let (delay_index, _) = graph.delays().next().unwrap();
        assert_eq!(graph.timer_outputs(timer_index).len(), 1);
        assert_eq!(graph.delay_outputs(delay_index).len(), 1);
        assert_eq!(graph.delay_outputs(delay_index)[0].sink, "output");

        for count in [MAX_DELAYS, MAX_DELAYS + 1] {
            let mut nodes = vec![key("source", "a", 65), node("rise", NodeKind::RisingEdge)];
            let mut edges = vec![edge("source", "value", "rise", "value")];
            let mut previous = "rise".to_string();
            for index in 0..count {
                let id = format!("delay-{index}");
                nodes.push(node(
                    &id,
                    NodeKind::Delay {
                        signal: SignalType::Pulse,
                        delay_ms: 1,
                    },
                ));
                edges.push(edge(&previous, "value", &id, "value"));
                previous = id;
            }
            let candidate = definition(nodes, edges).compile();
            if count == MAX_DELAYS {
                assert_eq!(candidate.unwrap().delays().count(), MAX_DELAYS);
            } else {
                assert!(
                    candidate
                        .unwrap_err()
                        .to_string()
                        .contains("16 Delay nodes")
                );
            }
        }
    }

    #[test]
    fn delay_preserves_all_six_signal_types_and_commits_due_waves_atomically() {
        let cases = [
            SignalType::Position,
            SignalType::Delta,
            SignalType::State,
            SignalType::Impulse,
            SignalType::Hold,
            SignalType::Pulse,
        ];
        for signal in cases {
            let mut nodes = Vec::new();
            let mut edges = Vec::new();
            let input = match signal {
                SignalType::Position | SignalType::Delta => {
                    nodes.push(pointer("source", "a"));
                    edges.push(edge(
                        "source",
                        if signal == SignalType::Position {
                            "position"
                        } else {
                            "delta"
                        },
                        "delay",
                        "value",
                    ));
                    motion("a", [3.0, 4.0])
                }
                SignalType::State => {
                    nodes.extend([pointer("source", "a"), node("velocity", NodeKind::Velocity)]);
                    edges.extend([
                        edge("source", "position", "velocity", "value"),
                        edge("velocity", "value", "delay", "value"),
                    ]);
                    motion("a", [3.0, 4.0])
                }
                SignalType::Impulse => {
                    nodes.push(node(
                        "source",
                        NodeKind::Wheel {
                            user: "a".into(),
                            horizontal: false,
                            disposition: Disposition::Consume,
                        },
                    ));
                    edges.push(edge("source", "value", "delay", "value"));
                    wheel("a", 2.5)
                }
                SignalType::Hold => {
                    nodes.push(key("source", "a", 65));
                    edges.push(edge("source", "value", "delay", "value"));
                    key_input("a", 65, true)
                }
                SignalType::Pulse => {
                    nodes.extend([key("source", "a", 65), node("rise", NodeKind::RisingEdge)]);
                    edges.extend([
                        edge("source", "value", "rise", "value"),
                        edge("rise", "value", "delay", "value"),
                    ]);
                    key_input("a", 65, true)
                }
                SignalType::Text => unreachable!("Text is not supported by Delay"),
            };
            nodes.extend([
                node(
                    "delay",
                    NodeKind::Delay {
                        signal,
                        delay_ms: 1,
                    },
                ),
                node("monitor", NodeKind::Monitor { signal }),
            ]);
            edges.push(edge("delay", "value", "monitor", "value"));
            let compiled = definition(nodes, edges).compile().unwrap();
            let (index, delay_ms) = compiled.delays().next().unwrap();
            assert_eq!(delay_ms, 1);
            assert!(compiled.delay_outputs(index).is_empty());
            let mut state = State::new(&compiled);
            assert!(
                state
                    .evaluate_at(&compiled, input, 10_000)
                    .unwrap()
                    .is_empty()
            );
            let pending = state.pending_delays();
            assert_eq!(pending.len(), 1);
            assert_eq!(pending[0].node_index, index);
            assert_eq!(pending[0].due_elapsed_us, None);
            assert!(
                state
                    .evaluate_delay_at(&compiled, pending[0].id, 11_000)
                    .is_err()
            );
            state.anchor_delays(&[pending[0].id], 10_000).unwrap();
            assert_eq!(state.next_delay_due().unwrap().due_elapsed_us, Some(11_000));
            let before_due = state.clone();
            assert!(
                state
                    .evaluate_delay_at(&compiled, pending[0].id, 10_999)
                    .is_err()
            );
            assert_eq!(state.pending_delays(), before_due.pending_delays());
            state
                .evaluate_delay_at(&compiled, pending[0].id, 11_000)
                .unwrap();
            assert!(state.pending_delays().is_empty());
            assert_eq!(state.monitor_samples()[0].value.signal(), signal);
            assert!(
                state
                    .evaluate_delay_at(&compiled, pending[0].id, 12_000)
                    .is_err()
            );
        }
    }

    #[test]
    fn delayed_hold_down_and_release_remain_fifo_and_capacity_failure_rolls_back() {
        let graph = definition(
            vec![
                key("key", "a", 65),
                node(
                    "delay",
                    NodeKind::Delay {
                        signal: SignalType::Hold,
                        delay_ms: 1,
                    },
                ),
                node(
                    "watch",
                    NodeKind::Monitor {
                        signal: SignalType::Hold,
                    },
                ),
                sink("sink", "b", OutputChannel::KeyHold { key: 66 }),
            ],
            vec![
                edge("key", "value", "delay", "value"),
                edge("delay", "value", "watch", "value"),
                edge("delay", "value", "sink", "value"),
            ],
        )
        .compile()
        .unwrap();
        let (delay_index, _) = graph.delays().next().unwrap();
        assert_eq!(graph.delay_outputs(delay_index).len(), 1);
        let mut state = State::new(&graph);
        assert!(
            state
                .evaluate_at(&graph, key_input("a", 65, true), 1)
                .unwrap()
                .is_empty()
        );
        assert!(
            state
                .evaluate_at(&graph, key_input("a", 65, false), 2)
                .unwrap()
                .is_empty()
        );
        let pending = state.pending_delays();
        assert_eq!(pending.len(), 2);
        assert!(pending.iter().all(|job| job.due_elapsed_us.is_none()));
        assert!(
            state
                .anchor_delays(&[pending[0].id, pending[0].id], 100)
                .is_err()
        );
        assert!(
            state
                .anchor_delays(&[pending[0].id, pending[1].id], u64::MAX - 500)
                .is_err()
        );
        assert!(
            state
                .pending_delays()
                .iter()
                .all(|job| job.due_elapsed_us.is_none())
        );
        state
            .anchor_delays(&[pending[0].id, pending[1].id], 100)
            .unwrap();
        assert!(
            state
                .evaluate_delay_at(&graph, pending[1].id, 1_100)
                .is_err()
        );
        assert_eq!(
            state
                .evaluate_delay_at(&graph, pending[0].id, 1_100)
                .unwrap()[0]
                .value,
            Value::Hold(true)
        );
        assert_eq!(state.monitor_samples()[0].value, Value::Hold(true));
        assert_eq!(
            state
                .evaluate_delay_at(&graph, pending[1].id, 1_100)
                .unwrap()[0]
                .value,
            Value::Hold(false)
        );
        assert_eq!(state.monitor_samples()[0].value, Value::Hold(false));
        assert!(state.pending_delays().is_empty());

        let capacity_graph = definition(
            vec![
                node(
                    "wheel",
                    NodeKind::Wheel {
                        user: "a".into(),
                        horizontal: false,
                        disposition: Disposition::Consume,
                    },
                ),
                key("key", "a", 65),
                node(
                    "wheel_delay",
                    NodeKind::Delay {
                        signal: SignalType::Impulse,
                        delay_ms: 1,
                    },
                ),
                node(
                    "hold_delay",
                    NodeKind::Delay {
                        signal: SignalType::Hold,
                        delay_ms: 1,
                    },
                ),
                node(
                    "hold_watch",
                    NodeKind::Monitor {
                        signal: SignalType::Hold,
                    },
                ),
            ],
            vec![
                edge("wheel", "value", "wheel_delay", "value"),
                edge("key", "value", "hold_delay", "value"),
                edge("key", "value", "hold_watch", "value"),
            ],
        )
        .compile()
        .unwrap();
        let mut actor = State::new(&capacity_graph);
        let other_actor = State::new(&capacity_graph);
        for _ in 0..(MAX_PENDING_DELAYS_PER_ACTOR - 1) {
            actor.evaluate(&capacity_graph, wheel("a", 1.0)).unwrap();
        }
        actor
            .evaluate(&capacity_graph, key_input("a", 65, true))
            .unwrap();
        assert_eq!(actor.pending_delays().len(), MAX_PENDING_DELAYS_PER_ACTOR);
        let before = actor.clone();
        assert!(
            actor
                .evaluate(&capacity_graph, key_input("a", 65, false))
                .is_err()
        );
        assert_eq!(actor.pending_delays(), before.pending_delays());
        assert_eq!(actor.monitor_samples(), before.monitor_samples());
        assert_eq!(actor.monitor_samples()[0].value, Value::Hold(true));
        assert!(other_actor.pending_delays().is_empty());
    }

    #[test]
    fn delay_preserves_zero_values_but_never_queues_a_false_pulse() {
        let impulse_graph = definition(
            vec![
                node(
                    "wheel",
                    NodeKind::Wheel {
                        user: "a".into(),
                        horizontal: false,
                        disposition: Disposition::Consume,
                    },
                ),
                node(
                    "delay",
                    NodeKind::Delay {
                        signal: SignalType::Impulse,
                        delay_ms: 1,
                    },
                ),
            ],
            vec![edge("wheel", "value", "delay", "value")],
        )
        .compile()
        .unwrap();
        let mut state = State::new(&impulse_graph);
        state.evaluate(&impulse_graph, wheel("a", 0.0)).unwrap();
        assert_eq!(
            state.pending_delays().len(),
            1,
            "zero is still a typed value"
        );

        let pulse_graph = definition(
            vec![
                key("key", "a", 65),
                node("rise", NodeKind::RisingEdge),
                node(
                    "delay",
                    NodeKind::Delay {
                        signal: SignalType::Pulse,
                        delay_ms: 1,
                    },
                ),
            ],
            vec![
                edge("key", "value", "rise", "value"),
                edge("rise", "value", "delay", "value"),
            ],
        )
        .compile()
        .unwrap();
        let mut state = State::new(&pulse_graph);
        state
            .evaluate(&pulse_graph, key_input("a", 65, true))
            .unwrap();
        state
            .evaluate(&pulse_graph, key_input("a", 65, false))
            .unwrap();
        assert_eq!(
            state.pending_delays().len(),
            1,
            "Pulse(false) is not a due event"
        );
    }

    #[test]
    fn failed_delayed_wave_keeps_job_and_observer_state_uncommitted() {
        let graph = definition(
            vec![
                pointer("pointer", "a"),
                node("velocity", NodeKind::Velocity),
                node(
                    "delay",
                    NodeKind::Delay {
                        signal: SignalType::State,
                        delay_ms: 1,
                    },
                ),
                node(
                    "scale",
                    NodeKind::ScaleScalar {
                        signal: ScalarType::State,
                        factor: 2.0,
                    },
                ),
                node(
                    "monitor",
                    NodeKind::Monitor {
                        signal: SignalType::State,
                    },
                ),
            ],
            vec![
                edge("pointer", "position", "velocity", "value"),
                edge("velocity", "value", "delay", "value"),
                edge("delay", "value", "scale", "value"),
                edge("scale", "value", "monitor", "value"),
            ],
        )
        .compile()
        .unwrap();
        let mut state = State::new(&graph);
        state
            .evaluate_at(&graph, motion("a", [0.0, 0.0]), 0)
            .unwrap();
        state
            .evaluate_at(&graph, motion("a", [NUMERIC_LIMIT, 0.0]), 1)
            .unwrap();
        let pending = state.pending_delays();
        assert_eq!(pending.len(), 2);
        state.anchor_delays(&[pending[0].id], 0).unwrap();
        state.anchor_delays(&[pending[1].id], 1).unwrap();
        state
            .evaluate_delay_at(&graph, pending[0].id, 1_000)
            .unwrap();
        assert_eq!(state.monitor_samples()[0].value, Value::State(0.0));
        let before = state.clone();
        assert!(
            state
                .evaluate_delay_at(&graph, pending[1].id, 1_001)
                .is_err()
        );
        assert_eq!(state.pending_delays(), before.pending_delays());
        assert_eq!(state.monitor_samples(), before.monitor_samples());
    }

    #[test]
    fn throttle_is_leading_edge_preserves_releases_and_false_pulses_do_not_quota() {
        let hold_graph = definition(
            vec![
                key("key", "a", 65),
                node(
                    "throttle",
                    NodeKind::Throttle {
                        signal: SignalType::Hold,
                        interval_ms: 10,
                    },
                ),
                sink("sink", "b", OutputChannel::KeyHold { key: 66 }),
            ],
            vec![
                edge("key", "value", "throttle", "value"),
                edge("throttle", "value", "sink", "value"),
            ],
        )
        .compile()
        .unwrap();
        let mut state = State::new(&hold_graph);
        assert_eq!(
            state
                .evaluate_at(&hold_graph, key_input("a", 65, true), 0)
                .unwrap()[0]
                .value,
            Value::Hold(true)
        );
        assert_eq!(
            state
                .evaluate_at(&hold_graph, key_input("a", 65, false), 1_000)
                .unwrap()[0]
                .value,
            Value::Hold(false)
        );
        assert!(
            state
                .evaluate_at(&hold_graph, key_input("a", 65, true), 2_000)
                .unwrap()
                .is_empty(),
            "UP bypass preserves the active leading-edge window"
        );
        assert!(
            state
                .evaluate_at(&hold_graph, key_input("a", 65, false), 10_000)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            state
                .evaluate_at(&hold_graph, key_input("a", 65, true), 10_001)
                .unwrap()[0]
                .value,
            Value::Hold(true)
        );

        let pulse_graph = definition(
            vec![
                node(
                    "wheel",
                    NodeKind::Wheel {
                        user: "a".into(),
                        horizontal: false,
                        disposition: Disposition::Consume,
                    },
                ),
                node(
                    "negate",
                    NodeKind::ScaleScalar {
                        signal: ScalarType::Impulse,
                        factor: -1.0,
                    },
                ),
                node(
                    "compare",
                    NodeKind::Compare {
                        signal: ScalarType::Impulse,
                        operation: Comparison::Less,
                    },
                ),
                node(
                    "throttle",
                    NodeKind::Throttle {
                        signal: SignalType::Pulse,
                        interval_ms: 10,
                    },
                ),
                node(
                    "watch",
                    NodeKind::Monitor {
                        signal: SignalType::Pulse,
                    },
                ),
            ],
            vec![
                edge("wheel", "value", "compare", "a"),
                edge("wheel", "value", "negate", "value"),
                edge("negate", "value", "compare", "b"),
                edge("compare", "value", "throttle", "value"),
                edge("throttle", "value", "watch", "value"),
            ],
        )
        .compile()
        .unwrap();
        let mut state = State::new(&pulse_graph);
        state
            .evaluate_at(&pulse_graph, wheel("a", 1.0), 100)
            .unwrap();
        assert!(
            state.monitor_samples().is_empty(),
            "Pulse(false) does not schedule a monitor event"
        );
        state
            .evaluate_at(&pulse_graph, wheel("a", -1.0), 101)
            .unwrap();
        assert_eq!(
            state.monitor_samples()[0].value,
            Value::Pulse(true),
            "false pulse did not consume the throttle window"
        );
    }

    fn pixel_graph() -> Compiled {
        definition(
            vec![
                node(
                    "wheel",
                    NodeKind::Wheel {
                        user: "a".into(),
                        horizontal: false,
                        disposition: Disposition::Consume,
                    },
                ),
                node("pulse", NodeKind::ImpulseToPulse),
                node(
                    "pixel",
                    NodeKind::PixelColor {
                        capture_user: "screen-user".into(),
                        x: 2,
                        y: 3,
                    },
                ),
                node(
                    "red-monitor",
                    NodeKind::Monitor {
                        signal: SignalType::State,
                    },
                ),
                node(
                    "done-monitor",
                    NodeKind::Monitor {
                        signal: SignalType::Pulse,
                    },
                ),
                sink(
                    "wheel-sink",
                    "b",
                    OutputChannel::Wheel { horizontal: false },
                ),
            ],
            vec![
                edge("wheel", "value", "pulse", "value"),
                edge("wheel", "value", "wheel-sink", "value"),
                edge("pulse", "value", "pixel", "trigger"),
                edge("pixel", "red", "red-monitor", "value"),
                edge("pixel", "done", "done-monitor", "value"),
            ],
        )
        .compile()
        .unwrap()
    }

    #[test]
    fn triggered_pixel_observation_is_metadata_only_and_completes_on_current_state() {
        let graph = pixel_graph();
        let descriptor = graph.observations().pop().unwrap();
        assert_eq!(descriptor.node_id, "pixel");
        assert_eq!(descriptor.capture_user, "screen-user");
        assert_eq!(descriptor.kind, ObservationKind::PixelColor);
        assert_eq!(descriptor.source_users, vec!["a"]);
        let mut state = State::new(&graph);
        assert_eq!(state.evaluate(&graph, wheel("a", 1.0)).unwrap().len(), 1);
        let pending = state.pending_observations();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].node_index, descriptor.node_index);
        assert!(state.monitor_samples().is_empty());

        // Unrelated live state is changed after trigger acceptance. Completion
        // uses that current actor state instead of restoring a trigger snapshot.
        state.outputs[0].insert("sentinel".into(), Value::State(17.0));
        state
            .complete_observation(
                &graph,
                pending[0].id,
                ObservationResult::PixelColor {
                    rgba: [12, 34, 56, 78],
                },
                100,
            )
            .unwrap();
        assert_eq!(state.outputs[0].get("sentinel"), Some(&Value::State(17.0)));
        assert!(state.pending_observations().is_empty());
        let samples = state.monitor_samples();
        assert_eq!(
            samples
                .iter()
                .find(|s| s.node == "red-monitor")
                .unwrap()
                .value,
            Value::State(12.0)
        );
        assert_eq!(
            samples
                .iter()
                .find(|s| s.node == "done-monitor")
                .unwrap()
                .value,
            Value::Pulse(true)
        );
        let committed = state.clone();
        assert!(
            state
                .complete_observation(
                    &graph,
                    pending[0].id,
                    ObservationResult::PixelColor { rgba: [1, 2, 3, 4] },
                    101,
                )
                .is_err()
        );
        assert_eq!(
            state.pending_observations(),
            committed.pending_observations()
        );
        assert_eq!(
            state.monitor_samples().len(),
            committed.monitor_samples().len()
        );
    }

    #[test]
    fn image_match_miss_has_no_fabricated_or_cleared_position() {
        let graph = definition(
            vec![
                node(
                    "wheel",
                    NodeKind::Wheel {
                        user: "a".into(),
                        horizontal: false,
                        disposition: Disposition::Consume,
                    },
                ),
                node("pulse", NodeKind::ImpulseToPulse),
                node(
                    "match",
                    NodeKind::ImageMatch {
                        capture_user: "screen-user".into(),
                        template_png_base64: "AQ==".into(),
                        region: Some(crate::screen::Region {
                            x: 1,
                            y: 2,
                            width: 3,
                            height: 4,
                        }),
                        min_score: 0.9,
                    },
                ),
                node(
                    "matched-monitor",
                    NodeKind::Monitor {
                        signal: SignalType::Hold,
                    },
                ),
                node(
                    "score-monitor",
                    NodeKind::Monitor {
                        signal: SignalType::State,
                    },
                ),
                node(
                    "position-monitor",
                    NodeKind::Monitor {
                        signal: SignalType::Position,
                    },
                ),
            ],
            vec![
                edge("wheel", "value", "pulse", "value"),
                edge("pulse", "value", "match", "trigger"),
                edge("match", "matched", "matched-monitor", "value"),
                edge("match", "score", "score-monitor", "value"),
                edge("match", "position", "position-monitor", "value"),
            ],
        )
        .compile()
        .unwrap();
        let mut state = State::new(&graph);
        state.evaluate(&graph, wheel("a", 1.0)).unwrap();
        let request = state.pending_observations()[0];
        state
            .complete_observation(
                &graph,
                request.id,
                ObservationResult::ImageMatch { matched: None },
                20,
            )
            .unwrap();
        let sample = |state: &State, node: &str| {
            state
                .monitor_samples()
                .into_iter()
                .find(|sample| sample.node == node)
                .map(|sample| sample.value)
        };
        assert_eq!(sample(&state, "matched-monitor"), Some(Value::Hold(false)));
        assert_eq!(sample(&state, "score-monitor"), Some(Value::State(0.0)));
        assert_eq!(sample(&state, "position-monitor"), None);

        state.evaluate(&graph, wheel("a", 1.0)).unwrap();
        let request = state.pending_observations()[0];
        state
            .complete_observation(
                &graph,
                request.id,
                ObservationResult::ImageMatch {
                    matched: Some(ObservationMatch {
                        desktop_center: [30.5, 40.5],
                        score: 0.98,
                    }),
                },
                30,
            )
            .unwrap();
        assert_eq!(
            sample(&state, "position-monitor"),
            Some(Value::Position([30.5, 40.5]))
        );

        state.evaluate(&graph, wheel("a", 1.0)).unwrap();
        let request = state.pending_observations()[0];
        state
            .complete_observation(
                &graph,
                request.id,
                ObservationResult::ImageMatch { matched: None },
                40,
            )
            .unwrap();
        assert_eq!(
            sample(&state, "position-monitor"),
            Some(Value::Position([30.5, 40.5]))
        );
    }

    #[test]
    fn observation_duplicates_drop_new_but_commit_other_effects() {
        let graph = pixel_graph();
        let mut state = State::new(&graph);
        for _ in 0..20 {
            assert_eq!(state.evaluate(&graph, wheel("a", 1.0)).unwrap().len(), 1);
        }
        let pending = state.pending_observations();
        assert_eq!(pending.len(), 1);
        assert_eq!(state.dropped_observations(), 19);
        assert_eq!(state.pending_observations(), pending);

        let before = state.clone();
        assert!(
            state
                .complete_observation(
                    &graph,
                    pending[0].id,
                    ObservationResult::ImageMatch { matched: None },
                    100,
                )
                .is_err()
        );
        assert_eq!(state.pending_observations(), before.pending_observations());
        assert_eq!(state.dropped_observations(), before.dropped_observations());

        let failing_graph = definition(
            vec![
                node(
                    "wheel",
                    NodeKind::Wheel {
                        user: "a".into(),
                        horizontal: false,
                        disposition: Disposition::Consume,
                    },
                ),
                node("pulse", NodeKind::ImpulseToPulse),
                node(
                    "pixel",
                    NodeKind::PixelColor {
                        capture_user: "screen-user".into(),
                        x: 0,
                        y: 0,
                    },
                ),
                node(
                    "scale",
                    NodeKind::ScaleScalar {
                        signal: ScalarType::State,
                        factor: NUMERIC_LIMIT,
                    },
                ),
                node(
                    "monitor",
                    NodeKind::Monitor {
                        signal: SignalType::State,
                    },
                ),
            ],
            vec![
                edge("wheel", "value", "pulse", "value"),
                edge("pulse", "value", "pixel", "trigger"),
                edge("pixel", "red", "scale", "value"),
                edge("scale", "value", "monitor", "value"),
            ],
        )
        .compile()
        .unwrap();
        let mut actor = State::new(&failing_graph);
        actor.evaluate(&failing_graph, wheel("a", 1.0)).unwrap();
        let request = actor.pending_observations()[0];
        assert!(
            actor
                .complete_observation(
                    &failing_graph,
                    request.id,
                    ObservationResult::PixelColor {
                        rgba: [2, 0, 0, 255]
                    },
                    1,
                )
                .is_err()
        );
        assert_eq!(actor.pending_observations(), vec![request]);
        assert!(actor.monitor_samples().is_empty());
    }

    #[test]
    fn discard_observations_validates_all_ids_before_counting_and_releases_node() {
        let graph = pixel_graph();
        let mut state = State::new(&graph);
        state.evaluate(&graph, wheel("a", 1.0)).unwrap();
        state.evaluate(&graph, wheel("a", 1.0)).unwrap();
        let pending = state.pending_observations();
        assert_eq!(state.dropped_observations(), 1);

        for ids in [
            vec![pending[0].id, pending[0].id],
            vec![pending[0].id, u64::MAX],
        ] {
            assert!(state.discard_observations(&ids).is_err());
            assert_eq!(state.pending_observations(), pending);
            assert_eq!(state.dropped_observations(), 1);
        }
        state.discard_observations(&[pending[0].id]).unwrap();
        assert!(state.pending_observations().is_empty());
        assert_eq!(state.dropped_observations(), 2);

        state.evaluate(&graph, wheel("a", 1.0)).unwrap();
        let next = state.pending_observations();
        assert_eq!(next.len(), 1);
        assert_ne!(next[0].id, pending[0].id);
        state
            .complete_observation(
                &graph,
                next[0].id,
                ObservationResult::PixelColor { rgba: [1, 2, 3, 4] },
                10,
            )
            .unwrap();
        assert!(state.pending_observations().is_empty());
        assert_eq!(state.dropped_observations(), 2);
    }

    #[test]
    fn observation_nodes_fill_actor_bound_once_each() {
        let mut nodes = vec![
            node(
                "wheel",
                NodeKind::Wheel {
                    user: "a".into(),
                    horizontal: false,
                    disposition: Disposition::Consume,
                },
            ),
            node("pulse", NodeKind::ImpulseToPulse),
        ];
        let mut edges = vec![edge("wheel", "value", "pulse", "value")];
        for index in 0..MAX_PENDING_OBSERVATIONS_PER_ACTOR {
            let node_id = format!("pixel-{index}");
            nodes.push(node(
                &node_id,
                NodeKind::PixelColor {
                    capture_user: "screen".into(),
                    x: index as u32,
                    y: 0,
                },
            ));
            edges.push(edge("pulse", "value", &node_id, "trigger"));
        }
        let graph = definition(nodes, edges).compile().unwrap();
        let mut state = State::new(&graph);
        state.evaluate(&graph, wheel("a", 1.0)).unwrap();
        assert_eq!(
            state.pending_observations().len(),
            MAX_PENDING_OBSERVATIONS_PER_ACTOR
        );
        assert_eq!(state.dropped_observations(), 0);
    }

    #[test]
    fn observation_nodes_are_bounded_typed_and_require_virtual_ancestry() {
        let graph = pixel_graph();
        let descriptor = graph.observations().pop().unwrap();
        assert_eq!(
            graph.definition().nodes[descriptor.node_index]
                .kind
                .ports()
                .inputs,
            vec![Port {
                name: "trigger".into(),
                signal: SignalType::Pulse
            }]
        );
        assert_eq!(graph.observation_outputs(descriptor.node_index).len(), 0);
        assert_eq!(
            graph.definition().nodes[descriptor.node_index]
                .kind
                .category(),
            NodeCategory::Observers
        );

        let mut invalid = definition(
            vec![
                node(
                    "constant",
                    NodeKind::Constant {
                        value: Value::Pulse(true),
                    },
                ),
                node(
                    "pixel",
                    NodeKind::PixelColor {
                        capture_user: "screen".into(),
                        x: 0,
                        y: 0,
                    },
                ),
            ],
            vec![edge("constant", "value", "pixel", "trigger")],
        );
        assert!(invalid.validate().is_err());

        let nodes = std::iter::once(node(
            "wheel",
            NodeKind::Wheel {
                user: "a".into(),
                horizontal: false,
                disposition: Disposition::Consume,
            },
        ))
        .chain(std::iter::once(node("pulse", NodeKind::ImpulseToPulse)))
        .chain((0..=MAX_OBSERVATIONS).map(|index| {
            node(
                &format!("pixel-{index}"),
                NodeKind::PixelColor {
                    capture_user: "screen".into(),
                    x: 0,
                    y: 0,
                },
            )
        }))
        .collect();
        let edges = (0..=MAX_OBSERVATIONS)
            .map(|index| edge("pulse", "value", &format!("pixel-{index}"), "trigger"))
            .chain(std::iter::once(edge("wheel", "value", "pulse", "value")))
            .collect();
        invalid = definition(nodes, edges);
        assert!(invalid.validate().is_err());
    }

    #[test]
    fn throttle_dropped_impulses_do_not_replay_and_failed_wave_rolls_back_quota() {
        let graph = definition(
            vec![
                node(
                    "wheel",
                    NodeKind::Wheel {
                        user: "a".into(),
                        horizontal: false,
                        disposition: Disposition::Consume,
                    },
                ),
                pointer("pointer", "a"),
                node(
                    "throttle",
                    NodeKind::Throttle {
                        signal: SignalType::Impulse,
                        interval_ms: 10,
                    },
                ),
                sink("sink", "b", OutputChannel::Wheel { horizontal: false }),
            ],
            vec![
                edge("wheel", "value", "throttle", "value"),
                edge("throttle", "value", "sink", "value"),
            ],
        )
        .compile()
        .unwrap();
        let mut state = State::new(&graph);
        assert_eq!(
            state
                .evaluate_at(&graph, wheel("a", 1.0), 1_000)
                .unwrap()
                .len(),
            1
        );
        assert!(
            state
                .evaluate_at(&graph, wheel("a", 2.0), 2_000)
                .unwrap()
                .is_empty()
        );
        assert!(
            state
                .evaluate_at(&graph, motion("a", [2.0, 3.0]), 3_000)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            state
                .evaluate_at(&graph, wheel("a", 3.0), 11_000)
                .unwrap()
                .len(),
            1
        );
        let mut other_actor = State::new(&graph);
        assert_eq!(
            other_actor
                .evaluate_at(&graph, wheel("a", 4.0), 2_000)
                .unwrap()
                .len(),
            1,
            "throttle memory belongs to an actor"
        );

        let failing_graph = definition(
            vec![
                node(
                    "wheel",
                    NodeKind::Wheel {
                        user: "a".into(),
                        horizontal: false,
                        disposition: Disposition::Consume,
                    },
                ),
                node(
                    "throttle",
                    NodeKind::Throttle {
                        signal: SignalType::Impulse,
                        interval_ms: 10_000,
                    },
                ),
                node(
                    "scale",
                    NodeKind::ScaleScalar {
                        signal: ScalarType::Impulse,
                        factor: NUMERIC_LIMIT,
                    },
                ),
                sink("sink", "b", OutputChannel::Wheel { horizontal: false }),
            ],
            vec![
                edge("wheel", "value", "throttle", "value"),
                edge("throttle", "value", "scale", "value"),
                edge("scale", "value", "sink", "value"),
            ],
        )
        .compile()
        .unwrap();
        let mut state = State::new(&failing_graph);
        assert!(
            state
                .evaluate_at(&failing_graph, wheel("a", 2.0), 100)
                .is_err()
        );
        let valid = state
            .evaluate_at(&failing_graph, wheel("a", 1.0), 101)
            .unwrap();
        assert_eq!(
            valid.len(),
            1,
            "failed downstream validation must not commit throttle quota"
        );
        assert_eq!(valid[0].value, Value::Impulse(NUMERIC_LIMIT));
    }

    #[test]
    fn text_values_and_literals_are_bounded_by_utf8_bytes() {
        for text in [
            String::new(),
            "é".repeat(MAX_TEXT_BYTES / 2),
            "a\t\r\nb".into(),
        ] {
            Value::Text(text.clone()).validate().unwrap();
            let encoded = serde_json::to_string(&Value::Text(text.clone())).unwrap();
            assert_eq!(
                serde_json::from_str::<Value>(&encoded).unwrap(),
                Value::Text(text)
            );
        }
        for text in [
            "é".repeat(MAX_TEXT_BYTES / 2 + 1),
            "a\0b".into(),
            "a\u{0085}b".into(),
        ] {
            assert!(Value::Text(text.clone()).validate().is_err());
            assert!(
                definition(
                    vec![node(
                        "constant",
                        NodeKind::Constant {
                            value: Value::Text(text)
                        }
                    )],
                    vec![]
                )
                .compile()
                .is_err()
            );
        }
        for needle in [
            String::new(),
            "é".repeat(MAX_TEXT_LITERAL_BYTES / 2 + 1),
            "x\0".into(),
        ] {
            assert!(
                NodeKind::TextContains {
                    needle,
                    case_sensitive: false
                }
                .validate()
                .is_err()
            );
        }
        NodeKind::TextContains {
            needle: "é".repeat(MAX_TEXT_LITERAL_BYTES / 2),
            case_sensitive: false,
        }
        .validate()
        .unwrap();
        NodeKind::TextCompare {
            expected: String::new(),
            operation: TextComparison::Equals,
            case_sensitive: false,
        }
        .validate()
        .unwrap();
        assert!(
            NodeKind::TextCompare {
                expected: "x".repeat(MAX_TEXT_LITERAL_BYTES + 1),
                operation: TextComparison::Equals,
                case_sensitive: true
            }
            .validate()
            .is_err()
        );
        assert!(!SignalType::Text.transient());
    }

    fn text_predicate_result(text: &str, predicate: NodeKind) -> bool {
        let graph = definition(
            vec![
                node(
                    "constant",
                    NodeKind::Constant {
                        value: Value::Text(text.into()),
                    },
                ),
                node("predicate", predicate),
            ],
            vec![edge("constant", "value", "predicate", "value")],
        )
        .compile()
        .unwrap();
        let state = State::new(&graph);
        let index = graph
            .definition
            .nodes
            .iter()
            .position(|node| node.id == "predicate")
            .unwrap();
        let Value::Hold(result) = state.outputs[index]["value"] else {
            panic!("predicate must produce Hold")
        };
        result
    }

    #[test]
    fn literal_text_predicates_fold_only_ascii_and_never_normalize_or_parse() {
        for (text, needle, sensitive, expected) in [
            ("Ready: ABC", "abc", false, true),
            ("Ready: ABC", "abc", true, false),
            ("[ab]", "[ab]", true, true),
            ("acb", "a.b", false, false),
            ("", "ready", false, false),
            ("É", "é", false, false),
            ("é", "e\u{0301}", false, false),
            ("你好世界", "好世", false, true),
        ] {
            assert_eq!(
                text_predicate_result(
                    text,
                    NodeKind::TextContains {
                        needle: needle.into(),
                        case_sensitive: sensitive
                    }
                ),
                expected
            );
        }
        for (text, expected, sensitive, equals) in [
            ("Ready", "READY", false, true),
            ("Ready", "READY", true, false),
            ("", "", false, true),
            // No recognized text is an empty string, so NotEquals(nonempty) is true.
            ("", "Ready", false, false),
            ("01", "1", false, false),
            (" ready ", "ready", false, false),
            ("É", "é", false, false),
            ("é", "e\u{0301}", false, false),
        ] {
            for operation in [TextComparison::Equals, TextComparison::NotEquals] {
                assert_eq!(
                    text_predicate_result(
                        text,
                        NodeKind::TextCompare {
                            expected: expected.into(),
                            operation,
                            case_sensitive: sensitive
                        }
                    ),
                    if operation == TextComparison::Equals {
                        equals
                    } else {
                        !equals
                    }
                );
            }
        }
    }

    #[test]
    fn text_ports_are_strict_and_unsupported_operators_fail_visibly() {
        for kind in [
            NodeKind::Gate {
                signal: SignalType::Text,
            },
            NodeKind::Switch {
                signal: SignalType::Text,
            },
            NodeKind::Delay {
                signal: SignalType::Text,
                delay_ms: 1,
            },
            NodeKind::Throttle {
                signal: SignalType::Text,
                interval_ms: 1,
            },
            NodeKind::Scope {
                signal: SignalType::Text,
            },
            NodeKind::Smooth {
                signal: SignalType::Text,
                alpha: 0.5,
            },
        ] {
            assert!(kind.validate().is_err());
        }
        NodeKind::Monitor {
            signal: SignalType::Text,
        }
        .validate()
        .unwrap();
        let graph = definition(
            vec![
                node(
                    "text",
                    NodeKind::Constant {
                        value: Value::Text("1".into()),
                    },
                ),
                node(
                    "number",
                    NodeKind::ScaleScalar {
                        signal: ScalarType::State,
                        factor: 1.0,
                    },
                ),
            ],
            vec![edge("text", "value", "number", "value")],
        );
        assert!(graph.compile().is_err(), "text never coerces to a number");
        let graph = definition(
            vec![
                node(
                    "number",
                    NodeKind::Constant {
                        value: Value::State(1.0),
                    },
                ),
                node(
                    "predicate",
                    NodeKind::TextContains {
                        needle: "1".into(),
                        case_sensitive: true,
                    },
                ),
            ],
            vec![edge("number", "value", "predicate", "value")],
        );
        assert!(
            graph.compile().is_err(),
            "numeric values never coerce to text"
        );
        let catalogue = catalogue();
        let ocr = catalogue.iter().find(|node| node.name == "ocr").unwrap();
        assert_eq!(ocr.label, "Extract text (Latin)");
        assert_eq!(
            ocr.ports.outputs,
            vec![
                Port {
                    name: "text".into(),
                    signal: SignalType::Text
                },
                Port {
                    name: "has_text".into(),
                    signal: SignalType::Hold
                },
                Port {
                    name: "done".into(),
                    signal: SignalType::Pulse
                }
            ]
        );
    }

    fn ocr_graph() -> Compiled {
        definition(
            vec![
                node(
                    "wheel",
                    NodeKind::Wheel {
                        user: "a".into(),
                        horizontal: false,
                        disposition: Disposition::Consume,
                    },
                ),
                node("pulse", NodeKind::ImpulseToPulse),
                node(
                    "ocr",
                    NodeKind::Ocr {
                        capture_user: "screen-user".into(),
                        region: None,
                    },
                ),
                node(
                    "contains",
                    NodeKind::TextContains {
                        needle: "ready".into(),
                        case_sensitive: false,
                    },
                ),
                key("enabled", "a", 65),
                node(
                    "both",
                    NodeKind::Logic {
                        operation: Logic::And,
                    },
                ),
                node(
                    "gate",
                    NodeKind::Gate {
                        signal: SignalType::Pulse,
                    },
                ),
                sink("output", "b", OutputChannel::KeyPulse { key: 66 }),
                node(
                    "text-monitor",
                    NodeKind::Monitor {
                        signal: SignalType::Text,
                    },
                ),
                node(
                    "has-text-monitor",
                    NodeKind::Monitor {
                        signal: SignalType::Hold,
                    },
                ),
                node(
                    "done-monitor",
                    NodeKind::Monitor {
                        signal: SignalType::Pulse,
                    },
                ),
            ],
            vec![
                edge("wheel", "value", "pulse", "value"),
                edge("pulse", "value", "ocr", "trigger"),
                edge("ocr", "text", "contains", "value"),
                edge("contains", "value", "both", "a"),
                edge("enabled", "value", "both", "b"),
                edge("both", "value", "gate", "open"),
                edge("ocr", "done", "gate", "value"),
                edge("gate", "value", "output", "value"),
                edge("ocr", "text", "text-monitor", "value"),
                edge("ocr", "has_text", "has-text-monitor", "value"),
                edge("ocr", "done", "done-monitor", "value"),
            ],
        )
        .compile()
        .unwrap()
    }

    #[test]
    fn ocr_completion_uses_current_actor_state_repeats_done_and_clears_empty_text() {
        let graph = ocr_graph();
        let shared_definition = graph.definition.to_json().unwrap();
        let descriptor = graph.observations().pop().unwrap();
        assert_eq!(descriptor.kind, ObservationKind::Ocr);
        assert_eq!(descriptor.capture_user, "screen-user");
        assert_eq!(descriptor.source_users, vec!["a"]);
        assert_eq!(graph.observation_outputs(descriptor.node_index).len(), 1);
        let mut state = State::new(&graph);
        state.evaluate(&graph, key_input("a", 65, true)).unwrap();
        let private_text = "READY private captured phrase";
        for count in 1..=2 {
            assert!(state.evaluate(&graph, wheel("a", 1.0)).unwrap().is_empty());
            let request = state.pending_observations()[0];
            let effects = state
                .complete_observation(
                    &graph,
                    request.id,
                    ObservationResult::Ocr {
                        text: private_text.into(),
                    },
                    count,
                )
                .unwrap();
            assert_eq!(
                effects.len(),
                1,
                "each successful capture emits a fresh done pulse"
            );
            assert_eq!(effects[0].value, Value::Pulse(true));
            assert_eq!(
                state
                    .monitor_samples()
                    .iter()
                    .find(|sample| sample.node == "done-monitor")
                    .unwrap()
                    .samples,
                count
            );
        }
        state.evaluate(&graph, wheel("a", 1.0)).unwrap();
        let request = state.pending_observations()[0];
        state.evaluate(&graph, key_input("a", 65, false)).unwrap();
        assert!(
            state
                .complete_observation(
                    &graph,
                    request.id,
                    ObservationResult::Ocr {
                        text: private_text.into()
                    },
                    3
                )
                .unwrap()
                .is_empty(),
            "completion observes current key state"
        );
        assert!(state.scope_traces().is_empty());
        assert_eq!(graph.definition.to_json().unwrap(), shared_definition);
        assert!(!shared_definition.contains(private_text));
        assert!(
            State::new(&graph).monitor_samples().is_empty(),
            "actors do not share captured values"
        );

        state.evaluate(&graph, wheel("a", 1.0)).unwrap();
        let request = state.pending_observations()[0];
        state
            .complete_observation(
                &graph,
                request.id,
                ObservationResult::Ocr {
                    text: String::new(),
                },
                4,
            )
            .unwrap();
        let samples = state.monitor_samples();
        assert_eq!(
            samples
                .iter()
                .find(|sample| sample.node == "text-monitor")
                .unwrap()
                .value,
            Value::Text(String::new())
        );
        assert_eq!(
            samples
                .iter()
                .find(|sample| sample.node == "has-text-monitor")
                .unwrap()
                .value,
            Value::Hold(false)
        );
        assert_eq!(
            samples
                .iter()
                .find(|sample| sample.node == "done-monitor")
                .unwrap()
                .samples,
            4
        );
        assert!(state.pending_observations().is_empty());
        state.clear();
        assert!(state.monitor_samples().is_empty());
        assert!(state.outputs.iter().all(|outputs| {
            outputs
                .values()
                .all(|value| !matches!(value, Value::Text(_)))
        }));
    }

    #[test]
    fn ocr_wrong_or_invalid_result_rolls_back_without_fabricating_empty_success() {
        let graph = ocr_graph();
        let mut state = State::new(&graph);
        state.evaluate(&graph, wheel("a", 1.0)).unwrap();
        let pending = state.pending_observations();
        let outputs = state.outputs.clone();
        for result in [
            ObservationResult::PixelColor { rgba: [1, 2, 3, 4] },
            ObservationResult::Ocr {
                text: "x".repeat(MAX_TEXT_BYTES + 1),
            },
            ObservationResult::Ocr {
                text: "bad\0text".into(),
            },
        ] {
            assert!(
                state
                    .complete_observation(&graph, pending[0].id, result, 1)
                    .is_err()
            );
            assert_eq!(state.pending_observations(), pending);
            assert_eq!(state.outputs, outputs);
            assert!(state.monitor_samples().is_empty());
        }
        state
            .complete_observation(
                &graph,
                pending[0].id,
                ObservationResult::Ocr {
                    text: "é".repeat(MAX_TEXT_BYTES / 2),
                },
                2,
            )
            .unwrap();
        assert_eq!(
            state
                .monitor_samples()
                .iter()
                .find(|sample| sample.node == "text-monitor")
                .unwrap()
                .value,
            Value::Text("é".repeat(MAX_TEXT_BYTES / 2))
        );
        assert_eq!(state.monitor_samples().len(), 3);
    }

    #[test]
    fn ocr_region_capture_user_and_shared_observation_caps_are_validated() {
        for kind in [
            NodeKind::Ocr {
                capture_user: String::new(),
                region: None,
            },
            NodeKind::Ocr {
                capture_user: "screen".into(),
                region: Some(crate::screen::Region {
                    x: 0,
                    y: 0,
                    width: 0,
                    height: 1,
                }),
            },
            NodeKind::Ocr {
                capture_user: "screen".into(),
                region: Some(crate::screen::Region {
                    x: u32::MAX,
                    y: 0,
                    width: 1,
                    height: 1,
                }),
            },
        ] {
            assert!(kind.validate().is_err());
        }
        let mut definition = ocr_graph().definition;
        definition
            .nodes
            .retain(|node| node.id == "wheel" || node.id == "pulse" || node.id == "ocr");
        definition
            .edges
            .retain(|edge| edge.to.node == "pulse" || edge.to.node == "ocr");
        for index in 1..MAX_OBSERVATIONS {
            let id = format!("ocr-{index}");
            definition.nodes.push(node(
                &id,
                NodeKind::Ocr {
                    capture_user: "screen-user".into(),
                    region: None,
                },
            ));
            definition
                .edges
                .push(edge("pulse", "value", &id, "trigger"));
        }
        let graph = definition.compile().unwrap();
        let mut state = State::new(&graph);
        state.evaluate(&graph, wheel("a", 1.0)).unwrap();
        assert_eq!(
            state.pending_observations().len(),
            MAX_PENDING_OBSERVATIONS_PER_ACTOR
        );
        state.evaluate(&graph, wheel("a", 1.0)).unwrap();
        assert_eq!(
            state.pending_observations().len(),
            MAX_PENDING_OBSERVATIONS_PER_ACTOR
        );
        assert_eq!(
            state.dropped_observations(),
            u64::try_from(MAX_OBSERVATIONS).unwrap()
        );
        definition.nodes.push(node(
            "pixel",
            NodeKind::PixelColor {
                capture_user: "screen-user".into(),
                x: 0,
                y: 0,
            },
        ));
        definition
            .edges
            .push(edge("pulse", "value", "pixel", "trigger"));
        assert!(
            definition.compile().is_err(),
            "OCR shares the existing screen observation cap"
        );
        let mut no_source = ocr_graph().definition;
        no_source.nodes.retain(|node| node.id == "ocr");
        no_source.edges.clear();
        assert!(no_source.compile().is_err());
    }
    fn typing_constants(texts: &[String], key_pulse: bool) -> Definition {
        let mut nodes = vec![
            node(
                "wheel",
                NodeKind::Wheel {
                    user: "a".into(),
                    horizontal: false,
                    disposition: Disposition::Passthrough,
                },
            ),
            node("pulse", NodeKind::ImpulseToPulse),
        ];
        let mut edges = vec![edge("wheel", "value", "pulse", "value")];
        for (index, text) in texts.iter().enumerate() {
            let literal = format!("literal-{index}");
            let output = format!("typed-{index}");
            nodes.push(node(
                &literal,
                NodeKind::Constant {
                    value: Value::Text(text.clone()),
                },
            ));
            nodes.push(sink(&output, "target", OutputChannel::TypeText));
            edges.push(edge(&literal, "value", &output, "text"));
            edges.push(edge("pulse", "value", &output, "trigger"));
        }
        if key_pulse {
            nodes.push(sink(
                "key-output",
                "target",
                OutputChannel::KeyPulse { key: 67 },
            ));
            edges.push(edge("pulse", "value", "key-output", "value"));
        }
        definition(nodes, edges)
    }

    fn typing_ocr(done_triggers: bool) -> Compiled {
        let mut nodes = vec![
            node(
                "read-wheel",
                NodeKind::Wheel {
                    user: "read-user".into(),
                    horizontal: false,
                    disposition: Disposition::Passthrough,
                },
            ),
            node("read-pulse", NodeKind::ImpulseToPulse),
            node(
                "ocr",
                NodeKind::Ocr {
                    capture_user: "screen-user".into(),
                    region: None,
                },
            ),
            sink("typed", "target", OutputChannel::TypeText),
            node(
                "private-monitor",
                NodeKind::Monitor {
                    signal: SignalType::Text,
                },
            ),
        ];
        let mut edges = vec![
            edge("read-wheel", "value", "read-pulse", "value"),
            edge("read-pulse", "value", "ocr", "trigger"),
            edge("ocr", "text", "typed", "text"),
            edge("ocr", "text", "private-monitor", "value"),
        ];
        if done_triggers {
            edges.push(edge("ocr", "done", "typed", "trigger"));
        } else {
            nodes.push(node(
                "type-wheel",
                NodeKind::Wheel {
                    user: "type-user".into(),
                    horizontal: false,
                    disposition: Disposition::Passthrough,
                },
            ));
            nodes.push(node("type-pulse", NodeKind::ImpulseToPulse));
            edges.push(edge("type-wheel", "value", "type-pulse", "value"));
            edges.push(edge("type-pulse", "value", "typed", "trigger"));
        }
        definition(nodes, edges).compile().unwrap()
    }

    fn complete_typing_read(
        state: &mut State,
        graph: &Compiled,
        text: &str,
        elapsed: u64,
    ) -> Result<Vec<Emission>> {
        assert!(
            state
                .evaluate_at(graph, wheel("read-user", 1.0), elapsed)?
                .is_empty()
        );
        let request = state.pending_observations()[0];
        state.complete_observation(
            graph,
            request.id,
            ObservationResult::Ocr { text: text.into() },
            elapsed + 1,
        )
    }

    #[test]
    fn type_text_schema_requires_two_semantic_ports_and_preserves_source_ancestry() {
        assert_eq!(
            serde_json::to_value(OutputChannel::TypeText).unwrap(),
            serde_json::json!({"type":"type_text"})
        );
        let entry = catalogue()
            .into_iter()
            .find(|entry| entry.name == "type_text")
            .unwrap();
        assert_eq!(entry.category, NodeCategory::Outputs);
        assert_eq!(
            entry.ports.inputs,
            vec![
                Port {
                    name: "text".into(),
                    signal: SignalType::Text
                },
                Port {
                    name: "trigger".into(),
                    signal: SignalType::Pulse
                }
            ]
        );
        assert!(entry.ports.outputs.is_empty());
        let valid = typing_constants(&["reference".into()], false);
        assert_eq!(
            Definition::from_json(&valid.to_json().unwrap()).unwrap(),
            valid
        );
        for missing in ["text", "trigger"] {
            let mut graph = valid.clone();
            graph.edges.retain(|edge| edge.to.port != missing);
            assert!(graph.compile().is_err());
        }
        let mut old_port = valid.clone();
        old_port.edges[1].to.port = "value".into();
        assert!(
            old_port.compile().is_err(),
            "TypeText cannot silently reuse the ordinary value port"
        );
        let mut wrong_semantics = valid;
        wrong_semantics.edges[2].from = PortRef {
            node: "literal-0".into(),
            port: "value".into(),
        };
        assert!(wrong_semantics.compile().is_err());
        let graph = typing_ocr(false);
        let outputs = graph.affected_outputs(&Source::Wheel {
            user: "type-user".into(),
            horizontal: false,
        });
        assert_eq!(outputs.len(), 1);
        assert_eq!(
            outputs[0].source_users,
            BTreeSet::from(["read-user".into(), "type-user".into()])
        );
        assert_eq!(
            graph.observation_outputs(graph.observations()[0].node_index)[0].channel,
            OutputChannel::TypeText
        );
    }

    #[test]
    fn type_text_unicode_cost_is_bounded_and_does_not_change_generic_text() {
        assert_eq!(MAX_TYPE_TEXT_UTF16_UNITS, 128);
        for text in ["", "A", "é", "漢字", "\n\t", "🙂"] {
            assert_eq!(type_text_cost(text).unwrap(), text.encode_utf16().count());
            assert_eq!(
                OutputChannel::TypeText
                    .value_effect_cost(&Value::Text(text.into()))
                    .unwrap(),
                text.encode_utf16().count()
            );
        }
        assert_eq!(type_text_cost(&"🙂".repeat(64)).unwrap(), 128);
        assert_eq!(type_text_cost(&"x".repeat(128)).unwrap(), 128);
        assert!(type_text_cost(&"🙂".repeat(65)).is_err());
        assert!(type_text_cost(&format!("{}🙂", "x".repeat(127))).is_err());
        for text in ["\r", "\0", "\u{7f}", "line\r\n"] {
            assert!(type_text_cost(text).is_err());
        }
        Value::Text("\r".into()).validate().unwrap();
        Value::Text("x".repeat(MAX_TEXT_BYTES)).validate().unwrap();
        assert!(
            OutputChannel::TypeText
                .value_effect_cost(&Value::State(1.0))
                .is_err()
        );
        assert_eq!(
            OutputChannel::KeyPulse { key: 67 }
                .value_effect_cost(&Value::Pulse(true))
                .unwrap(),
            2
        );
    }

    #[test]
    fn type_text_uses_actor_cached_text_only_on_fresh_true_pulses() {
        let graph = typing_ocr(false);
        let mut first = State::new(&graph);
        let mut second = State::new(&graph);
        let original_outputs = second.outputs.clone();
        assert!(
            second.evaluate(&graph, wheel("type-user", 1.0)).is_err(),
            "triggering without any current text is an atomic error"
        );
        assert_eq!(second.outputs, original_outputs);
        assert!(second.monitors.is_empty());
        assert!(
            complete_typing_read(&mut first, &graph, "actor one 🙂", 1)
                .unwrap()
                .is_empty(),
            "a text-only update must not type"
        );
        assert!(
            complete_typing_read(&mut second, &graph, "actor two", 2)
                .unwrap()
                .is_empty()
        );
        assert!(
            first
                .evaluate_at(&graph, wheel("type-user", 0.0), 500)
                .unwrap()
                .is_empty()
        );
        assert!(
            first
                .evaluate_at(&graph, key_input("unrelated", 65, true), 90_000_000)
                .unwrap()
                .is_empty(),
            "time and unrelated input must not replay cached text"
        );
        for elapsed in [90_000_001, 90_000_002] {
            let typed = first
                .evaluate_at(&graph, wheel("type-user", 1.0), elapsed)
                .unwrap();
            assert_eq!(typed.len(), 1);
            assert_eq!(typed[0].value, Value::Text("actor one 🙂".into()));
        }
        assert_eq!(
            second.evaluate(&graph, wheel("type-user", 1.0)).unwrap()[0].value,
            Value::Text("actor two".into())
        );
        assert!(!graph.definition.to_json().unwrap().contains("actor one"));
        assert_eq!(
            first.monitors["private-monitor"].value,
            Value::Text("actor one 🙂".into())
        );
        assert!(
            complete_typing_read(&mut first, &graph, "", 90_000_003)
                .unwrap()
                .is_empty()
        );
        assert!(
            first
                .evaluate(&graph, wheel("type-user", 1.0))
                .unwrap()
                .is_empty(),
            "empty cached text is a no-op"
        );
        first.clear();
        assert!(
            first.evaluate(&graph, wheel("type-user", 1.0)).is_err(),
            "retirement must clear the actor's cached text"
        );
        let graph = typing_constants(&["seeded".into()], false)
            .compile()
            .unwrap();
        let mut state = State::new(&graph);
        assert!(state.emitted.iter().all(Option::is_none));
        assert!(state.evaluate(&graph, wheel("a", 0.0)).unwrap().is_empty());
        assert_eq!(
            state.evaluate(&graph, wheel("a", 1.0)).unwrap()[0].value,
            Value::Text("seeded".into())
        );
    }

    #[test]
    fn type_text_ocr_done_reads_same_wave_text_and_rejection_preserves_current_state() {
        let graph = typing_ocr(true);
        let mut state = State::new(&graph);
        let typed = complete_typing_read(&mut state, &graph, "MUX OCR 7319", 10).unwrap();
        assert_eq!(typed.len(), 1);
        assert_eq!(typed[0].value, Value::Text("MUX OCR 7319".into()));
        assert!(
            state
                .evaluate_at(&graph, wheel("read-user", 1.0), 20)
                .unwrap()
                .is_empty()
        );
        let request = state.pending_observations()[0];
        let outputs = state.outputs.clone();
        let monitors = state.monitors.clone();
        for invalid in ["x".repeat(129), "unsupported\r".into()] {
            assert!(
                state
                    .complete_observation(
                        &graph,
                        request.id,
                        ObservationResult::Ocr { text: invalid },
                        21
                    )
                    .is_err()
            );
            assert_eq!(state.pending_observations(), vec![request]);
            assert_eq!(state.outputs, outputs);
            assert_eq!(state.monitors, monitors);
        }
        assert!(
            state
                .complete_observation(
                    &graph,
                    request.id,
                    ObservationResult::Ocr {
                        text: String::new()
                    },
                    22
                )
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            state.monitors["private-monitor"].value,
            Value::Text(String::new())
        );
    }

    #[test]
    fn type_text_actual_payloads_share_the_whole_wave_budget_atomically() {
        for (texts, key_pulse, expected_cost) in [
            (vec!["x".repeat(128)], false, 128),
            (vec!["x".repeat(126)], true, 128),
            (vec!["x".repeat(64), "🙂".repeat(32)], false, 128),
        ] {
            let graph = typing_constants(&texts, key_pulse).compile().unwrap();
            let effects = State::new(&graph)
                .evaluate(&graph, wheel("a", 1.0))
                .unwrap();
            assert_eq!(
                effects
                    .iter()
                    .map(|emission| emission.channel.value_effect_cost(&emission.value).unwrap())
                    .sum::<usize>(),
                expected_cost
            );
        }
        for (texts, key_pulse) in [
            (vec!["x".repeat(127)], true),
            (vec!["x".repeat(65), "x".repeat(64)], false),
            (vec!["x".repeat(129)], false),
        ] {
            let graph = typing_constants(&texts, key_pulse).compile().unwrap();
            let mut state = State::new(&graph);
            let outputs = state.outputs.clone();
            let emitted = state.emitted.clone();
            assert!(
                state.evaluate(&graph, wheel("a", 1.0)).is_err(),
                "payload costs must be checked together before returning any effects"
            );
            assert_eq!(state.outputs, outputs);
            assert_eq!(state.emitted, emitted);
        }
    }
}
