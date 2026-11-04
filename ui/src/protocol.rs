use serde::{Deserialize, Serialize};

pub const VERSION: u32 = 1;
pub const PIPE_NAME: &str = r"\.\pipe\daisyloop";

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

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Pid,
    Manual,
    Curve,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    Low,
    High,
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
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct PidParams {
    pub source: String,
    pub setpoint: f64,
    pub kp: f64,
    pub ki: f64,
    pub kd: f64,
    pub min_duty: f64,
    pub max_duty: f64,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct PidState {
    pub output: f64,
    pub error: f64,
    pub p: f64,
    pub i: f64,
    pub d: f64,
    pub saturated: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Control {
    pub id: String,
    pub label: String,
    pub kind: ControlKind,
    pub rpm_channel: String,
    pub duty_channel: String,
    pub mode: Mode,
    pub floor: f64,
    pub pid: PidParams,
    pub state: PidState,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Limit {
    pub channel: String,
    pub low: Option<f64>,
    pub high: Option<f64>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Alarm {
    pub channel: String,
    pub side: Side,
    pub limit: f64,
    pub value: f64,
    pub since_ms: i64,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Snapshot {
    pub ts_ms: i64,
    pub seq: u64,
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
    SetMode { id: String, mode: Mode },
    SetPid { id: String, pid: PidParams },
    SetLimit { channel: String, low: Option<f64>, high: Option<f64> },
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
