//! Optional profile bindings to the selected backend ruleset.
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Ruleset {
    Osu,
    Taiko,
    Fruits,
    Mania,
}

impl Ruleset {
    pub fn from_number(number: u64) -> Option<Self> {
        match number {
            0 => Some(Self::Osu),
            1 => Some(Self::Taiko),
            2 => Some(Self::Fruits),
            3 => Some(Self::Mania),
            _ => None,
        }
    }
}

/// Retains the last valid value across reconnects: a reconnect is not a ruleset
/// change and must not undo a manual profile selection.
#[derive(Default)]
pub struct Selection {
    pub current: Option<Ruleset>,
    last: Option<Ruleset>,
}
impl Selection {
    pub fn update(&mut self, current: Option<Ruleset>) -> bool {
        self.current = current;
        let Some(current) = current else {
            return false;
        };
        if self.last == Some(current) {
            return false;
        }
        self.last = Some(current);
        true
    }
}

/// Decode a selected-field v2 frame for the single `/settings/mode` pointer.
pub fn subscription_value(frame: &Value) -> Option<Option<Ruleset>> {
    let parts = frame.as_array()?;
    if !matches!(parts.first()?.as_u64()?, 0 | 1) {
        return Some(None);
    }
    if parts
        .get(3)
        .and_then(Value::as_array)
        .is_some_and(|removed| removed.iter().any(|index| index.as_u64() == Some(0)))
    {
        return Some(None);
    }
    for entry in parts.get(2)?.as_array()? {
        let pair = entry.as_array()?;
        if pair.first()?.as_u64() == Some(0) {
            return Some(
                pair.get(1)?
                    .get("number")
                    .and_then(Value::as_u64)
                    .and_then(Ruleset::from_number),
            );
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn subscription_accepts_only_available_selected_modes() {
        assert_eq!(
            subscription_value(&json!([0, 1, [[0, {"number":3,"name":"mania"}]]])),
            Some(Some(Ruleset::Mania))
        );
        assert_eq!(subscription_value(&json!([1, 2, [], [0]])), Some(None));
        assert_eq!(
            subscription_value(&json!([0, 1, [[0, {"name":""}]]])),
            Some(None)
        );
        assert_eq!(subscription_value(&json!([1, 2, []])), None);
    }
    #[test]
    fn manual_selection_survives_same_mode_and_reconnect() {
        let mut selection = Selection::default();
        assert!(selection.update(Some(Ruleset::Mania)));
        assert!(!selection.update(Some(Ruleset::Mania)));
        assert!(!selection.update(None));
        assert!(!selection.update(Some(Ruleset::Mania)));
        assert!(selection.update(Some(Ruleset::Taiko)));
    }
}
