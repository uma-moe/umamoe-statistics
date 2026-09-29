use super::*;

impl Compiler {
    pub(in crate::statistics) fn globalJson(&self) -> Value {
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
            self.globalTeamClassDistributionJson(),
        );
        root.insert(
            "scenario_distribution".to_string(),
            self.scenarioDistributionJson(),
        );
        root.insert(
            "uma_distribution".to_string(),
            self.globalUmaDistributionJson(),
        );
        root.insert("stat_averages".to_string(), self.globalStatAveragesJson());
        root.insert("support_cards".to_string(), self.globalSupportCardsJson());
        root.insert(
            "support_card_combinations".to_string(),
            self.globalCombinationsJson(),
        );
        root.insert("skills".to_string(), self.globalSkillsJson());
        root.insert("by_distance".to_string(), self.globalDistancesJson());
        Value::Object(root)
    }

    fn globalDistancesJson(&self) -> Value {
        let mut by_distance = Map::new();
        for distance_id in DISTANCE_IDS {
            let Some(distance) = self.distances.get(&distance_id) else {
                continue;
            };
            if distance.total_entries == 0 {
                continue;
            }
            by_distance.insert(
                distance_id.to_string(),
                self.distanceJson(distance_id, distance),
            );
        }
        Value::Object(by_distance)
    }

    fn globalTeamClassDistributionJson(&self) -> Value {
        let mut root = Map::new();
        root.insert(
            "total_trainers".to_string(),
            json!(self.trainer_counts.total_trainers),
        );
        root.insert("total_trained_umas".to_string(), json!(self.total_entries));

        let mut by_scenario = Map::new();
        for scenario in sortedKeys(&self.trainer_counts.scenario_total_trainers) {
            let total_trainers = self.trainer_counts.scenario_total_trainers[&scenario];
            let scenario_entries = self
                .by_scenario
                .get(&scenario)
                .map_or(0, |report| report.entries);
            let mut scenario_map = Map::new();
            scenario_map.insert("total_trainers".to_string(), json!(total_trainers));
            scenario_map.insert("total_trained_umas".to_string(), json!(scenario_entries));

            for team_class in sortedKeys(&self.trainer_counts.class_trainers) {
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

        let mut classes = sortedKeys(&self.trainer_counts.class_trainers);
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

    fn scenarioDistributionJson(&self) -> Value {
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

    fn globalUmaDistributionJson(&self) -> Value {
        let mut root = umaDistributionJson(&self.global.uma_counts, 30, self.global.entries);

        let mut by_team_class = Map::new();
        for team_class in sortedKeys(&self.by_team_class) {
            let Some(report) = self.by_team_class.get(&team_class) else {
                continue;
            };
            let mut class_map = Map::new();
            class_map.insert(
                "overall".to_string(),
                umaDistributionJson(&report.uma_counts, 30, report.entries),
            );

            let mut by_scenario = Map::new();
            for scenario in sortedScenariosForTeam(&self.by_team_class_scenario, team_class) {
                let report = &self.by_team_class_scenario[&(team_class, scenario)];
                by_scenario.insert(
                    scenario.to_string(),
                    umaDistributionJson(&report.uma_counts, 30, report.entries),
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

    fn globalStatAveragesJson(&self) -> Value {
        let mut root = Map::new();
        root.insert("overall".to_string(), self.global.statsJson());

        let mut by_team_class = Map::new();
        for team_class in sortedKeys(&self.by_team_class) {
            let report = &self.by_team_class[&team_class];
            let mut class_map = Map::new();
            class_map.insert(
                "overall".to_string(),
                if report.entries > 100 {
                    report.statsJson()
                } else {
                    Value::Object(Map::new())
                },
            );

            let mut by_scenario = Map::new();
            for scenario in sortedScenariosForTeam(&self.by_team_class_scenario, team_class) {
                by_scenario.insert(
                    scenario.to_string(),
                    self.by_team_class_scenario[&(team_class, scenario)].statsJson(),
                );
            }
            class_map.insert("by_scenario".to_string(), Value::Object(by_scenario));
            by_team_class.insert(team_class.to_string(), Value::Object(class_map));
        }
        root.insert("by_team_class".to_string(), Value::Object(by_team_class));

        let mut by_scenario = Map::new();
        for scenario in sortedKeys(&self.by_scenario) {
            let report = &self.by_scenario[&scenario];
            if report.entries > 100 {
                by_scenario.insert(scenario.to_string(), report.statsJson());
            }
        }
        root.insert("by_scenario".to_string(), Value::Object(by_scenario));
        Value::Object(root)
    }

    fn globalSupportCardsJson(&self) -> Value {
        self.globalMetricJson(
            ReportAgg::supportCardsJson,
            |report| report.support_count,
            "total_support_cards",
        )
    }

    fn globalCombinationsJson(&self) -> Value {
        self.globalMetricJson(
            |report| report.combinationsJson(&self.support_decks),
            |report| report.combo_total,
            "total_combinations",
        )
    }

    fn globalSkillsJson(&self) -> Value {
        self.globalMetricJson(
            ReportAgg::skillsJson,
            |report| report.skill_count,
            "total_skills",
        )
    }

    fn globalMetricJson<F, T>(&self, value_fn: F, total_fn: T, total_prefix: &str) -> Value
    where
        F: Fn(&ReportAgg) -> Value + Copy,
        T: Fn(&ReportAgg) -> u64 + Copy,
    {
        let mut root = Map::new();
        root.insert("overall".to_string(), value_fn(&self.global));
        root.insert(total_prefix.to_string(), json!(total_fn(&self.global)));
        root.insert("by_team_class".to_string(), self.byTeamNestedJson(value_fn));

        let mut by_scenario = Map::new();
        for scenario in sortedKeys(&self.by_scenario) {
            by_scenario.insert(scenario.to_string(), value_fn(&self.by_scenario[&scenario]));
            root.insert(
                format!("{total_prefix}_scenario_{scenario}"),
                json!(total_fn(&self.by_scenario[&scenario])),
            );
        }
        root.insert("by_scenario".to_string(), Value::Object(by_scenario));

        for team_class in sortedKeys(&self.by_team_class) {
            root.insert(
                format!("{total_prefix}_{team_class}"),
                json!(total_fn(&self.by_team_class[&team_class])),
            );
        }

        Value::Object(root)
    }

    fn byTeamNestedJson<F>(&self, value_fn: F) -> Value
    where
        F: Fn(&ReportAgg) -> Value + Copy,
    {
        let mut by_team_class = Map::new();
        for team_class in sortedKeys(&self.by_team_class) {
            let mut class_map = Map::new();
            class_map.insert(
                "overall".to_string(),
                value_fn(&self.by_team_class[&team_class]),
            );

            let mut by_scenario = Map::new();
            for scenario in sortedScenariosForTeam(&self.by_team_class_scenario, team_class) {
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

    fn distanceJson(&self, distance_id: u8, distance: &DistanceAgg) -> Value {
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
        for team_class in sortedKeys(&distance.by_team_class) {
            let report = &distance.by_team_class[&team_class];
            if report.entries <= 50 {
                continue;
            }

            let mut class_map = Map::new();
            class_map.insert(
                "overall".to_string(),
                distanceReportJson(report, 20, &self.support_decks),
            );

            let mut by_scenario = Map::new();
            for scenario in sortedScenariosForTeam(&distance.by_team_class_scenario, team_class) {
                let report = &distance.by_team_class_scenario[&(team_class, scenario)];
                by_scenario.insert(
                    scenario.to_string(),
                    distanceReportJson(report, 20, &self.support_decks),
                );
            }
            class_map.insert("by_scenario".to_string(), Value::Object(by_scenario));
            by_team_class.insert(team_class.to_string(), Value::Object(class_map));
        }
        root.insert("by_team_class".to_string(), Value::Object(by_team_class));

        let mut by_scenario = Map::new();
        for scenario in sortedKeys(&distance.by_scenario) {
            let report = &distance.by_scenario[&scenario];
            if report.entries <= 50 {
                continue;
            }

            by_scenario.insert(
                scenario.to_string(),
                distanceReportJson(report, 20, &self.support_decks),
            );
        }
        root.insert("by_scenario".to_string(), Value::Object(by_scenario));

        Value::Object(root)
    }
}
