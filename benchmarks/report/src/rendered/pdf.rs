//! Renders a [`MultiRunReport`] to a PDF, typeset with [Typst](https://typst.app) via
//! `typst-as-lib`.
//!
//! The document itself lives in `templates/report.typ` (embedded at compile time so the binary
//! stays self-contained) and uses the
//! [lilaq](https://typst.app/universe/package/lilaq) package for the latency violin plots.
//! `@preview` packages (`lilaq` and its dependencies) are resolved by downloading into Typst's
//! package cache on first use, same as the `typst` CLI. Fonts are Typst's own defaults, resolved
//! via typst-kit rather than vendored.

use serde::Serialize;
use typst_as_lib::{TypstEngine, typst_kit_options::TypstKitFontOptions};

use super::super::raw::{LoadDetail, MultiRunReport, RawCall};

static TEMPLATE: &str = include_str!("../../templates/report.typ");

/// Compiles a [`MultiRunReport`] (one pass per load value tested) into a PDF report and returns
/// the raw PDF bytes.
#[must_use]
pub fn render(report: &MultiRunReport) -> Vec<u8> {
    let input = PdfInput {
        report,
        // aligned index-for-index with `report.detail`, so the template can zip them together.
        detail: report.detail.iter().map(Charts::from_detail).collect(),
        latency_by_load: LatencyByLoad::from_summary(report),
    };
    let data_json = serde_json::to_vec(&input).expect("PDF render input serializes to JSON");

    let engine = TypstEngine::builder()
        .main_file(TEMPLATE)
        // typst-kit's embedded fonts only, never the (possibly absent/inconsistent) system fonts.
        .search_fonts_with(TypstKitFontOptions::default().include_system_fonts(false))
        .with_static_file_resolver([("data.json", data_json)])
        .with_package_file_resolver()
        .build();

    let document = engine
        .compile()
        .output
        .expect("report.typ failed to compile");
    typst_pdf::pdf(&document, &typst_pdf::PdfOptions::default())
        .expect("failed to export compiled report to PDF")
}

/// Everything `templates/report.typ` reads from `data.json`.
#[derive(Serialize)]
struct PdfInput<'a> {
    /// the raw report, serialized exactly as `--output` would write it — every field the
    /// template's tables display comes from here.
    report: &'a MultiRunReport,
    /// per-load call samples reshaped into flat arrays the template can hand straight to
    /// lilaq's violin plots, one entry per `report.detail` (same index). Kept separate from
    /// `report` (rather than folded into [`LoadDetail`] itself) so the `--output` JSON schema
    /// stays exactly as documented — this reshaping only exists for rendering.
    detail: Vec<Charts>,
    /// median/std-deviation of each pass's end-to-end latency, indexed the same way as
    /// `report.summary.loads`, for the "Latency vs Load" chart.
    latency_by_load: LatencyByLoad,
}

/// Median/std-deviation of end-to-end latency across a load sweep, for charting against load.
#[derive(Serialize)]
struct LatencyByLoad {
    loads: Vec<u64>,
    median_nanos: Vec<u64>,
    std_deviation_nanos: Vec<u64>,
}

impl LatencyByLoad {
    fn from_summary(report: &MultiRunReport) -> Self {
        Self {
            loads: report.summary.loads.clone(),
            median_nanos: report
                .summary
                .full_latency
                .iter()
                .map(|d| d.median_nanos)
                .collect(),
            std_deviation_nanos: report
                .summary
                .full_latency
                .iter()
                .map(|d| d.std_deviation_nanos)
                .collect(),
        }
    }
}

/// Latency samples reshaped for plotting: the same values as `report.detail[i].raw_calls`, just
/// flattened into a plain array instead of pre-aggregated into percentiles, since lilaq's
/// `violin`/`hviolin` plots compute their own kernel density estimate from raw samples.
#[derive(Serialize)]
struct Charts {
    /// every ingested call's end-to-end latency, in nanoseconds for this load.
    full_latency_nanos: Vec<u64>,
}

impl Charts {
    fn from_detail(detail: &LoadDetail) -> Self {
        let calls: &[RawCall] = &detail.raw_calls;
        Self {
            full_latency_nanos: calls.iter().map(|call| call.full_latency_nanos).collect(),
        }
    }
}
