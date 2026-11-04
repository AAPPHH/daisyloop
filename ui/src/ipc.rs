use crate::protocol::*;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const POLL: Duration = Duration::from_millis(1000);

pub struct Framed {
    reader: BufReader<File>,
    writer: File,
}

impl Framed {
    pub fn new(file: File) -> std::io::Result<Self> {
        Ok(Self { writer: file.try_clone()?, reader: BufReader::new(file) })
    }

    pub fn send<T: serde::Serialize>(&mut self, msg: &T) -> std::io::Result<()> {
        let mut line = serde_json::to_vec(msg).map_err(std::io::Error::other)?;
        line.push(b'\n');
        self.writer.write_all(&line)?;
        self.writer.flush()
    }

    pub fn recv<T: serde::de::DeserializeOwned>(&mut self) -> std::io::Result<T> {
        let mut line = String::new();
        if self.reader.read_line(&mut line)? == 0 {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        serde_json::from_str(&line).map_err(std::io::Error::other)
    }
}

pub enum Job {
    Command(Command),
    History(u64, HistoryQuery),
}

#[derive(Default)]
pub struct LinkState {
    pub snapshot: Option<Snapshot>,
    pub last_ok: Option<Instant>,
    pub connected: bool,
    pub history: Option<(u64, HistoryResponse)>,
    pub error: Option<String>,
}

pub struct Link {
    jobs: Sender<Job>,
    pub state: Arc<Mutex<LinkState>>,
}

struct Wake {
    ctx: eframe::egui::Context,
    visible: Arc<AtomicBool>,
}

impl Wake {
    fn ping(&self) {
        if self.visible.load(Ordering::Relaxed) {
            self.ctx.request_repaint();
        }
    }
}

impl Link {
    pub fn start(ctx: eframe::egui::Context, visible: Arc<AtomicBool>) -> Self {
        let (jobs, rx) = channel::<Job>();
        let state = Arc::new(Mutex::new(LinkState::default()));
        let shared = state.clone();
        let wake = Wake { ctx, visible };
        std::thread::spawn(move || {
            let mut next_id = 0u64;
            loop {
                let file = match OpenOptions::new().read(true).write(true).open(PIPE_NAME) {
                    Ok(f) => f,
                    Err(e) => {
                        Self::mark_down(&shared, e.to_string(), &wake);
                        match rx.recv_timeout(POLL) {
                            Err(RecvTimeoutError::Disconnected) => return,
                            _ => continue,
                        }
                    }
                };
                let Ok(mut io) = Framed::new(file) else { continue };
                let mut due = Instant::now();
                let outcome: std::io::Result<()> = (|| loop {
                    let job = match rx.recv_timeout(due.saturating_duration_since(Instant::now())) {
                        Ok(job) => Some(job),
                        Err(RecvTimeoutError::Timeout) => None,
                        Err(RecvTimeoutError::Disconnected) => return Err(std::io::ErrorKind::Interrupted.into()),
                    };
                    next_id += 1;
                    let body = match &job {
                        Some(Job::Command(c)) => RequestBody::Command(c.clone()),
                        Some(Job::History(_, q)) => RequestBody::History(q.clone()),
                        None => RequestBody::Snapshot,
                    };
                    io.send(&Request::new(next_id, body))?;
                    let resp: Response = io.recv()?;
                    if resp.v != VERSION {
                        return Err(std::io::Error::other(format!("Protokoll v{} statt v{}", resp.v, VERSION)));
                    }
                    let mut st = shared.lock().unwrap();
                    match (resp.body, job) {
                        (ResponseBody::Snapshot(s), _) => {
                            st.snapshot = Some(s);
                            st.last_ok = Some(Instant::now());
                            st.connected = true;
                            st.error = None;
                            due = Instant::now() + POLL;
                        }
                        (ResponseBody::History(h), Some(Job::History(tag, _))) => st.history = Some((tag, h)),
                        (ResponseBody::Error { message }, _) => st.error = Some(message),
                        (ResponseBody::Ack, Some(Job::Command(_))) => due = Instant::now(),
                        _ => {}
                    }
                    drop(st);
                    wake.ping();
                })();
                if let Err(e) = outcome {
                    if e.kind() == std::io::ErrorKind::Interrupted {
                        return;
                    }
                    Self::mark_down(&shared, e.to_string(), &wake);
                    std::thread::sleep(POLL);
                }
            }
        });
        Self { jobs, state }
    }

    fn mark_down(state: &Arc<Mutex<LinkState>>, err: String, wake: &Wake) {
        let mut st = state.lock().unwrap();
        st.connected = false;
        st.error = Some(err);
        drop(st);
        wake.ping();
    }

    pub fn command(&self, c: Command) {
        let _ = self.jobs.send(Job::Command(c));
    }

    pub fn history(&self, tag: u64, q: HistoryQuery) {
        let _ = self.jobs.send(Job::History(tag, q));
    }
}
