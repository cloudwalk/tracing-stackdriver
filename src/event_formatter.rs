use crate::{
    google::LogSeverity,
    serializers::{SerializableSpan, SourceLocation},
    visitor::Visitor,
    writer::WriteAdaptor,
};
use serde::ser::{SerializeMap, Serializer as _};
use std::fmt;
use std::fmt::Debug;
use time::{format_description::well_known::Rfc3339, OffsetDateTime};
use tracing_core::field::Value;
use tracing_core::field::Visit;
use tracing_core::{Event, Field, Subscriber};
use tracing_subscriber::{
    field::VisitOutput,
    fmt::{
        format::{self, JsonFields},
        FmtContext, FormatEvent,
    },
    registry::LookupSpan,
};

#[derive(Debug, thiserror::Error)]
enum Error {
    #[error(transparent)]
    Formatting(#[from] fmt::Error),
    #[error("JSON serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("Time formatting error: {0}")]
    Time(#[from] time::error::Format),
}

impl From<Error> for fmt::Error {
    fn from(_: Error) -> Self {
        Self
    }
}

/// Tracing Event formatter for Stackdriver layers
pub struct EventFormatter {
    pub(crate) include_source_location: bool,
}

impl EventFormatter {
    /// Internal event formatting for a given serializer
    fn format_event<S>(
        &self,
        context: &FmtContext<S, JsonFields>,
        mut serializer: serde_json::Serializer<WriteAdaptor>,
        event: &Event,
    ) -> Result<(), Error>
    where
        S: Subscriber + for<'span> LookupSpan<'span>,
    {
        let time = OffsetDateTime::now_utc().format(&Rfc3339)?;
        let meta = event.metadata();
        let severity = LogSeverity::from(meta.level());

        let span = event
            .parent()
            .and_then(|id| context.span(id))
            .or_else(|| context.lookup_current());

        // FIXME: derive an accurate entry count ahead of time
        let mut map = serializer.serialize_map(None)?;

        // serialize custom fields
        map.serialize_entry("time", &time)?;
        map.serialize_entry("target", &meta.target())?;

        if self.include_source_location {
            if let Some(file) = meta.file() {
                map.serialize_entry(
                    "logging.googleapis.com/sourceLocation",
                    &SourceLocation {
                        file,
                        line: meta.line(),
                    },
                )?;
            }
        }

        // serialize the current span // and its leaves
        if let Some(span) = span {
            map.serialize_entry("span", &SerializableSpan::new(&span))?;
            // map.serialize_entry("spans", &SerializableContext::new(context))?; TODO: remove
        }
        let mut trace_id = TraceIdVisitor::new();
        context
            .visit_spans(|span| {
                for field in span.fields() {
                    if field.name() == "trace_id" {
                        let extensions = span.extensions();
                        if let Some(json_fields) = extensions
                            .get::<tracing_subscriber::fmt::FormattedFields<
                            tracing_subscriber::fmt::format::JsonFields,
                        >>() {
                            json_fields.record(&field, &mut trace_id);
                        }
                    }
                }
                Ok::<(), Error>(())
            })?;

        if let Some(trace_id) = trace_id.trace_id {
            map.serialize_entry("traceId", &trace_id)?;
        }

        // TODO: obtain and serialize trace_id here.
        // if let Some(trace_id) = trace_id {
        //     map.serialize_entry(
        //         "logging.googleapis.com/trace",
        //         &format!("projects/{project_id}/traces/{trace_id}",),
        //     )?;
        // }

        // serialize the stackdriver-specific fields with a visitor
        let mut visitor = Visitor::new(severity, map);
        event.record(&mut visitor);
        visitor.finish().map_err(Error::from)?;
        Ok(())
    }
}

/// A custom visitor that looks for the `trace_id` field and store its value.
struct TraceIdVisitor {
    trace_id: Option<String>,
}
impl TraceIdVisitor {
    fn new() -> Self {
        TraceIdVisitor { trace_id: None }
    }
}

impl Visit for TraceIdVisitor {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "trace_id" {
            // `trace_id` can be a json serialized string
            // -- if so, we unpack it
            let value = value
                .split(':')
                .skip(1)
                .map(|quoted| &quoted[1..quoted.len() - 2])
                .find(|_| true)
                .unwrap_or(value);

            self.trace_id = Some(value.to_string());
        }
    }
    fn record_debug(&mut self, _field: &Field, _value: &dyn Debug) {}
}

impl<S> FormatEvent<S, JsonFields> for EventFormatter
where
    S: Subscriber + for<'span> LookupSpan<'span>,
{
    fn format_event(
        &self,
        context: &FmtContext<S, JsonFields>,
        mut writer: format::Writer,
        event: &Event,
    ) -> fmt::Result
    where
        S: Subscriber + for<'span> LookupSpan<'span>,
    {
        let serializer = serde_json::Serializer::new(WriteAdaptor::new(&mut writer));
        self.format_event(context, serializer, event)?;
        writeln!(writer)
    }
}

impl Default for EventFormatter {
    fn default() -> Self {
        Self {
            include_source_location: true,
        }
    }
}
