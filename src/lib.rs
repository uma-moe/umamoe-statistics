#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Statistics {
    pub count: usize,
    pub min: u64,
    pub max: u64,
    pub average: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Config {
    pub interval_ms: u64,
    pub iterations: u32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            interval_ms: 1_000,
            iterations: 5,
        }
    }
}

pub fn calculate_statistics(values: &[u64]) -> Option<Statistics> {
    if values.is_empty() {
        return None;
    }

    let count = values.len();
    let min = *values.iter().min()?;
    let max = *values.iter().max()?;
    let sum: u64 = values.iter().sum();

    Some(Statistics {
        count,
        min,
        max,
        average: sum as f64 / count as f64,
    })
}

pub fn metric_from_timestamp_ms(timestamp_ms: u128) -> u64 {
    ((timestamp_ms % 97) + 3) as u64
}

pub fn parse_config(args: &[String]) -> Result<Config, String> {
    let mut config = Config::default();

    if let Some(interval) = args.first() {
        config.interval_ms = interval
            .parse::<u64>()
            .map_err(|_| format!("Invalid interval_ms: {interval}"))?;
        if config.interval_ms == 0 {
            return Err("interval_ms must be greater than 0".to_string());
        }
    }

    if let Some(iterations) = args.get(1) {
        config.iterations = iterations
            .parse::<u32>()
            .map_err(|_| format!("Invalid iterations: {iterations}"))?;
        if config.iterations == 0 {
            return Err("iterations must be greater than 0".to_string());
        }
    }

    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::{Config, calculate_statistics, metric_from_timestamp_ms, parse_config};

    #[test]
    fn calculate_statistics_returns_none_for_empty_values() {
        assert!(calculate_statistics(&[]).is_none());
    }

    #[test]
    fn calculate_statistics_returns_expected_result() {
        let stats = calculate_statistics(&[4, 8, 12]).expect("stats should exist");

        assert_eq!(stats.count, 3);
        assert_eq!(stats.min, 4);
        assert_eq!(stats.max, 12);
        assert_eq!(stats.average, 8.0);
    }

    #[test]
    fn metric_generation_stays_in_expected_range() {
        let metric = metric_from_timestamp_ms(1_234_567_890);
        assert!((3..=99).contains(&metric));
    }

    #[test]
    fn parse_config_uses_defaults_without_args() {
        let config = parse_config(&[]).expect("config should parse");
        assert_eq!(config, Config::default());
    }

    #[test]
    fn parse_config_reads_custom_values() {
        let args = vec!["250".to_string(), "9".to_string()];
        let config = parse_config(&args).expect("config should parse");

        assert_eq!(config.interval_ms, 250);
        assert_eq!(config.iterations, 9);
    }

    #[test]
    fn parse_config_rejects_zero_values() {
        let interval_error = parse_config(&["0".to_string()]).expect_err("should reject interval");
        assert!(interval_error.contains("interval_ms"));

        let iteration_error = parse_config(&["100".to_string(), "0".to_string()])
            .expect_err("should reject iterations");
        assert!(iteration_error.contains("iterations"));
    }
}
