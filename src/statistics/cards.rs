use anyhow::{anyhow, Context, Result};
use serde_json::{json, Map, Value};

use super::*;
impl SupportDeckKey {
    pub(super) fn fromIds(type_ids: &[u32]) -> Option<Self> {
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

    pub(super) fn ids(&self) -> &[u32] {
        match self {
            Self::Inline { len, ids } => &ids[..*len as usize],
            Self::Overflow(ids) => ids.as_slice(),
        }
    }

    fn composition(&self) -> Vec<(&'static str, u32)> {
        let mut counts: Vec<(&'static str, u32)> = Vec::new();
        for type_id in self.ids() {
            let name = supportCardTypeName(*type_id);
            if let Some((_, count)) = counts.iter_mut().find(|(existing, _)| *existing == name) {
                *count += 1;
            } else {
                counts.push((name, 1));
            }
        }
        counts
    }

    pub(super) fn compositionKey(&self) -> String {
        self.composition()
            .into_iter()
            .map(|(name, count)| format!("{count}x{name}"))
            .collect::<Vec<_>>()
            .join("_")
    }

    pub(super) fn compositionJson(&self) -> Value {
        let mut composition = Map::new();
        for (name, count) in self.composition() {
            composition.insert(name.to_string(), json!(count));
        }
        Value::Object(composition)
    }
}

fn supportCardTypeName(type_id: u32) -> &'static str {
    match type_id {
        0 => "speed",
        1 => "stamina",
        2 => "power",
        3 => "guts",
        4 => "wisdom",
        FRIEND_SUPPORT_CARD_TYPE => "friend",
        GROUP_SUPPORT_CARD_TYPE => "group",
        UNKNOWN_SUPPORT_CARD_TYPE => "unknown",
        _ => "unknown",
    }
}

fn normalizeSupportCardType(raw_type: u32, is_group: bool) -> u32 {
    match raw_type {
        GROUP_SUPPORT_CARD_TYPE if is_group => GROUP_SUPPORT_CARD_TYPE,
        GROUP_SUPPORT_CARD_TYPE => FRIEND_SUPPORT_CARD_TYPE,
        other => other,
    }
}
impl SupportCardTypes {
    fn cardType(&self, card_id: u32) -> u32 {
        self.by_card_id
            .get(&card_id)
            .copied()
            .unwrap_or(UNKNOWN_SUPPORT_CARD_TYPE)
    }

    pub(crate) fn len(&self) -> usize {
        self.by_card_id.len()
    }
}
impl SupportDeckInterner {
    pub(super) fn intern(&mut self, key: SupportDeckKey) -> u32 {
        if let Some(id) = self.ids.get(&key) {
            return *id;
        }

        let id = self.keys.len() as u32;
        self.keys.push(key.clone());
        self.ids.insert(key, id);
        id
    }

    pub(super) fn get(&self, id: u32) -> Option<&SupportDeckKey> {
        self.keys.get(id as usize)
    }
}

pub(super) fn prepareRow(
    row: &RowData,
    support_decks: &mut SupportDeckInterner,
    support_card_types: &SupportCardTypes,
) -> PreparedRow {
    let mut support_type_ids = Vec::with_capacity(row.support_cards.len());
    let support_items = row
        .support_cards
        .iter()
        .filter_map(|raw| {
            parseSupportCard(*raw).map(|card_id| {
                support_type_ids.push(support_card_types.cardType(card_id));
                (card_id, 0)
            })
        })
        .collect::<Vec<_>>();
    let skill_items = row
        .skills
        .iter()
        .filter_map(|raw| parseItem(*raw))
        .collect::<Vec<_>>();

    let support_deck_id =
        SupportDeckKey::fromIds(&support_type_ids).map(|deck_key| support_decks.intern(deck_key));

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

fn parseItem(raw: u32) -> Option<(u32, usize)> {
    if raw == 0 {
        return None;
    }
    Some((raw / 10, (raw % 10) as usize))
}

fn parseSupportCard(raw: u32) -> Option<u32> {
    if raw == 0 {
        return None;
    }

    if raw >= 1_000_000 {
        Some(raw / 100)
    } else {
        Some(raw / 10)
    }
}

pub(crate) fn loadSupportCardTypes() -> Result<SupportCardTypes> {
    let value =
        serde_json::from_str::<Value>(SUPPORT_CARDS_JSON).context("parse src/cards.json")?;
    let cards = value
        .as_array()
        .ok_or_else(|| anyhow!("src/cards.json must contain an array"))?;
    let mut by_card_id = HashMap::new();

    for card in cards {
        let card_id = jsonU32(card.get("id"))
            .ok_or_else(|| anyhow!("card entry missing numeric id in src/cards.json"))?;
        let raw_type = jsonU32(card.get("type"))
            .ok_or_else(|| anyhow!("card {card_id} missing numeric type in src/cards.json"))?;
        let is_group = jsonBool(card.get("group"))
            .ok_or_else(|| anyhow!("card {card_id} missing boolean group in src/cards.json"))?;
        let cardType = normalizeSupportCardType(raw_type, is_group);

        if let Some(existing_type) = by_card_id.insert(card_id, cardType) {
            if existing_type != cardType {
                return Err(anyhow!(
                    "card {card_id} has conflicting types {existing_type} and {cardType} in src/cards.json"
                ));
            }
        }
    }

    Ok(SupportCardTypes { by_card_id })
}

fn jsonBool(value: Option<&Value>) -> Option<bool> {
    match value? {
        Value::Bool(boolean) => Some(*boolean),
        Value::String(text) => match text.as_str() {
            "true" | "TRUE" | "True" => Some(true),
            "false" | "FALSE" | "False" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unpacksItemIdAndAppendedLevel() {
        assert_eq!(parseItem(100024), Some((10002, 4)));
        assert_eq!(parseItem(0), None);
    }

    #[test]
    fn stripsSupportCardLbAndLevelSuffixes() {
        assert_eq!(parseSupportCard(100014), Some(10001));
        assert_eq!(parseSupportCard(1000142), Some(10001));
        assert_eq!(parseSupportCard(0), None);
    }

    #[test]
    fn loadsSupportCardTypesFromCardsJson() {
        let support_card_types = loadSupportCardTypes().unwrap();
        assert_eq!(support_card_types.cardType(10001), 3);
        assert_eq!(support_card_types.cardType(10021), FRIEND_SUPPORT_CARD_TYPE);
        assert_eq!(support_card_types.cardType(30067), GROUP_SUPPORT_CARD_TYPE);
        assert_eq!(
            support_card_types.cardType(999_999),
            UNKNOWN_SUPPORT_CARD_TYPE
        );
    }

    #[test]
    fn supportDeckKeyUsesTypeIds() {
        let support_card_types = loadSupportCardTypes().unwrap();
        let row = RowData {
            trainer_id: "trainer".to_string(),
            card_id: 100101,
            distance_type: 1,
            scenario_id: 1,
            running_style: 1,
            team_class: Some(1),
            stats: [0; 6],
            skills: Vec::new(),
            support_cards: vec![100214, 300674],
        };
        let mut support_decks = SupportDeckInterner::default();
        let prepared = prepareRow(&row, &mut support_decks, &support_card_types);
        let deck = support_decks
            .get(prepared.support_deck_id.unwrap())
            .unwrap();

        assert_eq!(deck.compositionKey(), "1xfriend_1xgroup");
        assert_eq!(deck.compositionJson(), json!({"friend": 1, "group": 1}));
    }

    #[test]
    fn supportDeckKeySerializesTypeComposition() {
        let deck = SupportDeckKey::fromIds(&[0, 0, 0, 0, 1, 1]).unwrap();

        assert_eq!(deck.compositionKey(), "4xspeed_2xstamina");
        assert_eq!(deck.compositionJson(), json!({"speed": 4, "stamina": 2}));
    }
}
