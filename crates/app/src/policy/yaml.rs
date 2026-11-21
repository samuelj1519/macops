use crate::error::{Error, Result};
use serde::{
    Deserializer,
    de::{self, DeserializeSeed, MapAccess, SeqAccess, Visitor},
};
use serde_json::{Map, Value};
type Mapping = Map<String, Value>;
use std::{cell::Cell, fmt};

const BYTE_LIMIT: usize = 1024 * 1024;
const NODE_LIMIT: usize = 20_000;
const DEPTH_LIMIT: usize = 32;

#[derive(Clone, Copy)]
struct Seed<'a> {
    nodes: &'a Cell<usize>,
    depth: usize,
}
struct YamlVisitor<'a>(Seed<'a>);

impl Seed<'_> {
    fn child(&self) -> Self {
        Self {
            nodes: self.nodes,
            depth: self.depth + 1,
        }
    }
}

impl<'de> DeserializeSeed<'de> for Seed<'_> {
    type Value = Value;
    fn deserialize<D: Deserializer<'de>>(self, input: D) -> std::result::Result<Value, D::Error> {
        if self.depth > DEPTH_LIMIT || self.nodes.get() >= NODE_LIMIT {
            return Err(de::Error::custom("YAML structure budget exceeded"));
        }
        self.nodes.set(self.nodes.get() + 1);
        input.deserialize_any(YamlVisitor(self))
    }
}

impl<'de> Visitor<'de> for YamlVisitor<'_> {
    type Value = Value;
    fn expecting(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.write_str("bounded metadata YAML without tags or merge keys")
    }
    fn visit_unit<E: de::Error>(self) -> std::result::Result<Value, E> {
        Ok(Value::Null)
    }
    fn visit_none<E: de::Error>(self) -> std::result::Result<Value, E> {
        Ok(Value::Null)
    }
    fn visit_bool<E: de::Error>(self, value: bool) -> std::result::Result<Value, E> {
        Ok(Value::Bool(value))
    }
    fn visit_i64<E: de::Error>(self, value: i64) -> std::result::Result<Value, E> {
        Ok(Value::Number(value.into()))
    }
    fn visit_u64<E: de::Error>(self, value: u64) -> std::result::Result<Value, E> {
        Ok(Value::Number(value.into()))
    }
    fn visit_f64<E: de::Error>(self, value: f64) -> std::result::Result<Value, E> {
        if !value.is_finite() {
            return Err(E::custom("non-finite metadata number"));
        }
        Ok(Value::from(value))
    }
    fn visit_str<E: de::Error>(self, value: &str) -> std::result::Result<Value, E> {
        Ok(Value::String(value.into()))
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut input: A) -> std::result::Result<Value, A::Error> {
        let mut items = vec![];
        while let Some(value) = input.next_element_seed(self.0.child())? {
            items.push(value);
        }
        Ok(Value::Array(items))
    }
    fn visit_map<A: MapAccess<'de>>(self, mut input: A) -> std::result::Result<Value, A::Error> {
        let mut fields = Mapping::new();
        while let Some(key) = input.next_key_seed(self.0.child())? {
            let Value::String(key) = key else {
                return Err(de::Error::custom("unsupported metadata key"));
            };
            if key == "<<" {
                return Err(de::Error::custom("unsupported metadata key"));
            }
            if fields.contains_key(&key) {
                return Err(de::Error::custom("duplicate metadata key"));
            }
            fields.insert(key, input.next_value_seed(self.0.child())?);
        }
        Ok(Value::Object(fields))
    }
}

pub fn parse(bytes: &[u8]) -> Result<Value> {
    if bytes.len() > BYTE_LIMIT {
        return Err(Error::new("yaml_budget_limit", "YAML 输入超过限制"));
    }
    let nodes = Cell::new(0);
    let options = serde_saphyr::options! {
        duplicate_keys:serde_saphyr::DuplicateKeyPolicy::PassThrough,
        merge_keys:serde_saphyr::MergeKeyPolicy::AsOrdinary,
        strict_booleans:true,
        reject_unsupported_tags:true,
        reject_tagged_field_names:true,
        with_snippet:false,
        budget:serde_saphyr::budget! {
            max_reader_input_bytes:Some(BYTE_LIMIT),max_documents:1,max_depth:DEPTH_LIMIT*2,
            flow_nesting_limit:DEPTH_LIMIT*2,max_nodes:NODE_LIMIT*2,max_events:NODE_LIMIT*4,
            max_total_scalar_bytes:BYTE_LIMIT,max_aliases:1000,max_anchors:1000,
            max_recorded_anchor_bytes:BYTE_LIMIT,max_recorded_anchor_events:NODE_LIMIT*2,
        },
    };
    serde_saphyr::with_deserializer_from_slice_with_options(bytes, options, |input| {
        Seed {
            nodes: &nodes,
            depth: 0,
        }
        .deserialize(input)
    })
    .map_err(|_| Error::new("invalid_metadata_yaml", "YAML 结构不受支持或超过预算"))
}

#[cfg(test)]
#[path = "../../tests/unit/yaml_policy.rs"]
mod tests;
