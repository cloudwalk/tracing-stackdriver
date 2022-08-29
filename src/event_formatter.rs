use crate::{
    google::LogSeverity,
    serializers::{SerializableContext, SerializableSpan},
    visitor::Visitor,
    writer::WriteAdaptor,
};
use serde::ser::{SerializeMap, Serializer as _};
use std::fmt;
use time::{format_description::well_known::Rfc3339, OffsetDateTime};
use tracing_core::{Event, Subscriber};
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
#[derive(Default)]
pub struct EventFormatter {
    project_id: Option<String>,
}

impl EventFormatter {
    pub(crate) fn project_id(mut self, project_id: String) -> Self {
        self.project_id = Some(project_id);
        self
    }
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

        // serialize the current span and its leaves
        if let Some(span) = span {
            map.serialize_entry("span", &SerializableSpan::new(&span))?;
            map.serialize_entry("spans", &SerializableContext::new(context))?;

            #[cfg(feature = "opentelemetry")]
            if let Some(otel_data) = span.extensions().get::<tracing_opentelemetry::OtelData>() {
                use opentelemetry::trace::TraceContextExt;
                println!("{:?}", otel_data.parent_cx);
                let builder = &otel_data.builder;

                println!("{builder:?}");

                // FIXME: make sure that a span ID is actually generated for OtelData by this point
                if let Some(span_id) = builder.span_id {
                    map.serialize_entry("logging.googleapis.com/spanId", &span_id.to_string())?;
                }

                let (trace_id, trace_sampled) = if otel_data.parent_cx.has_active_span() {
                    let span_ref = otel_data.parent_cx.span();
                    let span_context = span_ref.span_context();
                    (Some(span_context.trace_id()), span_context.is_sampled())
                } else {
                    (builder.trace_id, false)
                };

                if let (Some(trace_id), Some(project_id)) = (trace_id, self.project_id.as_ref()) {
                    map.serialize_entry(
                        "logging.googleapis.com/trace",
                        &format!("projects/{project_id}/traces/{trace_id}"),
                    )?;
                }
                if trace_sampled {
                    map.serialize_entry("logging.googleapis.com/trace_sampled", &true)?;
                }
            }
        }

        // serialize the stackdriver-specific fields with a visitor
        let mut visitor = Visitor::new(severity, map);
        event.record(&mut visitor);
        visitor.finish().map_err(Error::from)?;
        Ok(())
    }
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
