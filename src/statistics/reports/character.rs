use super::*;

impl Compiler {
    pub(in crate::statistics) fn characterJson(
        &self,
        character_id: u32,
        character: &CharacterAgg,
    ) -> Value {
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
            characterIdDistributionJson(
                &character.distance_counts,
                character.overall.entries,
                character_id,
            ),
        );
        global.insert(
            "running_style_distribution".to_string(),
            characterIdDistributionJson(
                &character.running_style_counts,
                character.overall.entries,
                character_id,
            ),
        );
        global.insert(
            "scenario_distribution".to_string(),
            characterIdDistributionJson(
                &character.scenario_counts,
                character.overall.entries,
                character_id,
            ),
        );
        global.insert(
            "team_class_distribution".to_string(),
            characterTeamClassDistributionJson(character, character_id),
        );
        root.insert("global".to_string(), Value::Object(global));

        root.insert(
            "overall".to_string(),
            characterOverallJson(&character.overall, &self.support_decks),
        );

        let mut by_scenario = Map::new();
        for scenario in sortedKeys(&character.by_scenario) {
            by_scenario.insert(
                scenario.to_string(),
                characterOverallJson(&character.by_scenario[&scenario], &self.support_decks),
            );
        }
        root.insert("by_scenario".to_string(), Value::Object(by_scenario));

        let mut by_distance = Map::new();
        for distance_id in sortedKeys(&character.distance_counts) {
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
                    characterDistanceReportJson(report, &self.support_decks),
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
                        characterDistanceReportJson(
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
