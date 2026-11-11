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

//! Port of ADK-Go `cmd/launcher/web/triggers/internal/triggerauth/triggerauth_test.go` (Task 11).
//!
//! Go builds `Flags{audience, serviceAccounts}` literals (no flag set, so nothing counts as
//! passed) and swaps the package's `newGoogleOIDC`; here those cases call
//! [`authenticator_with`] with the values, a `passed` that reports nothing, and a recording
//! constructor that delegates to the real `authn::google_oidc`.

use std::sync::{Arc, Mutex};

use gadk_platform::go_flag::FlagSet;
use gadk_server::authn::{self, Authenticator, AuthnError, GoogleOidcConfig};
use pretty_assertions::assert_eq;

use crate::error::Error;
use crate::web::triggers::triggerauth::{Flags, NewGoogleOidc, authenticator_with};

type Recorded = Arc<Mutex<Vec<GoogleOidcConfig>>>;

/// Go `captureNewGoogleOIDC`: a constructor recording every config `Authenticator` hands it,
/// then building the real authenticator.
fn capture_new_google_oidc() -> (
    impl Fn(GoogleOidcConfig) -> Result<Arc<dyn Authenticator>, AuthnError>,
    Recorded,
) {
    let got: Recorded = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&got);
    let hook = move |cfg: GoogleOidcConfig| {
        sink.lock().unwrap().push(cfg.clone());
        authn::google_oidc(cfg)
    };
    (hook, got)
}

/// Go `(&Flags{audience, serviceAccounts}).Authenticator()` with `newGoogleOIDC` = `hook`.
fn authenticator(
    audience: &str,
    service_accounts: &str,
    hook: &NewGoogleOidc,
) -> Result<Option<Arc<dyn Authenticator>>, Error> {
    authenticator_with(audience, service_accounts, &|_| false, hook)
}

/// `Err` text or `None` for success (authenticators have no `Debug`).
fn err_text(r: &Result<Option<Arc<dyn Authenticator>>, Error>) -> Option<String> {
    r.as_ref().err().map(ToString::to_string)
}

fn config(audience: &str, accounts: &[&str]) -> GoogleOidcConfig {
    GoogleOidcConfig {
        audience: audience.to_owned(),
        allowed_service_accounts: accounts.iter().map(|s| (*s).to_owned()).collect(),
    }
}

#[test]
fn test_authenticator_hands_flag_values_to_new_google_oidc() {
    for (audience, service_accounts, want) in [
        (
            "https://svc.run.app",
            "a@p.iam.gserviceaccount.com, b@p.iam.gserviceaccount.com",
            config(
                "https://svc.run.app",
                &["a@p.iam.gserviceaccount.com", "b@p.iam.gserviceaccount.com"],
            ),
        ),
        (
            // A second audience, mixed case, and an entry with a trailing space.
            "https://Trigger.Example/Path",
            "c@p.iam.gserviceaccount.com ,d@p.iam.gserviceaccount.com",
            config(
                "https://Trigger.Example/Path",
                &["c@p.iam.gserviceaccount.com", "d@p.iam.gserviceaccount.com"],
            ),
        ),
        (
            // Three entries, one mixed case: matching is exact.
            "https://svc.run.app",
            "E@p.iam.gserviceaccount.com,f@p.iam.gserviceaccount.com,g@p.iam.gserviceaccount.com",
            config(
                "https://svc.run.app",
                &[
                    "E@p.iam.gserviceaccount.com",
                    "f@p.iam.gserviceaccount.com",
                    "g@p.iam.gserviceaccount.com",
                ],
            ),
        ),
    ] {
        let (hook, got) = capture_new_google_oidc();

        let a = authenticator(audience, service_accounts, &hook);
        assert!(
            matches!(a, Ok(Some(_))),
            "{service_accounts}: Authenticator() = {:?}, want an authenticator",
            err_text(&a)
        );
        assert_eq!(
            *got.lock().unwrap(),
            vec![want],
            "{service_accounts}: config handed to NewGoogleOIDC"
        );
    }
}

/// Go `triggerFlagAlphabet`: letters of both cases, the list separator, the flag prefix, and four
/// characters that `strings.TrimSpace` removes (space, tab, U+00A0 and U+3000).
const TRIGGER_FLAG_ALPHABET: [&str; 8] = ["a", "B", ",", "-", " ", "\t", "\u{a0}", "\u{3000}"];

fn is_reference_space(r: char) -> bool {
    r == ' ' || r == '\t' || r == '\u{a0}' || r == '\u{3000}'
}

/// Go `allStrings`: every string of zero to `n` symbols from `alphabet`.
fn all_strings(alphabet: &[&str], n: usize) -> Vec<String> {
    let mut out = vec![String::new()];
    let mut last = vec![String::new()];
    for _ in 0..n {
        let mut next = Vec::new();
        for prefix in &last {
            for c in alphabet {
                next.push(format!("{prefix}{c}"));
            }
        }
        out.extend(next.iter().cloned());
        last = next;
    }
    out
}

/// Go `generatedAudiences`: every audience of up to two symbols, plus one with each space
/// character between two letters, which only a longer audience can show.
fn generated_audiences() -> Vec<String> {
    let mut out = all_strings(&TRIGGER_FLAG_ALPHABET, 2);
    out.extend(["a B", "a\tB", "a\u{a0}B", "a\u{3000}B"].map(str::to_owned));
    out
}

/// Go `referenceConfig`: the flag rules stated independently of `Authenticator`. `ok` is false
/// when the flags must be rejected; the config is `None` when neither flag is set.
fn reference_config(audience: &str, accounts: &str) -> (Option<GoogleOidcConfig>, bool) {
    if audience.is_empty() && accounts.is_empty() {
        return (None, true);
    }
    if audience.is_empty() || accounts.is_empty() {
        return (None, false);
    }
    let aud: Vec<char> = audience.chars().collect();
    if is_reference_space(aud[0]) || is_reference_space(aud[aud.len() - 1]) || aud[0] == '-' {
        return (None, false);
    }
    let mut entries = Vec::new();
    let mut entry: Vec<char> = Vec::new();
    for r in accounts.chars().chain([',']) {
        if r != ',' {
            entry.push(r);
            continue;
        }
        let (mut lo, mut hi) = (0, entry.len());
        while lo < hi && is_reference_space(entry[lo]) {
            lo += 1;
        }
        while hi > lo && is_reference_space(entry[hi - 1]) {
            hi -= 1;
        }
        if lo == hi || entry[lo] == '-' {
            return (None, false);
        }
        entries.push(entry[lo..hi].iter().collect::<String>());
        entry.clear();
    }
    (
        Some(GoogleOidcConfig {
            audience: audience.to_owned(),
            allowed_service_accounts: entries,
        }),
        true,
    )
}

// Authenticator must reject exactly what the reference rejects, and otherwise hand NewGoogleOIDC
// exactly the reference config. Checked for every pairing of an audience from
// generated_audiences with an allow-list of up to four symbols from TRIGGER_FLAG_ALPHABET, and
// for every allow-list of up to five symbols with two fixed audiences.
#[test]
fn test_authenticator_matches_reference() {
    let (hook, got) = capture_new_google_oidc();
    let check = |audience: &str, accounts: &str| {
        got.lock().unwrap().clear();
        let a = authenticator(audience, accounts, &hook);
        match (reference_config(audience, accounts), &a) {
            ((_, false), _) => assert!(
                a.is_err(),
                "Authenticator({audience:?}, {accounts:?}) succeeded, want an error"
            ),
            (_, Err(e)) => {
                panic!("Authenticator({audience:?}, {accounts:?}) = {e}, want no error")
            }
            ((None, true), Ok(built)) => assert!(
                built.is_none() && got.lock().unwrap().is_empty(),
                "Authenticator({audience:?}, {accounts:?}) built an authenticator, want none"
            ),
            ((Some(want), true), Ok(_)) => assert_eq!(
                *got.lock().unwrap(),
                vec![want],
                "Authenticator({audience:?}, {accounts:?}): config handed to NewGoogleOIDC"
            ),
        }
    };
    let lists = all_strings(&TRIGGER_FLAG_ALPHABET, 4);
    for audience in generated_audiences() {
        for accounts in &lists {
            check(&audience, accounts);
        }
    }
    for accounts in all_strings(&TRIGGER_FLAG_ALPHABET, 5) {
        check("https://svc.run.app", &accounts);
        check("B", &accounts);
    }
}

#[test]
fn test_authenticator_nil_without_flags() {
    let (hook, got) = capture_new_google_oidc();

    let a = authenticator("", "", &hook);
    assert!(
        matches!(a, Ok(None)),
        "Authenticator() = {:?}, want nil, nil",
        err_text(&a)
    );
    assert!(
        got.lock().unwrap().is_empty(),
        "NewGoogleOIDC called with {:?}, want no call",
        got.lock().unwrap()
    );
}

#[test]
fn test_authenticator_rejects_padded_audience() {
    let (hook, got) = capture_new_google_oidc();
    for aud in [
        " https://svc.run.app",
        "https://svc.run.app ",
        "https://svc.run.app\t",
        " ",
    ] {
        assert!(
            authenticator(aud, "a@p.iam.gserviceaccount.com", &hook).is_err(),
            "Authenticator() with audience {aud:?} succeeded, want an error"
        );
    }
    assert!(
        got.lock().unwrap().is_empty(),
        "NewGoogleOIDC called with {:?}, want the padded audience rejected before it",
        got.lock().unwrap()
    );
}

// A lone flag must name the missing one, not surface NewGoogleOIDC's error for the empty value.
#[test]
fn test_authenticator_names_the_missing_flag() {
    for (audience, accounts, want) in [
        ("https://svc.run.app", "", "requires -oidc_service_accounts"),
        ("", "a@p.iam.gserviceaccount.com", "requires -oidc_audience"),
        (
            "https://svc.run.app",
            "a@p.iam.gserviceaccount.com,",
            r#"-oidc_service_accounts "a@p.iam.gserviceaccount.com," has an empty entry"#,
        ),
    ] {
        let got = err_text(&authenticator(audience, accounts, &authn::google_oidc));
        assert!(
            got.as_deref().is_some_and(|e| e.contains(want)),
            "Authenticator() with {:?} = {got:?}, want an error containing {want:?}",
            (audience, accounts)
        );
    }
}

/// `args` parsed by a fresh set with the flags registered, then `Authenticator()`.
fn after_parse(args: &[&str]) -> Result<Option<Arc<dyn Authenticator>>, Error> {
    let mut fs = FlagSet::new("test");
    let f = Flags::register(&mut fs);
    let args: Vec<String> = args.iter().map(|s| (*s).to_owned()).collect();
    if let Err(e) = fs.parse(&args) {
        panic!("Parse({args:?}) = {e}");
    }
    f.authenticator(&fs)
}

#[test]
fn test_authenticator_rejects_a_flag_passed_empty() {
    for (args, want) in [
        (
            &["-oidc_audience=", "-oidc_service_accounts="][..],
            "-oidc_audience is empty",
        ),
        (&["-oidc_audience="][..], "-oidc_audience is empty"),
        (
            &["-oidc_service_accounts="][..],
            "-oidc_service_accounts is empty",
        ),
        (
            &[
                "-oidc_audience=https://svc.run.app",
                "-oidc_service_accounts=",
            ][..],
            "-oidc_service_accounts is empty",
        ),
    ] {
        let got = err_text(&after_parse(args));
        assert!(
            got.as_deref().is_some_and(|e| e.contains(want)),
            "Authenticator() after {args:?} = {got:?}, want an error containing {want:?}"
        );
    }
}

#[test]
fn test_authenticator_nil_when_flags_are_registered_but_not_passed() {
    let a = after_parse(&[]);
    assert!(
        matches!(a, Ok(None)),
        "Authenticator() = {:?}, want nil, nil",
        err_text(&a)
    );
}

// -oidc_audience $AUD -oidc_service_accounts $SA with one variable unset and unquoted: the flag
// package gives that flag the next flag as its value. The error names the flag whose value was
// taken, rather than saying the other flag is missing.
#[test]
fn test_authenticator_rejects_a_value_taken_from_the_next_flag() {
    for (args, want) in [
        (
            [
                "-oidc_audience",
                "-oidc_service_accounts",
                "a@p.iam.gserviceaccount.com",
            ],
            r#"-oidc_audience "-oidc_service_accounts" starts with "-""#,
        ),
        (
            [
                "-oidc_service_accounts",
                "-oidc_audience",
                "https://svc.run.app",
            ],
            r#"-oidc_service_accounts "-oidc_audience" starts with "-""#,
        ),
    ] {
        let got = err_text(&after_parse(&args));
        assert!(
            got.as_deref().is_some_and(|e| e.contains(want)),
            "Authenticator() after {args:?} = {got:?}, want an error containing {want:?}"
        );
    }
}

#[test]
fn test_authenticator_rejects_a_value_starting_with_a_dash() {
    for (audience, accounts, want) in [
        (
            "-x",
            "a@p.iam.gserviceaccount.com",
            r#"-oidc_audience "-x" starts with "-""#,
        ),
        (
            "https://svc.run.app",
            "a@p.iam.gserviceaccount.com, -b",
            r#"-oidc_service_accounts "a@p.iam.gserviceaccount.com, -b" has an entry starting with "-""#,
        ),
    ] {
        let got = err_text(&authenticator(audience, accounts, &authn::google_oidc));
        assert!(
            got.as_deref().is_some_and(|e| e.contains(want)),
            "Authenticator() with {:?} = {got:?}, want an error containing {want:?}",
            (audience, accounts)
        );
    }
}
