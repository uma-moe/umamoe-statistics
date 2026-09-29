use chrono::Local;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::hash::Hash;

mod cards;
mod histogram;
mod output;
mod reports;
pub(crate) mod source;

pub(crate) use cards::loadSupportCardTypes;
use cards::prepareRow;

// Keep declarations separate without widening private fields for sibling modules.
include!("../types/statistics.rs");

impl DistanceAgg {
    fn merge(&mut self, other: Self, deck_id_map: &[u32]) {
        self.total_entries += other.total_entries;
        mergeReportMap(&mut self.by_team_class, other.by_team_class, deck_id_map);
        mergeReportMap(
            &mut self.by_team_class_scenario,
            other.by_team_class_scenario,
            deck_id_map,
        );
        mergeReportMap(&mut self.by_scenario, other.by_scenario, deck_id_map);
    }
}

impl CharacterAgg {
    fn merge(&mut self, other: Self, deck_id_map: &[u32]) {
        self.overall.merge(other.overall, deck_id_map);
        mergeReportMap(&mut self.by_scenario, other.by_scenario, deck_id_map);
        mergeReportMap(
            &mut self.by_distance_class,
            other.by_distance_class,
            deck_id_map,
        );
        mergeReportMap(
            &mut self.by_distance_class_scenario,
            other.by_distance_class_scenario,
            deck_id_map,
        );
        mergeCountMap(&mut self.distance_counts, other.distance_counts);
        mergeCountMap(&mut self.running_style_counts, other.running_style_counts);
        mergeCountMap(&mut self.scenario_counts, other.scenario_counts);
        mergeCountMap(&mut self.team_class_rows, other.team_class_rows);
        self.total_trainers += other.total_trainers;
        mergeCountMap(&mut self.team_class_trainers, other.team_class_trainers);
    }
}

impl TrainerCounts {
    fn merge(&mut self, other: Self) {
        self.total_trainers += other.total_trainers;
        mergeCountMap(&mut self.class_trainers, other.class_trainers);
        mergeCountMap(
            &mut self.scenario_total_trainers,
            other.scenario_total_trainers,
        );
        mergeCountMap(
            &mut self.scenario_class_trainers,
            other.scenario_class_trainers,
        );
    }
}

impl Compiler {
    pub(crate) fn new(dataset_version: String) -> Self {
        Self {
            generated_at: chronoTimestamp(),
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

        let deck_id_map = self.mergeSupportDecks(support_decks);
        self.total_entries += total_entries;
        self.character_ids.extend(character_ids);
        self.global.merge(global, &deck_id_map);
        mergeReportMap(&mut self.by_team_class, by_team_class, &deck_id_map);
        mergeReportMap(
            &mut self.by_team_class_scenario,
            by_team_class_scenario,
            &deck_id_map,
        );
        mergeReportMap(&mut self.by_scenario, by_scenario, &deck_id_map);
        mergeDistanceMap(&mut self.distances, distances, &deck_id_map);
        mergeCharacterMap(&mut self.characters, characters, &deck_id_map);
        self.trainer_counts.merge(trainer_counts);
    }

    fn mergeSupportDecks(&mut self, support_decks: SupportDeckInterner) -> Vec<u32> {
        support_decks
            .keys
            .into_iter()
            .map(|deck_key| self.support_decks.intern(deck_key))
            .collect()
    }

    fn addRow(&mut self, row: RowData, support_card_types: &SupportCardTypes) {
        self.observeTrainer(&row);
        let prepared = prepareRow(&row, &mut self.support_decks, support_card_types);

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

    fn observeTrainer(&mut self, row: &RowData) {
        let should_flush = self
            .active_trainer
            .as_ref()
            .map_or(false, |active| active.trainer_id != row.trainer_id);

        if should_flush {
            self.flushActiveTrainer();
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

    fn flushActiveTrainer(&mut self) {
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

    pub(crate) fn finish(&mut self) {
        self.flushActiveTrainer();
    }
}

impl Compiler {
    pub(crate) fn totalEntries(&self) -> u64 {
        self.total_entries
    }
}
fn mergeCountMap<K>(target: &mut HashMap<K, u64>, source: HashMap<K, u64>)
where
    K: Eq + Hash,
{
    for (key, count) in source {
        *target.entry(key).or_insert(0) += count;
    }
}

fn mergeItemLevelMap(
    target: &mut HashMap<u32, ItemLevelCounts>,
    source: HashMap<u32, ItemLevelCounts>,
) {
    for (item_id, counts) in source {
        target.entry(item_id).or_default().merge(counts);
    }
}

fn mergeReportMap<K>(
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

fn mergeDistanceMap(
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

fn mergeCharacterMap(
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

fn chronoTimestamp() -> String {
    Local::now()
        .naive_local()
        .format("%Y-%m-%dT%H:%M:%S%.6f")
        .to_string()
}

fn jsonU32(value: Option<&Value>) -> Option<u32> {
    match value? {
        Value::Number(number) => number.as_u64().and_then(|value| u32::try_from(value).ok()),
        Value::String(text) => text.parse::<u32>().ok(),
        _ => None,
    }
}
