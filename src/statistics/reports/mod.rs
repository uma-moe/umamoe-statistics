use serde_json::{json, Map, Value};

use super::*;

mod character;
mod global;

impl ItemLevelCounts {
    pub(super) fn merge(&mut self, other: Self) {
        self.total += other.total;
        for (target, source) in self.levels.iter_mut().zip(other.levels) {
            *target += source;
        }
    }
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
    pub(super) fn add(&mut self, row: &RowData, prepared: &PreparedRow) {
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

    pub(super) fn merge(&mut self, other: Self, deck_id_map: &[u32]) {
        self.entries += other.entries;
        for (target, source) in self.stats.iter_mut().zip(other.stats) {
            target.merge(source);
        }
        mergeCountMap(&mut self.uma_counts, other.uma_counts);
        mergeItemLevelMap(&mut self.support_items, other.support_items);
        mergeItemLevelMap(&mut self.skill_items, other.skill_items);
        self.support_count += other.support_count;
        self.skill_count += other.skill_count;
        self.combo_total += other.combo_total;

        for (old_deck_id, count) in other.combo_counts {
            let new_deck_id = deck_id_map[old_deck_id as usize];
            *self.combo_counts.entry(new_deck_id).or_insert(0) += count;
        }
    }

    pub(super) fn statsJson(&self) -> Value {
        let mut map = Map::new();
        for (index, stat_name) in STAT_NAMES.iter().enumerate() {
            map.insert(
                (*stat_name).to_string(),
                self.stats[index].fullJson(stat_name),
            );
        }
        Value::Object(map)
    }

    pub(super) fn partialStatsJson(&self) -> Value {
        let mut map = Map::new();
        for (index, stat_name) in STAT_NAMES.iter().enumerate() {
            map.insert((*stat_name).to_string(), self.stats[index].partialJson());
        }
        Value::Object(map)
    }

    pub(super) fn supportCardsJson(&self) -> Value {
        itemCounterJson(&self.support_items)
    }

    pub(super) fn skillsJson(&self) -> Value {
        itemCounterJson(&self.skill_items)
    }

    pub(super) fn combinationsJson(&self, support_decks: &SupportDeckInterner) -> Value {
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
                combo.compositionKey(),
                json!({
                    "count": count,
                    "percentage": percentage(*count, self.combo_total),
                    "composition": combo.compositionJson()
                }),
            );
        }

        Value::Object(result)
    }
}

impl Compiler {
    pub(super) fn indexJson(&self) -> Value {
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
}

fn distanceReportJson(
    report: &ReportAgg,
    uma_limit: usize,
    support_decks: &SupportDeckInterner,
) -> Value {
    json!({
        "total_entries": report.entries,
        "total_trained_umas": report.entries,
        "uma_distribution": umaDistributionJson(&report.uma_counts, uma_limit, report.entries),
        "stat_averages": report.statsJson(),
        "support_cards": report.supportCardsJson(),
        "total_support_cards": report.support_count,
        "support_card_combinations": report.combinationsJson(support_decks),
        "total_combinations": report.combo_total,
        "skills": report.skillsJson(),
        "total_skills": report.skill_count
    })
}

fn characterOverallJson(report: &ReportAgg, support_decks: &SupportDeckInterner) -> Value {
    json!({
        "total_entries": report.entries,
        "total_trained_umas": report.entries,
        "stat_averages": report.statsJson(),
        "support_cards": report.supportCardsJson(),
        "total_support_cards": report.support_count,
        "support_card_combinations": report.combinationsJson(support_decks),
        "total_combinations": report.combo_total,
        "skills": report.skillsJson(),
        "total_skills": report.skill_count
    })
}

fn characterDistanceReportJson(report: &ReportAgg, support_decks: &SupportDeckInterner) -> Value {
    json!({
        "total_entries": report.entries,
        "total_trained_umas": report.entries,
        "stat_averages": if report.entries > 20 { report.statsJson() } else { report.partialStatsJson() },
        "common_support_cards": report.supportCardsJson(),
        "total_support_cards": report.support_count,
        "support_card_combinations": report.combinationsJson(support_decks),
        "total_combinations": report.combo_total,
        "common_skills": report.skillsJson(),
        "total_skills": report.skill_count
    })
}

fn umaDistributionJson(counts: &HashMap<u32, u64>, limit: usize, total: u64) -> Value {
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

fn characterIdDistributionJson(counts: &HashMap<u8, u64>, total: u64, character_id: u32) -> Value {
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

fn characterTeamClassDistributionJson(character: &CharacterAgg, character_id: u32) -> Value {
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

fn itemCounterJson(counts: &HashMap<u32, ItemLevelCounts>) -> Value {
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

fn sortedKeys<K, V>(map: &HashMap<K, V>) -> Vec<K>
where
    K: Copy + Ord + Eq + Hash,
{
    let mut keys: Vec<K> = map.keys().copied().collect();
    keys.sort_unstable();
    keys
}

fn sortedScenariosForTeam(map: &HashMap<(u8, u8), ReportAgg>, team_class: u8) -> Vec<u8> {
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
