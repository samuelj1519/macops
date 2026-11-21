use crate::error::{Error, Result};
use serde_json::Value;
use std::ops::Range;

#[derive(Clone, Debug)]
pub struct Edit {
    pub path: Vec<String>,
    pub value: Option<Value>,
}
fn invalid() -> Error {
    Error::attention(
        "invalid_profile_json_edit",
        "配置 JSON 的结构、预算或编辑范围无法验证",
    )
}
#[derive(Debug)]
struct Node {
    span: Range<usize>,
    kind: Kind,
}
#[derive(Debug)]
enum Kind {
    Object(Vec<Member>),
    Array(Vec<Node>),
    Scalar,
}
#[derive(Debug)]
struct Member {
    key: String,
    start: usize,
    value: Node,
}
struct Scanner<'a> {
    bytes: &'a [u8],
    at: usize,
    left: usize,
}
impl Scanner<'_> {
    fn whitespace(&mut self) {
        while self.bytes.get(self.at).is_some_and(u8::is_ascii_whitespace) {
            self.at += 1;
        }
    }
    fn byte(&mut self, expected: u8) -> Result<()> {
        self.whitespace();
        if self.bytes.get(self.at) != Some(&expected) {
            return Err(invalid());
        }
        self.at += 1;
        Ok(())
    }
    fn string(&mut self) -> Result<Range<usize>> {
        self.whitespace();
        let start = self.at;
        self.byte(b'"')?;
        while let Some(byte) = self.bytes.get(self.at) {
            match byte {
                b'\\' => self.at += 2,
                b'"' => {
                    self.at += 1;
                    return Ok(start..self.at);
                }
                _ => self.at += 1,
            }
        }
        Err(invalid())
    }
    fn value(&mut self, depth: usize) -> Result<Node> {
        if depth > 32 || self.left == 0 {
            return Err(invalid());
        }
        self.left -= 1;
        self.whitespace();
        let start = self.at;
        let kind = match self.bytes.get(self.at).copied().ok_or_else(invalid)? {
            b'{' => self.object(depth)?,
            b'[' => self.array(depth)?,
            b'"' => {
                self.string()?;
                Kind::Scalar
            }
            _ => {
                while self
                    .bytes
                    .get(self.at)
                    .is_some_and(|b| !b.is_ascii_whitespace() && !b",]}".contains(b))
                {
                    self.at += 1;
                }
                if self.at == start {
                    return Err(invalid());
                }
                Kind::Scalar
            }
        };
        Ok(Node {
            span: start..self.at,
            kind,
        })
    }
    fn object(&mut self, depth: usize) -> Result<Kind> {
        self.byte(b'{')?;
        self.whitespace();
        let mut members = vec![];
        if self.bytes.get(self.at) == Some(&b'}') {
            self.at += 1;
            return Ok(Kind::Object(members));
        }
        loop {
            let key = self.string()?;
            let start = key.start;
            let key = serde_json::from_slice(&self.bytes[key])?;
            self.byte(b':')?;
            let value = self.value(depth + 1)?;
            members.push(Member { key, start, value });
            self.whitespace();
            if self.bytes.get(self.at) == Some(&b'}') {
                self.at += 1;
                return Ok(Kind::Object(members));
            }
            self.byte(b',')?;
        }
    }
    fn array(&mut self, depth: usize) -> Result<Kind> {
        self.byte(b'[')?;
        self.whitespace();
        let mut values = vec![];
        if self.bytes.get(self.at) == Some(&b']') {
            self.at += 1;
            return Ok(Kind::Array(values));
        }
        loop {
            values.push(self.value(depth + 1)?);
            self.whitespace();
            if self.bytes.get(self.at) == Some(&b']') {
                self.at += 1;
                return Ok(Kind::Array(values));
            }
            self.byte(b',')?;
        }
    }
}
fn set(value: &mut Value, path: &[String], after: Option<&Value>) -> Result<()> {
    let (key, rest) = path.split_first().ok_or_else(invalid)?;
    let object = value.as_object_mut().ok_or_else(invalid)?;
    if rest.is_empty() {
        if let Some(after) = after {
            object.insert(key.clone(), after.clone());
        } else {
            object.remove(key);
        }
        return Ok(());
    }
    if !object.contains_key(key) && after.is_none() {
        return Ok(());
    }
    set(
        object
            .entry(key.clone())
            .or_insert_with(|| Value::Object(serde_json::Map::new())),
        rest,
        after,
    )
}
#[derive(Clone, Copy)]
struct Limits {
    bytes: usize,
    nodes: usize,
}
const PROFILE: Limits = Limits {
    bytes: 1024 * 1024,
    nodes: 20000,
};
const GOVERNANCE: Limits = Limits {
    bytes: 16 * 1024 * 1024,
    nodes: 500_000,
};
struct Output {
    limit: usize,
    bytes: Vec<u8>,
    invalid: bool,
}
impl Output {
    fn write(&mut self, bytes: &[u8]) {
        if self.invalid || self.bytes.len().saturating_add(bytes.len()) > self.limit {
            self.invalid = true;
        } else {
            self.bytes.extend_from_slice(bytes);
        }
    }
    fn finish(self) -> Result<Vec<u8>> {
        if self.invalid {
            Err(invalid())
        } else {
            Ok(self.bytes)
        }
    }
}
fn render(out: &mut Output, node: &Node, before: &Value, after: &Value, input: &[u8]) {
    if before == after {
        out.write(&input[node.span.clone()]);
        return;
    }
    match (&node.kind, before, after) {
        (Kind::Object(members), Value::Object(before), Value::Object(after)) => {
            render_object(out, node, members, before, after, input);
        }
        (Kind::Array(nodes), Value::Array(before), Value::Array(after)) => {
            render_array(out, node, nodes, before, after, input);
        }
        _ => out.write(after.to_string().as_bytes()),
    }
}
fn render_object(
    out: &mut Output,
    node: &Node,
    members: &[Member],
    before: &serde_json::Map<String, Value>,
    after: &serde_json::Map<String, Value>,
    input: &[u8],
) {
    let first = members.first().map_or(node.span.end - 1, |m| m.start);
    let tail = members
        .last()
        .map_or(node.span.end - 1, |m| m.value.span.end);
    let separator = if members.len() > 1 {
        &input[members[0].value.span.end..members[1].start]
    } else {
        b","
    };
    out.write(&input[node.span.start..first]);
    let mut written = 0;
    for (index, member) in members.iter().enumerate() {
        if let Some(next) = after.get(&member.key) {
            if written > 0 {
                out.write(&input[members[index - 1].value.span.end..member.start]);
            }
            out.write(&input[member.start..member.value.span.start]);
            let Some(previous) = before.get(&member.key) else {
                out.invalid = true;
                return;
            };
            render(out, &member.value, previous, next, input);
            written += 1;
        }
    }
    for (key, value) in after.iter().filter(|(key, _)| !before.contains_key(*key)) {
        if written > 0 {
            out.write(separator);
        }
        out.write(Value::String(key.clone()).to_string().as_bytes());
        out.write(b":");
        out.write(value.to_string().as_bytes());
        written += 1;
    }
    out.write(&input[tail..node.span.end]);
}
fn render_array(
    out: &mut Output,
    node: &Node,
    nodes: &[Node],
    before: &[Value],
    after: &[Value],
    input: &[u8],
) {
    let first = nodes.first().map_or(node.span.end - 1, |n| n.span.start);
    let tail = nodes.last().map_or(node.span.end - 1, |n| n.span.end);
    let separator = if nodes.len() > 1 {
        &input[nodes[0].span.end..nodes[1].span.start]
    } else {
        b","
    };
    let mut used = vec![false; before.len()];
    out.write(&input[node.span.start..first]);
    let mut previous: Option<(usize, &Node)> = None;
    for (position, value) in after.iter().enumerate() {
        let index = before
            .iter()
            .enumerate()
            .position(|(index, old)| !used[index] && old == value);
        let matched = if let Some(index) = index {
            let Some(node) = nodes.get(index) else {
                out.invalid = true;
                return;
            };
            Some((index, node))
        } else {
            None
        };
        if position > 0 {
            let gap = if let (Some((left, previous)), Some((right, node))) = (previous, matched) {
                if left + 1 == right {
                    &input[previous.span.end..node.span.start]
                } else {
                    separator
                }
            } else {
                separator
            };
            out.write(gap);
        }
        if let Some((index, node)) = matched {
            used[index] = true;
            out.write(&input[node.span.clone()]);
        } else {
            out.write(value.to_string().as_bytes());
        }
        previous = matched;
    }
    out.write(&input[tail..node.span.end]);
}
fn verify_output(output: &[u8], desired: &Value, limits: Limits) -> Result<()> {
    if crate::policy::json::parse(output)? != *desired {
        return Err(invalid());
    }
    let mut scanner = Scanner {
        bytes: output,
        at: 0,
        left: limits.nodes,
    };
    scanner.value(0)?;
    Ok(())
}
pub fn apply(input: &[u8], edits: &[Edit]) -> Result<Vec<u8>> {
    apply_limits(input, edits, PROFILE)
}
pub fn parse_governance(input: &[u8]) -> Result<Value> {
    if input.len() > GOVERNANCE.bytes {
        return Err(invalid());
    }
    let mut scanner = Scanner {
        bytes: input,
        at: 0,
        left: GOVERNANCE.nodes,
    };
    scanner.value(0)?;
    crate::policy::json::parse(input)
}
pub fn apply_governance(input: &[u8], edits: &[Edit]) -> Result<Vec<u8>> {
    parse_governance(input)?;
    apply_limits(input, edits, GOVERNANCE)
}
fn apply_limits(input: &[u8], edits: &[Edit], limits: Limits) -> Result<Vec<u8>> {
    if input.len() > limits.bytes
        || edits.len() > 1000
        || edits.iter().any(|e| e.path.is_empty() || e.path.len() > 32)
    {
        return Err(invalid());
    }
    let before = crate::policy::json::parse(input)?;
    if !before.is_object() {
        return Err(invalid());
    }
    let mut scanner = Scanner {
        bytes: input,
        at: 0,
        left: limits.nodes,
    };
    let node = scanner.value(0)?;
    scanner.whitespace();
    let mut paths: Vec<_> = edits.iter().map(|e| &e.path).collect();
    paths.sort();
    if paths.windows(2).any(|p| p[1].starts_with(p[0])) {
        return Err(invalid());
    }
    let mut desired = before.clone();
    for edit in edits {
        set(&mut desired, &edit.path, edit.value.as_ref())?;
    }
    if desired.to_string().len() > limits.bytes {
        return Err(invalid());
    }
    let mut out = Output {
        limit: limits.bytes,
        bytes: Vec::new(),
        invalid: false,
    };
    out.write(&input[..node.span.start]);
    render(&mut out, &node, &before, &desired, input);
    out.write(&input[node.span.end..]);
    let output = out.finish()?;
    verify_output(&output, &desired, limits)?;
    Ok(output)
}

#[cfg(test)]
#[path = "../../tests/unit/json_fields_policy.rs"]
mod tests;
