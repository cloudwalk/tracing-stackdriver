use crate::{
    google::LogSeverity,
    serializers::{SerializableSpan, SourceLocation},
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

        // serialize the current span and its leaves
        if let Some(span) = span {
            map.serialize_entry("span", &SerializableSpan::new(&span))?;
            // map.serialize_entry("spans", &SerializableContext::new(context))?; TODO: remove
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
