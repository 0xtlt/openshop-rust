//! Per-run logger: every line goes to the process logger and to the `logs` table,
//! and is left on the trail of what the run reports.

use std::borrow::Cow;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::PgPool;
use tokio::sync::{mpsc, oneshot};

use super::logger::{self, logger};
use super::report::Trail;

/// How long a line may wait for the next ones before it is written.
const WRITE_DELAY: Duration = Duration::from_millis(50);

/// How many lines wait to be written. A flow that logs faster than the
/// database takes them loses the newest ones, and the run says how many.
const QUEUE_CAPACITY: usize = 10_000;

/// The most a single statement holds, in lines and in bytes of text: a batch is
/// written once it reaches either, whatever is still waiting.
const MAX_BATCH_LINES: usize = 1_000;
const MAX_BATCH_BYTES: usize = 1 << 20;

struct Line {
    level: &'static str,
    message: String,
    /// The JSON text of the payload.
    payload: String,
    at: DateTime<Utc>,
}

impl Line {
    fn size(&self) -> usize {
        self.message.len() + self.payload.len()
    }
}

enum Message {
    Line(Line),
    Flush(oneshot::Sender<()>),
}

/// Writes are queued and persisted in emission order by a background task, so
/// logging never blocks or fails a flow. The queue is bounded: when it is full
/// a line is dropped instead of waiting, and a line saying how many were
/// dropped is written next to the others.
#[derive(Clone)]
pub struct FlowLogger {
    sender: mpsc::Sender<Message>,
    dropped: Arc<AtomicUsize>,
    trail: Trail,
}

impl FlowLogger {
    pub fn new(db: PgPool, flow_run_id: String) -> Self {
        let (sender, mut receiver) = mpsc::channel(QUEUE_CAPACITY);
        let dropped = Arc::new(AtomicUsize::new(0));
        let lost = dropped.clone();

        tokio::spawn(async move {
            let mut lines: Vec<Line> = Vec::new();
            let mut flushes = Vec::new();
            while let Some(first) = receiver.recv().await {
                let mut bytes = 0;
                match first {
                    Message::Line(line) => {
                        bytes += line.size();
                        lines.push(line);
                    }
                    Message::Flush(done) => flushes.push(done),
                }
                // The lines logged within `WRITE_DELAY` of the first one go to the
                // database with it, up to a batch: a flow that logs without pause
                // would otherwise keep the batch growing and never write it. A
                // flush, at the end of the run, does not wait.
                if flushes.is_empty() {
                    let delay = tokio::time::sleep(WRITE_DELAY);
                    tokio::pin!(delay);
                    while lines.len() < MAX_BATCH_LINES && bytes < MAX_BATCH_BYTES && !delay.is_elapsed() {
                        tokio::select! {
                            message = receiver.recv() => match message {
                                Some(Message::Line(line)) => {
                                    bytes += line.size();
                                    lines.push(line);
                                }
                                Some(Message::Flush(done)) => {
                                    flushes.push(done);
                                    break;
                                }
                                None => break,
                            },
                            _ = &mut delay => break,
                        }
                    }
                }
                let lost = lost.swap(0, Ordering::Relaxed);
                if lost > 0 {
                    let notice = format!("{lost} log lines were dropped");
                    logger::warn(format!("[openshop] Run {flow_run_id}: {notice}"));
                    lines.push(Line {
                        level: "warn",
                        message: notice,
                        payload: "{}".to_string(),
                        at: Utc::now(),
                    });
                }
                write(&db, &flow_run_id, &lines).await;
                lines.clear();
                for done in flushes.drain(..) {
                    let _ = done.send(());
                }
            }
        });

        Self {
            sender,
            dropped,
            trail: Trail::current(),
        }
    }

    fn log(&self, level: &'static str, mut payload: Value, message: &str) {
        let line = format!("[{}] {message}", level.to_uppercase());
        let logger = logger();
        match level {
            "warn" => logger.warn(&line, Some(&payload)),
            "error" => logger.error(&line, Some(&payload)),
            _ => logger.info(&line, Some(&payload)),
        }
        self.trail.leave(level, message);

        // PostgreSQL stores no NUL in a text, and one fails the whole statement.
        strip_nul(&mut payload);
        let line = Line {
            level,
            message: without_nul(message).into_owned(),
            payload: payload.to_string(),
            at: Utc::now(),
        };
        // A full queue loses the line; a closed one belongs to a stopped runtime.
        if let Err(mpsc::error::TrySendError::Full(_)) = self.sender.try_send(Message::Line(line)) {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Logs a line of the run.
    pub fn info(&self, message: impl AsRef<str>) {
        self.info_with(message, Value::Object(Default::default()));
    }

    /// Logs a line with data the admin shows next to it: `json!({ "count": 3 })`.
    pub fn info_with(&self, message: impl AsRef<str>, payload: Value) {
        self.log("info", payload, message.as_ref());
    }

    pub fn warn(&self, message: impl AsRef<str>) {
        self.warn_with(message, Value::Object(Default::default()));
    }

    pub fn warn_with(&self, message: impl AsRef<str>, payload: Value) {
        self.log("warn", payload, message.as_ref());
    }

    pub fn error(&self, message: impl AsRef<str>) {
        self.error_with(message, Value::Object(Default::default()));
    }

    pub fn error_with(&self, message: impl AsRef<str>, payload: Value) {
        self.log("error", payload, message.as_ref());
    }

    /// Resolves once every line logged so far has been written.
    pub async fn flush(&self) {
        let (done, wait) = oneshot::channel();
        if self.sender.send(Message::Flush(done)).await.is_ok() {
            let _ = wait.await;
        }
    }
}

/// What replaces a NUL byte: visible, so that two texts that differ by one
/// still differ.
const NUL_REPLACEMENT: char = '\u{FFFD}';

fn without_nul(text: &str) -> Cow<'_, str> {
    if text.contains('\0') {
        Cow::Owned(text.replace('\0', &NUL_REPLACEMENT.to_string()))
    } else {
        Cow::Borrowed(text)
    }
}

/// Replaces the NUL bytes in the strings and the keys of a payload. Keys that
/// become equal stay apart: the later ones get `#2`, `#3`… after them.
fn strip_nul(value: &mut Value) {
    match value {
        Value::String(text) => {
            if let Cow::Owned(clean) = without_nul(text) {
                *text = clean;
            }
        }
        Value::Array(items) => items.iter_mut().for_each(strip_nul),
        Value::Object(map) => {
            if map.keys().any(|key| key.contains('\0')) {
                let mut clean = serde_json::Map::new();
                for (key, value) in std::mem::take(map) {
                    let mut name = without_nul(&key).into_owned();
                    let mut copy = 1;
                    while clean.contains_key(&name) {
                        copy += 1;
                        name = format!("{}#{copy}", without_nul(&key));
                    }
                    clean.insert(name, value);
                }
                *map = clean;
            }
            map.values_mut().for_each(strip_nul);
        }
        _ => {}
    }
}

/// Inserts the lines in one statement, in the order and at the time they were logged.
async fn write(db: &PgPool, flow_run_id: &str, lines: &[Line]) {
    if lines.is_empty() {
        return;
    }
    let levels: Vec<&str> = lines.iter().map(|line| line.level).collect();
    let messages: Vec<&str> = lines.iter().map(|line| line.message.as_str()).collect();
    // Sent as text: the `json` column keeps it as it is, where a `jsonb` parameter
    // would come back with spaces and its keys in another order.
    let payloads: Vec<&str> = lines.iter().map(|line| line.payload.as_str()).collect();
    let dates: Vec<DateTime<Utc>> = lines.iter().map(|line| line.at).collect();

    // A failed log write must not crash the flow: it is told to the process logger.
    let written = sqlx::query(
        "INSERT INTO logs (flow_run_id, level, message, payload, created_at) \
         SELECT $1, line.level, line.message, line.payload::json, line.created_at \
         FROM UNNEST($2::text[], $3::text[], $4::text[], $5::timestamptz[]) AS line (level, message, payload, created_at)",
    )
    .bind(flow_run_id)
    .bind(levels)
    .bind(messages)
    .bind(payloads)
    .bind(dates)
    .execute(db)
    .await;
    if let Err(error) = written {
        let count = lines.len();
        logger::warn(format!(
            "[openshop] {count} log line{} of run {flow_run_id} could not be written: {error}",
            if count == 1 { "" } else { "s" }
        ));
    }
}
