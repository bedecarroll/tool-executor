use std::path::Path;

use color_eyre::Result;
use color_eyre::eyre::eyre;
use toml::Value;
use toml::map::Entry;

pub fn merge_tables(
    target: &mut toml::map::Map<String, Value>,
    addition: toml::map::Map<String, Value>,
    source_path: Option<&Path>,
) -> Result<()> {
    for (raw_key, value) in addition {
        if raw_key.ends_with('+') {
            let key = raw_key.trim_end_matches('+');
            let addition_array = expect_array(value, key, source_path)?;
            match target.entry(key.to_string()) {
                Entry::Occupied(mut occ) => {
                    let existing = occ.get_mut();
                    let Value::Array(existing_values) = existing else {
                        return Err(eyre!("cannot append to non-array key '{key}'")
                            .wrap_err(format!("the key is defined earlier in {}", occ.key())));
                    };
                    existing_values.extend(addition_array);
                }
                Entry::Vacant(vac) => {
                    vac.insert(Value::Array(addition_array));
                }
            }
            continue;
        }

        match value {
            Value::Table(table) => match target.entry(raw_key.clone()) {
                Entry::Occupied(mut occ) => {
                    if let Value::Table(existing_table) = occ.get_mut() {
                        merge_tables(existing_table, table, source_path)?;
                    } else {
                        occ.insert(Value::Table(table));
                    }
                }
                Entry::Vacant(vac) => {
                    vac.insert(Value::Table(table));
                }
            },
            other => {
                if matches_null(&other) {
                    target.remove(&raw_key);
                } else {
                    target.insert(raw_key, other);
                }
            }
        }
    }

    Ok(())
}

fn expect_array(value: Value, key: &str, source: Option<&Path>) -> Result<Vec<Value>> {
    match value {
        Value::Array(items) => Ok(items),
        other => Err(
            eyre!("value for '{key}+' must be an array, received {other:?}").wrap_err(
                match source {
                    Some(path) => format!("while merging {}", path.display()),
                    None => "while merging configuration".to_string(),
                },
            ),
        ),
    }
}

fn matches_null(value: &Value) -> bool {
    match value {
        Value::String(s) => s.eq_ignore_ascii_case("null"),
        Value::Array(items) => items.is_empty(),
        Value::Table(table) => table.is_empty(),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table<K: Into<String>>(pairs: Vec<(K, Value)>) -> toml::map::Map<String, Value> {
        pairs.into_iter().map(|(k, v)| (k.into(), v)).collect()
    }

    #[test]
    fn merge_tables_appends_arrays_with_plus_suffix() -> Result<()> {
        let mut target = table(vec![("list", Value::Array(vec![Value::Integer(1)]))]);
        let addition = table(vec![(
            "list+".to_string(),
            Value::Array(vec![Value::Integer(2)]),
        )]);
        merge_tables(&mut target, addition, None)?;
        let Value::Array(values) = target.get("list").unwrap() else {
            panic!("expected array");
        };
        assert_eq!(values, &vec![Value::Integer(1), Value::Integer(2)]);
        Ok(())
    }

    #[test]
    fn merge_tables_overwrites_with_null_like_values() -> Result<()> {
        let mut target = table(vec![("key", Value::Integer(42))]);
        let addition = table(vec![("key", Value::String("null".into()))]);
        merge_tables(&mut target, addition, None)?;
        assert!(!target.contains_key("key"));
        Ok(())
    }

    #[test]
    fn merge_tables_merges_nested_tables() -> Result<()> {
        let mut target = table(vec![(
            "outer",
            Value::Table(table(vec![("inner", Value::Integer(1))])),
        )]);
        let addition = table(vec![(
            "outer",
            Value::Table(table(vec![("extra", Value::Integer(2))])),
        )]);
        merge_tables(&mut target, addition, None)?;
        let Value::Table(inner) = target.get("outer").unwrap() else {
            panic!("expected table");
        };
        assert_eq!(inner.get("inner"), Some(&Value::Integer(1)));
        assert_eq!(inner.get("extra"), Some(&Value::Integer(2)));
        Ok(())
    }

    #[test]
    fn merge_tables_replaces_non_table_with_table() -> Result<()> {
        let mut target = table(vec![("outer", Value::Integer(5))]);
        let addition = table(vec![(
            "outer",
            Value::Table(table(vec![("inner", Value::Integer(1))])),
        )]);

        merge_tables(&mut target, addition, None)?;

        let Value::Table(updated) = target.get("outer").unwrap() else {
            panic!("expected table");
        };
        assert_eq!(updated.get("inner"), Some(&Value::Integer(1)));
        Ok(())
    }

    #[test]
    fn merge_tables_creates_array_when_target_missing() -> Result<()> {
        let mut target = table(Vec::<(String, Value)>::new());
        let addition = table(vec![(
            "items+".to_string(),
            Value::Array(vec![Value::Integer(5)]),
        )]);
        merge_tables(&mut target, addition, None)?;
        let Value::Array(values) = target.get("items").expect("array present") else {
            panic!("expected array");
        };
        assert_eq!(values, &vec![Value::Integer(5)]);
        Ok(())
    }

    #[test]
    fn merge_tables_errors_when_appending_non_array_value() {
        let mut target = table(vec![("items", Value::Array(vec![Value::Integer(1)]))]);
        let addition = table(vec![("items+".to_string(), Value::Integer(2))]);
        let err = merge_tables(&mut target, addition, Some(Path::new("extra.toml"))).unwrap_err();
        let message = format!("{err:?}");
        assert!(
            message.contains("value for 'items+' must be an array"),
            "unexpected error: {message}"
        );
        assert!(
            message.contains("extra.toml"),
            "expected path context in error message: {message}"
        );
    }

    #[test]
    fn merge_tables_errors_when_existing_value_not_array() {
        let mut target = table(vec![("items", Value::String("nope".into()))]);
        let addition = table(vec![(
            "items+".to_string(),
            Value::Array(vec![Value::Integer(2)]),
        )]);
        let err = merge_tables(&mut target, addition, None).unwrap_err();
        let message = format!("{err:?}");
        assert!(
            message.contains("cannot append to non-array key 'items'"),
            "unexpected error: {message}"
        );
    }

    #[test]
    fn expect_array_errors_on_non_array() {
        let err = expect_array(Value::Integer(5), "items", None).unwrap_err();
        let mut matched = false;
        for cause in err.chain() {
            if cause.to_string().contains("must be an array") {
                matched = true;
                break;
            }
        }
        assert!(matched, "error chain did not contain expected message");
    }

    #[test]
    fn matches_null_detects_empty_table_and_array() {
        assert!(matches_null(&Value::Table(toml::map::Map::new())));
        assert!(matches_null(&Value::Array(Vec::new())));
        assert!(!matches_null(&Value::Integer(1)));
        assert!(matches_null(&Value::String("NULL".into())));
    }
}
