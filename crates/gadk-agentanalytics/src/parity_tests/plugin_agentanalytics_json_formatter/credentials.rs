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

//! `json_formatter_test.go` (ADK-Go `0990f9e`, #2249): the `JSONFormatter` tests of
//! credential-named types, invalid UTF-8, omit guards and colliding map keys.
//!
//! Go's defined types named for a credential (`type Token string`, `type APIKey *string`,
//! `type Credential any`, …) are Rust newtype structs: serde hands their name to the
//! serializer (`serialize_newtype_struct`), as reflection hands Go its type name. Go cases
//! whose value has no Rust form (nil pointers of a named pointer type, struct embedding and
//! promoted methods, `MarshalJSON` / `MarshalText` methods, `url.Userinfo`, generic type
//! arguments, invalid UTF-8) are left out per case with a "skipped case" comment.

// Go's test types keep their Go names (APIKey).
#![allow(clippy::upper_case_acronyms)]

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use pretty_assertions::assert_eq;
use serde::ser::SerializeMap;
use serde::{Serialize, Serializer};
use serde_json::Value;

use super::{GoBytes, st_with};
use crate::formatter::{MAX_SANITIZE_NODES, SANITIZE_BUDGET_PLACEHOLDER};
use crate::redaction::{REDACTED_VALUE, is_sensitive_key};

/// Go `checkCredentialNamed(t, want, types...)`, by the serde names of the types.
fn check_credential_named(want: bool, names: &[&str]) {
    for name in names {
        assert_eq!(is_sensitive_key(name), want, "isSensitiveKey({name:?})");
    }
}

/// Go `SmartTruncate(in, maxLength)` (redaction on) as text.
fn on(v: &(impl Serialize + ?Sized), max_len: i64) -> (String, bool) {
    st_with(v, max_len, true)
}

/// Go `smartTruncate(in, maxLength, false)` as text.
fn off(v: &(impl Serialize + ?Sized), max_len: i64) -> (String, bool) {
    st_with(v, max_len, false)
}

/// Go `labelValueProbe`: a value that records being written.
struct LabelValueProbe {
    called: Arc<AtomicBool>,
    raw: &'static str,
}

impl Serialize for LabelValueProbe {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.called.store(true, Ordering::SeqCst);
        s.serialize_str(self.raw)
    }
}

/// A Go map whose entries are written in the given order (Go's maps have none, and its
/// results do not depend on one).
struct Entries<K, V>(Vec<(K, V)>);

impl<K: Serialize, V: Serialize> Serialize for Entries<K, V> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut m = s.serialize_map(Some(self.0.len()))?;
        for (k, v) in &self.0 {
            m.serialize_entry(k, v)?;
        }
        m.end()
    }
}

/// Go `[]any` of a sanitized list, parsed.
fn parse_list(s: &str) -> Vec<Value> {
    serde_json::from_str(s).unwrap()
}

// parity: adapted: Go's credential-named defined types are Rust newtype structs (serde passes their name); the cases with no Rust value are left out (see the "skipped case" comments)
#[test]
#[ignore = "8a bug: values of credential-named newtypes (Go defined types Token, APIKey, Credential, …) and map keys of credential-named types are walked instead of written [REDACTED] (Go #2249)"]
fn test_json_formatter_credential_named_types() {
    const SECRET: &str = "SYNTHETIC_SECRET_8642";
    #[derive(Serialize, Clone, PartialEq, Eq, PartialOrd, Ord)]
    struct Token(String);
    #[derive(Serialize)]
    struct Secret(i64);
    struct Passphrase(Vec<u8>);
    impl Serialize for Passphrase {
        fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
            s.serialize_newtype_struct("Passphrase", &GoBytes(&self.0))
        }
    }
    #[derive(Serialize)]
    struct Signature([u8; 4]);
    #[derive(Serialize)]
    struct Credentials(BTreeMap<String, String>);
    #[derive(Serialize, Clone)]
    struct APIKey(Option<String>);
    #[derive(Serialize)]
    struct Credential(Value);
    #[derive(Serialize)]
    struct ClientSecret {
        id: String,
        #[serde(skip)]
        _raw: String,
    }
    #[derive(Serialize)]
    struct Label(String);
    #[derive(Serialize)]
    struct Pointer(Option<String>);
    #[derive(Serialize)]
    #[serde(rename = "Value")]
    struct NeutralValue(Value);
    #[derive(Serialize)]
    #[allow(non_snake_case)]
    struct KField {
        K: APIKey,
    }
    #[derive(Serialize)]
    #[allow(non_snake_case)]
    struct PField {
        P: Option<Token>,
    }
    #[derive(Serialize)]
    #[allow(non_snake_case)]
    struct CField {
        C: Credential,
    }
    #[derive(Serialize)]
    #[allow(non_snake_case)]
    struct TField {
        T: Token,
    }
    #[derive(Serialize)]
    #[allow(non_snake_case)]
    struct TokenField {
        Token: Token,
    }
    #[derive(Serialize)]
    struct PwdField {
        #[serde(rename = "pwd")]
        password: Token,
    }
    #[derive(Serialize)]
    #[allow(non_snake_case)]
    struct NeutralTokenField {
        Token: String,
    }
    #[derive(Serialize)]
    #[allow(non_snake_case)]
    struct VField {
        V: NeutralValue,
    }
    check_credential_named(
        true,
        &[
            "Token",
            "Secret",
            "Passphrase",
            "Signature",
            "Credentials",
            "APIKey",
            "Credential",
            "ClientSecret",
        ],
    );
    check_credential_named(false, &["Label", "Pointer", "Value"]);
    let tok = || Token(SECRET.into());
    let key = || APIKey(Some(SECRET.into()));
    let cred = || Credential(Value::from(SECRET));
    let redacted = format!("{REDACTED_VALUE:?}");

    // values
    let values: Vec<(&str, (String, bool), String, bool)> = vec![
        ("string", on(&tok(), 1000), redacted.clone(), true),
        (
            "empty string",
            on(&Token(String::new()), 1000),
            redacted.clone(),
            true,
        ),
        ("integer", on(&Secret(8642), 1000), redacted.clone(), true),
        ("zero integer", on(&Secret(0), 1000), redacted.clone(), true),
        (
            "slice",
            on(&Passphrase(SECRET.as_bytes().to_vec()), 1000),
            redacted.clone(),
            true,
        ),
        // skipped case: "nil slice" — a Rust Vec has no nil.
        (
            "array",
            on(&Signature([8, 6, 4, 2]), 1000),
            redacted.clone(),
            true,
        ),
        (
            "map",
            on(
                &Credentials(BTreeMap::from([("user".into(), SECRET.into())])),
                1000,
            ),
            redacted.clone(),
            true,
        ),
        ("pointer", on(&key(), 1000), redacted.clone(), true),
        (
            "pointer to the pointer",
            on(&Some(key()), 1000),
            redacted.clone(),
            true,
        ),
        (
            "nil pointer",
            on(&APIKey(None), 1000),
            redacted.clone(),
            true,
        ),
        (
            "nil pointer field",
            on(&KField { K: APIKey(None) }, 1000),
            format!("{{\"K\":{redacted}}}"),
            true,
        ),
        (
            "nil pointer slice element",
            on(&vec![APIKey(None)], 1000),
            format!("[{redacted}]"),
            true,
        ),
        (
            "unnamed pointer to credential",
            on(&Some(tok()), 1000),
            redacted.clone(),
            true,
        ),
        // skipped case: "nil unnamed pointer to credential" (field, slice element) — a Rust None
        // carries no pointee type, so nothing names the credential.
        (
            "interface field",
            on(&CField { C: cred() }, 1000),
            format!("{{\"C\":{redacted}}}"),
            true,
        ),
        (
            "nil interface field",
            on(
                &CField {
                    C: Credential(Value::Null),
                },
                1000,
            ),
            format!("{{\"C\":{redacted}}}"),
            true,
        ),
        (
            "interface slice",
            on(&vec![cred()], 1000),
            format!("[{redacted}]"),
            true,
        ),
        (
            "nil interface slice element",
            on(&vec![Credential(Value::Null)], 1000),
            format!("[{redacted}]"),
            true,
        ),
        (
            "interface array",
            on(&[cred()], 1000),
            format!("[{redacted}]"),
            true,
        ),
        (
            "interface map value",
            on(&BTreeMap::from([("c", cred())]), 1000),
            format!("{{\"c\":{redacted}}}"),
            true,
        ),
        (
            "nil interface map value",
            on(&BTreeMap::from([("c", Credential(Value::Null))]), 1000),
            format!("{{\"c\":{redacted}}}"),
            true,
        ),
        // skipped case: the "struct with MarshalJSON / MarshalText" cases, the "promoted …" and
        // "embedded …" cases and the shadowed / ambiguous embedded marshalers — a Rust custom
        // Serialize does not hand its type's name to the serializer, and serde has no embedding.
        (
            "field",
            on(&TField { T: tok() }, 1000),
            format!("{{\"T\":{redacted}}}"),
            true,
        ),
        (
            "field with credential name and credential type",
            on(&TokenField { Token: tok() }, 1000),
            format!("{{\"Token\":{redacted}}}"),
            true,
        ),
        // skipped case: "zero field with credential name and credential type omitempty" — serde's
        // skip_serializing_if is decided by the caller, which the serializer never sees.
        (
            "field with renamed json tag and credential type",
            on(&PwdField { password: tok() }, 1000),
            format!("{{\"pwd\":{redacted}}}"),
            true,
        ),
        (
            "field named for credential of neutral type",
            on(
                &NeutralTokenField {
                    Token: SECRET.into(),
                },
                1000,
            ),
            format!("{{\"Token\":{redacted}}}"),
            false,
        ),
        (
            "slice element",
            on(&vec![tok()], 1000),
            format!("[{redacted}]"),
            true,
        ),
        (
            "map value",
            on(&BTreeMap::from([("t", tok())]), 1000),
            format!("{{\"t\":{redacted}}}"),
            true,
        ),
        // skipped case: the "generic …" cases — a Rust generic newtype's serde name has no type
        // arguments, so they are the cases above.
        (
            "struct without a marshaler",
            on(
                &ClientSecret {
                    id: "id-1".into(),
                    _raw: SECRET.into(),
                },
                1000,
            ),
            r#"{"id":"id-1"}"#.into(),
            false,
        ),
        (
            "neutral string",
            on(&Label("kept".into()), 1000),
            r#""kept""#.into(),
            false,
        ),
        (
            "neutral pointer",
            on(&Pointer(Some("kept".into())), 1000),
            r#""kept""#.into(),
            false,
        ),
        (
            "neutral interface",
            on(
                &VField {
                    V: NeutralValue(Value::from("kept")),
                },
                1000,
            ),
            r#"{"V":"kept"}"#.into(),
            false,
        ),
        // skipped case: "embedded neutral marshaler" — serde has no embedding.
    ];
    for (name, got, want, want_trunc) in values {
        assert_eq!(got, (want, want_trunc), "values/{name}");
    }

    // values with redaction off
    for (name, (got, _)) in [
        ("Token", off(&tok(), 1000)),
        ("APIKey", off(&key(), 1000)),
        ("struct{ C Credential }", off(&CField { C: cred() }, 1000)),
        ("[]Credential", off(&vec![cred()], 1000)),
    ] {
        assert!(
            got.contains(SECRET),
            "smartTruncate({name}, redact off) = {got}, want it to hold {SECRET:?}"
        );
    }
    for (name, got, want) in [
        ("APIKey(nil)", off(&APIKey(None), 1000), "null"),
        (
            "struct{ C Credential }{}",
            off(
                &CField {
                    C: Credential(Value::Null),
                },
                1000,
            ),
            r#"{"C":null}"#,
        ),
        (
            "[]Credential{nil}",
            off(&vec![Credential(Value::Null)], 1000),
            "[null]",
        ),
        (
            "struct{ P *Token }{}",
            off(&PField { P: None }, 1000),
            r#"{"P":null}"#,
        ),
    ] {
        assert_eq!(
            got,
            (want.to_owned(), false),
            "smartTruncate({name}, redact off)"
        );
    }

    // redacted values count against the node budget
    let nodes = MAX_SANITIZE_NODES as usize;
    for (name, over, exact) in [
        (
            "[]APIKey",
            on(&vec![key(); nodes + 10], 100),
            on(&vec![key(); nodes - 1], 100),
        ),
        (
            "[]Token",
            on(&vec![tok(); nodes + 10], 100),
            on(&vec![tok(); nodes - 1], 100),
        ),
    ] {
        // skipped case: "[]AccessToken", "[]*url.Userinfo" — no Rust value (see above).
        assert!(over.1, "SmartTruncate({name} over budget) not truncated");
        let parsed = parse_list(&over.0);
        assert_eq!(
            (parsed.len(), parsed.last().cloned()),
            (nodes, Some(Value::from(SANITIZE_BUDGET_PLACEHOLDER))),
            "SmartTruncate({name} over budget)"
        );
        assert!(exact.1, "SmartTruncate({name} exact budget) not truncated");
        let parsed = parse_list(&exact.0);
        assert_eq!(
            (parsed.len(), parsed.last().cloned()),
            (nodes - 1, Some(Value::from(REDACTED_VALUE))),
            "SmartTruncate({name} exact budget)"
        );
    }
    let many: BTreeMap<Token, &str> = (0..nodes + 10)
        .map(|i| (Token(format!("{SECRET}_{i}")), "v"))
        .collect();
    let (got, truncated) = on(&many, 100);
    assert!(truncated, "SmartTruncate(map[Token]string) not truncated");
    let parsed: BTreeMap<String, Value> = serde_json::from_str(&got).unwrap();
    assert_eq!(
        parsed.get(SANITIZE_BUDGET_PLACEHOLDER),
        Some(&Value::from(SANITIZE_BUDGET_PLACEHOLDER)),
        "SmartTruncate(map[Token]string)"
    );
    // skipped case: "map[Token]*self", "pointers_at_max_depth", "nil_pointer_at_max_depth" —
    // a Rust value cannot hold itself, and has no chain of named pointer types.

    // map keys
    let val_called = Arc::new(AtomicBool::new(false));
    let probe = || LabelValueProbe {
        called: val_called.clone(),
        raw: SECRET,
    };
    let both = format!("{{{redacted}:{redacted}}}");
    let keys: Vec<(&str, (String, bool), String, bool)> = vec![
        (
            "string key",
            on(&BTreeMap::from([(tok(), SECRET)]), 1000),
            both.clone(),
            true,
        ),
        (
            "credential-named key holding sensitive key text",
            on(&BTreeMap::from([(Token("password".into()), SECRET)]), 1000),
            both.clone(),
            true,
        ),
        (
            "string key with marshaler value",
            on(&Entries(vec![(Token("password".into()), probe())]), 1000),
            both.clone(),
            true,
        ),
        (
            "multiple credential keys",
            on(
                &BTreeMap::from([(Token("a".into()), "v1"), (Token("b".into()), "v2")]),
                1000,
            ),
            both.clone(),
            true,
        ),
        (
            "neutral key",
            on(&Entries(vec![(Label("kept".into()), "v")]), 1000),
            r#"{"kept":"v"}"#.into(),
            false,
        ),
        // skipped case: the interface, pointer, struct-with-MarshalText, generic and recursive
        // pointer keys, and the colliding neutral keys of invalid UTF-8 — no Rust value.
    ];
    for (name, got, want, want_trunc) in keys {
        assert_eq!(got, (want, want_trunc), "map keys/{name}");
    }
    assert!(
        !val_called.load(Ordering::SeqCst),
        "SmartTruncate called the MarshalJSON method of a redacted key's value"
    );

    // plain credential struct without marshaler
    {
        #[derive(Serialize)]
        #[allow(non_snake_case)]
        struct Token {
            Value: String,
            ExpiresIn: i64,
        }
        #[derive(Serialize)]
        #[allow(non_snake_case)]
        struct PrivateKey {
            AccessToken: String,
            ExpiresIn: i64,
        }
        #[derive(Serialize)]
        #[allow(non_snake_case)]
        struct Signature {
            Value: OuterToken,
            ExpiresIn: i64,
        }
        #[derive(Serialize)]
        #[serde(rename = "Token")]
        struct OuterToken(String);
        check_credential_named(true, &["Token", "PrivateKey", "Signature"]);
        for (name, got, want, want_trunc) in [
            (
                "neutral exported field with neutral value is emitted",
                on(
                    &Token {
                        Value: "opaque-handle".into(),
                        ExpiresIn: 3600,
                    },
                    1000,
                ),
                r#"{"ExpiresIn":3600,"Value":"opaque-handle"}"#,
                false,
            ),
            (
                "neutral exported field matching content pattern is scrubbed",
                on(
                    &Token {
                        Value: format!("Bearer {SECRET}"),
                        ExpiresIn: 3600,
                    },
                    1000,
                ),
                r#"{"ExpiresIn":3600,"Value":"Bearer [REDACTED]"}"#,
                true,
            ),
            (
                "sensitively named exported field is redacted by name",
                on(
                    &PrivateKey {
                        AccessToken: SECRET.into(),
                        ExpiresIn: 3600,
                    },
                    1000,
                ),
                r#"{"AccessToken":"[REDACTED]","ExpiresIn":3600}"#,
                false,
            ),
            (
                "credential-typed exported field is redacted by static type",
                on(
                    &Signature {
                        Value: OuterToken(SECRET.into()),
                        ExpiresIn: 3600,
                    },
                    1000,
                ),
                r#"{"ExpiresIn":3600,"Value":"[REDACTED]"}"#,
                true,
            ),
        ] {
            assert_eq!(
                got,
                (want.to_owned(), want_trunc),
                "plain credential struct/{name}"
            );
        }
    }
    // skipped case: "url.Userinfo", "opaque errors stay empty objects", "implementer boxes the
    // address of an addressable value" — no Rust url.Userinfo, Go error value or reflection box.
}

// parity: skipped TestJSONFormatter_InvalidUTF8: a Rust &str / String is always valid UTF-8, so no string, map key, MIME type, URI or name can carry the invalid bytes the test replaces
// parity: skipped TestJSONFormatter_OmitEmptyAndOmitZeroGuards: omitempty / omitzero reading a field through reflection, and a field's Go name behind its json tag, have no serde counterpart (skip_serializing_if is decided by the caller; serde passes only the renamed key)
// parity: skipped TestJSONFormatter_ConcurrentCredentialStructNoRace: a Go -race check of fields updated atomically or under a lock while formatted; Rust's borrow rules rule out the data race
// parity: skipped TestJSONFormatter_EmbeddedRedactsWholeNoShadow: Go struct embedding and promoted MarshalJSON / MarshalText methods have no serde counterpart
// parity: skipped TestJSONFormatter_DefinedUserinfoTypes: url.Userinfo, types defined over it and its promoted MarshalJSON have no Rust counterpart
// parity: skipped TestJSONFormatter_CircularReferenceBudget: a Rust value cannot reference itself (pointer, slice and map cycles), so no Serialize value reaches [CIRCULAR_REFERENCE]

// parity: adapted: Go's `type Token byte` is a newtype over u8 and `type Passphrase []byte` writes Go bytes; Go writes a []Token / []MyByte as a byte string with redaction off (a Rust Vec of newtypes is a sequence), so those redaction-off wants and the "nil slice" and "neutral defined byte slice" cases are left out
#[test]
#[ignore = "8a bug: elements of a credential-named byte newtype (Go `type Token byte`) and a credential-named byte-slice newtype are not written [REDACTED] (Go #2249)"]
fn test_json_formatter_credential_byte_slice() {
    #[derive(Serialize, Clone, Copy)]
    struct Token(u8);
    struct Passphrase(&'static [u8]);
    impl Serialize for Passphrase {
        fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
            s.serialize_newtype_struct("Passphrase", &GoBytes(self.0))
        }
    }
    check_credential_named(true, &["Token", "Passphrase"]);
    check_credential_named(false, &["MyByte"]);
    let jwt: Vec<Token> = b"jwt".iter().map(|&b| Token(b)).collect();
    let three = r#"["[REDACTED]","[REDACTED]","[REDACTED]"]"#;
    for (name, got, want, want_trunc, got_off, want_off) in [
        (
            "slice of credential-named byte type redacts elements",
            on(&jwt, 1000),
            three,
            true,
            None,
            "",
        ),
        (
            "array of credential-named byte type matches slice",
            on(&[Token(b'j'), Token(b'w'), Token(b't')], 1000),
            three,
            true,
            Some(off(&[Token(b'j'), Token(b'w'), Token(b't')], 1000)),
            "[106,119,116]",
        ),
        (
            "empty slice of credential-named byte type",
            on(&Vec::<Token>::new(), 1000),
            "[]",
            false,
            None,
            "",
        ),
        (
            "credential-named byte slice type redacts whole",
            on(&Passphrase(b"jwt"), 1000),
            r#""[REDACTED]""#,
            true,
            Some(off(&Passphrase(b"jwt"), 1000)),
            r#""jwt""#,
        ),
        (
            "plain byte slice uses bytes shortcut",
            on(&GoBytes(b"kept"), 1000),
            r#""kept""#,
            false,
            Some(off(&GoBytes(b"kept"), 1000)),
            r#""kept""#,
        ),
    ] {
        assert_eq!(got, (want.to_owned(), want_trunc), "{name}");
        if let Some(got_off) = got_off {
            assert_eq!(got_off, (want_off.to_owned(), false), "{name} (redact off)");
        }
    }

    let nodes = MAX_SANITIZE_NODES as usize;
    let (got, truncated) = on(&vec![Token(0); nodes + 10], 100);
    assert!(
        truncated,
        "SmartTruncate([]Token over budget) not truncated"
    );
    let parsed = parse_list(&got);
    assert_eq!(
        (parsed.len(), parsed.last().cloned()),
        (nodes, Some(Value::from(SANITIZE_BUDGET_PLACEHOLDER))),
        "SmartTruncate([]Token over budget)"
    );
}

// parity: adapted: a Go struct map key is a serde struct key; the two walks with a budget of 2 and 3 nodes (Go's internal sanitizer) are left out
#[test]
#[ignore = "8a bug: a map key of a credential-named struct (Go `type Secret struct{…}`) is written [UNSUPPORTED_KEY] with its value walked instead of {\"[REDACTED]\":\"[REDACTED]\"} (Go #2249)"]
fn test_json_formatter_plain_credential_struct_map_keys() {
    const SECRET: &str = "SYNTHETIC_SECRET_7531";
    #[derive(Serialize)]
    #[allow(non_snake_case)]
    struct Secret {
        ID: String,
    }
    #[derive(Serialize)]
    #[allow(non_snake_case)]
    struct LabelStruct {
        ID: String,
    }
    check_credential_named(true, &["Secret"]);
    check_credential_named(false, &["LabelStruct"]);
    let val_called = Arc::new(AtomicBool::new(false));
    let probe = |raw| LabelValueProbe {
        called: val_called.clone(),
        raw,
    };

    // skipped case: the pointer, double pointer, interface and generic keys — the same serde
    // struct key (a Rust reference serializes as its referent; a None key is no struct).
    let (got, truncated) = on(
        &Entries(vec![(
            Secret {
                ID: "key-id".into(),
            },
            probe(SECRET),
        )]),
        1000,
    );
    assert_eq!(
        (got, truncated),
        (r#"{"[REDACTED]":"[REDACTED]"}"#.to_owned(), true),
        "plain credential struct key"
    );
    assert!(
        !val_called.load(Ordering::SeqCst),
        "SmartTruncate called MarshalJSON on map value under plain credential struct key"
    );

    // Non-credential plain struct keys remain [UNSUPPORTED_KEY] and walk values.
    let (got, truncated) = on(
        &Entries(vec![(LabelStruct { ID: "k".into() }, probe("kept"))]),
        1000,
    );
    assert_eq!(
        (got, truncated, val_called.load(Ordering::SeqCst)),
        (r#"{"[UNSUPPORTED_KEY]":"kept"}"#.to_owned(), true, true),
        "map[LabelStruct]labelValueProbe"
    );
    val_called.store(false, Ordering::SeqCst);
    let (got, truncated) = off(
        &Entries(vec![(
            Secret {
                ID: "key-id".into(),
            },
            probe("kept"),
        )]),
        1000,
    );
    assert_eq!(
        (got, truncated, val_called.load(Ordering::SeqCst)),
        (r#"{"[UNSUPPORTED_KEY]":"kept"}"#.to_owned(), true, true),
        "map[Secret]labelValueProbe, redact off"
    );
}

// parity: adapted: Go's map[any]any with an int and a string key is a serde map with entries in a fixed order (Go's result does not depend on the order); the invalid-UTF-8 keys case has no Rust input
#[test]
#[ignore = "8a bug: map keys that are written alike keep the last value instead of [REDACTED] / [COLLIDING_KEY] when their values differ (Go #2249)"]
fn test_json_formatter_colliding_map_keys() {
    for (name, entries, want_on, want_off, want_trunc) in [
        (
            "int and string keys disagreeing",
            vec![(Value::from(1), "a"), (Value::from("1"), "b")],
            r#"{"1":"[REDACTED]"}"#,
            r#"{"1":"[COLLIDING_KEY]"}"#,
            true,
        ),
        (
            "int and string keys agreeing",
            vec![(Value::from(1), "a"), (Value::from("1"), "a")],
            r#"{"1":"a"}"#,
            r#"{"1":"a"}"#,
            false,
        ),
        // skipped case: "invalid UTF-8 keys disagreeing" — a Rust key is valid UTF-8.
        (
            "colliding key where one value equals placeholder",
            vec![(Value::from(1), "[COLLIDING_KEY]"), (Value::from("1"), "a")],
            r#"{"1":"[REDACTED]"}"#,
            r#"{"1":"[COLLIDING_KEY]"}"#,
            true,
        ),
    ] {
        let m = Entries(entries);
        assert_eq!(on(&m, 1000), (want_on.to_owned(), want_trunc), "{name}");
        assert_eq!(
            off(&m, 1000),
            (want_off.to_owned(), want_trunc),
            "{name} (redact off)"
        );
    }
}
