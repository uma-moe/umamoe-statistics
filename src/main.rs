use std::collections::VecDeque;
use std::env;
use std::process::ExitCode;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use umamoe_statistics::{calculate_statistics, metric_from_timestamp_ms, parse_config};

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let config = match parse_config(&args) {
        Ok(config) => config,
        Err(error) => {
            eprintln!("{error}");
            eprintln!("Usage: umamoe-statistics [interval_ms] [iterations]");
            return ExitCode::from(2);
        }
    };

    let mut history: VecDeque<u64> = VecDeque::with_capacity(10);

    for iteration in 0..config.iterations {
        let timestamp_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should be after UNIX_EPOCH")
            .as_millis();

        let metric = metric_from_timestamp_ms(timestamp_ms);
        if history.len() == 10 {
            history.pop_front();
        }
        history.push_back(metric);

        let snapshot: Vec<u64> = history.iter().copied().collect();
        if let Some(stats) = calculate_statistics(&snapshot) {
            println!(
                "run={}/{} metric={} window={} min={} max={} avg={:.2}",
                iteration + 1,
                config.iterations,
                metric,
                stats.count,
                stats.min,
                stats.max,
                stats.average
            );
        }

        if iteration + 1 < config.iterations {
            thread::sleep(Duration::from_millis(config.interval_ms));
        }
    }

    ExitCode::SUCCESS
}
