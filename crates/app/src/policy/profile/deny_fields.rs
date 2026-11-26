use crate::{
    error::{Error, Result},
    policy::{
        config_local::valid_digest,
        profile::profile_fields::{ConfigProfile, Intent, Owner},
    },
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Anchor {
    pub sha256: String,
    pub occurrence: usize,
    pub count: usize,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Slot {
    pub entry: String,
    #[serde(default, deserialize_with = "crate::policy::json::optional_object")]
    pub previous: Option<Anchor>,
    #[serde(default, deserialize_with = "crate::policy::json::optional_object")]
    pub next: Option<Anchor>,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Claim {
    #[serde(deserialize_with = "crate::policy::json::object")]
    pub owner: Owner,
    pub intent_sha256: String,
    pub desired: Vec<String>,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Record {
    #[serde(deserialize_with = "crate::policy::json::object_vec")]
    pub original: Vec<Slot>,
    pub applied: Vec<String>,
    #[serde(deserialize_with = "crate::policy::json::object_map")]
    pub claims: BTreeMap<String, Claim>,
}
#[derive(Debug)]
pub struct Proposal {
    pub after: Vec<String>,
    pub record: Option<Record>,
}
fn invalid() -> Error {
    Error::attention(
        "invalid_profile_deny_record",
        "deny 策略原值、owner 或锚点无法验证",
    )
}
pub fn owned(entry: &str) -> bool {
    entry.starts_with("Skill(pstack:") && entry.ends_with(')')
}
fn anchors(entries: &[String]) -> Vec<Anchor> {
    let hashes: Vec<_> = entries
        .iter()
        .map(|s| crate::capability::rules::invocation::hash(s.as_bytes()))
        .collect();
    let mut counts = BTreeMap::new();
    for hash in &hashes {
        *counts.entry(hash).or_insert(0usize) += 1;
    }
    let mut seen = BTreeMap::new();
    hashes
        .iter()
        .map(|hash| {
            let occurrence = seen.entry(hash).or_insert(0usize);
            let value = Anchor {
                sha256: hash.clone(),
                occurrence: *occurrence,
                count: counts[hash],
            };
            *occurrence += 1;
            value
        })
        .collect()
}
fn capture(entries: &[String]) -> Vec<Slot> {
    let unowned: Vec<_> = entries.iter().filter(|s| !owned(s)).cloned().collect();
    let markers = anchors(&unowned);
    let mut at: usize = 0;
    entries
        .iter()
        .filter_map(|s| {
            if owned(s) {
                Some(Slot {
                    entry: s.clone(),
                    previous: at.checked_sub(1).and_then(|n| markers.get(n)).cloned(),
                    next: markers.get(at).cloned(),
                })
            } else {
                at += 1;
                None
            }
        })
        .collect()
}
fn validate_claim(id: &str, claim: &Claim) -> Result<()> {
    claim.owner.validate()?;
    if claim.owner.intent != Intent::Pstack
        || !matches!(claim.owner.profile, ConfigProfile::Claude(_))
        || claim.owner.id() != id
        || !valid_digest(&claim.intent_sha256)
        || claim.desired.len() > 5000
    {
        return Err(invalid());
    }
    let mut seen = BTreeSet::new();
    if claim.desired.iter().any(|s| !owned(s) || !seen.insert(s)) {
        return Err(invalid());
    }
    Ok(())
}
impl Record {
    pub fn validate(&self) -> Result<()> {
        if self.claims.is_empty() || self.claims.len() > 10000 || self.original.len() > 5000 {
            return Err(invalid());
        }
        for (id, claim) in &self.claims {
            validate_claim(id, claim)?;
            if claim.desired != self.applied {
                return Err(invalid());
            }
        }
        for slot in &self.original {
            if !owned(&slot.entry)
                || [slot.previous.as_ref(), slot.next.as_ref()]
                    .into_iter()
                    .flatten()
                    .any(|a| !valid_digest(&a.sha256) || a.count > 5000 || a.occurrence >= a.count)
            {
                return Err(invalid());
            }
        }
        Ok(())
    }
}
fn restore(current: &[String], slots: &[Slot]) -> Result<Vec<String>> {
    let unowned: Vec<_> = current.iter().filter(|s| !owned(s)).cloned().collect();
    let markers = anchors(&unowned);
    let mut buckets = vec![vec![]; unowned.len() + 1];
    let locate = |anchor: &Anchor| {
        markers.iter().position(|a| a == anchor).ok_or_else(|| {
            Error::attention(
                "profile_deny_anchor_drift",
                "非自有 deny 锚点已变化，无法证明原始顺序",
            )
        })
    };
    for slot in slots {
        let left = slot
            .previous
            .as_ref()
            .map(&locate)
            .transpose()?
            .map_or(0, |n| n + 1);
        let right = slot
            .next
            .as_ref()
            .map(&locate)
            .transpose()?
            .unwrap_or(unowned.len());
        if left != right {
            return Err(Error::attention(
                "profile_deny_anchor_ambiguous",
                "非自有 deny 的变化导致恢复位置不唯一",
            ));
        }
        buckets[left].push(slot.entry.clone());
    }
    let mut result = vec![];
    for (position, bucket) in buckets.into_iter().enumerate() {
        result.extend(bucket);
        if let Some(value) = unowned.get(position) {
            result.push(value.clone());
        }
    }
    Ok(result)
}
pub fn merge(
    current: &[String],
    previous: Option<&Record>,
    selected: &BTreeMap<String, Option<Claim>>,
) -> Result<Proposal> {
    if current.len() > 10000 || selected.len() > 10000 {
        return Err(invalid());
    }
    if let Some(previous) = previous {
        previous.validate()?;
    }
    if selected.is_empty() {
        return Ok(Proposal {
            after: current.into(),
            record: previous.cloned(),
        });
    }
    let current_owned: Vec<_> = current.iter().filter(|s| owned(s)).cloned().collect();
    if let Some(previous) = previous
        && current_owned != previous.applied
        && current_owned
            != previous
                .original
                .iter()
                .map(|s| s.entry.clone())
                .collect::<Vec<_>>()
    {
        return Err(Error::attention(
            "profile_owned_deny_drift",
            "自有 deny 与原值及已应用值均不符，保留修改",
        ));
    }
    let mut claims = previous.map_or_default(|p| p.claims.clone());
    for (id, change) in selected {
        if let Some(change) = change {
            validate_claim(id, change)?;
            claims.insert(id.clone(), change.clone());
        } else {
            claims.remove(id);
        }
    }
    let Some(applied) = claims.values().next().map(|c| c.desired.clone()) else {
        let after = if let Some(previous) = previous {
            restore(current, &previous.original)?
        } else {
            current.into()
        };
        if after.len() > 10000 {
            return Err(invalid());
        }
        return Ok(Proposal {
            after,
            record: None,
        });
    };
    if claims.values().any(|c| c.desired != applied) {
        return Err(Error::attention(
            "profile_field_claim_conflict",
            "共享 deny 字段存在冲突 owner 意图",
        ));
    }
    let original = previous.map_or_else(|| capture(current), |p| p.original.clone());
    let mut after: Vec<_> = current.iter().filter(|s| !owned(s)).cloned().collect();
    after.extend(applied.iter().cloned());
    if after.len() > 10000 {
        return Err(invalid());
    }
    let record = Record {
        original,
        applied,
        claims,
    };
    record.validate()?;
    Ok(Proposal {
        after,
        record: Some(record),
    })
}

#[cfg(test)]
#[path = "../../../tests/unit/deny_fields_policy.rs"]
mod tests;
