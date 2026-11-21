use crate::error::Result;
use serde::{
    Deserialize, Deserializer,
    de::{self, MapAccess, SeqAccess, Visitor},
};
use serde_json::{Map, Number, Value};
use std::fmt;

pub(crate) struct UniqueValue(pub(crate) Value);
struct JsonVisitor;

impl<'de> Visitor<'de> for JsonVisitor {
    type Value = UniqueValue;

    fn expecting(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.write_str("a JSON value with unique object keys")
    }
    fn visit_unit<E: de::Error>(self) -> std::result::Result<Self::Value, E> {
        Ok(UniqueValue(Value::Null))
    }
    fn visit_bool<E: de::Error>(self, value: bool) -> std::result::Result<Self::Value, E> {
        Ok(UniqueValue(Value::Bool(value)))
    }
    fn visit_i64<E: de::Error>(self, value: i64) -> std::result::Result<Self::Value, E> {
        Ok(UniqueValue(Value::Number(value.into())))
    }
    fn visit_u64<E: de::Error>(self, value: u64) -> std::result::Result<Self::Value, E> {
        Ok(UniqueValue(Value::Number(value.into())))
    }
    fn visit_f64<E: de::Error>(self, value: f64) -> std::result::Result<Self::Value, E> {
        Number::from_f64(value)
            .map(|number| UniqueValue(Value::Number(number)))
            .ok_or_else(|| E::custom("non-finite JSON number"))
    }
    fn visit_str<E: de::Error>(self, value: &str) -> std::result::Result<Self::Value, E> {
        Ok(UniqueValue(Value::String(value.into())))
    }
    fn visit_seq<A: SeqAccess<'de>>(
        self,
        mut input: A,
    ) -> std::result::Result<Self::Value, A::Error> {
        let mut values = vec![];
        while let Some(UniqueValue(value)) = input.next_element()? {
            values.push(value);
        }
        Ok(UniqueValue(Value::Array(values)))
    }
    fn visit_map<A: MapAccess<'de>>(
        self,
        mut input: A,
    ) -> std::result::Result<Self::Value, A::Error> {
        let mut values = Map::new();
        while let Some((key, UniqueValue(value))) = input.next_entry::<String, UniqueValue>()? {
            if values.insert(key, value).is_some() {
                return Err(de::Error::custom("duplicate JSON object key"));
            }
        }
        Ok(UniqueValue(Value::Object(values)))
    }
}

impl<'de> Deserialize<'de> for UniqueValue {
    fn deserialize<D: Deserializer<'de>>(input: D) -> std::result::Result<Self, D::Error> {
        input.deserialize_any(JsonVisitor)
    }
}

pub fn parse(bytes: &[u8]) -> Result<Value> {
    serde_json::from_slice::<UniqueValue>(bytes)
        .map(|value| value.0)
        .map_err(Into::into)
}

pub(crate) struct Object<T>(pub T);

impl<'de, T: serde::Deserialize<'de>> serde::Deserialize<'de> for Object<T> {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        struct MapVisitor<T>(std::marker::PhantomData<T>);
        impl<'de, T: serde::Deserialize<'de>> serde::de::Visitor<'de> for MapVisitor<T> {
            type Value = Object<T>;
            fn expecting(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                out.write_str("a JSON object")
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                map: A,
            ) -> std::result::Result<Self::Value, A::Error> {
                T::deserialize(serde::de::value::MapAccessDeserializer::new(map)).map(Object)
            }
        }
        deserializer.deserialize_map(MapVisitor(std::marker::PhantomData))
    }
}

pub fn object<'de, T: serde::Deserialize<'de>, D: serde::Deserializer<'de>>(
    input: D,
) -> std::result::Result<T, D::Error> {
    <Object<T> as serde::Deserialize>::deserialize(input).map(|value| value.0)
}

pub fn optional_object<'de, T: serde::Deserialize<'de>, D: serde::Deserializer<'de>>(
    input: D,
) -> std::result::Result<Option<T>, D::Error> {
    <Option<Object<T>> as serde::Deserialize>::deserialize(input)
        .map(|value| value.map(|value| value.0))
}

pub fn object_vec<'de, T: serde::Deserialize<'de>, D: serde::Deserializer<'de>>(
    input: D,
) -> std::result::Result<Vec<T>, D::Error> {
    Vec::<Object<T>>::deserialize(input)
        .map(|values| values.into_iter().map(|value| value.0).collect())
}

pub fn object_map<
    'de,
    K: serde::Deserialize<'de> + Ord,
    T: serde::Deserialize<'de>,
    D: serde::Deserializer<'de>,
>(
    input: D,
) -> std::result::Result<std::collections::BTreeMap<K, T>, D::Error> {
    std::collections::BTreeMap::<K, Object<T>>::deserialize(input).map(|values| {
        values
            .into_iter()
            .map(|(key, value)| (key, value.0))
            .collect()
    })
}

pub fn object_vec_map<
    'de,
    K: serde::Deserialize<'de> + Ord,
    T: serde::Deserialize<'de>,
    D: serde::Deserializer<'de>,
>(
    input: D,
) -> std::result::Result<std::collections::BTreeMap<K, Vec<T>>, D::Error> {
    std::collections::BTreeMap::<K, Vec<Object<T>>>::deserialize(input).map(|values| {
        values
            .into_iter()
            .map(|(key, rows)| (key, rows.into_iter().map(|row| row.0).collect()))
            .collect()
    })
}

#[cfg(test)]
#[path = "../../tests/unit/json_policy.rs"]
mod tests;
