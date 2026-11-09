use serde::{Deserialize, Serialize};

pub const VERSION: u32 = 5;
pub const PIPE_NAME: &str = r"\\.\pipe\daisyloop";

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Temperature,
    Fan,
    Duty,
    Power,
    Voltage,
    Current,
    Load,
    Clock,
    Flow,
    Data,
    Other,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum ControlKind {
    Pump,
    Fan,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    #[default]
    Pid = 1,
    Model = 2,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "snake_case")]
pub enum Drive {
    #[default]
    Firmware,
    Duty,
    Test,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[serde(rename_all = "snake_case")]
pub enum AlarmKind {
    WaterHigh,
    WaterTop,
    WaterFrozen,
    PumpLow,
    FanStopped,
    Stale,
    Fallback,
    Foreign,
    Budget,
    ModelDrift,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Channel {
    pub id: String,
    pub label: String,
    pub hardware: String,
    pub kind: Kind,
    pub unit: String,
    pub value: Option<f64>,
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub age_ms: i64,
    pub stale: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Control {
    pub id: String,
    pub label: String,
    pub kind: ControlKind,
    pub rpm_channel: String,
    pub duty_channel: String,
    pub mode: Mode,
    pub chosen: Mode,
    pub allowed: bool,
    pub identified: bool,
    pub drive: Drive,
    pub target_rpm: Option<f64>,
    pub actual_rpm: Option<f64>,
    pub duty: Option<f64>,
    pub commanded_duty: Option<f64>,
    pub floor_duty: f64,
    #[serde(default)]
    pub start_duty: f64,
    pub min_rpm: f64,
    pub max_rpm: f64,
    pub fallback: Option<String>,
    pub note: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Limit {
    pub channel: String,
    pub low: Option<f64>,
    pub high: Option<f64>,
    #[serde(default)]
    pub plan: Option<f64>,
    #[serde(default)]
    pub top: Option<f64>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Alarm {
    pub kind: AlarmKind,
    pub channel: String,
    pub message: String,
    pub value: Option<f64>,
    pub limit: Option<f64>,
    pub since_ms: i64,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct ModelInfo {
    pub loaded: bool,
    pub fan_identified: bool,
    pub pump_identified: bool,
    pub error: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Correction {
    pub horizon_s: u32,
    pub bias: f64,
    pub scale: f64,
    pub delta: f64,
    pub updates: u32,
    pub acting: bool,
    #[serde(default)]
    pub drift: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Predicted {
    pub horizon_s: u32,
    pub water: Option<f64>,
    #[serde(default)]
    pub raw: Option<f64>,
    #[serde(default)]
    pub correction: Vec<Correction>,
    pub water_limit: f64,
    pub feasible: bool,
    pub age_ms: i64,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct StepStatus {
    pub id: String,
    pub phase: u32,
    pub phases: u32,
    pub duty: f64,
    #[serde(default)]
    pub overridden: bool,
    pub left_s: u32,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Snapshot {
    pub ts_ms: i64,
    pub seq: u64,
    pub target: f64,
    pub target_min: f64,
    pub target_max: f64,
    pub model: ModelInfo,
    pub predicted: Option<Predicted>,
    pub step_test: Option<StepStatus>,
    pub channels: Vec<Channel>,
    pub controls: Vec<Control>,
    pub limits: Vec<Limit>,
    pub alarms: Vec<Alarm>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct HistoryQuery {
    pub channels: Vec<String>,
    pub from_ms: i64,
    pub to_ms: i64,
    pub max_points: usize,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Series {
    pub channel: String,
    pub ts_ms: Vec<i64>,
    pub values: Vec<f64>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct HistoryResponse {
    pub series: Vec<Series>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Command {
    SetMode {
        id: String,
        mode: Mode,
        #[serde(default)]
        confirmed: bool,
    },
    SetWaterTarget {
        celsius: f64,
    },
    StartStepTest {
        id: String,
        #[serde(default)]
        confirmed: bool,
    },
    StopStepTest,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RequestBody {
    Snapshot,
    History(HistoryQuery),
    Command(Command),
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Request {
    pub v: u32,
    pub id: u64,
    pub body: RequestBody,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResponseBody {
    Snapshot(Snapshot),
    History(HistoryResponse),
    Ack,
    Error { message: String },
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Response {
    pub v: u32,
    pub id: u64,
    pub body: ResponseBody,
}

impl Request {
    pub fn new(id: u64, body: RequestBody) -> Self {
        Self { v: VERSION, id, body }
    }
}

impl Response {
    pub fn new(id: u64, body: ResponseBody) -> Self {
        Self { v: VERSION, id, body }
    }
}
