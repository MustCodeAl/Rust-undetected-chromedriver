//! JSON helper utilities for preference merging.

use serde_json::{Map, Value};

/// Insert a value into a nested preference map using dot-delimited keys.
///
/// For example, `"profile.password_manager_enabled"` inserts into
/// `{ "profile": { "password_manager_enabled": ... } }`.
pub(crate) fn insert_nested_pref(prefs: &mut Map<String, Value>, key: &str, value: Value) {
    if let Some((cur, rest)) = key.split_once('.') {
        let nested = prefs
            .entry(cur.to_string())
            .or_insert_with(|| Value::Object(Map::new()))
            .as_object_mut()
            .expect("pref node must be an object");
        insert_nested_pref(nested, rest, value);
    } else {
        prefs.insert(key.to_string(), value);
    }
}

/// Deep-merge JSON value `b` into `a`. Objects are merged recursively;
/// scalars in `b` overwrite those in `a`.
pub(crate) fn merge_json(a: &mut Value, b: Value) {
    match (a, b) {
        (Value::Object(ref mut am), Value::Object(bm)) => {
            for (k, v) in bm {
                merge_json(am.entry(k).or_insert(Value::Null), v);
            }
        }
        (av, bv) => *av = bv,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn nested_pref_insertion() {
        let mut map = Map::new();
        insert_nested_pref(&mut map, "a.b.c", json!(true));
        assert_eq!(map["a"]["b"]["c"], json!(true));
    }

    #[test]
    fn merge_overwrites_scalars() {
        let mut a = json!({"x": 1, "y": 2});
        merge_json(&mut a, json!({"y": 3, "z": 4}));
        assert_eq!(a, json!({"x": 1, "y": 3, "z": 4}));
    }

    #[test]
    fn merge_deep_objects() {
        let mut a = json!({"a": {"b": 1}});
        merge_json(&mut a, json!({"a": {"c": 2}}));
        assert_eq!(a, json!({"a": {"b": 1, "c": 2}}));
    }
}
