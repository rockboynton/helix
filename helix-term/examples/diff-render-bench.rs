//! Measure initialized sequential rendering, without adding a daemon or CLI mode.
//! cargo run --release -p helix-term --example diff-render-bench -- LEFT RIGHT [inline|side-by-side] [ITERATIONS]
use std::{collections::BTreeMap, path::PathBuf, time::Instant};

use anyhow::{ensure, Result};
use helix_term::{
    args::Args,
    diff_render::{ColorMode, Layout, RenderSession},
};

#[tokio::main]
async fn main() -> Result<()> {
    helix_loader::initialize_config_file(None);
    helix_loader::initialize_log_file(None);
    let mut argv = std::env::args().skip(1);
    let left = PathBuf::from(argv.next().expect("LEFT path"));
    let right = PathBuf::from(argv.next().expect("RIGHT path"));
    let layout = match argv.next().as_deref() {
        None | Some("side-by-side") => Layout::SideBySide,
        Some("inline") => Layout::Inline,
        _ => anyhow::bail!("expected inline or side-by-side"),
    };
    let iterations = argv
        .next()
        .map(|n| n.parse::<usize>())
        .transpose()?
        .unwrap_or(20);
    ensure!(iterations > 0, "iterations must be positive");
    let mut args = Args {
        render_diff: true,
        diff_layout: Some(layout),
        diff_width: Some(160),
        diff_color: Some(ColorMode::Always),
        ..Args::default()
    };
    args.files.insert(left, Vec::new());
    args.files.insert(right, Vec::new());
    let start = Instant::now();
    let mut session = RenderSession::new(&args)?;
    let entries = std::env::var("DIFF_BENCH_CACHE_ENTRIES")
        .ok()
        .map(|value| value.parse::<usize>())
        .transpose()?;
    let bytes = std::env::var("DIFF_BENCH_CACHE_BYTES")
        .ok()
        .map(|value| value.parse::<usize>())
        .transpose()?;
    if entries.is_some() || bytes.is_some() {
        session.set_document_cache_limits(entries.unwrap_or(64), bytes.unwrap_or(16 * 1024 * 1024));
    }
    let init_ms = start.elapsed().as_secs_f64() * 1000.0;
    if let Some(manifest) = std::env::var_os("DIFF_BENCH_SEQUENCE") {
        return sequence(&mut session, args, PathBuf::from(manifest), init_ms);
    }
    let start = Instant::now();
    let mut expected = Vec::new();
    session.render(&args, &mut expected, false)?;
    let first_ms = start.elapsed().as_secs_f64() * 1000.0;
    let mut times = Vec::with_capacity(iterations);
    let profile = std::env::var_os("DIFF_BENCH_PROFILE").is_some();
    let mut profiles = BTreeMap::<PathBuf, [f64; 6]>::new();
    let mut cache_hits = 0;
    for _ in 0..iterations {
        let mut output = Vec::new();
        let start = Instant::now();
        let stages = if profile {
            session.render_profiled(&args, &mut output, false)?
        } else {
            session.render(&args, &mut output, false)?;
            Vec::new()
        };
        times.push(start.elapsed().as_secs_f64() * 1000.0);
        ensure!(output == expected, "repeated request changed output");
        for file in stages {
            cache_hits += file.cache_hits;
            let sums = profiles.entry(file.path).or_default();
            for (sum, duration) in sums.iter_mut().zip([
                file.read,
                file.document,
                file.diff,
                file.render,
                file.prepare,
                file.syntax,
            ]) {
                *sum += duration.as_secs_f64() * 1000.0;
            }
        }
    }
    times.sort_by(f64::total_cmp);
    let median = (times[(iterations - 1) / 2] + times[iterations / 2]) / 2.0;
    if let Some(path) = std::env::var_os("DIFF_BENCH_OUTPUT") {
        std::fs::write(path, &expected)?;
    }
    println!("init_ms={init_ms:.2} first_ms={first_ms:.2} warm_median_ms={median:.2} warm_min_ms={:.2} warm_max_ms={:.2} iterations={iterations} bytes={}", times[0], times[iterations-1], expected.len());
    let mut total = [0.0; 6];
    for (path, sums) in profiles {
        let average = sums.map(|sum| sum / iterations as f64);
        for (total, value) in total.iter_mut().zip(average) {
            *total += value;
        }
        println!(
            "profile_avg_ms read={:.2} document={:.2} diff={:.2} render={:.2} prepare={:.2} syntax={:.2} file={}",
            average[0],
            average[1],
            average[2],
            average[3],
            average[4],
            average[5],
            path.display()
        );
    }
    if profile {
        println!(
            "profile_total_avg_ms read={:.2} document={:.2} diff={:.2} render={:.2} prepare={:.2} syntax={:.2} cache_hits={cache_hits}",
            total[0], total[1], total[2], total[3], total[4], total[5]
        );
    }
    Ok(())
}

/// A sequence manifest contains [left, right, expected-CLI-output] path triples.
/// Unlike the repeat benchmark, no requests are warmed before timing begins.
fn sequence(
    session: &mut RenderSession,
    mut args: Args,
    manifest: PathBuf,
    init_ms: f64,
) -> Result<()> {
    let requests: Vec<[PathBuf; 3]> = serde_json::from_slice(&std::fs::read(manifest)?)?;
    ensure!(!requests.is_empty(), "empty request sequence");
    println!("sequence_init_ms={init_ms:.2}");
    for (index, [left, right, expected]) in requests.into_iter().enumerate() {
        let expected = std::fs::read(expected)?;
        args.files.clear();
        args.files.insert(left, Vec::new());
        args.files.insert(right, Vec::new());
        let mut output = Vec::new();
        let start = Instant::now();
        let profile = session.render_profiled(&args, &mut output, false)?;
        let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;
        ensure!(
            output == expected,
            "request {index} differs from CLI output"
        );
        let hits: usize = profile.iter().map(|file| file.cache_hits).sum();
        let syntax_ms: f64 = profile
            .iter()
            .map(|file| file.syntax.as_secs_f64() * 1000.0)
            .sum();
        let (entries, source_bytes) = session.document_cache_usage();
        println!("request={index} elapsed_ms={elapsed_ms:.2} files={} cache_hits={hits} syntax_ms={syntax_ms:.2} cached_entries={entries} cached_source_bytes={source_bytes} output_matches_CLI=true", profile.len());
    }
    Ok(())
}
