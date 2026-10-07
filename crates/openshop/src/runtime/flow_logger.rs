//! Per-run logger: every line goes to the process logger and to the `logs` table.

use std::time::Duration;

use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::PgPool;
use tokio::sync::{mpsc, oneshot};

use super::logger::get_runtime_logger;

/// How long a line may wait for the next ones before it is written.
const WRITE_DELAY: Duration = Duration::from_millis(50);

struct Line {
    level: &'static str,
    message: String,
    payload: Value,
    at: DateTime<Utc>,
}

enum Message {
    Line(Line),
    Flush(oneshot::Sender<()>),
}

/// Writes are queued and persisted in emission order by a background task, so
/// logging never blocks or fails a flow.
#[derive(Clone)]
pub struct FlowLogger {
    sender: mpsc::UnboundedSender<Message>,
}

impl FlowLogger {
    pub fn new(db: PgPool, flow_run_id: String) -> Self {
        let (sender, mut receiver) = mpsc::unbounded_channel();

        tokio::spawn(async move {
            let mut lines = Vec::new();
            let mut flushes = Vec::new();
            while let Some(first) = receiver.recv().await {
                match first {
                    Message::Line(line) => lines.push(line),
                    Message::Flush(done) => flushes.push(done),
                }
                // The lines logged within `WRITE_DELAY` of the first one go to the
                // database with it. A flush, at the end of the run, does not wait.
                if flushes.is_empty() {
                    let delay = tokio::time::sleep(WRITE_DELAY);
                    tokio::pin!(delay);
                    loop {
                        tokio::select! {
                            biased;
                            message = receiver.recv() => match message {
                                Some(Message::Line(line)) => lines.push(line),
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
                write(&db, &flow_run_id, &lines).await;
                lines.clear();
                for done in flushes.drain(..) {
                    let _ = done.send(());
                }
            }
        });

        Self { sender }
    }

    fn log(&self, level: &'static str, payload: Value, message: &str) {
        let line = format!("[{}] {message}", level.to_uppercase());
        let logger = get_runtime_logger();
        match level {
            "warn" => logger.warn(&line, Some(&payload)),
            "error" => logger.error(&line, Some(&payload)),
            _ => logger.info(&line, Some(&payload)),
        }

        let _ = self.sender.send(Message::Line(Line {
            level,
            message: message.to_string(),
            payload,
            at: Utc::now(),
        }));
    }

    pub fn info(&self, payload: Value, message: impl AsRef<str>) {
        self.log("info", payload, message.as_ref());
    }

    pub fn warn(&self, payload: Value, message: impl AsRef<str>) {
        self.log("warn", payload, message.as_ref());
    }

    pub fn error(&self, payload: Value, message: impl AsRef<str>) {
        self.log("error", payload, message.as_ref());
    }

    /// Resolves once every line logged so far has been written.
    pub async fn flush(&self) {
        let (done, wait) = oneshot::channel();
        if self.sender.send(Message::Flush(done)).is_ok() {
            let _ = wait.await;
        }
    }
}

/// Inserts the lines in one statement, in the order and at the time they were logged.
async fn write(db: &PgPool, flow_run_id: &str, lines: &[Line]) {
    if lines.is_empty() {
        return;
    }
    let levels: Vec<&str> = lines.iter().map(|line| line.level).collect();
    let messages: Vec<&str> = lines.iter().map(|line| line.message.as_str()).collect();
    let payloads: Vec<sqlx::types::Json<&Value>> = lines.iter().map(|line| sqlx::types::Json(&line.payload)).collect();
    let dates: Vec<DateTime<Utc>> = lines.iter().map(|line| line.at).collect();

    // A failed log write must not crash the flow.
    let _ = sqlx::query(
        "INSERT INTO logs (flow_run_id, level, message, payload, created_at) \
         SELECT $1, line.* FROM UNNEST($2::text[], $3::text[], $4::jsonb[], $5::timestamptz[]) AS line",
    )
    .bind(flow_run_id)
    .bind(levels)
    .bind(messages)
    .bind(payloads)
    .bind(dates)
    .execute(db)
    .await;
}
