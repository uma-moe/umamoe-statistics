const STAT_NAMES: [&str; 6] = ["speed", "power", "stamina", "wiz", "guts", "rank_score"];
const DISTANCE_IDS: [u8; 5] = [1, 2, 3, 4, 5];
pub(crate) const DATA_FORMAT: &str = "ids-v1";
pub(crate) const DATA_FORMAT_VERSION: u8 = 4;
const INLINE_SUPPORT_DECK_SIZE: usize = 6;
const FRIEND_SUPPORT_CARD_TYPE: u32 = 5;
const GROUP_SUPPORT_CARD_TYPE: u32 = 6;
const UNKNOWN_SUPPORT_CARD_TYPE: u32 = 99;
const SUPPORT_CARDS_JSON: &str = include_str!("../cards.json");

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

pub(crate) struct SupportCardTypes {
    by_card_id: HashMap<u32, u32>,
}

#[derive(Default)]
struct SupportDeckInterner {
    keys: Vec<SupportDeckKey>,
    ids: HashMap<SupportDeckKey, u32>,
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

#[derive(Clone, Default)]
struct ItemLevelCounts {
    total: u64,
    levels: [u64; 10],
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

#[derive(Default)]
struct DistanceAgg {
    total_entries: u64,
    by_team_class: HashMap<u8, ReportAgg>,
    by_team_class_scenario: HashMap<(u8, u8), ReportAgg>,
    by_scenario: HashMap<u8, ReportAgg>,
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

#[derive(Default)]
struct TrainerCounts {
    total_trainers: u64,
    class_trainers: HashMap<u8, u64>,
    scenario_total_trainers: HashMap<u8, u64>,
    scenario_class_trainers: HashMap<(u8, u8), u64>,
}

#[derive(Default)]
struct ActiveTrainer {
    trainer_id: String,
    team_class: Option<u8>,
    scenarios: HashSet<u8>,
    characters: HashSet<u32>,
}

#[derive(Default)]
pub(crate) struct Compiler {
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
