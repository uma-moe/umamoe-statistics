use anyhow::{anyhow, Context, Result};
use chrono::Local;
use clap::Parser;
use csv::StringRecord;
use postgres::{Client, NoTls};
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};
use std::env;
use std::fs;
use std::hash::Hash;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use sysinfo::{get_current_pid, Pid, System};

const STAT_NAMES: [&str; 6] = ["speed", "power", "stamina", "wiz", "guts", "rank_score"];
const DISTANCE_IDS: [u8; 5] = [1, 2, 3, 4, 5];
const DATA_FORMAT: &str = "ids-v1";
const DATA_FORMAT_VERSION: u8 = 3;
const INLINE_SUPPORT_DECK_SIZE: usize = 6;
const UNKNOWN_SUPPORT_CARD_TYPE: u32 = 99;
const SUPPORT_CARDS_JSON: &str = include_str!("cards.json");

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

#[derive(Clone)]
struct RowData {
    trainer_id: String,
    card_id: u32,
    distance_type: u8,
    scenario_id: u8,
    running_style: u8,
    team_class: Option<u8>,
    stats: [i32; 6],
    skills: Vec<u32>,
    support_cards: Vec<u32>,
}

struct PreparedRow {
    support_items: Vec<(u32, usize)>,
    skill_items: Vec<(u32, usize)>,
    support_count: u64,
    skill_count: u64,
    support_deck_id: Option<u32>,
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
enum SupportDeckKey {
    Inline {
        len: u8,
        ids: [u32; INLINE_SUPPORT_DECK_SIZE],
    },
    Overflow(Vec<u32>),
}

impl SupportDeckKey {
    fn from_ids(type_ids: &[u32]) -> Option<Self> {
        if type_ids.is_empty() {
            return None;
        }

        if type_ids.len() <= INLINE_SUPPORT_DECK_SIZE {
            let mut ids = [0_u32; INLINE_SUPPORT_DECK_SIZE];
            for (index, type_id) in type_ids.iter().enumerate() {
                ids[index] = *type_id;
            }
            ids[..type_ids.len()].sort_unstable();
            Some(Self::Inline {
                len: type_ids.len() as u8,
                ids,
            })
        } else {
            let mut deck_ids = type_ids.to_vec();
            deck_ids.sort_unstable();
            Some(Self::Overflow(deck_ids))
        }
    }

    fn ids(&self) -> &[u32] {
        match self {
            Self::Inline { len, ids } => &ids[..*len as usize],
            Self::Overflow(ids) => ids.as_slice(),
        }
    }

    fn key_string(&self) -> String {
        let mut key = String::new();
        for (index, id) in self.ids().iter().enumerate() {
            if index > 0 {
                key.push('_');
            }
            key.push_str(&id.to_string());
        }
        key
    }

    fn ids_json(&self) -> Value {
        Value::Array(self.ids().iter().map(|id| json!(id.to_string())).collect())
    }
}

struct SupportCardTypes {
    by_card_id: HashMap<u32, u32>,
}

impl SupportCardTypes {
    fn card_type(&self, card_id: u32) -> u32 {
        self.by_card_id
            .get(&card_id)
            .copied()
            .unwrap_or(UNKNOWN_SUPPORT_CARD_TYPE)
    }

    fn len(&self) -> usize {
        self.by_card_id.len()
    }
}

#[derive(Default)]
struct SupportDeckInterner {
    keys: Vec<SupportDeckKey>,
    ids: HashMap<SupportDeckKey, u32>,
}

impl SupportDeckInterner {
    fn intern(&mut self, key: SupportDeckKey) -> u32 {
        if let Some(id) = self.ids.get(&key) {
            return *id;
        }

        let id = self.keys.len() as u32;
        self.keys.push(key.clone());
        self.ids.insert(key, id);
        id
    }

    fn get(&self, id: u32) -> Option<&SupportDeckKey> {
        self.keys.get(id as usize)
    }
}

struct ResourceMonitor {
    system: System,
    pid: Pid,
    peak_exporter_memory: u64,
}

impl ResourceMonitor {
    fn new() -> Result<Self> {
        let pid = get_current_pid().map_err(|error| anyhow!(error))?;
        let mut system = System::new_all();
        system.refresh_all();

        Ok(Self {
            system,
            pid,
            peak_exporter_memory: 0,
        })
    }

    fn summary(&mut self) -> String {
        self.system.refresh_all();
        let cpu_scale = self.system.cpus().len().max(1) as f32;

        let process = self.system.process(self.pid);
        let exporter_cpu = process.map_or(0.0, |process| process.cpu_usage()) / cpu_scale;
        let exporter_memory = process.map_or(0, |process| process.memory());
        self.peak_exporter_memory = self.peak_exporter_memory.max(exporter_memory);

        let mut postgres_count = 0_u64;
        let mut postgres_cpu = 0.0_f32;
        let mut postgres_memory = 0_u64;
        for process in self.system.processes().values() {
            if process.name().to_ascii_lowercase().contains("postgres") {
                postgres_count += 1;
                postgres_cpu += process.cpu_usage();
                postgres_memory += process.memory();
            }
        }
        postgres_cpu /= cpu_scale;

        format!(
            "cpu exporter {:.1}% postgres {:.1}% system {:.1}% | mem exporter {:.1} MB peak {:.1} MB postgres {:.1} MB/{} proc system {:.1}/{:.1} GB",
            exporter_cpu,
            postgres_cpu,
            self.system.global_cpu_info().cpu_usage(),
            bytes_to_mb(exporter_memory),
            bytes_to_mb(self.peak_exporter_memory),
            bytes_to_mb(postgres_memory),
            postgres_count,
            bytes_to_gb(self.system.used_memory()),
            bytes_to_gb(self.system.total_memory())
        )
    }
}

#[derive(Clone, Default)]
struct StatAccumulator {
    count: u64,
    sum: f64,
    sum_sq: f64,
    min: Option<i32>,
    max: Option<i32>,
    values: HashMap<i32, u64>,
}

impl StatAccumulator {
    fn add(&mut self, value: i32) {
        self.count += 1;
        let value_f64 = value as f64;
        self.sum += value_f64;
        self.sum_sq += value_f64 * value_f64;
        self.min = Some(self.min.map_or(value, |current| current.min(value)));
        self.max = Some(self.max.map_or(value, |current| current.max(value)));
        *self.values.entry(value).or_insert(0) += 1;
    }

    fn merge(&mut self, other: Self) {
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
        merge_count_map(&mut self.values, other.values);
    }

    fn full_json(&self, stat_name: &str) -> Value {
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

    fn partial_json(&self) -> Value {
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
        let lower_value = self.value_at_rank(lower);
        let upper_value = self.value_at_rank(upper);
        lower_value + (upper_value - lower_value) * (position - lower as f64)
    }

    fn value_at_rank(&self, rank: u64) -> f64 {
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
        let (min_value, max_value, buckets) = stat_config(stat_name);
        let bucket_width = (max_value - min_value) / buckets as i32;
        let mut counts = vec![0_u64; buckets];

        for (value, count) in &self.values {
            if *value < min_value || *value > max_value {
                continue;
            }

            let index = if *value == max_value {
                buckets - 1
            } else {
                ((*value - min_value) / bucket_width).clamp(0, buckets as i32 - 1) as usize
            };
            counts[index] += count;
        }

        let mut map = Map::new();
        for index in 0..buckets {
            let start = min_value + bucket_width * index as i32;
            let end = start + bucket_width;
            map.insert(format!("{start}-{end}"), json!(counts[index]));
        }
        Value::Object(map)
    }
}

#[derive(Clone, Default)]
struct ItemLevelCounts {
    total: u64,
    levels: [u64; 10],
}

impl ItemLevelCounts {
    fn merge(&mut self, other: Self) {
        self.total += other.total;
        for (target, source) in self.levels.iter_mut().zip(other.levels) {
            *target += source;
        }
    }
}

#[derive(Clone)]
struct ReportAgg {
    entries: u64,
    stats: [StatAccumulator; 6],
    uma_counts: HashMap<u32, u64>,
    support_items: HashMap<u32, ItemLevelCounts>,
    skill_items: HashMap<u32, ItemLevelCounts>,
    support_count: u64,
    skill_count: u64,
    combo_counts: HashMap<u32, u64>,
    combo_total: u64,
}

impl Default for ReportAgg {
    fn default() -> Self {
        Self {
            entries: 0,
            stats: std::array::from_fn(|_| StatAccumulator::default()),
            uma_counts: HashMap::new(),
            support_items: HashMap::new(),
            skill_items: HashMap::new(),
            support_count: 0,
            skill_count: 0,
            combo_counts: HashMap::new(),
            combo_total: 0,
        }
    }
}

impl ReportAgg {
    fn add(&mut self, row: &RowData, prepared: &PreparedRow) {
        self.entries += 1;
        for (index, value) in row.stats.iter().enumerate() {
            self.stats[index].add(*value);
        }

        *self.uma_counts.entry(row.card_id).or_insert(0) += 1;
        self.support_count += prepared.support_count;
        self.skill_count += prepared.skill_count;

        for (item_id, level) in &prepared.support_items {
            let entry = self.support_items.entry(*item_id).or_default();
            entry.total += 1;
            entry.levels[*level] += 1;
        }

        for (item_id, level) in &prepared.skill_items {
            let entry = self.skill_items.entry(*item_id).or_default();
            entry.total += 1;
            entry.levels[*level] += 1;
        }

        if let Some(deck_id) = prepared.support_deck_id {
            self.combo_total += 1;
            *self.combo_counts.entry(deck_id).or_insert(0) += 1;
        }
    }

    fn merge(&mut self, other: Self, deck_id_map: &[u32]) {
        self.entries += other.entries;
        for (target, source) in self.stats.iter_mut().zip(other.stats) {
            target.merge(source);
        }
        merge_count_map(&mut self.uma_counts, other.uma_counts);
        merge_item_level_map(&mut self.support_items, other.support_items);
        merge_item_level_map(&mut self.skill_items, other.skill_items);
        self.support_count += other.support_count;
        self.skill_count += other.skill_count;
        self.combo_total += other.combo_total;

        for (old_deck_id, count) in other.combo_counts {
            let new_deck_id = deck_id_map[old_deck_id as usize];
            *self.combo_counts.entry(new_deck_id).or_insert(0) += count;
        }
    }

    fn stats_json(&self) -> Value {
        let mut map = Map::new();
        for (index, stat_name) in STAT_NAMES.iter().enumerate() {
            map.insert(
                (*stat_name).to_string(),
                self.stats[index].full_json(stat_name),
            );
        }
        Value::Object(map)
    }

    fn partial_stats_json(&self) -> Value {
        let mut map = Map::new();
        for (index, stat_name) in STAT_NAMES.iter().enumerate() {
            map.insert((*stat_name).to_string(), self.stats[index].partial_json());
        }
        Value::Object(map)
    }

    fn support_cards_json(&self) -> Value {
        item_counter_json(&self.support_items)
    }

    fn skills_json(&self) -> Value {
        item_counter_json(&self.skill_items)
    }

    fn combinations_json(&self, support_decks: &SupportDeckInterner) -> Value {
        if self.combo_total == 0 {
            return Value::Object(Map::new());
        }

        let mut combos: Vec<(&u32, &u64)> = self.combo_counts.iter().collect();
        combos.sort_by(|left, right| right.1.cmp(left.1).then_with(|| left.0.cmp(right.0)));

        let mut result = Map::new();
        for (deck_id, count) in combos.into_iter().take(50) {
            let combo = support_decks
                .get(*deck_id)
                .expect("support deck id should resolve");
            result.insert(
                combo.key_string(),
                json!({
                    "count": count,
                    "percentage": percentage(*count, self.combo_total),
                    "support_card_type_ids": combo.ids_json()
                }),
            );
        }

        Value::Object(result)
    }
}

#[derive(Default)]
struct DistanceAgg {
    total_entries: u64,
    by_team_class: HashMap<u8, ReportAgg>,
    by_team_class_scenario: HashMap<(u8, u8), ReportAgg>,
    by_scenario: HashMap<u8, ReportAgg>,
}

impl DistanceAgg {
    fn merge(&mut self, other: Self, deck_id_map: &[u32]) {
        self.total_entries += other.total_entries;
        merge_report_map(&mut self.by_team_class, other.by_team_class, deck_id_map);
        merge_report_map(
            &mut self.by_team_class_scenario,
            other.by_team_class_scenario,
            deck_id_map,
        );
        merge_report_map(&mut self.by_scenario, other.by_scenario, deck_id_map);
    }
}

#[derive(Default)]
struct CharacterAgg {
    overall: ReportAgg,
    by_scenario: HashMap<u8, ReportAgg>,
    by_distance_class: HashMap<(u8, u8), ReportAgg>,
    by_distance_class_scenario: HashMap<(u8, u8, u8), ReportAgg>,
    distance_counts: HashMap<u8, u64>,
    running_style_counts: HashMap<u8, u64>,
    scenario_counts: HashMap<u8, u64>,
    team_class_rows: HashMap<u8, u64>,
    total_trainers: u64,
    team_class_trainers: HashMap<u8, u64>,
}

impl CharacterAgg {
    fn merge(&mut self, other: Self, deck_id_map: &[u32]) {
        self.overall.merge(other.overall, deck_id_map);
        merge_report_map(&mut self.by_scenario, other.by_scenario, deck_id_map);
        merge_report_map(
            &mut self.by_distance_class,
            other.by_distance_class,
            deck_id_map,
        );
        merge_report_map(
            &mut self.by_distance_class_scenario,
            other.by_distance_class_scenario,
            deck_id_map,
        );
        merge_count_map(&mut self.distance_counts, other.distance_counts);
        merge_count_map(&mut self.running_style_counts, other.running_style_counts);
        merge_count_map(&mut self.scenario_counts, other.scenario_counts);
        merge_count_map(&mut self.team_class_rows, other.team_class_rows);
        self.total_trainers += other.total_trainers;
        merge_count_map(&mut self.team_class_trainers, other.team_class_trainers);
    }
}

#[derive(Default)]
struct TrainerCounts {
    total_trainers: u64,
    class_trainers: HashMap<u8, u64>,
    scenario_total_trainers: HashMap<u8, u64>,
    scenario_class_trainers: HashMap<(u8, u8), u64>,
}

impl TrainerCounts {
    fn merge(&mut self, other: Self) {
        self.total_trainers += other.total_trainers;
        merge_count_map(&mut self.class_trainers, other.class_trainers);
        merge_count_map(
            &mut self.scenario_total_trainers,
            other.scenario_total_trainers,
        );
        merge_count_map(
            &mut self.scenario_class_trainers,
            other.scenario_class_trainers,
        );
    }
}

#[derive(Default)]
struct ActiveTrainer {
    trainer_id: String,
    team_class: Option<u8>,
    scenarios: HashSet<u8>,
    characters: HashSet<u32>,
}

#[derive(Default)]
struct Compiler {
    generated_at: String,
    dataset_version: String,
    dataset_name: String,
    total_entries: u64,
    character_ids: HashSet<u32>,
    global: ReportAgg,
    by_team_class: HashMap<u8, ReportAgg>,
    by_team_class_scenario: HashMap<(u8, u8), ReportAgg>,
    by_scenario: HashMap<u8, ReportAgg>,
    distances: HashMap<u8, DistanceAgg>,
    characters: HashMap<u32, CharacterAgg>,
    trainer_counts: TrainerCounts,
    active_trainer: Option<ActiveTrainer>,
    support_decks: SupportDeckInterner,
}

impl Compiler {
    fn new(dataset_version: String) -> Self {
        Self {
            generated_at: chrono_timestamp(),
            dataset_name: format!("Statistics {dataset_version}"),
            dataset_version,
            ..Self::default()
        }
    }

    fn merge(&mut self, mut other: Self) {
        other.finish();
        let Compiler {
            generated_at: _,
            dataset_version: _,
            dataset_name: _,
            total_entries,
            character_ids,
            global,
            by_team_class,
            by_team_class_scenario,
            by_scenario,
            distances,
            characters,
            trainer_counts,
            active_trainer: _,
            support_decks,
        } = other;

        let deck_id_map = self.merge_support_decks(support_decks);
        self.total_entries += total_entries;
        self.character_ids.extend(character_ids);
        self.global.merge(global, &deck_id_map);
        merge_report_map(&mut self.by_team_class, by_team_class, &deck_id_map);
        merge_report_map(
            &mut self.by_team_class_scenario,
            by_team_class_scenario,
            &deck_id_map,
        );
        merge_report_map(&mut self.by_scenario, by_scenario, &deck_id_map);
        merge_distance_map(&mut self.distances, distances, &deck_id_map);
        merge_character_map(&mut self.characters, characters, &deck_id_map);
        self.trainer_counts.merge(trainer_counts);
    }

    fn merge_support_decks(&mut self, support_decks: SupportDeckInterner) -> Vec<u32> {
        support_decks
            .keys
            .into_iter()
            .map(|deck_key| self.support_decks.intern(deck_key))
            .collect()
    }

    fn add_row(&mut self, row: RowData, support_card_types: &SupportCardTypes) {
        self.observe_trainer(&row);
        let prepared = prepare_row(&row, &mut self.support_decks, support_card_types);

        self.total_entries += 1;
        self.character_ids.insert(row.card_id);
        self.global.add(&row, &prepared);

        if row.scenario_id >= 1 {
            self.by_scenario
                .entry(row.scenario_id)
                .or_default()
                .add(&row, &prepared);
        }

        if let Some(team_class) = row.team_class.filter(|value| *value >= 1) {
            self.by_team_class
                .entry(team_class)
                .or_default()
                .add(&row, &prepared);
            if row.scenario_id >= 1 {
                self.by_team_class_scenario
                    .entry((team_class, row.scenario_id))
                    .or_default()
                    .add(&row, &prepared);
            }
        }

        let distance = self.distances.entry(row.distance_type).or_default();
        distance.total_entries += 1;
        if row.scenario_id >= 1 {
            distance
                .by_scenario
                .entry(row.scenario_id)
                .or_default()
                .add(&row, &prepared);
        }
        if let Some(team_class) = row.team_class.filter(|value| *value >= 1) {
            distance
                .by_team_class
                .entry(team_class)
                .or_default()
                .add(&row, &prepared);
            if row.scenario_id >= 1 {
                distance
                    .by_team_class_scenario
                    .entry((team_class, row.scenario_id))
                    .or_default()
                    .add(&row, &prepared);
            }
        }

        let character = self.characters.entry(row.card_id).or_default();
        character.overall.add(&row, &prepared);
        *character
            .distance_counts
            .entry(row.distance_type)
            .or_insert(0) += 1;
        *character
            .running_style_counts
            .entry(row.running_style)
            .or_insert(0) += 1;
        *character
            .scenario_counts
            .entry(row.scenario_id)
            .or_insert(0) += 1;
        if let Some(team_class) = row.team_class {
            *character.team_class_rows.entry(team_class).or_insert(0) += 1;
        }
        if row.scenario_id >= 1 {
            character
                .by_scenario
                .entry(row.scenario_id)
                .or_default()
                .add(&row, &prepared);
        }
        if let Some(team_class) = row.team_class.filter(|value| *value >= 1) {
            character
                .by_distance_class
                .entry((row.distance_type, team_class))
                .or_default()
                .add(&row, &prepared);
            if row.scenario_id >= 1 {
                character
                    .by_distance_class_scenario
                    .entry((row.distance_type, team_class, row.scenario_id))
                    .or_default()
                    .add(&row, &prepared);
            }
        }
    }

    fn observe_trainer(&mut self, row: &RowData) {
        let should_flush = self
            .active_trainer
            .as_ref()
            .map_or(false, |active| active.trainer_id != row.trainer_id);

        if should_flush {
            self.flush_active_trainer();
        }

        if self.active_trainer.is_none() {
            self.active_trainer = Some(ActiveTrainer {
                trainer_id: row.trainer_id.clone(),
                team_class: row.team_class,
                scenarios: HashSet::new(),
                characters: HashSet::new(),
            });
        }

        if let Some(active) = &mut self.active_trainer {
            if active.team_class.is_none() {
                active.team_class = row.team_class;
            }
            if row.scenario_id >= 1 {
                active.scenarios.insert(row.scenario_id);
            }
            active.characters.insert(row.card_id);
        }
    }

    fn flush_active_trainer(&mut self) {
        let Some(active) = self.active_trainer.take() else {
            return;
        };

        self.trainer_counts.total_trainers += 1;
        if let Some(team_class) = active.team_class {
            *self
                .trainer_counts
                .class_trainers
                .entry(team_class)
                .or_insert(0) += 1;
        }

        for scenario in active.scenarios {
            *self
                .trainer_counts
                .scenario_total_trainers
                .entry(scenario)
                .or_insert(0) += 1;
            if let Some(team_class) = active.team_class {
                *self
                    .trainer_counts
                    .scenario_class_trainers
                    .entry((scenario, team_class))
                    .or_insert(0) += 1;
            }
        }

        for character_id in active.characters {
            let character = self.characters.entry(character_id).or_default();
            character.total_trainers += 1;
            if let Some(team_class) = active.team_class.filter(|value| *value >= 6) {
                *character.team_class_trainers.entry(team_class).or_insert(0) += 1;
            }
        }
    }

    fn finish(&mut self) {
        self.flush_active_trainer();
    }

    fn write_outputs(&self, output_root: &Path) -> Result<()> {
        fs::create_dir_all(output_root)
            .with_context(|| format!("create {}", output_root.display()))?;
        let dataset_root = output_root.join(&self.dataset_version);
        let staging_root = output_root.join(format!(
            ".{}.tmp-{}",
            self.dataset_version,
            std::process::id()
        ));
        let backup_root = output_root.join(format!(
            ".{}.old-{}",
            self.dataset_version,
            std::process::id()
        ));

        remove_path(&staging_root)?;
        fs::create_dir_all(staging_root.join("global"))?;
        fs::create_dir_all(staging_root.join("distance"))?;
        fs::create_dir_all(staging_root.join("characters"))?;

        write_json_pretty(
            &staging_root.join("global/global.json"),
            &self.global_json(),
        )?;

        for distance_id in DISTANCE_IDS {
            if let Some(distance) = self.distances.get(&distance_id) {
                if distance.total_entries > 0 {
                    let filename = format!("{distance_id}.json");
                    write_json_pretty(
                        &staging_root.join("distance").join(filename),
                        &self.distance_json(distance_id, distance),
                    )?;
                }
            }
        }

        let mut character_ids: Vec<u32> = self.characters.keys().copied().collect();
        character_ids.sort_unstable();
        for character_id in character_ids {
            let character = self
                .characters
                .get(&character_id)
                .expect("character key exists");
            write_json_pretty(
                &staging_root
                    .join("characters")
                    .join(format!("{character_id}.json")),
                &self.character_json(character_id, character),
            )?;
        }

        let index = self.index_json();
        write_json_pretty(&staging_root.join("index.json"), &index)?;
        replace_directory(&staging_root, &dataset_root, &backup_root)?;
        update_master_index(
            output_root,
            &self.dataset_version,
            &self.dataset_name,
            &self.generated_at,
            index,
        )?;

        Ok(())
    }

    fn index_json(&self) -> Value {
        let mut character_ids: Vec<u32> = self.character_ids.iter().copied().collect();
        character_ids.sort_unstable();

        json!({
            "generated_at": self.generated_at,
            "format": DATA_FORMAT,
            "format_version": DATA_FORMAT_VERSION,
            "total_entries": self.total_entries,
            "total_trainers": self.trainer_counts.total_trainers,
            "total_characters": character_ids.len(),
            "distances": DISTANCE_IDS.iter().map(|id| id.to_string()).collect::<Vec<_>>(),
            "character_ids": character_ids.iter().map(|id| id.to_string()).collect::<Vec<_>>(),
            "version": self.dataset_version,
            "name": self.dataset_name
        })
    }

    fn global_json(&self) -> Value {
        let mut root = Map::new();
        root.insert(
            "metadata".to_string(),
            json!({
                "generated_at": self.generated_at,
                "format": DATA_FORMAT,
                "format_version": DATA_FORMAT_VERSION,
                "total_entries": self.total_entries,
                "total_trainers": self.trainer_counts.total_trainers,
                "total_unique_umas": self.character_ids.len(),
                "total_trained_umas": self.total_entries
            }),
        );
        root.insert(
            "team_class_distribution".to_string(),
            self.global_team_class_distribution_json(),
        );
        root.insert(
            "scenario_distribution".to_string(),
            self.scenario_distribution_json(),
        );
        root.insert(
            "uma_distribution".to_string(),
            self.global_uma_distribution_json(),
        );
        root.insert(
            "stat_averages".to_string(),
            self.global_stat_averages_json(),
        );
        root.insert(
            "support_cards".to_string(),
            self.global_support_cards_json(),
        );
        root.insert(
            "support_card_combinations".to_string(),
            self.global_combinations_json(),
        );
        root.insert("skills".to_string(), self.global_skills_json());
        Value::Object(root)
    }

    fn global_team_class_distribution_json(&self) -> Value {
        let mut root = Map::new();
        root.insert(
            "total_trainers".to_string(),
            json!(self.trainer_counts.total_trainers),
        );
        root.insert("total_trained_umas".to_string(), json!(self.total_entries));

        let mut by_scenario = Map::new();
        for scenario in sorted_keys(&self.trainer_counts.scenario_total_trainers) {
            let total_trainers = self.trainer_counts.scenario_total_trainers[&scenario];
            let scenario_entries = self
                .by_scenario
                .get(&scenario)
                .map_or(0, |report| report.entries);
            let mut scenario_map = Map::new();
            scenario_map.insert("total_trainers".to_string(), json!(total_trainers));
            scenario_map.insert("total_trained_umas".to_string(), json!(scenario_entries));

            for team_class in sorted_keys(&self.trainer_counts.class_trainers) {
                let trainer_count = *self
                    .trainer_counts
                    .scenario_class_trainers
                    .get(&(scenario, team_class))
                    .unwrap_or(&0);
                if trainer_count == 0 {
                    continue;
                }
                let uma_count = self
                    .by_team_class_scenario
                    .get(&(team_class, scenario))
                    .map_or(0, |report| report.entries);
                scenario_map.insert(
                    team_class.to_string(),
                    json!({
                        "count": trainer_count,
                        "percentage": percentage(trainer_count, total_trainers),
                        "trained_umas": uma_count,
                        "trained_umas_percentage": percentage(uma_count, scenario_entries)
                    }),
                );
            }

            by_scenario.insert(scenario.to_string(), Value::Object(scenario_map));
        }
        root.insert("by_scenario".to_string(), Value::Object(by_scenario));

        let mut classes = sorted_keys(&self.trainer_counts.class_trainers);
        classes.sort_by(|left, right| {
            self.trainer_counts.class_trainers[right]
                .cmp(&self.trainer_counts.class_trainers[left])
                .then_with(|| left.cmp(right))
        });
        for team_class in classes {
            let trainer_count = self.trainer_counts.class_trainers[&team_class];
            let uma_count = self
                .by_team_class
                .get(&team_class)
                .map_or(0, |report| report.entries);
            root.insert(
                team_class.to_string(),
                json!({
                    "count": trainer_count,
                    "percentage": percentage(trainer_count, self.trainer_counts.total_trainers),
                    "trained_umas": uma_count,
                    "trained_umas_percentage": percentage(uma_count, self.total_entries)
                }),
            );
        }

        Value::Object(root)
    }

    fn scenario_distribution_json(&self) -> Value {
        let mut root = Map::new();
        root.insert("total_entries".to_string(), json!(self.total_entries));

        let mut scenarios: Vec<(u8, u64)> = self
            .by_scenario
            .iter()
            .map(|(scenario, report)| (*scenario, report.entries))
            .collect();
        scenarios.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));

        for (scenario, count) in scenarios {
            root.insert(
                scenario.to_string(),
                json!({
                    "id": scenario.to_string(),
                    "count": count,
                    "percentage": percentage(count, self.total_entries)
                }),
            );
        }

        Value::Object(root)
    }

    fn global_uma_distribution_json(&self) -> Value {
        let mut root = uma_distribution_json(&self.global.uma_counts, 30, self.global.entries);

        let mut by_team_class = Map::new();
        for team_class in sorted_keys(&self.by_team_class) {
            let Some(report) = self.by_team_class.get(&team_class) else {
                continue;
            };
            let mut class_map = Map::new();
            class_map.insert(
                "overall".to_string(),
                uma_distribution_json(&report.uma_counts, 30, report.entries),
            );

            let mut by_scenario = Map::new();
            for scenario in sorted_scenarios_for_team(&self.by_team_class_scenario, team_class) {
                let report = &self.by_team_class_scenario[&(team_class, scenario)];
                by_scenario.insert(
                    scenario.to_string(),
                    uma_distribution_json(&report.uma_counts, 30, report.entries),
                );
            }
            class_map.insert("by_scenario".to_string(), Value::Object(by_scenario));
            by_team_class.insert(team_class.to_string(), Value::Object(class_map));
        }
        root.as_object_mut()
            .expect("uma distribution is object")
            .insert("by_team_class".to_string(), Value::Object(by_team_class));
        root
    }

    fn global_stat_averages_json(&self) -> Value {
        let mut root = Map::new();
        root.insert("overall".to_string(), self.global.stats_json());

        let mut by_team_class = Map::new();
        for team_class in sorted_keys(&self.by_team_class) {
            let report = &self.by_team_class[&team_class];
            let mut class_map = Map::new();
            class_map.insert(
                "overall".to_string(),
                if report.entries > 100 {
                    report.stats_json()
                } else {
                    Value::Object(Map::new())
                },
            );

            let mut by_scenario = Map::new();
            for scenario in sorted_scenarios_for_team(&self.by_team_class_scenario, team_class) {
                by_scenario.insert(
                    scenario.to_string(),
                    self.by_team_class_scenario[&(team_class, scenario)].stats_json(),
                );
            }
            class_map.insert("by_scenario".to_string(), Value::Object(by_scenario));
            by_team_class.insert(team_class.to_string(), Value::Object(class_map));
        }
        root.insert("by_team_class".to_string(), Value::Object(by_team_class));

        let mut by_scenario = Map::new();
        for scenario in sorted_keys(&self.by_scenario) {
            let report = &self.by_scenario[&scenario];
            if report.entries > 100 {
                by_scenario.insert(scenario.to_string(), report.stats_json());
            }
        }
        root.insert("by_scenario".to_string(), Value::Object(by_scenario));
        Value::Object(root)
    }

    fn global_support_cards_json(&self) -> Value {
        self.global_metric_json(
            ReportAgg::support_cards_json,
            |report| report.support_count,
            "total_support_cards",
        )
    }

    fn global_combinations_json(&self) -> Value {
        self.global_metric_json(
            |report| report.combinations_json(&self.support_decks),
            |report| report.combo_total,
            "total_combinations",
        )
    }

    fn global_skills_json(&self) -> Value {
        self.global_metric_json(
            ReportAgg::skills_json,
            |report| report.skill_count,
            "total_skills",
        )
    }

    fn global_metric_json<F, T>(&self, value_fn: F, total_fn: T, total_prefix: &str) -> Value
    where
        F: Fn(&ReportAgg) -> Value + Copy,
        T: Fn(&ReportAgg) -> u64 + Copy,
    {
        let mut root = Map::new();
        root.insert("overall".to_string(), value_fn(&self.global));
        root.insert(total_prefix.to_string(), json!(total_fn(&self.global)));
        root.insert(
            "by_team_class".to_string(),
            self.by_team_nested_json(value_fn),
        );

        let mut by_scenario = Map::new();
        for scenario in sorted_keys(&self.by_scenario) {
            by_scenario.insert(scenario.to_string(), value_fn(&self.by_scenario[&scenario]));
            root.insert(
                format!("{total_prefix}_scenario_{scenario}"),
                json!(total_fn(&self.by_scenario[&scenario])),
            );
        }
        root.insert("by_scenario".to_string(), Value::Object(by_scenario));

        for team_class in sorted_keys(&self.by_team_class) {
            root.insert(
                format!("{total_prefix}_{team_class}"),
                json!(total_fn(&self.by_team_class[&team_class])),
            );
        }

        Value::Object(root)
    }

    fn by_team_nested_json<F>(&self, value_fn: F) -> Value
    where
        F: Fn(&ReportAgg) -> Value + Copy,
    {
        let mut by_team_class = Map::new();
        for team_class in sorted_keys(&self.by_team_class) {
            let mut class_map = Map::new();
            class_map.insert(
                "overall".to_string(),
                value_fn(&self.by_team_class[&team_class]),
            );

            let mut by_scenario = Map::new();
            for scenario in sorted_scenarios_for_team(&self.by_team_class_scenario, team_class) {
                by_scenario.insert(
                    scenario.to_string(),
                    value_fn(&self.by_team_class_scenario[&(team_class, scenario)]),
                );
            }
            class_map.insert("by_scenario".to_string(), Value::Object(by_scenario));
            by_team_class.insert(team_class.to_string(), Value::Object(class_map));
        }
        Value::Object(by_team_class)
    }

    fn distance_json(&self, distance_id: u8, distance: &DistanceAgg) -> Value {
        let mut root = Map::new();
        root.insert(
            "metadata".to_string(),
            json!({
                "distance_id": distance_id.to_string(),
                "format": DATA_FORMAT,
                "format_version": DATA_FORMAT_VERSION,
                "total_entries": distance.total_entries,
                "generated_at": self.generated_at
            }),
        );

        let mut by_team_class = Map::new();
        for team_class in sorted_keys(&distance.by_team_class) {
            let report = &distance.by_team_class[&team_class];
            if report.entries <= 50 {
                continue;
            }

            let mut class_map = Map::new();
            class_map.insert(
                "overall".to_string(),
                distance_report_json(report, 20, &self.support_decks),
            );

            let mut by_scenario = Map::new();
            for scenario in sorted_scenarios_for_team(&distance.by_team_class_scenario, team_class)
            {
                let report = &distance.by_team_class_scenario[&(team_class, scenario)];
                by_scenario.insert(
                    scenario.to_string(),
                    distance_report_json(report, 20, &self.support_decks),
                );
            }
            class_map.insert("by_scenario".to_string(), Value::Object(by_scenario));
            by_team_class.insert(team_class.to_string(), Value::Object(class_map));
        }
        root.insert("by_team_class".to_string(), Value::Object(by_team_class));

        let mut by_scenario = Map::new();
        for scenario in sorted_keys(&distance.by_scenario) {
            let report = &distance.by_scenario[&scenario];
            if report.entries <= 50 {
                continue;
            }

            by_scenario.insert(
                scenario.to_string(),
                distance_report_json(report, 20, &self.support_decks),
            );
        }
        root.insert("by_scenario".to_string(), Value::Object(by_scenario));

        Value::Object(root)
    }

    fn character_json(&self, character_id: u32, character: &CharacterAgg) -> Value {
        let mut root = Map::new();
        root.insert(
            "metadata".to_string(),
            json!({
                "character_id": character_id.to_string(),
                "format": DATA_FORMAT,
                "format_version": DATA_FORMAT_VERSION,
                "total_entries": character.overall.entries,
                "total_trained_umas": character.overall.entries,
                "generated_at": self.generated_at
            }),
        );

        let mut global = Map::new();
        global.insert(
            "distance_distribution".to_string(),
            character_id_distribution_json(
                &character.distance_counts,
                character.overall.entries,
                character_id,
            ),
        );
        global.insert(
            "running_style_distribution".to_string(),
            character_id_distribution_json(
                &character.running_style_counts,
                character.overall.entries,
                character_id,
            ),
        );
        global.insert(
            "scenario_distribution".to_string(),
            character_id_distribution_json(
                &character.scenario_counts,
                character.overall.entries,
                character_id,
            ),
        );
        global.insert(
            "team_class_distribution".to_string(),
            character_team_class_distribution_json(character, character_id),
        );
        root.insert("global".to_string(), Value::Object(global));

        root.insert(
            "overall".to_string(),
            character_overall_json(&character.overall, &self.support_decks),
        );

        let mut by_scenario = Map::new();
        for scenario in sorted_keys(&character.by_scenario) {
            by_scenario.insert(
                scenario.to_string(),
                character_overall_json(&character.by_scenario[&scenario], &self.support_decks),
            );
        }
        root.insert("by_scenario".to_string(), Value::Object(by_scenario));

        let mut by_distance = Map::new();
        for distance_id in sorted_keys(&character.distance_counts) {
            let distance_total = character.distance_counts[&distance_id];
            if distance_total <= 10 {
                continue;
            }

            let mut distance_map = Map::new();
            let mut class_map = Map::new();
            let mut classes: Vec<u8> = character
                .by_distance_class
                .keys()
                .filter(|(distance, _)| *distance == distance_id)
                .map(|(_, team_class)| *team_class)
                .collect();
            classes.sort_unstable();
            classes.dedup();

            for team_class in classes {
                let report = &character.by_distance_class[&(distance_id, team_class)];
                if report.entries <= 5 {
                    continue;
                }

                let mut team_map = Map::new();
                team_map.insert(
                    "overall".to_string(),
                    character_distance_report_json(report, &self.support_decks),
                );

                let mut scenario_map = Map::new();
                let mut scenarios: Vec<u8> = character
                    .by_distance_class_scenario
                    .keys()
                    .filter(|(distance, class, _)| *distance == distance_id && *class == team_class)
                    .map(|(_, _, scenario)| *scenario)
                    .collect();
                scenarios.sort_unstable();
                scenarios.dedup();
                for scenario in scenarios {
                    scenario_map.insert(
                        scenario.to_string(),
                        character_distance_report_json(
                            &character.by_distance_class_scenario
                                [&(distance_id, team_class, scenario)],
                            &self.support_decks,
                        ),
                    );
                }
                team_map.insert("by_scenario".to_string(), Value::Object(scenario_map));
                class_map.insert(team_class.to_string(), Value::Object(team_map));
            }

            distance_map.insert("by_team_class".to_string(), Value::Object(class_map));
            by_distance.insert(distance_id.to_string(), Value::Object(distance_map));
        }
        root.insert("by_distance".to_string(), Value::Object(by_distance));

        Value::Object(root)
    }
}

fn main() -> Result<()> {
    dotenvy::dotenv().ok();

    let args = Args::parse();
    let mut resource_monitor = if args.resource_usage {
        Some(ResourceMonitor::new()?)
    } else {
        None
    };
    let worker_threads = args
        .worker_threads
        .unwrap_or_else(default_worker_threads)
        .max(1);
    let batch_rows = args.batch_rows.max(1);
    let progress_every = args.progress_every;
    let support_card_types = Arc::new(load_support_card_types()?);
    let repo_root = args.repo_root.canonicalize().unwrap_or(args.repo_root);
    let database_url = args
        .database_url
        .or_else(|| env::var("DATABASE_URL").ok())
        .ok_or_else(|| anyhow!("Set DATABASE_URL or pass --database-url"))?;
    let dataset_version = args
        .dataset_version
        .unwrap_or_else(|| Local::now().format("%Y-%m-%d").to_string());
    let output_root = resolve_from(&repo_root, args.output_dir);
    let publish_roots = if args.publish_dirs.is_empty() {
        vec![output_root]
    } else {
        args.publish_dirs
            .into_iter()
            .map(|path| resolve_from(&repo_root, path))
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
    let query = statistics_copy_query(args.limit);
    let reader = client
        .copy_out(query.as_str())
        .context("start PostgreSQL COPY stream")?;
    let mut csv_reader = csv::ReaderBuilder::new()
        .has_headers(true)
        .from_reader(reader);
    let headers = csv_reader.headers().context("read COPY headers")?.clone();

    if worker_threads > 1 {
        println!("Using {worker_threads} worker threads with {batch_rows} row batches...");
        stream_rows_parallel(
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
        stream_rows_serial(
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
        compiler.total_entries,
        started_at.elapsed().as_secs_f64()
    );
    print_progress(
        &mut resource_monitor,
        "streamed",
        compiler.total_entries,
        started_at.elapsed(),
    );

    for publish_root in &publish_roots {
        println!("Writing statistics to {}...", publish_root.display());
        compiler.write_outputs(publish_root)?;
    }

    println!(
        "Done. Dataset {} written to {} output root(s).",
        compiler.dataset_version,
        publish_roots.len()
    );
    print_progress(
        &mut resource_monitor,
        "completed",
        compiler.total_entries,
        started_at.elapsed(),
    );
    Ok(())
}

fn default_worker_threads() -> usize {
    thread::available_parallelism()
        .map(|threads| threads.get().min(3))
        .unwrap_or(1)
}

fn stream_rows_serial<R: Read>(
    csv_reader: &mut csv::Reader<R>,
    headers: &StringRecord,
    compiler: &mut Compiler,
    progress_every: u64,
    support_card_types: &SupportCardTypes,
    resource_monitor: &mut Option<ResourceMonitor>,
    started_at: Instant,
) -> Result<()> {
    for result in csv_reader.records() {
        let record = result.context("read COPY row")?;
        let row = parse_record(headers, &record)?;
        compiler.add_row(row, support_card_types);

        if progress_every > 0 && compiler.total_entries % progress_every == 0 {
            print_progress(
                resource_monitor,
                "processed",
                compiler.total_entries,
                started_at.elapsed(),
            );
        }
    }

    Ok(())
}

fn stream_rows_parallel<R: Read>(
    csv_reader: &mut csv::Reader<R>,
    headers: &StringRecord,
    compiler: &mut Compiler,
    dataset_version: &str,
    worker_threads: usize,
    batch_rows: usize,
    progress_every: u64,
    support_card_types: Arc<SupportCardTypes>,
    resource_monitor: &mut Option<ResourceMonitor>,
    started_at: Instant,
) -> Result<()> {
    let (batch_sender, batch_receiver) = mpsc::sync_channel::<Vec<RowData>>(worker_threads * 2);
    let batch_receiver = Arc::new(Mutex::new(batch_receiver));
    let (result_sender, result_receiver) = mpsc::channel::<Compiler>();
    let mut handles = Vec::with_capacity(worker_threads);

    for _ in 0..worker_threads {
        let batch_receiver = Arc::clone(&batch_receiver);
        let result_sender = result_sender.clone();
        let dataset_version = dataset_version.to_string();
        let support_card_types = Arc::clone(&support_card_types);
        handles.push(thread::spawn(move || loop {
            let batch = {
                let receiver = batch_receiver.lock().expect("batch receiver lock poisoned");
                receiver.recv()
            };

            let Ok(batch) = batch else {
                break;
            };

            let mut partial = Compiler::new(dataset_version.clone());
            for row in batch {
                partial.add_row(row, support_card_types.as_ref());
            }
            partial.finish();

            if result_sender.send(partial).is_err() {
                break;
            }
        }));
    }
    drop(result_sender);

    let mut batch = Vec::with_capacity(batch_rows);
    let mut current_trainer_id: Option<String> = None;
    let mut rows_read = 0_u64;
    let mut sent_batches = 0_usize;
    let mut merged_batches = 0_usize;
    let mut next_progress = progress_every;

    for result in csv_reader.records() {
        let record = result.context("read COPY row")?;
        let row = parse_record(headers, &record)?;
        let starts_new_trainer = current_trainer_id
            .as_deref()
            .map_or(false, |trainer_id| trainer_id != row.trainer_id);

        if starts_new_trainer && batch.len() >= batch_rows {
            let full_batch = std::mem::replace(&mut batch, Vec::with_capacity(batch_rows));
            batch_sender
                .send(full_batch)
                .map_err(|_| anyhow!("statistics worker stopped before receiving a batch"))?;
            sent_batches += 1;
            merge_available_results(&result_receiver, compiler, &mut merged_batches);
        }

        current_trainer_id = Some(row.trainer_id.clone());
        batch.push(row);
        rows_read += 1;

        if progress_every > 0 && rows_read >= next_progress {
            print_progress(resource_monitor, "queued", rows_read, started_at.elapsed());
            while next_progress <= rows_read {
                next_progress += progress_every;
            }
            merge_available_results(&result_receiver, compiler, &mut merged_batches);
        }
    }

    if !batch.is_empty() {
        batch_sender
            .send(batch)
            .map_err(|_| anyhow!("statistics worker stopped before receiving the final batch"))?;
        sent_batches += 1;
    }
    drop(batch_sender);

    while merged_batches < sent_batches {
        let partial = result_receiver
            .recv()
            .context("receive worker statistics")?;
        compiler.merge(partial);
        merged_batches += 1;
    }

    for handle in handles {
        handle
            .join()
            .map_err(|_| anyhow!("statistics worker panicked"))?;
    }

    Ok(())
}

fn merge_available_results(
    result_receiver: &mpsc::Receiver<Compiler>,
    compiler: &mut Compiler,
    merged_batches: &mut usize,
) {
    while let Ok(partial) = result_receiver.try_recv() {
        compiler.merge(partial);
        *merged_batches += 1;
    }
}

fn print_progress(
    resource_monitor: &mut Option<ResourceMonitor>,
    label: &str,
    rows: u64,
    elapsed: Duration,
) {
    let elapsed_secs = elapsed.as_secs_f64();
    let rows_per_second = if elapsed_secs > 0.0 {
        rows as f64 / elapsed_secs
    } else {
        0.0
    };

    if let Some(resource_monitor) = resource_monitor {
        println!(
            "  {label} {rows:>12} rows in {elapsed_secs:.1}s ({rows_per_second:.0} rows/s) | {}",
            resource_monitor.summary()
        );
    } else {
        println!("  {label} {rows:>12} rows in {elapsed_secs:.1}s ({rows_per_second:.0} rows/s)");
    }
}

fn statistics_copy_query(limit: Option<u64>) -> String {
    let limit_clause = limit.map_or(String::new(), |value| format!(" LIMIT {value}"));
    format!(
        "COPY (\
            SELECT \
                ts.trainer_id::text AS trainer_id, \
                ts.card_id::bigint AS card_id, \
                ts.distance_type::int AS distance_type, \
                COALESCE(ts.scenario_id, 1)::int AS scenario_id, \
                ts.running_style::int AS running_style, \
                ts.speed::int AS speed, \
                ts.power::int AS power, \
                ts.stamina::int AS stamina, \
                ts.wiz::int AS wiz, \
                ts.guts::int AS guts, \
                ts.rank_score::int AS rank_score, \
                COALESCE(ts.skills::text, '[]') AS skills, \
                COALESCE(ts.support_cards::text, '[]') AS support_cards, \
                t.team_class::int AS team_class \
            FROM team_stadium ts \
            LEFT JOIN trainer t ON ts.trainer_id = t.account_id \
            ORDER BY ts.trainer_id, ts.distance_type, ts.member_id\
            {limit_clause}\
        ) TO STDOUT WITH (FORMAT csv, HEADER true)"
    )
}

fn parse_record(headers: &StringRecord, record: &StringRecord) -> Result<RowData> {
    Ok(RowData {
        trainer_id: field(headers, record, "trainer_id")?.to_string(),
        card_id: parse_required(headers, record, "card_id")?,
        distance_type: parse_required(headers, record, "distance_type")?,
        scenario_id: parse_required(headers, record, "scenario_id")?,
        running_style: parse_required(headers, record, "running_style")?,
        team_class: parse_optional(headers, record, "team_class")?,
        stats: [
            parse_required(headers, record, "speed")?,
            parse_required(headers, record, "power")?,
            parse_required(headers, record, "stamina")?,
            parse_required(headers, record, "wiz")?,
            parse_required(headers, record, "guts")?,
            parse_required(headers, record, "rank_score")?,
        ],
        skills: parse_u32_array(field(headers, record, "skills")?),
        support_cards: parse_u32_array(field(headers, record, "support_cards")?),
    })
}

fn prepare_row(
    row: &RowData,
    support_decks: &mut SupportDeckInterner,
    support_card_types: &SupportCardTypes,
) -> PreparedRow {
    let mut support_type_ids = Vec::with_capacity(row.support_cards.len());
    let support_items = row
        .support_cards
        .iter()
        .filter_map(|raw| {
            parse_support_card(*raw).map(|card_id| {
                support_type_ids.push(support_card_types.card_type(card_id));
                (card_id, 0)
            })
        })
        .collect::<Vec<_>>();
    let skill_items = row
        .skills
        .iter()
        .filter_map(|raw| parse_item(*raw))
        .collect::<Vec<_>>();

    let support_deck_id =
        SupportDeckKey::from_ids(&support_type_ids).map(|deck_key| support_decks.intern(deck_key));

    PreparedRow {
        support_items,
        skill_items,
        support_count: row
            .support_cards
            .iter()
            .filter(|value| **value != 0)
            .count() as u64,
        skill_count: row.skills.iter().filter(|value| **value != 0).count() as u64,
        support_deck_id,
    }
}

fn distance_report_json(
    report: &ReportAgg,
    uma_limit: usize,
    support_decks: &SupportDeckInterner,
) -> Value {
    json!({
        "total_entries": report.entries,
        "total_trained_umas": report.entries,
        "uma_distribution": uma_distribution_json(&report.uma_counts, uma_limit, report.entries),
        "stat_averages": report.stats_json(),
        "support_cards": report.support_cards_json(),
        "total_support_cards": report.support_count,
        "support_card_combinations": report.combinations_json(support_decks),
        "total_combinations": report.combo_total,
        "skills": report.skills_json(),
        "total_skills": report.skill_count
    })
}

fn character_overall_json(report: &ReportAgg, support_decks: &SupportDeckInterner) -> Value {
    json!({
        "total_entries": report.entries,
        "total_trained_umas": report.entries,
        "stat_averages": report.stats_json(),
        "support_cards": report.support_cards_json(),
        "total_support_cards": report.support_count,
        "support_card_combinations": report.combinations_json(support_decks),
        "total_combinations": report.combo_total,
        "skills": report.skills_json(),
        "total_skills": report.skill_count
    })
}

fn character_distance_report_json(
    report: &ReportAgg,
    support_decks: &SupportDeckInterner,
) -> Value {
    json!({
        "total_entries": report.entries,
        "total_trained_umas": report.entries,
        "stat_averages": if report.entries > 20 { report.stats_json() } else { report.partial_stats_json() },
        "common_support_cards": report.support_cards_json(),
        "total_support_cards": report.support_count,
        "support_card_combinations": report.combinations_json(support_decks),
        "total_combinations": report.combo_total,
        "common_skills": report.skills_json(),
        "total_skills": report.skill_count
    })
}

fn uma_distribution_json(counts: &HashMap<u32, u64>, limit: usize, total: u64) -> Value {
    let mut pairs: Vec<(u32, u64)> = counts.iter().map(|(id, count)| (*id, *count)).collect();
    pairs.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));

    let mut map = Map::new();
    for (character_id, count) in pairs.into_iter().take(limit) {
        map.insert(
            character_id.to_string(),
            json!({
                "id": character_id.to_string(),
                "count": count,
                "percentage": percentage(count, total)
            }),
        );
    }
    Value::Object(map)
}

fn character_id_distribution_json(
    counts: &HashMap<u8, u64>,
    total: u64,
    character_id: u32,
) -> Value {
    let mut pairs: Vec<(u8, u64)> = counts.iter().map(|(key, count)| (*key, *count)).collect();
    pairs.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));

    let mut map = Map::new();
    map.insert("total_entries".to_string(), json!(total));
    for (key, count) in pairs {
        map.insert(
            key.to_string(),
            json!({
                "id": key.to_string(),
                "count": count,
                "percentage": percentage(count, total),
                "character_id": character_id.to_string()
            }),
        );
    }
    Value::Object(map)
}

fn character_team_class_distribution_json(character: &CharacterAgg, character_id: u32) -> Value {
    let mut map = Map::new();
    map.insert(
        "total_trainers".to_string(),
        json!(character.total_trainers),
    );
    map.insert(
        "total_trained_umas".to_string(),
        json!(character.overall.entries),
    );

    let mut classes: Vec<(u8, u64)> = character
        .team_class_trainers
        .iter()
        .map(|(team_class, count)| (*team_class, *count))
        .collect();
    classes.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));

    for (team_class, trainer_count) in classes {
        let uma_count = *character.team_class_rows.get(&team_class).unwrap_or(&0);
        map.insert(
            team_class.to_string(),
            json!({
                "id": team_class.to_string(),
                "count": trainer_count,
                "percentage": percentage(trainer_count, character.total_trainers),
                "trained_umas": uma_count,
                "trained_umas_percentage": percentage(uma_count, character.overall.entries),
                "character_id": character_id.to_string()
            }),
        );
    }

    Value::Object(map)
}

fn item_counter_json(counts: &HashMap<u32, ItemLevelCounts>) -> Value {
    let mut items: Vec<(u32, &ItemLevelCounts)> =
        counts.iter().map(|(id, count)| (*id, count)).collect();
    items.sort_by(|left, right| {
        right
            .1
            .total
            .cmp(&left.1.total)
            .then_with(|| left.0.cmp(&right.0))
    });

    let mut map = Map::new();
    for (item_id, count) in items.into_iter().take(50) {
        let mut by_level = Map::new();
        let mut level_sum = 0_u64;
        for (level, level_count) in count.levels.iter().enumerate() {
            if *level_count > 0 {
                by_level.insert(level.to_string(), json!(level_count));
                level_sum += level as u64 * *level_count;
            }
        }
        let avg_level = if count.total > 0 {
            level_sum as f64 / count.total as f64
        } else {
            0.0
        };

        let value = json!({
            "id": item_id.to_string(),
            "total": count.total,
            "by_level": by_level,
            "avg_level": avg_level
        });
        map.insert(item_id.to_string(), value);
    }

    Value::Object(map)
}

fn update_master_index(
    output_root: &Path,
    dataset_version: &str,
    dataset_name: &str,
    generated_at: &str,
    index: Value,
) -> Result<()> {
    let path = output_root.join("datasets.json");
    let mut master = if path.exists() {
        read_json(&path)?
    } else {
        json!({"datasets": [], "last_updated": generated_at})
    };

    let datasets = master
        .get_mut("datasets")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| anyhow!("{} does not contain a datasets array", path.display()))?;
    datasets.retain(|entry| entry.get("id").and_then(Value::as_str) != Some(dataset_version));
    datasets.push(json!({
        "id": dataset_version,
        "version": dataset_version,
        "name": dataset_name,
        "format": DATA_FORMAT,
        "format_version": DATA_FORMAT_VERSION,
        "date": generated_at,
        "basePath": format!("/assets/statistics/{dataset_version}"),
        "index": index
    }));
    datasets.sort_by(|left, right| {
        let left_date = left.get("date").and_then(Value::as_str).unwrap_or_default();
        let right_date = right
            .get("date")
            .and_then(Value::as_str)
            .unwrap_or_default();
        right_date.cmp(left_date)
    });
    master["last_updated"] = json!(generated_at);
    write_json_pretty(&path, &master)
}

fn write_json_pretty(path: &Path, value: &Value) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec_pretty(value)?;
    fs::write(path, bytes).with_context(|| format!("write {}", path.display()))
}

fn resolve_from(base: &Path, path: PathBuf) -> PathBuf {
    if path.is_absolute() {
        path
    } else {
        base.join(path)
    }
}

fn replace_directory(staging_root: &Path, dataset_root: &Path, backup_root: &Path) -> Result<()> {
    remove_path(backup_root)?;

    let had_existing = dataset_root.exists();
    if had_existing {
        fs::rename(dataset_root, backup_root).with_context(|| {
            format!(
                "move existing dataset {} to {}",
                dataset_root.display(),
                backup_root.display()
            )
        })?;
    }

    if let Err(error) = fs::rename(staging_root, dataset_root) {
        if had_existing {
            let _ = fs::rename(backup_root, dataset_root);
        }
        return Err(error).with_context(|| {
            format!(
                "publish staged dataset {} to {}",
                staging_root.display(),
                dataset_root.display()
            )
        });
    }

    if had_existing {
        remove_path(backup_root)?;
    }

    Ok(())
}

fn remove_path(path: &Path) -> Result<()> {
    if path.is_dir() {
        fs::remove_dir_all(path).with_context(|| format!("remove {}", path.display()))?;
    } else if path.exists() {
        fs::remove_file(path).with_context(|| format!("remove {}", path.display()))?;
    }
    Ok(())
}

fn read_json(path: &Path) -> Result<Value> {
    let content = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    serde_json::from_str(&content).with_context(|| format!("parse {}", path.display()))
}

fn parse_u32_array(input: &str) -> Vec<u32> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }

    if trimmed.starts_with('[') {
        let Ok(value) = serde_json::from_str::<Value>(trimmed) else {
            return Vec::new();
        };
        let Some(array) = value.as_array() else {
            return Vec::new();
        };
        return array
            .iter()
            .filter_map(|value| json_u32(Some(value)))
            .collect();
    }

    if trimmed.starts_with('{') && trimmed.ends_with('}') {
        return trimmed[1..trimmed.len() - 1]
            .split(',')
            .filter_map(|value| {
                let value = value.trim().trim_matches('"');
                if value.is_empty() || value.eq_ignore_ascii_case("null") {
                    None
                } else {
                    value.parse::<u32>().ok()
                }
            })
            .collect();
    }

    Vec::new()
}

fn parse_item(raw: u32) -> Option<(u32, usize)> {
    if raw == 0 {
        return None;
    }
    Some((raw / 10, (raw % 10) as usize))
}

fn parse_support_card(raw: u32) -> Option<u32> {
    if raw == 0 {
        return None;
    }

    if raw >= 1_000_000 {
        Some(raw / 100)
    } else {
        Some(raw / 10)
    }
}

fn load_support_card_types() -> Result<SupportCardTypes> {
    let value =
        serde_json::from_str::<Value>(SUPPORT_CARDS_JSON).context("parse src/cards.json")?;
    let cards = value
        .as_array()
        .ok_or_else(|| anyhow!("src/cards.json must contain an array"))?;
    let mut by_card_id = HashMap::new();

    for card in cards {
        let card_id = json_u32(card.get("id"))
            .ok_or_else(|| anyhow!("card entry missing numeric id in src/cards.json"))?;
        let card_type = json_u32(card.get("type"))
            .ok_or_else(|| anyhow!("card {card_id} missing numeric type in src/cards.json"))?;

        if let Some(existing_type) = by_card_id.insert(card_id, card_type) {
            if existing_type != card_type {
                return Err(anyhow!(
                    "card {card_id} has conflicting types {existing_type} and {card_type} in src/cards.json"
                ));
            }
        }
    }

    Ok(SupportCardTypes { by_card_id })
}

fn field<'a>(headers: &StringRecord, record: &'a StringRecord, name: &str) -> Result<&'a str> {
    let index = headers
        .iter()
        .position(|header| header == name)
        .ok_or_else(|| anyhow!("missing COPY column {name}"))?;
    record
        .get(index)
        .ok_or_else(|| anyhow!("missing value for {name}"))
}

fn parse_required<T>(headers: &StringRecord, record: &StringRecord, name: &str) -> Result<T>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    field(headers, record, name)?
        .parse::<T>()
        .map_err(|error| anyhow!("invalid {name}: {error}"))
}

fn parse_optional<T>(headers: &StringRecord, record: &StringRecord, name: &str) -> Result<Option<T>>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    let value = field(headers, record, name)?;
    if value.is_empty() {
        Ok(None)
    } else {
        value
            .parse::<T>()
            .map(Some)
            .map_err(|error| anyhow!("invalid {name}: {error}"))
    }
}

fn json_u32(value: Option<&Value>) -> Option<u32> {
    match value? {
        Value::Number(number) => number.as_u64().and_then(|value| u32::try_from(value).ok()),
        Value::String(text) => text.parse::<u32>().ok(),
        _ => None,
    }
}

fn merge_count_map<K>(target: &mut HashMap<K, u64>, source: HashMap<K, u64>)
where
    K: Eq + Hash,
{
    for (key, count) in source {
        *target.entry(key).or_insert(0) += count;
    }
}

fn merge_item_level_map(
    target: &mut HashMap<u32, ItemLevelCounts>,
    source: HashMap<u32, ItemLevelCounts>,
) {
    for (item_id, counts) in source {
        target.entry(item_id).or_default().merge(counts);
    }
}

fn merge_report_map<K>(
    target: &mut HashMap<K, ReportAgg>,
    source: HashMap<K, ReportAgg>,
    deck_id_map: &[u32],
) where
    K: Eq + Hash,
{
    for (key, report) in source {
        target.entry(key).or_default().merge(report, deck_id_map);
    }
}

fn merge_distance_map(
    target: &mut HashMap<u8, DistanceAgg>,
    source: HashMap<u8, DistanceAgg>,
    deck_id_map: &[u32],
) {
    for (distance_id, distance) in source {
        target
            .entry(distance_id)
            .or_default()
            .merge(distance, deck_id_map);
    }
}

fn merge_character_map(
    target: &mut HashMap<u32, CharacterAgg>,
    source: HashMap<u32, CharacterAgg>,
    deck_id_map: &[u32],
) {
    for (character_id, character) in source {
        target
            .entry(character_id)
            .or_default()
            .merge(character, deck_id_map);
    }
}

fn sorted_keys<K, V>(map: &HashMap<K, V>) -> Vec<K>
where
    K: Copy + Ord + Eq + Hash,
{
    let mut keys: Vec<K> = map.keys().copied().collect();
    keys.sort_unstable();
    keys
}

fn sorted_scenarios_for_team(map: &HashMap<(u8, u8), ReportAgg>, team_class: u8) -> Vec<u8> {
    let mut scenarios: Vec<u8> = map
        .keys()
        .filter(|(class, _)| *class == team_class)
        .map(|(_, scenario)| *scenario)
        .collect();
    scenarios.sort_unstable();
    scenarios.dedup();
    scenarios
}

fn percentage(count: u64, total: u64) -> f64 {
    if total == 0 {
        0.0
    } else {
        round2(count as f64 / total as f64 * 100.0)
    }
}

fn round2(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

fn bytes_to_mb(bytes: u64) -> f64 {
    bytes as f64 / 1_048_576.0
}

fn bytes_to_gb(bytes: u64) -> f64 {
    bytes as f64 / 1_073_741_824.0
}

fn stat_config(stat_name: &str) -> (i32, i32, usize) {
    match stat_name {
        "rank_score" => (0, 17_000, 20),
        _ => (0, 1_200, 20),
    }
}

fn chrono_timestamp() -> String {
    Local::now()
        .naive_local()
        .format("%Y-%m-%dT%H:%M:%S%.6f")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_json_and_postgres_u32_arrays() {
        assert_eq!(
            parse_u32_array("[100011, \"100024\"]"),
            vec![100011, 100024]
        );
        assert_eq!(
            parse_u32_array("{100011,100024,NULL}"),
            vec![100011, 100024]
        );
        assert_eq!(parse_u32_array("{}"), Vec::<u32>::new());
    }

    #[test]
    fn unpacks_item_id_and_appended_level() {
        assert_eq!(parse_item(100024), Some((10002, 4)));
        assert_eq!(parse_item(0), None);
    }

    #[test]
    fn strips_support_card_lb_and_level_suffixes() {
        assert_eq!(parse_support_card(100014), Some(10001));
        assert_eq!(parse_support_card(1000142), Some(10001));
        assert_eq!(parse_support_card(0), None);
    }

    #[test]
    fn loads_support_card_types_from_cards_json() {
        let support_card_types = load_support_card_types().unwrap();
        assert_eq!(support_card_types.card_type(10001), 3);
        assert_eq!(support_card_types.card_type(20021), 6);
        assert_eq!(
            support_card_types.card_type(999_999),
            UNKNOWN_SUPPORT_CARD_TYPE
        );
    }

    #[test]
    fn support_deck_key_uses_type_ids() {
        let support_card_types = load_support_card_types().unwrap();
        let row = RowData {
            trainer_id: "trainer".to_string(),
            card_id: 100101,
            distance_type: 1,
            scenario_id: 1,
            running_style: 1,
            team_class: Some(1),
            stats: [0; 6],
            skills: Vec::new(),
            support_cards: vec![100014, 200214],
        };
        let mut support_decks = SupportDeckInterner::default();
        let prepared = prepare_row(&row, &mut support_decks, &support_card_types);
        let deck = support_decks
            .get(prepared.support_deck_id.unwrap())
            .unwrap();

        assert_eq!(deck.key_string(), "3_6");
        assert_eq!(deck.ids_json(), json!(["3", "6"]));
    }
}
