// Generic PDF report template, shared across benchmarks — not specific to any one benchmark's
// pipeline shape. `run.params` and `hop_coverage` entries are free-form label/value lists (see
// bench_report::raw::{RunConfig, HopCoverageRow}), so this template doesn't assume field names
// like "fan_out" or a fixed number of pipeline tiers.
//
// This is a deliberately concise summary — run configuration, latency-vs-load, ingestion, trace
// completeness, latency, and hop coverage — not a dump of every field in the raw report. Anything
// more granular (per-cell metrics, per-load per-cell latency distributions, raw per-call records)
// stays in the raw JSON report (bench_report::raw::MultiRunReport, written alongside this PDF) for
// other reports to build from, rather than being rendered here.
//
// Rendered from a single "data.json" input: `report` mirrors the raw JSON report exactly.
// `detail` (this template's top-level `payload.detail`, not `report.detail`) carries each load's
// per-call latency samples reshaped into a flat array for plotting, and `latency_by_load` the
// median/std-deviation series charted against load (both built in
// benchmarks/report/src/rendered/pdf.rs) — this template does not compute any statistics itself,
// only formatting and layout.
//
// Colors follow peeriot's brand (https://peeriot.io): the navy / orange / gold palette from the
// site's Elementor color kit. Typography uses Typst's default font.
#import "@preview/lilaq:0.6.0" as lq

#let payload = json("data.json")
#let m = payload.report
#let loads = m.summary.loads

#let brand-navy = rgb("#313458")
#let brand-orange = rgb("#ef9158")
#let brand-gold = rgb("#efb05f")
#let brand-text = rgb("#404040")
#let brand-bg = rgb("#f9fafd")
#let brand-bg-2 = rgb("#f3f5f8")
#let brand-border = rgb("#d9d9d9")
// cycled across multi-series charts (per-load/per-cell violins) so each series is visually
// distinct while staying within the brand palette, rather than lilaq's default rainbow.
#let brand-cycle = (brand-navy, brand-orange, brand-gold, rgb("#64c2d6"), rgb("#7c88b0"))

#set page(paper: "a4", margin: 2cm, numbering: "1")
#set heading(numbering: "1.")
#set text(size: 10pt, fill: brand-text)
#set table(
  stroke: 0.5pt + brand-border,
  inset: 6pt,
  fill: (_, row) => if row == 0 { brand-navy } else if calc.rem(row, 2) == 0 { brand-bg-2 } else { white },
)
#show table.cell.where(y: 0): set text(fill: white, weight: "bold")

#show heading.where(level: 1): it => block(above: 1.4em, below: 0.8em, width: 100%)[
  #set text(fill: brand-navy, size: 15pt, weight: "bold")
  #it
  #v(-0.35em)
  #line(length: 100%, stroke: 1.2pt + brand-orange)
]
#show heading.where(level: 2): set text(fill: brand-navy, size: 12pt, weight: "semibold")
#show heading.where(level: 3): set text(fill: brand-navy, size: 10.5pt, weight: "semibold")

// Mirrors test-framework's `format_duration` (swarm/test-framework/src/latency/mod.rs), so
// durations read the same way in the PDF as they do in the console output.
#let fmt-duration(ns) = {
  let ns = float(ns)
  if ns >= 1e9 {
    str(calc.round(ns / 1e9, digits: 3)) + "s"
  } else if ns >= 1e6 {
    str(calc.round(ns / 1e6, digits: 3)) + "ms"
  } else if ns >= 1e3 {
    str(calc.round(ns / 1e3, digits: 3)) + "us"
  } else {
    str(calc.round(ns, digits: 0)) + "ns"
  }
}

#let fmt-percent(p) = str(calc.round(float(p), digits: 2)) + "%"

// Dotted identifiers (e.g. "asset.object.100") have no spaces, so the table layout has nowhere
// to break them and they overflow their column. Insert a zero-width space after each "." so they
// wrap like any other text instead of spilling into neighboring cells.
#let breakable(s) = s.replace(".", "." + "\u{200B}")

// A table with one row per `rows` entry (label, then one value per load) and a header of
// `*Metric*`/`*Hop*`/etc. followed by one column per load value tested.
#let by-load-table(first-header, rows) = table(
  columns: 1 + loads.len(),
  table.header([*#first-header*], ..loads.map(l => [*#str(l)/sec*])),
  ..rows.flatten(),
)

#block(
  width: 100%,
  fill: brand-navy,
  radius: 3pt,
  inset: (x: 1.5em, y: 1.5em),
)[
  #text(fill: brand-gold, size: 10pt, weight: "semibold", tracking: 2pt)[PEERIOT]
  #v(0.4em)
  #text(fill: white, size: 20pt, weight: "bold")[#m.title]
]
#v(1em)

= Run Configuration

Shared across every load pass in this sweep.

#table(
  columns: 2,
  ..m.run.params.map(p => (p.label, p.value)).flatten(),
)

= Latency vs Load

#let lbl = payload.latency_by_load
#lq.diagram(
  width: 100%,
  height: 6cm,
  xlabel: [load (commands/sec)],
  ylabel: [seconds],
  xaxis: (ticks: lbl.loads.map(l => (l, str(l)))),
  lq.plot(
    lbl.loads,
    lbl.median_nanos.map(ns => ns / 1e9),
    yerr: lbl.std_deviation_nanos.map(ns => ns / 1e9),
    label: [Median full latency, #sym.plus.minus 1 std dev],
    color: brand-navy,
    mark: "o",
  ),
)

= Ingestion

#by-load-table("Metric", (
  ([Ingested], ..m.summary.ingestion.map(x => str(x.ingested))),
  ([Expected], ..m.summary.ingestion.map(x => str(x.expected))),
  ([Loss], ..m.summary.ingestion.map(x => str(x.loss))),
  ([Percent], ..m.summary.ingestion.map(x => fmt-percent(x.percent))),
))

= Trace Completeness

#let status-label(s) = if s == "Complete" [complete] else if s == "Stalled" [*stalled*] else if s == "TimedOut" [*timed out*] else [#s]

#by-load-table("Metric", (
  ([Successful traces], ..m.summary.trace_completeness.map(x => str(x.successful_traces))),
  ([Ingested], ..m.summary.trace_completeness.map(x => str(x.ingested))),
  ([Expected hops per call], ..m.summary.trace_completeness.map(x => str(x.expected_hop_count))),
  ([Percent], ..m.summary.trace_completeness.map(x => fmt-percent(x.percent))),
  ([Status], ..m.summary.completeness.map(status-label)),
))

// Always shown, not just when something's wrong: "complete" isn't the only value that means
// "trust these numbers", and the difference between the other two changes what to do next.
#block(fill: brand-bg-2, radius: 3pt, inset: 0.8em, width: 100%)[
  #text(size: 9pt)[
    *Status* — *complete*: every expected command and event was accounted for. *timed out*:
    still draining (or otherwise unsettled) when `drain_timeout` ran out, with no evidence either
    way of permanent loss; a longer `drain_timeout` might resolve it. *stalled*: processing
    permanently stopped short of the expected event count — some event(s) were genuinely lost; a longer `drain_timeout` will *not* fix this.
  ]
]

#if m.summary.completeness.any(s => s != "Complete") [
  #v(0.5em)
  #text(fill: brand-orange, weight: "semibold")[
    At least one load above is not complete — see its Status above and the legend for what that
    means before trusting its numbers.
  ]
]

= Latency

== Full (end-to-end)

#by-load-table("Stat", (
  ([Samples], ..m.summary.full_latency.map(d => str(d.samples))),
  ([Mean], ..m.summary.full_latency.map(d => fmt-duration(d.mean_nanos))),
  ([Median], ..m.summary.full_latency.map(d => fmt-duration(d.median_nanos))),
  ([Std dev], ..m.summary.full_latency.map(d => fmt-duration(d.std_deviation_nanos))),
  ([P95], ..m.summary.full_latency.map(d => fmt-duration(d.p95_nanos))),
  ([P99], ..m.summary.full_latency.map(d => fmt-duration(d.p99_nanos))),
))

#lq.diagram(
  width: 100%,
  height: 1cm * loads.len() + 2cm,
  xlabel: [seconds],
  yaxis: (ticks: loads.enumerate().map(((i, l)) => (i + 1, [#str(l)/sec]))),
  ..payload.detail.enumerate().map(((i, d)) => lq.hviolin(
    d.full_latency_nanos.map(ns => ns / 1e9),
    y: i + 1,
    color: brand-cycle.at(calc.rem(i, brand-cycle.len())),
  )),
)

== Event batches

One poll iteration of an event listener's dispatch loop (DB poll + dispatching every event it
returned) per sample — not tied to any one call, so no violin plot alongside it.

#by-load-table("Stat", (
  ([Batches], ..m.summary.event_batch.map(b => str(b.batches))),
  ([Mean batch size], ..m.summary.event_batch.map(b => str(calc.round(b.mean_batch_size, digits: 2)))),
  ([Mean duration], ..m.summary.event_batch.map(b => fmt-duration(b.duration.mean_nanos))),
  ([P95 duration], ..m.summary.event_batch.map(b => fmt-duration(b.duration.p95_nanos))),
))

= Hop Coverage

#by-load-table("Hop", m.summary.hop_coverage.map(hop => (
  breakable(hop.label),
  ..hop.counts.zip(hop.percents).map(((count, pct)) => str(count) + " (" + fmt-percent(pct) + ")"),
)))

= DB State

Ground truth from live DB row counts at the end of each pass, not exported/derived metrics — see
the Status legend above.

== Commands remaining

`Commands remaining` can't be an artifact of the mailbox cursor-visibility race: a nonzero value
here is a row that's definitely still in the cell's mailbox table, whether or not any cursor has
ever managed to see it.

#by-load-table("Cell", m.summary.db_backlog.map(row => (
  breakable(row.cell),
  ..row.commands_remaining.map(str),
)))

== Events produced

Events aren't scoped per cell in the DB — every publisher of a given event name shares one table
— so this is reported per event name, not per cell. A permanent total, not a snapshot: events are
never deleted.

#by-load-table("Event", m.summary.event_topics.map(row => (
  breakable(row.event),
  ..row.produced.map(str),
)))
