# Frontend v4 Statistics Notes

For datasets with `format_version >= 4`, exported JSON is minified. `datasets.json` and each dataset `index.json` stay as normal JSON files. Heavier payload files such as `global/global.json` and `characters/{umaId}.json` are stored on disk as `.json.gz`. Keep fetching the normal `.json` URLs. The server should serve the `.json.gz` file transparently with `Content-Encoding: gzip`, so browser `fetch(...).then(r => r.json())` still works.

Do not fetch `.json.gz` directly from the frontend.

Distance-specific files are gone in v4+. Load `global/global.json` for global stats; its `by_distance` field contains the old distance reports keyed by distance id. Load `characters/{umaId}.json` for per-uma stats; its `by_distance` field contains that uma's distance breakdowns.

## v4 JSON Layout

```text
assets/statistics/
├── datasets.json
└── {dataset}/
  ├── index.json
  ├── global/global.json
  └── characters/{umaId}.json
```

`datasets.json` lists available datasets and embeds each dataset's `index` metadata.

`{dataset}/index.json` contains dataset metadata, available distance ids, and available character ids.

`global/global.json` contains:

- `metadata`
- `team_class_distribution`
- `scenario_distribution`
- `uma_distribution`
- `stat_averages`
- `support_cards`
- `support_card_combinations`
- `skills`
- `by_distance`: old distance reports keyed by distance id

`characters/{umaId}.json` contains:

- `metadata`
- `global`: distributions for this uma
- `overall`: all entries for this uma
- `by_scenario`: this uma grouped by scenario
- `by_distance`: this uma grouped by distance, then team class/scenario

Most report blocks use the same core shape: `total_entries`, `stat_averages`, `support_cards`, `support_card_combinations`, `skills`, and matching total counters.

## How To Read v4

Use this loading order:

1. Load `datasets.json` or `{dataset}/index.json` to discover the active dataset version and available ids.
2. Load `global/global.json` for the main statistics page.
3. Load `characters/{umaId}.json` when the user opens a specific uma.

### Reading `global/global.json`

Use these top-level paths:

- `metadata`: dataset-wide metadata
- `team_class_distribution`: trainer and trained-uma distribution by class and scenario
- `scenario_distribution`: overall scenario split
- `uma_distribution`: popular umas overall, by team class, and by scenario
- `stat_averages`: overall, by team class, and by scenario stat averages
- `support_cards`: support-card usage
- `support_card_combinations`: support-card deck type compositions
- `skills`: skill usage
- `by_distance`: old distance pages, now embedded here

For `support_cards`, `support_card_combinations`, and `skills`, the container shape is:

```text
overall
by_team_class.{teamClass}.overall
by_team_class.{teamClass}.by_scenario.{scenario}
by_scenario.{scenario}
```

If you only need the default global view, read `overall` and ignore the nested breakdowns.

### Reading `global.by_distance`

`by_distance.{distanceId}` contains the old distance report for that distance. Its shape is:

```text
metadata
by_team_class.{teamClass}.overall
by_team_class.{teamClass}.by_scenario.{scenario}
by_scenario.{scenario}
```

This replaces the old `distance/{distanceId}.json` files.

### Reading `characters/{umaId}.json`

Use these top-level paths:

- `metadata`: per-uma metadata
- `global.distance_distribution`: this uma's overall distance split
- `global.running_style_distribution`: this uma's running style split
- `global.scenario_distribution`: this uma's scenario split
- `global.team_class_distribution`: this uma's team class split
- `overall`: this uma across all entries
- `by_scenario.{scenario}`: this uma filtered to one scenario
- `by_distance.{distanceId}`: this uma filtered to one distance

`overall` and each `by_scenario.{scenario}` value use the standard report shape:

```text
total_entries
total_trained_umas
stat_averages
support_cards
total_support_cards
support_card_combinations
total_combinations
skills
total_skills
```

`by_distance.{distanceId}` is more nested and currently uses:

```text
by_team_class.{teamClass}.overall
by_team_class.{teamClass}.by_scenario.{scenario}
```

So for a character distance page, first choose `by_distance.{distanceId}`, then read team class and scenario from there.

### Reading Common Value Types

`support_cards.{bucket}.{cardId}` values look like:

```json
{
  "id": "10001",
  "total": 12345,
  "by_level": {
    "0": 12345
  },
  "avg_level": 0.0
}
```

`skills.{bucket}.{skillId}` uses the same shape.

Support-card combinations also changed in v4. Read `composition` instead of `support_card_type_ids`:

```json
"4xspeed_2xstamina": {
  "count": 602672,
  "percentage": 5.46,
  "composition": {
    "speed": 4,
    "stamina": 2
  }
}
```

Datasets with `format_version < 4` should keep using the old parsing path.
