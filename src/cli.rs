use anyhow::{anyhow, Context, Result};
use chrono::Local;
use clap::Parser;
use postgres::{Client, NoTls};
use std::env;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::Instant;

use crate::resources::{printProgress, ResourceMonitor};
use crate::statistics::{
    loadSupportCardTypes,
    source::{statisticsCopyQuery, streamRowsParallel, streamRowsSerial},
    Compiler,
};

include!("types/cli.rs");

pub(crate) fn run() -> Result<()> {
    dotenvy::dotenv().ok();

    let args = Args::parse();
    let mut resource_monitor = if args.resource_usage {
        Some(ResourceMonitor::new()?)
    } else {
        None
    };
    let worker_threads = args
        .worker_threads
        .unwrap_or_else(defaultWorkerThreads)
        .max(1);
    let batch_rows = args.batch_rows.max(1);
    let progress_every = args.progress_every;
    let support_card_types = Arc::new(loadSupportCardTypes()?);
    let repo_root = args.repo_root.canonicalize().unwrap_or(args.repo_root);
    let database_url = args
        .database_url
        .or_else(|| env::var("DATABASE_URL").ok())
        .ok_or_else(|| anyhow!("Set DATABASE_URL or pass --database-url"))?;
    let dataset_version = args
        .dataset_version
        .unwrap_or_else(|| Local::now().format("%Y-%m-%d").to_string());
    let output_root = resolveFrom(&repo_root, args.output_dir);
    let publish_roots = if args.publish_dirs.is_empty() {
        vec![output_root]
    } else {
        args.publish_dirs
            .into_iter()
            .map(|path| resolveFrom(&repo_root, path))
            .collect::<Vec<_>>()
    };
    println!(
        "Loaded {} support-card type mappings from cards.json...",
        support_card_types.len()
    );
    println!("Connecting to PostgreSQL...");
    let mut client = Client::connect(&database_url, NoTls).context("connect to PostgreSQL")?;

    let mut compiler = Compiler::new(dataset_version.clone());
    let started_at = Instant::now();
    let query = statisticsCopyQuery(args.limit);
    let reader = client
        .copy_out(query.as_str())
        .context("start PostgreSQL COPY stream")?;
    let mut csv_reader = csv::ReaderBuilder::new()
        .has_headers(true)
        .from_reader(reader);
    let headers = csv_reader.headers().context("read COPY headers")?.clone();

    if worker_threads > 1 {
        println!("Using {worker_threads} worker threads with {batch_rows} row batches...");
        streamRowsParallel(
            &mut csv_reader,
            &headers,
            &mut compiler,
            &dataset_version,
            worker_threads,
            batch_rows,
            progress_every,
            Arc::clone(&support_card_types),
            &mut resource_monitor,
            started_at,
        )?;
    } else {
        streamRowsSerial(
            &mut csv_reader,
            &headers,
            &mut compiler,
            progress_every,
            support_card_types.as_ref(),
            &mut resource_monitor,
            started_at,
        )?;
    }

    compiler.finish();
    println!(
        "Finished streaming {:} rows in {:.1}s. Writing JSON...",
        compiler.totalEntries(),
        started_at.elapsed().as_secs_f64()
    );
    printProgress(
        &mut resource_monitor,
        "streamed",
        compiler.totalEntries(),
        started_at.elapsed(),
    );

    for publish_root in &publish_roots {
        println!("Writing statistics to {}...", publish_root.display());
        compiler.writeOutputs(publish_root)?;
    }

    println!(
        "Done. Dataset {} written to {} output root(s).",
        dataset_version,
        publish_roots.len()
    );
    printProgress(
        &mut resource_monitor,
        "completed",
        compiler.totalEntries(),
        started_at.elapsed(),
    );
    Ok(())
}

fn defaultWorkerThreads() -> usize {
    thread::available_parallelism()
        .map(|threads| threads.get().min(3))
        .unwrap_or(1)
}

fn resolveFrom(base: &Path, path: PathBuf) -> PathBuf {
    if path.is_absolute() {
        path
    } else {
        base.join(path)
    }
}
