//! Order-preserving edits of a JSON object (D16): untouched values retain
//! their original bytes while edited keys are re-serialized.
use serde::de::{Deserializer, MapAccess, Visitor};
use serde_json::value::RawValue;

/// Keys are held decoded, so `render` re-escapes them; only values keep their
/// original bytes (serde_json cannot expose raw object keys).
#[derive(Debug, Default)]
pub struct Obj(pub Vec<(String, Box<RawValue>)>, pub bool);

impl<'de> serde::Deserialize<'de> for Obj {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ObjectVisitor;

        impl<'de> Visitor<'de> for ObjectVisitor {
            type Value = Obj;

            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a JSON object")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Obj, A::Error> {
                let mut fields = Vec::new();
                while let Some((key, value)) = map.next_entry::<String, Box<RawValue>>()? {
                    if fields.iter().any(|(k, _)| *k == key) {
                        return Err(serde::de::Error::custom(format!("duplicate key {key:?}")));
                    }
                    fields.push((key, value));
                }
                Ok(Obj(fields, false))
            }
        }

        deserializer.deserialize_map(ObjectVisitor)
    }
}

impl Obj {
    pub fn parse(text: &str) -> anyhow::Result<Self> {
        if text.trim().is_empty() {
            return Ok(Self::default());
        }
        let mut obj: Obj = serde_json::from_str(text)?;
        obj.1 = text.ends_with('\n');
        Ok(obj)
    }

    pub fn get(&self, key: &str) -> Option<&RawValue> {
        self.0
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, value)| value.as_ref())
    }

    pub fn set(&mut self, key: &str, value: Box<RawValue>) {
        match self.0.iter_mut().find(|(k, _)| k == key) {
            Some(entry) => entry.1 = value,
            None => self.0.push((key.to_owned(), value)),
        }
    }

    pub fn remove(&mut self, key: &str) {
        self.0.retain(|(k, _)| k != key);
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Two-space indent at this level; nested values keep their bytes.
    pub fn render(&self) -> String {
        let tail = if self.1 { "\n" } else { "" };
        if self.0.is_empty() {
            return format!("{{}}{tail}");
        }
        let mut out = String::from("{\n");
        for (i, (key, value)) in self.0.iter().enumerate() {
            out.push_str("  ");
            out.push_str(&serde_json::to_string(key).expect("a string serializes"));
            out.push_str(": ");
            out.push_str(value.get());
            out.push_str(if i + 1 < self.0.len() { ",\n" } else { "\n" });
        }
        out.push('}');
        out.push_str(tail);
        out
    }
}

pub fn raw(value: &serde_json::Value) -> Box<RawValue> {
    RawValue::from_string(serde_json::to_string_pretty(value).expect("json")).expect("valid json")
}

pub fn raw_text(text: String) -> anyhow::Result<Box<RawValue>> {
    Ok(RawValue::from_string(text)?)
}

pub enum GroupEdit {
    Keep,
    Drop,
    Rewrite(Box<RawValue>),
}

pub fn edit_group(
    group: &RawValue,
    classify: impl Fn(&serde_json::Value) -> bool,
) -> anyhow::Result<GroupEdit> {
    let mut obj = Obj::parse(group.get())?;
    let Some(list) = obj.get("hooks") else {
        return Ok(GroupEdit::Keep);
    };
    let items: Vec<Box<RawValue>> = serde_json::from_str(list.get())?;
    let mut kept = Vec::new();
    let mut dropped = 0;
    for item in items {
        let value: serde_json::Value = serde_json::from_str(item.get())?;
        if classify(&value) {
            dropped += 1;
        } else {
            kept.push(item);
        }
    }
    Ok(match (dropped, kept.is_empty()) {
        (0, _) => GroupEdit::Keep,
        (_, true) => GroupEdit::Drop,
        _ => {
            obj.set("hooks", raw_text(render_array(&kept))?);
            GroupEdit::Rewrite(raw_text(obj.render())?)
        }
    })
}

pub fn render_array(items: &[Box<RawValue>]) -> String {
    if items.is_empty() {
        return "[]".into();
    }
    format!(
        "[\n{}\n]",
        items
            .iter()
            .map(|item| item.get())
            .collect::<Vec<_>>()
            .join(",\n")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn untouched_values_keep_their_bytes_and_order() {
        let text = "{\n \"permissions\": {\"allow\": [\"Bash(ls)\"]},\n \"zeta\": 1,\n \"alpha\":   [1,  2]\n}";
        let mut obj = Obj::parse(text).unwrap();
        assert_eq!(
            obj.0.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>(),
            vec!["permissions", "zeta", "alpha"]
        );
        obj.set("hooks", raw(&serde_json::json!({"Stop": []})));
        let out = obj.render();
        assert!(out.contains("\"alpha\": [1,  2]"), "{out}");
        assert!(out.find("permissions").unwrap() < out.find("hooks").unwrap());
        let value: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(value["permissions"]["allow"][0], "Bash(ls)");
    }

    #[test]
    fn empty_input_is_an_empty_object_and_non_objects_are_refused() {
        assert!(Obj::parse("").unwrap().is_empty());
        assert!(Obj::parse("  \n").unwrap().is_empty());
        assert!(Obj::parse("[1]").is_err());
        assert!(Obj::parse("{").is_err());
    }

    #[test]
    fn duplicate_keys_are_refused() {
        assert!(Obj::parse(r#"{"a": 1, "a": 2}"#).is_err());
        let group = RawValue::from_string(
            r#"{"hooks": [{"url": "OWNED"}], "hooks": [{"command": "x"}]}"#.into(),
        )
        .unwrap();
        assert!(edit_group(&group, |v| v["url"] == "OWNED").is_err());
    }

    #[test]
    fn render_keeps_a_trailing_newline_only_when_present() {
        let with = Obj::parse("{\"a\": 1}\n").unwrap();
        assert_eq!(with.render(), "{\n  \"a\": 1\n}\n");
        assert_eq!(
            Obj::parse("{\"a\": 1}").unwrap().render(),
            "{\n  \"a\": 1\n}"
        );
        assert_eq!(Obj::parse("{}\n").unwrap().render(), "{}\n");
    }

    #[test]
    fn a_mixed_group_keeps_foreign_entries_byte_exact() {
        let group = r#"{"matcher": "Bash",  "hooks": [{"type":"command",   "command":"guard.sh"}, {"type": "http", "url": "OWNED"}]}"#;
        let raw = RawValue::from_string(group.into()).unwrap();
        let GroupEdit::Rewrite(out) = edit_group(&raw, |v| v["url"] == "OWNED").unwrap() else {
            panic!()
        };
        assert!(
            out.get()
                .contains(r#"{"type":"command",   "command":"guard.sh"}"#),
            "{}",
            out.get()
        );
        assert!(
            out.get().contains(r#""matcher": "Bash""#) || out.get().contains(r#""matcher":"Bash""#)
        );
        assert!(!out.get().contains("OWNED"));
    }
}
