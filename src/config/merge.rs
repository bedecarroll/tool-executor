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
