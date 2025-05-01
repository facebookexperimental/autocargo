/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This source code is licensed under both the MIT license found in the
 * LICENSE-MIT file in the root directory of this source tree and the Apache
 * License, Version 2.0 found in the LICENSE-APACHE file in the root directory
 * of this source tree.
 */

use anyhow::Context;
use cargo_toml::Edition;
use cargo_toml::Value as CValue;
use itertools::Itertools;
use toml_edit::Array;
use toml_edit::ArrayOfTables;
use toml_edit::InlineTable;
use toml_edit::Item;
use toml_edit::Table;
use toml_edit::Value;

pub fn decorated_value(value: impl Into<Value>) -> Item {
    Item::Value(decorate(value.into()))
}

fn decorate(value: Value) -> Value {
    value.decorated(" ", "")
}

pub fn new_implicit_table() -> Table {
    let mut table = Table::new();
    table.set_implicit(true);
    table
}

pub fn sorted_array<'a>(values: impl IntoIterator<Item = &'a String>) -> Option<Array> {
    let mut array = values
        .into_iter()
        .sorted()
        .fold(Array::default(), |mut array, value| {
            array.push(value.as_str());
            array
        });
    array.fmt();

    if array.is_empty() { None } else { Some(array) }
}

pub fn ordered_array<'a>(values: impl IntoIterator<Item = &'a String>) -> Option<Array> {
    let mut array = values
        .into_iter()
        .fold(Array::default(), |mut array, value| {
            array.push(value.as_str());
            array
        });
    array.fmt();

    if array.is_empty() { None } else { Some(array) }
}

pub fn sorted_array_maybe_multiline<'a>(
    values: impl IntoIterator<Item = &'a String>,
) -> Option<Array> {
    let values: Vec<_> = values.into_iter().collect();
    if values.iter().map(|s| s.len() + 4).sum::<usize>() > 90 {
        // cargo_toml has the ability to modify how an array is displayed, but
        // it is hidden behind private methods. We could fork it and expose its
        // internals, but for now lets abuse it a little since it maintains
        // formatting of a parsed input.
        let array_str = format!(
            "[\n  {},\n]",
            values
                .into_iter()
                .sorted()
                .map(|value| Value::from(value.as_str()).to_string())
                .join(",\n  "),
        );
        let array_val = array_str
            .parse::<Value>()
            .with_context(|| format!("Failed to parse cargo_toml::Value from {array_str}"))
            .unwrap();
        match array_val {
            Value::Array(array) => Some(array),
            _ => panic!("Failed to parse cargo_toml::Array from {array_str}"),
        }
    } else {
        sorted_array(values)
    }
}

pub fn maybe_add_to_table<V: Into<Value>>(table: &mut Table, key: &str, maybe_value: Option<V>) {
    if let Some(value) = maybe_value {
        table[key] = decorated_value(value);
    }
}

pub fn maybe_add_to_inline_table<V: Into<Value>>(
    table: &mut InlineTable,
    key: &str,
    maybe_value: Option<V>,
) {
    if let Some(value) = maybe_value {
        table.get_or_insert(key, decorate(value.into()));
    }
}

pub fn edition_to_str(edition: &Edition) -> &'static str {
    match edition {
        Edition::E2015 => "2015",
        Edition::E2018 => "2018",
        Edition::E2021 => "2021",
        Edition::E2024 => "2024",
        &_ => "<unknown>", // Edition is non-exhaustive.
    }
}

pub fn cargo_toml_to_toml_edit_item(value: &CValue) -> Item {
    match value {
        CValue::Array(vs) if vs.iter().all(CValue::is_table) => {
            Item::ArrayOfTables(vs.iter().fold(ArrayOfTables::new(), |mut array, v| {
                if let Item::Table(table) = cargo_toml_to_toml_edit_item(v) {
                    array.push(table);
                }
                array
            }))
        }
        CValue::Table(vs) => Item::Table(vs.iter().fold(Table::new(), |mut table, (k, v)| {
            table[k] = cargo_toml_to_toml_edit_item(v);
            table
        })),
        other => Item::Value(cargo_toml_to_toml_edit_value(other)),
    }
}

pub fn cargo_toml_to_toml_edit_value(value: &CValue) -> Value {
    decorate(match value {
        CValue::String(v) => v.as_str().into(),
        CValue::Integer(v) => (*v).into(),
        CValue::Float(v) => (*v).into(),
        CValue::Boolean(v) => (*v).into(),
        CValue::Datetime(v) => v
            .to_string()
            .parse()
            .unwrap_or_else(|_| v.to_string().into()),
        CValue::Array(vs) => {
            let mut value = vs.iter().fold(Array::default(), |mut array, v| {
                array.push(cargo_toml_to_toml_edit_value(v));
                array
            });
            value.fmt();
            value.into()
        }
        CValue::Table(vs) => {
            let mut value = vs.iter().fold(InlineTable::default(), |mut table, (k, v)| {
                table.get_or_insert(k.as_str(), cargo_toml_to_toml_edit_value(v));
                table
            });
            value.fmt();
            value.into()
        }
    })
}

#[cfg(test)]
mod test {
    use super::*;

    fn s(s: &str) -> String {
        s.to_owned()
    }

    fn vec_s(s: &[&str]) -> Vec<String> {
        s.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn sorted_array_test() {
        assert_eq!(
            sorted_array(&vec_s(&["foo", "bar"])).unwrap().to_string(),
            r#"["bar", "foo"]"#
        );
        assert!(sorted_array(&vec![]).is_none());
    }

    #[test]
    fn sorted_array_maybe_multiline_test() {
        assert_eq!(
            sorted_array_maybe_multiline(&vec_s(&["foo", "bar"]))
                .unwrap()
                .to_string(),
            r#"["bar", "foo"]"#
        );
        assert_eq!(
            sorted_array_maybe_multiline(&vec_s(&[
                "very long arguments",
                "or",
                "many arguments",
                "will be stored in multiple lines for easier readability",
                "Lorem ipsum dolor sit amet, consectetur adipiscing elit",
            ]))
            .unwrap()
            .to_string(),
            r#"[
  "Lorem ipsum dolor sit amet, consectetur adipiscing elit",
  "many arguments",
  "or",
  "very long arguments",
  "will be stored in multiple lines for easier readability",
]"#
        );
    }

    #[test]
    fn cargo_toml_to_toml_edit_item_test() {
        let mut table = new_implicit_table();
        for (num, cvalue) in vec![
            CValue::String(s("foo")),
            CValue::Integer(42),
            CValue::Float(7.14),
            CValue::Boolean(true),
            CValue::Datetime("2021-02-16T12:12:12.12Z".parse().unwrap()),
            CValue::Array(
                vec![CValue::Table(
                    vec![(s("foo"), CValue::Integer(42))].into_iter().collect(),
                )]
                .into_iter()
                .collect(),
            ),
            CValue::Array(vec![CValue::Integer(42)].into_iter().collect()),
            CValue::Table(vec![(s("foo"), CValue::Integer(42))].into_iter().collect()),
        ]
        .into_iter()
        .enumerate()
        {
            table[&format!("value{num}")] = cargo_toml_to_toml_edit_item(&cvalue);
        }
        assert_eq!(
            toml_edit::DocumentMut::from(table).to_string(),
            r#"value0 = "foo"
value1 = 42
value2 = 7.14
value3 = true
value4 = 2021-02-16T12:12:12.12Z
value6 = [42]

[[value5]]
foo = 42

[value7]
foo = 42
"#
        );
    }

    #[test]
    fn cargo_toml_to_toml_edit_value_test() {
        let mut table = new_implicit_table();
        for (num, cvalue) in vec![
            CValue::String(s("foo")),
            CValue::Integer(42),
            CValue::Float(7.14),
            CValue::Boolean(true),
            CValue::Datetime("2021-02-16T12:12:12.12Z".parse().unwrap()),
            CValue::Array(
                vec![CValue::Table(
                    vec![(s("foo"), CValue::Integer(42))].into_iter().collect(),
                )]
                .into_iter()
                .collect(),
            ),
            CValue::Array(vec![CValue::Integer(42)].into_iter().collect()),
            CValue::Table(vec![(s("foo"), CValue::Integer(42))].into_iter().collect()),
        ]
        .into_iter()
        .enumerate()
        {
            table[&format!("value{num}")] = Item::Value(cargo_toml_to_toml_edit_value(&cvalue));
        }
        assert_eq!(
            table.to_string(),
            r#"value0 = "foo"
value1 = 42
value2 = 7.14
value3 = true
value4 = 2021-02-16T12:12:12.12Z
value5 = [{ foo = 42 }]
value6 = [42]
value7 = { foo = 42 }
"#
        );
    }
}
