//! Per-run logger: every line goes to the process logger and to the `logs` table.

use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::PgPool;
use tokio::sync::{mpsc, oneshot};

use super::logger::get_runtime_logger;

enum Message {
    Line {
        level: &'static str,
        message: String,
        payload: Value,
        at: DateTime<Utc>,
    },
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
            while let Some(message) = receiver.recv().await {
                match message {
                    Message::Line {
                        level,
                        message,
                        payload,
                        at,
                    } => {
                        // A failed log write must not crash the flow.
                        let _ = sqlx::query(
                            "INSERT INTO logs (flow_run_id, level, message, payload, created_at) \
                             VALUES ($1, $2, $3, $4, $5)",
                        )
                        .bind(&flow_run_id)
                        .bind(level)
                        .bind(&message)
                        .bind(sqlx::types::Json(&payload))
                        .bind(at)
                        .execute(&db)
                        .await;
                    }
                    Message::Flush(done) => {
                        let _ = done.send(());
                    }
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

        let _ = self.sender.send(Message::Line {
            level,
            message: message.to_string(),
            payload,
            at: Utc::now(),
        });
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
