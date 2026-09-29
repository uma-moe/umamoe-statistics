#[derive(Parser, Debug)]
#[command(
    about = "Generate Uma.moe statistics from PostgreSQL without loading the full table into memory"
)]
struct Args {
    #[arg(long)]
    database_url: Option<String>,

    #[arg(long, default_value = ".")]
    repo_root: PathBuf,

    #[arg(long)]
    dataset_version: Option<String>,

    #[arg(long, default_value = "statistics")]
    output_dir: PathBuf,

    #[arg(long = "publish-dir")]
    publish_dirs: Vec<PathBuf>,

    #[arg(long)]
    limit: Option<u64>,

    #[arg(long, default_value_t = 250_000)]
    progress_every: u64,

    #[arg(long)]
    resource_usage: bool,

    #[arg(long)]
    worker_threads: Option<usize>,

    #[arg(long, default_value_t = 100_000)]
    batch_rows: usize,
}
