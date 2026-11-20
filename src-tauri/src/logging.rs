use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tauri::{AppHandle, Emitter};
use tracing_subscriber::layer::Context;
use tracing_subscriber::{Layer, registry::LookupSpan};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtendedLogRecord {
    pub level: u8,
    pub message: String,
    pub target: Option<String>,
    pub timestamp: String,
}

pub struct FrontendLoggingLayer {
    app_handle: Arc<AppHandle>,
}

impl FrontendLoggingLayer {
    pub fn new(app_handle: AppHandle) -> Self {
        Self {
            app_handle: Arc::new(app_handle),
        }
    }
}

impl<S> Layer<S> for FrontendLoggingLayer
where
    S: tracing::Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        use tracing::field::Visit;

        let metadata = event.metadata();
        let level = match *metadata.level() {
            tracing::Level::TRACE => 1u8,
            tracing::Level::DEBUG => 2u8,
            tracing::Level::INFO => 3u8,
            tracing::Level::WARN => 4u8,
            tracing::Level::ERROR => 5u8,
        };

        let target = metadata.target().to_string();

        struct EventVisitor {
            message: String,
            fields: Vec<String>,
        }

        impl EventVisitor {
            fn push_field(&mut self, field: &tracing::field::Field, value: String) {
                if field.name() == "message" {
                    self.message = value;
                } else {
                    self.fields.push(format!("{}={}", field.name(), value));
                }
            }
        }

        impl Visit for EventVisitor {
            fn record_i64(&mut self, field: &tracing::field::Field, value: i64) {
                self.push_field(field, value.to_string());
            }

            fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
                self.push_field(field, value.to_string());
            }

            fn record_bool(&mut self, field: &tracing::field::Field, value: bool) {
                self.push_field(field, value.to_string());
            }

            fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
                self.push_field(field, value.to_string());
            }

            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                self.push_field(field, format!("{:?}", value));
            }
        }

        let mut visitor = EventVisitor {
            message: String::new(),
            fields: Vec::new(),
        };
        event.record(&mut visitor);

        let message = if visitor.fields.is_empty() {
            visitor.message
        } else {
            format!("{} ({})", visitor.message, visitor.fields.join(", "))
        };

        let timestamp = chrono::Local::now()
            .format("%Y-%m-%d %H:%M:%S%.3f")
            .to_string();

        let record = ExtendedLogRecord {
            level,
            message,
            target: Some(target),
            timestamp,
        };

        let _ = self.app_handle.emit("log-record", &record);
    }
}
