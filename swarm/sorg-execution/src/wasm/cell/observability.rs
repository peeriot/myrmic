//! Span creation and trace-context propagation for a cell's message
//! processing. Kept separate from cell state and the dispatch loop: it is a
//! cross-cutting concern that is transparent to cell logic — stripping it
//! leaves cell functionality intact (minus tracing).

use opentelemetry::trace::{SpanContext, TraceContextExt};
use tracing::Level;
use tracing_opentelemetry::OpenTelemetrySpanExt;

use crate::wasm::cell::IncomingMessage;

/// Creates an observability span for the incoming message, links it to any
/// previous span context carried in the message, and returns the span and the
/// extracted span context for outbound propagation.
#[track_caller]
pub(crate) fn begin_observability(
    incoming: &IncomingMessage,
    module_id: &str,
) -> (tracing::Span, Option<SpanContext>) {
    // retrieve the actual callsite of #[track_caller] chain as we want to track the actual
    // location where the span is started and not the location of the tracing::span! call in this
    // function
    let location = std::panic::Location::caller();
    let file = location.file();
    // casting to i64 here, otherwise it gets converted into a string in tracing/opentelemetry
    let line = i64::from(location.line());

    let uptime = crate::PROCESS_START.elapsed();
    let uptime_secs = uptime.as_secs();
    let uptime_subsec_nanos = uptime.subsec_nanos();

    let span = tracing::span!(
        parent: None,
        Level::INFO,
        "cell_task::message_handler",
        message_type = incoming.ty(),
        identifier = incoming.identifier(),
        module_id,
        file.path = file,
        file.line = line,
        uptime_secs,
        uptime_subsec_nanos
    );

    // if a span is disabled (level is ignored) setting the parent fails, and as it is ignored
    // anyways we are not even trying to the parent here
    if !span.is_disabled()
        && let Some(previous) = &incoming.span_context
    {
        let _ = span
            .set_parent(opentelemetry::Context::new().with_remote_span_context(previous.clone()))
            .inspect_err(|err| tracing::warn!("Failed to set span parent: {err}"));

        span.add_link(previous.clone());
    }

    // enter briefly to record start_time — tracing-opentelemetry defers
    // span start until first on_enter, so without this start == end
    drop(span.enter());

    let span_context = span.context().span().span_context().clone();
    let valid_context = span_context.is_valid().then_some(span_context);

    (span, valid_context)
}

/// Closes the observability span, recording its end time.
pub(crate) fn finish_observability(span: tracing::Span) {
    drop(span);
}
