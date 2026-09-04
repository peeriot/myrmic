//! Merges several single-load `report.json` files (one per load, each from its own fresh swarm —
//! see `benchmarks/warehouse/run_sweep.sh`) into one combined report, writing both `report.json`
//! (the merged raw data) and `report.pdf` (the rendered PDF built from it) into an output
//! directory.

use std::path::PathBuf;

use bench_report::raw::MultiRunReport;
use clap::Parser;

#[derive(Parser)]
struct Args {
    /// per-load `report.json` files to merge — order doesn't matter, the result is sorted by
    /// load.
    #[clap(required = true)]
    inputs: Vec<PathBuf>,

    /// directory to write the merged `report.json`/`report.pdf` into.
    #[clap(long)]
    output_dir: PathBuf,
}

fn main() {
    let args = Args::parse();

    let reports: Vec<MultiRunReport> = args
        .inputs
        .iter()
        .map(|path| {
            let bytes = std::fs::read(path)
                .unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display()));
            serde_json::from_slice(&bytes)
                .unwrap_or_else(|err| panic!("failed to parse {}: {err}", path.display()))
        })
        .collect();

    let merged = MultiRunReport::merge(reports);

    std::fs::create_dir_all(&args.output_dir).unwrap_or_else(|err| {
        panic!(
            "failed to create output directory {}: {err}",
            args.output_dir.display()
        )
    });

    let json_path = args.output_dir.join("report.json");
    let json_bytes = serde_json::to_vec_pretty(&merged).expect("report serializes to JSON");
    std::fs::write(&json_path, json_bytes)
        .unwrap_or_else(|err| panic!("failed to write {}: {err}", json_path.display()));

    let pdf_path = args.output_dir.join("report.pdf");
    let pdf_bytes = bench_report::rendered::pdf::render(&merged);
    std::fs::write(&pdf_path, pdf_bytes)
        .unwrap_or_else(|err| panic!("failed to write {}: {err}", pdf_path.display()));

    println!(
        "merged {} report(s) (loads {:?}) into {}",
        args.inputs.len(),
        merged.summary.loads,
        args.output_dir.display()
    );
}
