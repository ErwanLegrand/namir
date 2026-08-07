//! `#[serde(with = "hash_hex::option")]` for an `Option<ContentHash>` field — `entry.rs`'s
//! `LibraryEntry::hash`, the only field that needs it. `namir_core::ContentHash` deliberately has
//! no `serde` dependency (`namir-core` stays a single-dependency vocabulary crate — see its
//! `lib.rs`), so any crate that wants to put one in a JSON document needs an adapter like this
//! one; `namir-library` is the one that does.

use namir_core::ContentHash;
use serde::{Deserialize, Deserializer, Serializer};

pub fn serialize<S: Serializer>(
    hash: &Option<ContentHash>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    match hash {
        Some(h) => serializer.serialize_some(&h.to_string()),
        None => serializer.serialize_none(),
    }
}

pub fn deserialize<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<ContentHash>, D::Error> {
    let opt: Option<String> = Option::deserialize(deserializer)?;
    match opt {
        Some(s) => s.parse().map(Some).map_err(serde::de::Error::custom),
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Serialize;

    #[derive(Serialize, Deserialize, PartialEq, Debug)]
    struct Wrapper {
        #[serde(with = "crate::hash_hex")]
        hash: Option<ContentHash>,
    }

    #[test]
    fn some_round_trips() {
        let original = Wrapper {
            hash: Some(ContentHash::of(b"present")),
        };
        let json = serde_json::to_string(&original).unwrap();
        assert!(json.contains(&original.hash.unwrap().to_string()));
        let restored: Wrapper = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, original);
    }

    #[test]
    fn none_round_trips() {
        let original = Wrapper { hash: None };
        let json = serde_json::to_string(&original).unwrap();
        assert!(json.contains("null"));
        let restored: Wrapper = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, original);
    }
}
