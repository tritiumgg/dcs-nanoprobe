//! DCS tables in the API (spec/world.md, "Lua tables in the API").
//!
//! The API carries a Lua table as a `google.protobuf.Struct` or `Value`; the
//! store and the agent carry it as [`Lv`]. One rule converts both ways:
//!
//! - To Lua, a key that is a decimal integer without leading zeros becomes a
//!   number key; any other key stays a string. A list becomes keys 1 to n.
//! - From Lua, a table whose keys are exactly 1 to n becomes a list; any
//!   other table becomes an object, with each number key in decimal.
//! - A number key that is not an integer, a number that is not finite, and a
//!   value Lua cannot send come back as strings.

use nanoprobe_sidecar::ids::MAX_ID;
use nanoprobe_transport::codec::Lv;
use prost_types::value::Kind;
use prost_types::{ListValue, Struct, Value};

/// The number a key names, when it is a decimal integer without leading
/// zeros that a Lua number holds exactly.
fn integer_key(k: &str) -> Option<f64> {
    let digits = k.strip_prefix('-').unwrap_or(k);
    if digits.is_empty()
        || !digits.bytes().all(|b| b.is_ascii_digit())
        || (digits.len() > 1 && digits.starts_with('0'))
        || k == "-0"
    {
        return None;
    }
    let n: i64 = k.parse().ok()?;
    (n.unsigned_abs() <= MAX_ID).then_some(n as f64)
}

/// A value as Lua carries it.
pub fn to_lua(v: &Value) -> Lv {
    match &v.kind {
        None | Some(Kind::NullValue(_)) => Lv::Nil,
        Some(Kind::BoolValue(b)) => Lv::Bool(*b),
        Some(Kind::NumberValue(n)) => Lv::Number(*n),
        Some(Kind::StringValue(s)) => Lv::String(s.as_bytes().to_vec()),
        Some(Kind::StructValue(s)) => struct_to_lua(s),
        Some(Kind::ListValue(l)) => Lv::Table(
            l.values
                .iter()
                .enumerate()
                .map(|(i, x)| (Lv::Number((i + 1) as f64), to_lua(x)))
                .collect(),
        ),
    }
}

/// A struct as a Lua table. Its keys keep their sorted order.
pub fn struct_to_lua(s: &Struct) -> Lv {
    Lv::Table(
        s.fields
            .iter()
            .map(|(k, x)| {
                let key =
                    integer_key(k).map_or_else(|| Lv::String(k.as_bytes().to_vec()), Lv::Number);
                (key, to_lua(x))
            })
            .collect(),
    )
}

fn number_text(n: f64) -> String {
    if n.is_nan() {
        "nan".into()
    } else if n.is_infinite() {
        if n > 0.0 { "inf".into() } else { "-inf".into() }
    } else if n.fract() == 0.0 && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else {
        format!("{n}")
    }
}

fn key_text(k: &Lv) -> String {
    match k {
        Lv::Number(n) => number_text(*n),
        Lv::String(b) => String::from_utf8_lossy(b).into_owned(),
        Lv::Bool(b) => b.to_string(),
        Lv::Nil => "nil".into(),
        Lv::Table(_) => "table".into(),
        Lv::Other(t) => t.clone(),
    }
}

/// A Lua value as the API carries it.
pub fn from_lua(v: &Lv) -> Value {
    let kind = match v {
        Lv::Nil => Kind::NullValue(0),
        Lv::Bool(b) => Kind::BoolValue(*b),
        Lv::Number(n) if n.is_finite() => Kind::NumberValue(*n),
        Lv::Number(n) => Kind::StringValue(number_text(*n)),
        Lv::String(b) => Kind::StringValue(String::from_utf8_lossy(b).into_owned()),
        Lv::Other(t) => Kind::StringValue(t.clone()),
        Lv::Table(pairs) => {
            let is_list = !pairs.is_empty() && {
                let mut idx: Vec<f64> = pairs
                    .iter()
                    .filter_map(|(k, _)| match k {
                        Lv::Number(n) => Some(*n),
                        _ => None,
                    })
                    .collect();
                idx.sort_by(f64::total_cmp);
                idx.len() == pairs.len()
                    && idx.iter().enumerate().all(|(i, n)| *n == (i + 1) as f64)
            };
            if is_list {
                let mut items: Vec<(f64, &Lv)> = pairs
                    .iter()
                    .map(|(k, x)| match k {
                        Lv::Number(n) => (*n, x),
                        _ => unreachable!("checked"),
                    })
                    .collect();
                items.sort_by(|a, b| a.0.total_cmp(&b.0));
                Kind::ListValue(ListValue {
                    values: items.into_iter().map(|(_, x)| from_lua(x)).collect(),
                })
            } else {
                Kind::StructValue(Struct {
                    fields: pairs
                        .iter()
                        .map(|(k, x)| (key_text(k), from_lua(x)))
                        .collect(),
                })
            }
        }
    };
    Value { kind: Some(kind) }
}

/// A Lua table as a struct; `None` for nil or an empty table.
pub fn lua_to_struct(v: &Lv) -> Option<Struct> {
    match from_lua(v).kind {
        Some(Kind::StructValue(s)) if !s.fields.is_empty() => Some(s),
        Some(Kind::ListValue(l)) if !l.values.is_empty() => Some(Struct {
            fields: l
                .values
                .into_iter()
                .enumerate()
                .map(|(i, x)| ((i + 1).to_string(), x))
                .collect(),
        }),
        _ => None,
    }
}

/// An optional struct as a Lua table, nil for none.
pub fn extra_to_lua(s: Option<&Struct>) -> Lv {
    match s {
        Some(s) if !s.fields.is_empty() => struct_to_lua(s),
        _ => Lv::Nil,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn num(n: f64) -> Value {
        Value {
            kind: Some(Kind::NumberValue(n)),
        }
    }

    fn s(fields: &[(&str, Value)]) -> Struct {
        Struct {
            fields: fields
                .iter()
                .map(|(k, v)| ((*k).to_owned(), v.clone()))
                .collect(),
        }
    }

    #[test]
    fn integer_keys_become_numbers() {
        assert_eq!(integer_key("3"), Some(3.0));
        assert_eq!(integer_key("-1"), Some(-1.0));
        assert_eq!(integer_key("0"), Some(0.0));
        for k in ["03", "-0", "", "-", "1.5", "a1", "9007199254740992"] {
            assert_eq!(integer_key(k), None, "{k}");
        }
    }

    #[test]
    fn a_sparse_table_round_trips_as_an_object() {
        // Pylons 1, 3 and 5.
        let pylons = s(&[("1", num(10.0)), ("3", num(30.0)), ("5", num(50.0))]);
        let lua = struct_to_lua(&pylons);
        assert_eq!(
            lua,
            Lv::Table(vec![
                (Lv::Number(1.0), Lv::Number(10.0)),
                (Lv::Number(3.0), Lv::Number(30.0)),
                (Lv::Number(5.0), Lv::Number(50.0)),
            ])
        );
        assert_eq!(lua_to_struct(&lua), Some(pylons));
    }

    #[test]
    fn a_list_round_trips_and_odd_values_come_back_as_strings() {
        let list = Value {
            kind: Some(Kind::ListValue(ListValue {
                values: vec![num(1.0), num(2.0)],
            })),
        };
        let lua = to_lua(&list);
        assert_eq!(from_lua(&lua), list);
        let odd = Lv::Table(vec![
            (Lv::Number(1.5), Lv::Number(f64::NAN)),
            (Lv::String(b"f".to_vec()), Lv::Other("function".into())),
        ]);
        let back = lua_to_struct(&odd).expect("object");
        assert_eq!(
            back.fields["1.5"].kind,
            Some(Kind::StringValue("nan".into()))
        );
        assert_eq!(
            back.fields["f"].kind,
            Some(Kind::StringValue("function".into()))
        );
        assert_eq!(lua_to_struct(&Lv::Nil), None);
    }
}
