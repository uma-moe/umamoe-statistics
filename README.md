# umamoe-statistics

A small Rust CLI that intermittently generates statistics from a rolling window of sampled metrics.

## Usage

```bash
cargo run -- [interval_ms] [iterations]
```

- `interval_ms` (optional): delay between samples in milliseconds (default: `1000`)
- `iterations` (optional): number of samples to produce (default: `5`)
