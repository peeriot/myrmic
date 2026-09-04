use serde::{Deserialize, Serialize};
use tracing_core::Subscriber;
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::{EnvFilter, Layer, registry::LookupSpan};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub format: Format,
    pub env_filter: Option<String>,
    pub otel_endpoint: Option<String>,
    /// When set, logs are also written to rolling files in this directory.
    #[serde(default)]
    pub directory: Option<std::path::PathBuf>,
    /// How often the log file rolls over.
    #[serde(default)]
    pub rotation: Rotation,
    /// How many rolled files to keep before the oldest is deleted.
    #[serde(default = "default_max_files")]
    pub max_files: usize,
    #[serde(default)]
    pub batch: crate::config::batch::BatchConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            format: Format::default(),
            env_filter: None,
            otel_endpoint: None,
            directory: None,
            rotation: Rotation::default(),
            max_files: default_max_files(),
            batch: Default::default(),
        }
    }
}

fn default_max_files() -> usize {
    7
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Rotation {
    Minutely,
    Hourly,
    #[default]
    Daily,
    Never,
}

impl From<Rotation> for tracing_appender::rolling::Rotation {
    fn from(value: Rotation) -> Self {
        match value {
            Rotation::Minutely => Self::MINUTELY,
            Rotation::Hourly => Self::HOURLY,
            Rotation::Daily => Self::DAILY,
            Rotation::Never => Self::NEVER,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Format {
    /// The default formatter. This emits human-readable, single-line logs for each event that occurs, with the current span context displayed before the formatted
    /// representation of the event
    #[default]
    Full,
    /// A variant of the default formatter, optimized for short line lengths. Fields from the current span context are appended to the fields of the formatted event,
    /// and span names are not shown; the verbosity level is abbreviated to a single character.
    Compact,
    /// Emits excessively pretty, multi-line logs, optimized for human readability. This is primarily intended to be used in local development and debugging, or for
    /// command-line applications, where automated analysis and compact storage of logs is less of a priority than readability and visual appeal.
    Pretty,
    /// Outputs newline-delimited JSON logs. This is intended for production use with systems where structured logs are consumed as JSON by analysis and viewing tools.
    /// The JSON output is not optimized for human readability.
    Json,
}

impl Config {
    pub(crate) fn env_filter(&self) -> EnvFilter {
        self.env_filter
            .as_ref()
            .and_then(|filter| EnvFilter::try_new(filter).ok())
            .unwrap_or_else(EnvFilter::from_default_env)
    }

    pub(crate) fn fmt_layer<S>(&self) -> Box<dyn Layer<S> + Send + Sync + 'static>
    where
        S: Subscriber + for<'a> LookupSpan<'a>,
    {
        let layer = tracing_subscriber::fmt::layer();
        match self.format {
            Format::Full => layer.boxed(),
            Format::Compact => layer.compact().boxed(),
            Format::Pretty => layer.pretty().boxed(),
            Format::Json => json_layer(layer),
        }
    }

    /// A rolling-file copy of the fmt output, or `None` when no log
    /// directory is configured (or the appender can't be created).
    pub(crate) fn file_layer<S>(&self) -> Option<Box<dyn Layer<S> + Send + Sync + 'static>>
    where
        S: Subscriber + for<'a> LookupSpan<'a>,
    {
        let directory = self.directory.as_ref()?;

        let mut builder = tracing_appender::rolling::RollingFileAppender::builder()
            .rotation(self.rotation.into())
            .filename_prefix("runtime")
            .filename_suffix("log");
        if self.max_files > 0 {
            builder = builder.max_log_files(self.max_files);
        }

        let appender = match builder.build(directory) {
            Ok(appender) => appender,
            Err(err) => {
                // The subscriber isn't installed yet, so tracing would drop this.
                eprintln!(
                    "failed to open log directory {}: {err}; file logging disabled",
                    directory.display()
                );
                return None;
            }
        };

        let layer = tracing_subscriber::fmt::layer()
            .with_writer(appender)
            .with_ansi(false);

        // FileFields on every text arm: span fields are formatted once and
        // cached per formatter type, so sharing the stdout layer's type would
        // reuse its ANSI-colored fields.
        Some(match self.format {
            Format::Full => layer.fmt_fields(FileFields::default()).boxed(),
            Format::Compact => layer.compact().fmt_fields(FileFields::default()).boxed(),
            Format::Pretty => layer.pretty().fmt_fields(FileFields::default()).boxed(),
            Format::Json => json_layer(layer),
        })
    }
}

fn json_layer<S, N, W>(
    layer: tracing_subscriber::fmt::Layer<S, N, tracing_subscriber::fmt::format::Format, W>,
) -> Box<dyn Layer<S> + Send + Sync + 'static>
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'writer> tracing_subscriber::fmt::FormatFields<'writer> + 'static,
    W: for<'a> MakeWriter<'a> + Send + Sync + 'static,
{
    layer
        .json()
        .with_thread_ids(true)
        .with_current_span(true)
        .with_span_list(true)
        .with_file(true)
        .with_line_number(true)
        .flatten_event(true)
        .boxed()
}

/// [`DefaultFields`] under a private type, so the file layer caches its own
/// (plain) rendering of span fields instead of sharing the stdout layer's.
///
/// [`DefaultFields`]: tracing_subscriber::fmt::format::DefaultFields
#[derive(Default)]
struct FileFields(tracing_subscriber::fmt::format::DefaultFields);

impl<'writer> tracing_subscriber::fmt::FormatFields<'writer> for FileFields {
    fn format_fields<R: tracing_subscriber::field::RecordFields>(
        &self,
        writer: tracing_subscriber::fmt::format::Writer<'writer>,
        fields: R,
    ) -> std::fmt::Result {
        self.0.format_fields(writer, fields)
    }
}
