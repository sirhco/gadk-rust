// Copyright 2026 Chris Olson
// Portions derived from ADK-Go (https://github.com/google/adk-go), Copyright 2026 Google LLC.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Port of ADK-Go `tool/skilltoolset/skill/frontmatter.go`: the SKILL.md YAML frontmatter.

use std::collections::BTreeMap;
use std::fmt;
use std::io::BufRead;

use serde::de::{self, Deserializer, SeqAccess, Visitor};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufRead, AsyncBufReadExt};

use super::{SkillError, SkillErrorKind};

const FRONTMATTER_SEPARATOR: &[u8] = b"---\n";
const FRONTMATTER_SEPARATOR_WIN: &[u8] = b"---\r\n";

/// Go `allowedToolsFormat`.
const ALLOWED_TOOLS_FORMAT: &str = "allowed-tools must be a whitespace- or comma-separated list of Tool or Tool(well-balanced expression)";
/// Go's `allowedToolsYAML.UnmarshalYAML` error for a non-string scalar or a mapping.
const ALLOWED_TOOLS_KIND: &str = "allowed-tools must be a string or a sequence of strings";

/// The YAML metadata at the top of a SKILL.md file (Go `skill.Frontmatter`); see
/// <https://agentskills.io/specification#frontmatter>.
///
/// Serialization uses the YAML/JSON key `allowed-tools` and omits an empty `license`,
/// `compatibility`, `metadata` or `allowed-tools`, as Go's `omitempty` tags do. [`parse`] and
/// [`parse_bytes`] do not use this type's `Deserialize`: they decode through a private mirror
/// that rejects unknown keys and accepts Go's three `allowed-tools` forms.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Frontmatter {
    /// The skill name: 1-64 bytes of `[a-z0-9-]`, no leading, trailing or doubled hyphen.
    pub name: String,
    /// What the skill does: 1-1024 characters.
    pub description: String,
    /// The skill's license.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub license: String,
    /// Environment requirements: at most 500 characters.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub compatibility: String,
    /// Arbitrary string key/value metadata.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, String>,
    /// Pre-approved tools (`allowed-tools`).
    #[serde(rename = "allowed-tools", skip_serializing_if = "Vec::is_empty")]
    pub allowed_tools: Vec<String>,
}

/// Go `frontmatterYAML`: the decoding mirror. Every key is optional; a YAML `null` value
/// decodes to the zero value, as with Go's yaml.v3.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct FrontmatterYaml {
    name: Option<String>,
    description: Option<String>,
    license: Option<String>,
    compatibility: Option<String>,
    metadata: Option<BTreeMap<String, String>>,
    #[serde(rename = "allowed-tools")]
    allowed_tools: AllowedToolsYaml,
}

impl FrontmatterYaml {
    fn into_frontmatter(self) -> Frontmatter {
        Frontmatter {
            name: self.name.unwrap_or_default(),
            description: self.description.unwrap_or_default(),
            license: self.license.unwrap_or_default(),
            compatibility: self.compatibility.unwrap_or_default(),
            metadata: self.metadata.unwrap_or_default(),
            allowed_tools: self.allowed_tools.0,
        }
    }
}

/// Go `allowedToolsYAML` with its `UnmarshalYAML`: a sequence of strings, `null`, or a
/// `!!str` scalar split by [`split_allowed_tools`]. serde-saphyr reports a plain or `!!int`
/// scalar such as `5` as an integer, `true` as a bool and `1.5` as a float, so those reach
/// the non-string visitor methods and get Go's "must be a string or a sequence" error, as
/// Go's `node.Tag != "!!str"` check gives; a quoted or `!!str`-tagged scalar is a string.
#[derive(Debug, Default)]
struct AllowedToolsYaml(Vec<String>);

impl<'de> Deserialize<'de> for AllowedToolsYaml {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(AllowedToolsVisitor)
    }
}

struct AllowedToolsVisitor;

impl AllowedToolsVisitor {
    fn not_string<E: de::Error>() -> E {
        E::custom(ALLOWED_TOOLS_KIND)
    }
}

impl<'de> Visitor<'de> for AllowedToolsVisitor {
    type Value = AllowedToolsYaml;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(ALLOWED_TOOLS_KIND)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        let mut tools = Vec::new();
        while let Some(tool) = seq.next_element::<String>()? {
            tools.push(tool);
        }
        Ok(AllowedToolsYaml(tools))
    }

    fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
        split_allowed_tools(v)
            .map(AllowedToolsYaml)
            .map_err(E::custom)
    }

    fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
        Ok(AllowedToolsYaml(Vec::new()))
    }

    fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
        Ok(AllowedToolsYaml(Vec::new()))
    }

    fn visit_bool<E: de::Error>(self, _v: bool) -> Result<Self::Value, E> {
        Err(Self::not_string())
    }

    fn visit_i64<E: de::Error>(self, _v: i64) -> Result<Self::Value, E> {
        Err(Self::not_string())
    }

    fn visit_i128<E: de::Error>(self, _v: i128) -> Result<Self::Value, E> {
        Err(Self::not_string())
    }

    fn visit_u64<E: de::Error>(self, _v: u64) -> Result<Self::Value, E> {
        Err(Self::not_string())
    }

    fn visit_u128<E: de::Error>(self, _v: u128) -> Result<Self::Value, E> {
        Err(Self::not_string())
    }

    fn visit_f64<E: de::Error>(self, _v: f64) -> Result<Self::Value, E> {
        Err(Self::not_string())
    }

    fn visit_bytes<E: de::Error>(self, _v: &[u8]) -> Result<Self::Value, E> {
        Err(Self::not_string())
    }

    fn visit_map<A: de::MapAccess<'de>>(self, _map: A) -> Result<Self::Value, A::Error> {
        Err(Self::not_string())
    }
}

/// Port of Go `splitAllowedTools` (`frontmatter.go:91`): splits on whitespace (Go
/// `unicode.IsSpace`) and commas outside balanced parentheses.
fn split_allowed_tools(value: &str) -> Result<Vec<String>, String> {
    let mut tools = Vec::new();
    let mut current = String::new();
    let mut depth = 0usize;
    for r in value.chars() {
        match r {
            '(' => {
                depth += 1;
                current.push(r);
            }
            ')' => {
                if depth == 0 {
                    return Err(format!("{ALLOWED_TOOLS_FORMAT}: unexpected ')'"));
                }
                depth -= 1;
                current.push(r);
            }
            r if depth == 0 && (go_is_space(r) || r == ',') => {
                if !current.is_empty() {
                    tools.push(std::mem::take(&mut current));
                }
            }
            r => current.push(r),
        }
    }
    if depth != 0 {
        return Err(format!("{ALLOWED_TOOLS_FORMAT}: unclosed '('"));
    }
    if !current.is_empty() {
        tools.push(current);
    }
    Ok(tools)
}

/// Go `unicode.IsSpace`: `\t \n \v \f \r`, space, U+0085, U+00A0 and the Unicode `White_Space`
/// characters, which is exactly Rust's `char::is_whitespace`.
fn go_is_space(r: char) -> bool {
    r.is_whitespace()
}

/// Go `Parse`'s separator checks on one line read with `ReadBytes('\n')`.
fn is_separator(line: &[u8]) -> bool {
    line == FRONTMATTER_SEPARATOR || line == FRONTMATTER_SEPARATOR_WIN
}

/// The text of Go's `io.EOF` (`"EOF"`), the error a `ReadBytes` that hits the end of input
/// before a newline reports.
const EOF: &str = "EOF";

fn first_line_eof() -> SkillError {
    SkillError::new(SkillErrorKind::Other, format!("reading first line: {EOF}"))
}

fn bad_opening_separator() -> SkillError {
    SkillError::new(
        SkillErrorKind::Other,
        "invalid frontmatter separator line: must be '---' followed by a new line",
    )
}

fn missing_closing_separator() -> SkillError {
    SkillError::new(
        SkillErrorKind::Other,
        "missing closing frontmatter separator line: must be '---' followed by a new line",
    )
}

/// Port of Go `Parse` (`frontmatter.go:148`): reads and validates the YAML frontmatter of a
/// SKILL.md stream. On success `r` is positioned at the start of the Markdown body.
///
/// The first line must be exactly `---\n` or `---\r\n`, and the frontmatter ends at the next
/// line that is exactly one of those. Unknown keys are rejected (Go `KnownFields(true)`), an
/// empty YAML block is an error (Go's `io.EOF`, shown as `parse frontmatter: EOF`), and the
/// result must pass [`validate`]. Read errors keep the `std::io::Error` as the `source()`.
pub async fn parse<R: AsyncBufRead + Unpin + Send>(r: &mut R) -> Result<Frontmatter, SkillError> {
    let mut line = Vec::new();
    r.read_until(b'\n', &mut line)
        .await
        .map_err(|e| SkillError::io("reading first line", e))?;
    if line.last() != Some(&b'\n') {
        return Err(first_line_eof());
    }
    if !is_separator(&line) {
        return Err(bad_opening_separator());
    }
    let mut yaml = Vec::new();
    loop {
        line.clear();
        r.read_until(b'\n', &mut line)
            .await
            .map_err(|e| SkillError::io("read a line of frontmatter", e))?;
        if line.last() != Some(&b'\n') {
            return Err(missing_closing_separator());
        }
        if is_separator(&line) {
            break;
        }
        yaml.extend_from_slice(&line);
    }
    decode(&yaml)
}

/// The synchronous twin of [`parse`] over an in-memory reader.
fn parse_sync<R: BufRead>(r: &mut R) -> Result<Frontmatter, SkillError> {
    let mut line = Vec::new();
    r.read_until(b'\n', &mut line)
        .map_err(|e| SkillError::io("reading first line", e))?;
    if line.last() != Some(&b'\n') {
        return Err(first_line_eof());
    }
    if !is_separator(&line) {
        return Err(bad_opening_separator());
    }
    let mut yaml = Vec::new();
    loop {
        line.clear();
        r.read_until(b'\n', &mut line)
            .map_err(|e| SkillError::io("read a line of frontmatter", e))?;
        if line.last() != Some(&b'\n') {
            return Err(missing_closing_separator());
        }
        if is_separator(&line) {
            break;
        }
        yaml.extend_from_slice(&line);
    }
    decode(&yaml)
}

/// Go's `decoder.Decode(&fmYAML)` + `Validate` tail of `Parse`.
fn decode(yaml: &[u8]) -> Result<Frontmatter, SkillError> {
    let mut options = serde_saphyr::Options::default();
    options.with_snippet = false;
    let decoded: Option<FrontmatterYaml> = serde_saphyr::from_slice_with_options(yaml, options)
        .map_err(|e| {
            SkillError::with_source(SkillErrorKind::Other, format!("parse frontmatter: {e}"), e)
        })?;
    let fm_yaml = match decoded {
        Some(fm_yaml) => fm_yaml,
        // serde-saphyr reports both "no document" and a document that is only `null` as
        // `None`. yaml.v3 returns io.EOF only for the former and decodes the latter to the
        // zero value, which then fails validation.
        None if has_document_content(yaml) => FrontmatterYaml::default(),
        None => {
            return Err(SkillError::new(
                SkillErrorKind::Other,
                format!("parse frontmatter: {EOF}"),
            ));
        }
    };
    let fm = fm_yaml.into_frontmatter();
    validate(&fm).map_err(|e| SkillError::wrap("invalid frontmatter", e))?;
    Ok(fm)
}

/// Whether the YAML block holds anything besides blank lines, comments and document-end
/// markers, i.e. whether yaml.v3 would see a document in it.
fn has_document_content(yaml: &[u8]) -> bool {
    String::from_utf8_lossy(yaml).lines().any(|l| {
        let t = l.trim();
        !t.is_empty() && !t.starts_with('#') && t != "..."
    })
}

/// Port of Go `ParseBytes` (`frontmatter.go:186`): splits and validates the frontmatter of an
/// in-memory SKILL.md and returns it with the Markdown body. Errors carry Go's extra
/// `parse frontmatter: ` prefix. A body that is not UTF-8 is decoded lossily.
pub fn parse_bytes(content: &[u8]) -> Result<(Frontmatter, String), SkillError> {
    let mut reader = std::io::Cursor::new(content);
    let fm = parse_sync(&mut reader).map_err(|e| SkillError::wrap("parse frontmatter", e))?;
    let rest = reader.position();
    let body = usize::try_from(rest)
        .ok()
        .and_then(|at| content.get(at..))
        .unwrap_or_default();
    Ok((fm, String::from_utf8_lossy(body).into_owned()))
}

/// Port of Go `Validate` (`frontmatter.go:203`): the specification's field rules, checked in
/// Go's order with Go's texts. Lengths of `name` are in bytes; `description` and
/// `compatibility` are counted in characters (Go `utf8.RuneCountInString`).
pub fn validate(fm: &Frontmatter) -> Result<(), SkillError> {
    let fail = |msg: &str| Err(SkillError::new(SkillErrorKind::Other, msg));
    if fm.name.is_empty() || fm.name.len() > 64 {
        return fail("name must be between 1 and 64 characters long");
    }
    if fm.name.starts_with('-') || fm.name.ends_with('-') {
        return fail("name must not start or end with a hyphen");
    }
    if fm.name.contains("--") {
        return fail("name must not contain consecutive hyphens");
    }
    if !fm
        .name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return fail(
            "name may only contain lowercase alphanumeric characters (a-z, 0-9) and hyphens",
        );
    }
    if fm.description.is_empty() || fm.description.chars().count() > 1024 {
        return fail("description must be between 1 and 1024 characters long");
    }
    if fm.compatibility.chars().count() > 500 {
        return fail("compatibility must not exceed 500 characters");
    }
    Ok(())
}

/// Port of Go `Build` (`frontmatter.go:239`): validates `fm`, then returns
/// `---\n<yaml>---\n<markdown>`. The YAML text is serde-saphyr's; it round-trips through
/// [`parse_bytes`].
pub fn build(fm: &Frontmatter, markdown: &str) -> Result<Vec<u8>, SkillError> {
    validate(fm).map_err(|e| SkillError::wrap("invalid frontmatter", e))?;
    let mut yaml = serde_saphyr::to_string(fm).map_err(|e| {
        SkillError::with_source(
            SkillErrorKind::Other,
            format!("marshal frontmatter: {e}"),
            e,
        )
    })?;
    if !yaml.ends_with('\n') {
        yaml.push('\n');
    }
    let mut out = Vec::with_capacity(yaml.len() + markdown.len() + 2 * FRONTMATTER_SEPARATOR.len());
    out.extend_from_slice(FRONTMATTER_SEPARATOR);
    out.extend_from_slice(yaml.as_bytes());
    out.extend_from_slice(FRONTMATTER_SEPARATOR);
    out.extend_from_slice(markdown.as_bytes());
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_err(yaml: &str) -> String {
        let input = format!("---\n{yaml}---\nbody");
        parse_bytes(input.as_bytes()).unwrap_err().to_string()
    }

    /// Go's `node.Tag != "!!str"` branch: non-string scalars and mappings are rejected; a
    /// quoted or `!!str`-tagged scalar is split as a string.
    #[test]
    fn allowed_tools_accepts_only_strings_sequences_and_null() {
        let base = "name: skill\ndescription: skill\n";
        for value in ["5", "!!int 5", "true", "1.5", "{a: b}"] {
            let err = parse_err(&format!("{base}allowed-tools: {value}\n"));
            assert!(err.contains(ALLOWED_TOOLS_KIND), "{value}: {err}");
            assert!(
                err.starts_with("parse frontmatter: parse frontmatter: "),
                "{value}: {err}"
            );
        }
        for (value, want) in [
            ("\"5\"", vec!["5"]),
            ("!!str 5", vec!["5"]),
            ("", vec![]),
            ("null", vec![]),
            ("~", vec![]),
            ("\"\"", vec![]),
            ("[a, \"b c\"]", vec!["a", "b c"]),
            ("a,b\tc", vec!["a", "b", "c"]),
        ] {
            let input = format!("---\n{base}allowed-tools: {value}\n---\nbody");
            let (fm, _) = parse_bytes(input.as_bytes()).unwrap();
            assert_eq!(fm.allowed_tools, want, "{value}");
        }
    }

    /// Go's yaml.v3 decoder returns `io.EOF` for a block with no document.
    #[test]
    fn empty_yaml_block_is_eof() {
        assert_eq!(parse_err(""), "parse frontmatter: parse frontmatter: EOF");
        assert_eq!(
            parse_err("# only a comment\n\n"),
            "parse frontmatter: parse frontmatter: EOF"
        );
        // A `null` document decodes to the zero value, which then fails validation.
        assert_eq!(
            parse_err("~\n"),
            "parse frontmatter: invalid frontmatter: name must be between 1 and 64 characters long"
        );
    }

    #[test]
    fn errors_use_go_texts() {
        assert_eq!(
            parse_bytes(b"---").unwrap_err().to_string(),
            "parse frontmatter: reading first line: EOF"
        );
        assert_eq!(
            parse_bytes(b"--- \n").unwrap_err().to_string(),
            "parse frontmatter: invalid frontmatter separator line: must be '---' followed by a new line"
        );
        assert_eq!(
            parse_bytes(b"---\nname: a\n---").unwrap_err().to_string(),
            "parse frontmatter: missing closing frontmatter separator line: must be '---' followed by a new line"
        );
        assert_eq!(
            parse_err("name: skill\ndescription: \"\"\n"),
            "parse frontmatter: invalid frontmatter: description must be between 1 and 1024 characters long"
        );
        let err = parse_bytes(b"---\nname: skill\n---\n").unwrap_err();
        assert_eq!(err.kind(), SkillErrorKind::Other);
    }

    #[test]
    fn build_round_trips_every_field() {
        let fm = Frontmatter {
            name: "my-skill".into(),
            description: "Line one: with \"quotes\" and # hash".into(),
            license: "Apache-2.0".into(),
            compatibility: "needs: git".into(),
            metadata: BTreeMap::from([("version".to_string(), "1.0".to_string())]),
            allowed_tools: vec!["Bash(git:*)".into(), "Read".into()],
        };
        let out = build(&fm, "body\n").unwrap();
        let (parsed, body) = parse_bytes(&out).unwrap();
        assert_eq!(parsed, fm);
        assert_eq!(body, "body\n");
    }
}
