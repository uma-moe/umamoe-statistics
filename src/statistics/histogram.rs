use serde_json::{json, Map, Value};

use super::{mergeCountMap, StatAccumulator};

impl StatAccumulator {
    pub(super) fn add(&mut self, value: i32) {
        self.count += 1;
        let value_f64 = value as f64;
        self.sum += value_f64;
        self.sum_sq += value_f64 * value_f64;
        self.min = Some(self.min.map_or(value, |current| current.min(value)));
        self.max = Some(self.max.map_or(value, |current| current.max(value)));
        *self.values.entry(value).or_insert(0) += 1;
    }

    pub(super) fn merge(&mut self, other: Self) {
        self.count += other.count;
        self.sum += other.sum;
        self.sum_sq += other.sum_sq;
        self.min = match (self.min, other.min) {
            (Some(left), Some(right)) => Some(left.min(right)),
            (Some(value), None) | (None, Some(value)) => Some(value),
            (None, None) => None,
        };
        self.max = match (self.max, other.max) {
            (Some(left), Some(right)) => Some(left.max(right)),
            (Some(value), None) | (None, Some(value)) => Some(value),
            (None, None) => None,
        };
        mergeCountMap(&mut self.values, other.values);
    }

    pub(super) fn fullJson(&self, stat_name: &str) -> Value {
        if self.count == 0 {
            return Value::Object(Map::new());
        }

        let mean = self.sum / self.count as f64;
        let variance = if self.count > 1 {
            (self.sum_sq - (self.sum * self.sum / self.count as f64)) / (self.count - 1) as f64
        } else {
            0.0
        };

        json!({
            "mean": mean,
            "std": variance.max(0.0).sqrt(),
            "min": self.min.unwrap_or_default(),
            "max": self.max.unwrap_or_default(),
            "median": self.percentile(50.0),
            "percentiles": {
                "25": self.percentile(25.0),
                "50": self.percentile(50.0),
                "75": self.percentile(75.0),
                "95": self.percentile(95.0)
            },
            "count": self.count,
            "histogram": self.histogram(stat_name)
        })
    }

    pub(super) fn partialJson(&self) -> Value {
        if self.count == 0 {
            return Value::Object(Map::new());
        }

        json!({
            "mean": self.sum / self.count as f64,
            "median": self.percentile(50.0),
            "min": self.min.unwrap_or_default(),
            "max": self.max.unwrap_or_default(),
            "count": self.count
        })
    }

    fn percentile(&self, percentile: f64) -> f64 {
        if self.count == 0 {
            return 0.0;
        }

        let position = (self.count - 1) as f64 * percentile / 100.0;
        let lower = position.floor() as u64;
        let upper = position.ceil() as u64;
        let lower_value = self.valueAtRank(lower);
        let upper_value = self.valueAtRank(upper);
        lower_value + (upper_value - lower_value) * (position - lower as f64)
    }

    fn valueAtRank(&self, rank: u64) -> f64 {
        let mut pairs: Vec<(i32, u64)> = self
            .values
            .iter()
            .map(|(value, count)| (*value, *count))
            .collect();
        pairs.sort_unstable_by_key(|(value, _)| *value);

        let mut seen = 0_u64;
        for (value, count) in pairs {
            if rank < seen + count {
                return value as f64;
            }
            seen += count;
        }

        self.max.unwrap_or_default() as f64
    }

    fn histogram(&self, stat_name: &str) -> Value {
        let (default_min, default_max, buckets) = statConfig(stat_name);
        let min_value = i64::from(self.min.unwrap_or(default_min).min(default_min));
        let max_value = i64::from(self.max.unwrap_or(default_max).max(default_max));
        // Keep legacy ranges until data exceeds them; round up to whole-width buckets.
        let bucket_width = (max_value - min_value + buckets as i64 - 1) / buckets as i64;
        let mut counts = vec![0_u64; buckets];

        for (value, count) in &self.values {
            let index =
                ((i64::from(*value) - min_value) / bucket_width).min(buckets as i64 - 1) as usize;
            counts[index] += count;
        }

        let mut map = Map::new();
        for (index, count) in counts.into_iter().enumerate() {
            let start = min_value + bucket_width * index as i64;
            let end = start + bucket_width;
            map.insert(format!("{start}-{end}"), json!(count));
        }
        Value::Object(map)
    }
}

fn statConfig(stat_name: &str) -> (i32, i32, usize) {
    match stat_name {
        "rank_score" => (0, 17_000, 20),
        _ => (0, 1_200, 20),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn histogramsKeepLegacyBoundsAndIncludeLargerStats() {
        for (name, values, expected_bins) in [
            (
                "speed",
                vec![0, 59, 60, 1199, 1200],
                vec![("0-60", 2), ("60-120", 1), ("1140-1200", 2)],
            ),
            (
                "rank_score",
                vec![0, 850, 17000],
                vec![("0-850", 1), ("850-1700", 1), ("16150-17000", 1)],
            ),
            (
                "speed",
                vec![0, 1200, 1201, 2001],
                vec![("0-101", 1), ("1111-1212", 2), ("1919-2020", 1)],
            ),
            ("speed", vec![1900, 2000], vec![("1900-2000", 2)]),
            (
                "rank_score",
                vec![17000, 17001, 60001],
                vec![("15005-18006", 2), ("57019-60020", 1)],
            ),
            (
                "speed",
                vec![i32::MIN, 0, i32::MAX],
                vec![("1932735287-2147483652", 1)],
            ),
        ] {
            let mut accumulator = StatAccumulator::default();
            for value in &values {
                accumulator.add(*value);
            }
            let report = accumulator.fullJson(name);
            let histogram = report["histogram"].as_object().unwrap();
            assert_eq!(histogram.len(), 20);
            assert_eq!(
                histogram
                    .values()
                    .map(|value| value.as_u64().unwrap())
                    .sum::<u64>(),
                values.len() as u64
            );
            assert_eq!(report["count"], values.len());
            assert_eq!(report["max"], *values.iter().max().unwrap());
            for (range, count) in expected_bins {
                assert_eq!(histogram[range], count, "{name}: {range}");
            }
        }
        assert_eq!(StatAccumulator::default().fullJson("speed"), json!({}));
    }
}
