use crate::protocol::*;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::sync::mpsc::{channel, RecvTimeoutError, Sender};
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
    Command(Command, Sender<Result<(), String>>),
    History(u64, HistoryQuery),
}

pub enum LinkEvent {
    Snapshot(Snapshot),
    History(u64, HistoryResponse),
    Down(String),
    Error(String),
}

pub struct Link {
    jobs: Sender<Job>,
}

impl Link {
    pub fn start(on_event: impl Fn(LinkEvent) + Send + 'static) -> Self {
        let (jobs, rx) = channel::<Job>();
        std::thread::spawn(move || {
            let mut next_id = 0u64;
            loop {
                let file = match OpenOptions::new().read(true).write(true).open(PIPE_NAME) {
                    Ok(f) => f,
                    Err(e) => {
                        on_event(LinkEvent::Down(e.to_string()));
                        match rx.recv_timeout(POLL) {
                            Err(RecvTimeoutError::Disconnected) => return,
                            Ok(Job::Command(_, reply)) => {
                                let _ = reply.send(Err("Daemon nicht erreichbar".into()));
                                continue;
                            }
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
                        Some(Job::Command(c, _)) => RequestBody::Command(c.clone()),
                        Some(Job::History(_, q)) => RequestBody::History(q.clone()),
                        None => RequestBody::Snapshot,
                    };
                    io.send(&Request::new(next_id, body))?;
                    let resp: Response = io.recv()?;
                    if resp.v != VERSION {
                        return Err(std::io::Error::other(format!("Protokoll v{} statt v{}", resp.v, VERSION)));
                    }
                    match (resp.body, job) {
                        (ResponseBody::Snapshot(s), _) => {
                            due = Instant::now() + POLL;
                            on_event(LinkEvent::Snapshot(s));
                        }
                        (ResponseBody::History(h), Some(Job::History(tag, _))) => on_event(LinkEvent::History(tag, h)),
                        (ResponseBody::Error { message }, Some(Job::Command(_, reply))) => {
                            let _ = reply.send(Err(message));
                        }
                        (ResponseBody::Error { message }, _) => on_event(LinkEvent::Error(message)),
                        (ResponseBody::Ack, Some(Job::Command(_, reply))) => {
                            let _ = reply.send(Ok(()));
                            due = Instant::now();
                        }
                        _ => {}
                    }
                })();
                if let Err(e) = outcome {
                    if e.kind() == std::io::ErrorKind::Interrupted {
                        return;
                    }
                    on_event(LinkEvent::Down(e.to_string()));
                    std::thread::sleep(POLL);
                }
            }
        });
        Self { jobs }
    }

    pub fn command(&self, c: Command) -> Result<(), String> {
        let (tx, rx) = channel();
        self.jobs.send(Job::Command(c, tx)).map_err(|_| "Verbindung beendet".to_string())?;
        rx.recv_timeout(Duration::from_secs(8)).map_err(|_| "Keine Antwort vom Daemon".to_string())?
    }

    pub fn history(&self, tag: u64, q: HistoryQuery) {
        let _ = self.jobs.send(Job::History(tag, q));
    }
}
