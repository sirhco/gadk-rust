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

//! `gadk_platform::go_flag` against Go's `flag` package: ports of the `src/flag/flag_test.go`
//! cases that apply to the supported flag kinds and `ContinueOnError`, plus tables whose
//! expectations were produced by Go 1.27's `flag` (outputs and error texts byte for byte,
//! except that a bad number or bool reads `parsing "<s>" as an integer|a boolean: …`).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::{Arc, Mutex};

use gadk_platform::GoDuration;
use gadk_platform::go_flag::{FlagError, FlagSet, go_parse_bool, go_parse_int};

/// A shared buffer to pass to `set_output`, and a reader of its text.
fn sink(fs: &mut FlagSet) -> Arc<Mutex<Vec<u8>>> {
    let buf = Arc::new(Mutex::new(Vec::new()));
    fs.set_output(buf.clone());
    buf
}

fn text(buf: &Arc<Mutex<Vec<u8>>>) -> String {
    String::from_utf8(buf.lock().unwrap().clone()).unwrap()
}

fn clear(buf: &Arc<Mutex<Vec<u8>>>) {
    buf.lock().unwrap().clear();
}

fn args(a: &[&str]) -> Vec<String> {
    a.iter().map(|s| s.to_string()).collect()
}

/// Go `TestFlagSetParse` (`testParse`); Go's uint, uint64 and float64 flags are not supported.
#[test]
fn test_flag_set_parse() {
    let mut f = FlagSet::new("test");
    sink(&mut f);
    assert!(!f.parsed(), "f.parsed() = true before parse");
    let bool_flag = f.bool("bool", false, "bool value");
    let bool2_flag = f.bool("bool2", false, "bool2 value");
    let int_flag = f.int("int", 0, "int value");
    let int64_flag = f.int64("int64", 0, "int64 value");
    let string_flag = f.string("string", "0", "string value");
    let duration_flag = f.duration("duration", GoDuration::from_secs(5), "time.Duration value");
    let extra = "one-extra-argument";
    f.parse(&args(&[
        "-bool",
        "-bool2=true",
        "--int",
        "22",
        "--int64",
        "0x23",
        "-string",
        "hello",
        "-duration",
        "2m",
        extra,
    ]))
    .unwrap();
    assert!(f.parsed(), "f.parsed() = false after parse");
    assert!(bool_flag.get());
    assert!(bool2_flag.get());
    assert_eq!(int_flag.get(), 22);
    assert_eq!(int64_flag.get(), 0x23);
    assert_eq!(string_flag.get(), "hello");
    assert_eq!(duration_flag.get(), GoDuration::from_secs(120));
    assert_eq!(f.args(), vec![extra.to_string()]);
}

/// Go `TestEverything`, adapted: Go's `Visit`/`VisitAll` over flags set with `fs.Set` become
/// `is_set` over flags set by `parse`; the default `String()` forms show through
/// `print_defaults` (all zero values, so no `(default …)`).
#[test]
fn test_everything() {
    let mut f = FlagSet::new("everything");
    sink(&mut f);
    let names = [
        "test_bool",
        "test_boolfunc",
        "test_duration",
        "test_func",
        "test_int",
        "test_int64",
        "test_string",
    ];
    let b = f.bool("test_bool", false, "bool value");
    let i = f.int("test_int", 0, "int value");
    let i64 = f.int64("test_int64", 0, "int64 value");
    let s = f.string("test_string", "0", "string value");
    let d = f.duration("test_duration", GoDuration(0), "time.Duration value");
    let seen = Arc::new(Mutex::new(Vec::new()));
    let seen_func = seen.clone();
    f.func(
        "test_func",
        "func value",
        Box::new(move |v| {
            seen_func.lock().unwrap().push(format!("func:{v}"));
            Ok(())
        }),
    );
    let seen_bool_func = seen.clone();
    f.bool_func(
        "test_boolfunc",
        "func",
        Box::new(move |v| {
            seen_bool_func.lock().unwrap().push(format!("boolfunc:{v}"));
            Ok(())
        }),
    );
    for name in names {
        assert!(!f.is_set(name), "{name} set before parse");
    }
    assert_eq!(f.print_defaults().matches("(default").count(), 1); // only test_string's "0"
    f.parse(&args(&[
        "-test_bool=true",
        "-test_int=1",
        "-test_int64=1",
        "-test_string=1",
        "-test_duration=1s",
        "-test_func=1",
        "-test_boolfunc",
    ]))
    .unwrap();
    for name in names {
        assert!(f.is_set(name), "{name} not set after parse");
    }
    assert!(!f.is_set("test_missing"));
    assert!(b.get());
    assert_eq!((i.get(), i64.get(), s.get()), (1, 1, "1".to_string()));
    assert_eq!(d.get(), GoDuration::SECOND);
    assert_eq!(*seen.lock().unwrap(), vec!["func:1", "boolfunc:true"]);
}

/// Go `TestUserDefinedFunc`.
#[test]
fn test_user_defined_func() {
    let mut flags = FlagSet::new("test");
    let buf = sink(&mut flags);
    let ss = Arc::new(Mutex::new(Vec::<String>::new()));
    let ss2 = ss.clone();
    flags.func(
        "v",
        "usage",
        Box::new(move |s| {
            ss2.lock().unwrap().push(s.to_string());
            Ok(())
        }),
    );
    flags.parse(&args(&["-v", "1", "-v", "2", "-v=3"])).unwrap();
    assert_eq!(*ss.lock().unwrap(), vec!["1", "2", "3"]);
    // Test usage.
    clear(&buf);
    let err = flags.parse(&args(&["-h"])).unwrap_err();
    assert!(err.is_help());
    assert!(text(&buf).contains("usage"), "usage string not included");
    // Test Func error.
    let mut flags = FlagSet::new("test");
    sink(&mut flags);
    flags.func("v", "usage", Box::new(|_| Err("test error".to_string())));
    // Flag not set, so no error.
    flags.parse(&[]).unwrap();
    // Flag set, expect error.
    let err = flags.parse(&args(&["-v", "1"])).unwrap_err();
    assert!(err.to_string().contains("test error"), "{err}");
    assert_eq!(err.message, r#"invalid value "1" for flag -v: test error"#);
}

/// Go `TestUserDefinedBoolFunc`.
#[test]
fn test_user_defined_bool_func() {
    let mut flags = FlagSet::new("test");
    let buf = sink(&mut flags);
    let ss = Arc::new(Mutex::new(Vec::<String>::new()));
    let ss2 = ss.clone();
    flags.bool_func(
        "v",
        "usage",
        Box::new(move |s| {
            ss2.lock().unwrap().push(s.to_string());
            Ok(())
        }),
    );
    flags.parse(&args(&["-v", "", "-v", "1", "-v=2"])).unwrap();
    assert_eq!(*ss.lock().unwrap(), vec!["true"]);
    // Test usage.
    clear(&buf);
    assert!(flags.parse(&args(&["-h"])).unwrap_err().is_help());
    assert!(text(&buf).contains("usage"), "usage string not included");
    // Test BoolFunc error.
    let mut flags = FlagSet::new("test");
    sink(&mut flags);
    flags.bool_func("v", "usage", Box::new(|_| Err("test error".to_string())));
    flags.parse(&[]).unwrap();
    let err = flags.parse(&args(&["-v", ""])).unwrap_err();
    assert!(err.to_string().contains("test error"), "{err}");
    assert_eq!(err.message, "invalid boolean flag v: test error");
}

/// Go `TestSetOutput`.
#[test]
fn test_set_output() {
    let mut flags = FlagSet::new("test");
    let buf = sink(&mut flags);
    assert!(flags.parse(&args(&["-unknown"])).is_err());
    assert!(text(&buf).contains("-unknown"), "{:?}", text(&buf));
}

/// Go `TestHelp`, adapted: Go installs a custom `Usage` func to observe the call; here the
/// default usage written to the output is the observation.
#[test]
fn test_help() {
    let mut fs = FlagSet::new("help test");
    let buf = sink(&mut fs);
    let flag = fs.bool("flag", false, "regular flag");
    // Regular flag invocation should work.
    fs.parse(&args(&["-flag=true"])).unwrap();
    assert!(flag.get(), "flag was not set by -flag");
    assert_eq!(text(&buf), "", "help called for regular flag");
    // Help flag should work as expected.
    let err = fs.parse(&args(&["-help"])).unwrap_err();
    assert!(err.is_help(), "expected ErrHelp; got {err}");
    assert_eq!(err.to_string(), "flag: help requested");
    assert_eq!(
        text(&buf),
        "Usage of help test:\n  -flag\n    \tregular flag\n"
    );
    // If we define a help flag, that should override.
    let help = fs.bool("help", false, "help flag");
    clear(&buf);
    fs.parse(&args(&["-help"])).unwrap();
    assert!(help.get());
    assert_eq!(text(&buf), "", "help was called for a defined help flag");
}

/// Go `TestPrintDefaults`, adapted: the float64 (`-F`, `-G`), `Var` (`-V`) and panicking-`String`
/// (`-ZP0`, `-ZP1`) rows are dropped (unsupported kinds); every other line is Go's.
#[test]
fn test_print_defaults() {
    let mut fs = FlagSet::new("print defaults test");
    let buf = sink(&mut fs);
    fs.bool("A", false, "for bootstrapping, allow 'any' type");
    fs.bool("Alongflagname", false, "disable bounds checking");
    fs.bool("C", true, "a boolean defaulting to true");
    fs.string("D", "", "set relative `path` for local imports");
    fs.string("E", "0", "issue 23543");
    fs.string("M", "", "a multiline\nhelp\nstring");
    fs.int("N", 27, "a non-zero int");
    fs.bool("O", true, "a flag\nmultiline help string");
    fs.int("Z", 0, "an int that defaults to zero");
    fs.duration("maxT", GoDuration(0), "set `timeout` for dial");
    let want = "  -A\tfor bootstrapping, allow 'any' type
  -Alongflagname
    \tdisable bounds checking
  -C\ta boolean defaulting to true (default true)
  -D path
    \tset relative path for local imports
  -E string
    \tissue 23543 (default \"0\")
  -M string
    \ta multiline
    \thelp
    \tstring
  -N int
    \ta non-zero int (default 27)
  -O\ta flag
    \tmultiline help string (default true)
  -Z int
    \tan int that defaults to zero
  -maxT timeout
    \tset timeout for dial
";
    assert_eq!(fs.print_defaults(), want);
    assert_eq!(text(&buf), "", "print_defaults returns, it does not write");
}

/// `PrintDefaults` for the remaining kinds and layouts (func, bool func, int64, durations,
/// `%q` of a string default, a single back quote, an empty usage, a non-ASCII one-letter
/// name); `want` is Go's output for the same definitions.
#[test]
fn print_defaults_matches_go_for_every_supported_kind() {
    let mut fs = FlagSet::new("extra");
    fs.func(
        "allow_origins",
        "Optional. Origins (e.g., 'https://example.com'). Repeat the flag.",
        Box::new(|_| Ok(())),
    );
    fs.bool_func("v", "verbose `level` toggle", Box::new(|_| Ok(())));
    fs.bool_func("bf", "a bool func", Box::new(|_| Ok(())));
    fs.int64("I64", -5, "an int64");
    fs.int64("z64", 0, "zero int64");
    fs.duration(
        "write-timeout",
        GoDuration::from_secs(15),
        "Server write timeout (i.e. '10s', '2m')",
    );
    fs.duration("d0", GoDuration(0), "zero");
    fs.string("q", "a\"b\n\té", "quoted");
    fs.string("u", "x", "only one ` back quote");
    fs.int("i", 0, "`n`");
    fs.bool("é", false, "non-ASCII one-letter bool");
    fs.bool("x", false, "");
    let want = "  -I64 int\n    \tan int64 (default -5)\n  -allow_origins value\n    \tOptional. Origins (e.g., 'https://example.com'). Repeat the flag.\n  -bf\n    \ta bool func\n  -d0 duration\n    \tzero\n  -i n\n    \tn\n  -q string\n    \tquoted (default \"a\\\"b\\n\\té\")\n  -u string\n    \tonly one ` back quote (default \"x\")\n  -v level\n    \tverbose level toggle\n  -write-timeout duration\n    \tServer write timeout (i.e. '10s', '2m') (default 15s)\n  -x\t\n  -z64 int\n    \tzero int64\n  -é\n    \tnon-ASCII one-letter bool\n";
    assert_eq!(fs.print_defaults(), want);
}

/// Go `TestUsageOutput`, adapted to a named `FlagSet` (Go uses `CommandLine` named `os.Args[0]`).
#[test]
fn test_usage_output() {
    let mut fs = FlagSet::new("app");
    let buf = sink(&mut fs);
    let err = fs.parse(&args(&["-i=1", "-unknown"])).unwrap_err();
    assert_eq!(err.message, "flag provided but not defined: -i");
    assert_eq!(
        text(&buf),
        "flag provided but not defined: -i\nUsage of app:\n"
    );
}

/// Go `TestGetters` (the name; `ContinueOnError` is the only mode, and the default output is
/// stderr by construction).
#[test]
fn test_getters() {
    assert_eq!(FlagSet::new("flag set").name(), "flag set");
}

/// Go `TestParseError` for the supported kinds (`bool`, `int`, `int64`, `duration`).
#[test]
fn test_parse_error() {
    for typ in ["bool", "int", "int64", "duration"] {
        let mut fs = FlagSet::new("parse error test");
        sink(&mut fs);
        fs.bool("bool", false, "");
        fs.int("int", 0, "");
        fs.int64("int64", 0, "");
        fs.duration("duration", GoDuration(0), "");
        let a = args(&[&format!("-{typ}=x")]);
        let err = fs.parse(&a).unwrap_err().to_string();
        assert!(
            err.contains("invalid") && err.contains("parse error"),
            "parse({a:?}) = {err}"
        );
    }
}

/// Go `TestRangeError` for the supported kinds (`int`, `int64`).
#[test]
fn test_range_error() {
    for arg in ["-int=123456789012345678901", "-int64=123456789012345678901"] {
        let mut fs = FlagSet::new("parse error test");
        sink(&mut fs);
        fs.int("int", 0, "");
        fs.int64("int64", 0, "");
        let err = fs.parse(&args(&[arg])).unwrap_err().to_string();
        assert!(
            err.contains("invalid") && err.contains("value out of range"),
            "parse({arg:?}) = {err}"
        );
    }
}

/// Go `TestInvalidFlags`, adapted: Go writes the message and panics; gadk writes the same
/// message and ignores the definition.
#[test]
fn test_invalid_flags() {
    for (flag, msg) in [
        ("-foo", "flag \"-foo\" begins with -"),
        ("foo=bar", "flag \"foo=bar\" contains ="),
    ] {
        let mut fs = FlagSet::new("");
        let buf = sink(&mut fs);
        fs.string(flag, "", "");
        assert_eq!(text(&buf), format!("{msg}\n"));
        assert_eq!(fs.print_defaults(), "");
    }
}

/// Go `TestRedefinedFlags`, adapted as `test_invalid_flags`; the first definition is kept.
#[test]
fn test_redefined_flags() {
    for (name, msg) in [
        ("", "flag redefined: foo"),
        ("fs", "fs flag redefined: foo"),
    ] {
        let mut fs = FlagSet::new(name);
        let buf = sink(&mut fs);
        let first = fs.string("foo", "1", "");
        let second = fs.string("foo", "2", "");
        assert_eq!(text(&buf), format!("{msg}\n"));
        fs.parse(&args(&["-foo", "x"])).unwrap();
        assert_eq!((first.get().as_str(), second.get().as_str()), ("x", "2"));
    }
}

/// The set the error table runs against, with its `PrintDefaults` as Go prints it.
fn error_set(name: &str) -> (FlagSet, Arc<Mutex<Vec<u8>>>) {
    let mut fs = FlagSet::new(name);
    let buf = sink(&mut fs);
    fs.bool("b", false, "a bool");
    fs.int("i", 7, "an int");
    fs.duration("d", GoDuration::from_secs(2), "a duration");
    fs.string("s", "dflt", "a string");
    fs.func(
        "f",
        "a func",
        Box::new(|s| {
            if s == "bad" {
                Err("bad origin".to_string())
            } else {
                Ok(())
            }
        }),
    );
    fs.bool_func("bf", "a bool func", Box::new(|s| Err(format!("no {s}"))));
    (fs, buf)
}

const ERROR_SET_DEFAULTS: &str = "  -b\ta bool\n  -bf\n    \ta bool func\n  -d duration\n    \ta duration (default 2s)\n  -f value\n    \ta func\n  -i int\n    \tan int (default 7)\n  -s string\n    \ta string (default \"dflt\")\n";

/// Every `parseOne` failure: the returned error and the output (`failf`: the error line, the
/// usage header — `Usage:` for an unnamed set — and the defaults; `-h`/`-help`: the usage
/// only). Expectations are Go's for the same set and arguments.
#[test]
fn parse_errors_and_usage_output_match_go() {
    let cases: &[(&[&str], &str)] = &[
        (&["-x"], "flag provided but not defined: -x"),
        (&["--x=1"], "flag provided but not defined: -x"),
        (&["---x"], "bad flag syntax: ---x"),
        (&["-=x"], "bad flag syntax: -=x"),
        (&["--=x"], "bad flag syntax: --=x"),
        (
            &["-b=maybe"],
            "invalid boolean value \"maybe\" for -b: parse error",
        ),
        (
            &["-b=", "rest"],
            "invalid boolean value \"\" for -b: parse error",
        ),
        (&["-i"], "flag needs an argument: -i"),
        (
            &["-i", "abc"],
            "invalid value \"abc\" for flag -i: parse error",
        ),
        (
            &["-i=99999999999999999999"],
            "invalid value \"99999999999999999999\" for flag -i: value out of range",
        ),
        (
            &["-i=-99999999999999999999"],
            "invalid value \"-99999999999999999999\" for flag -i: value out of range",
        ),
        (&["-d=5"], "invalid value \"5\" for flag -d: parse error"),
        (
            &["-d", "1d"],
            "invalid value \"1d\" for flag -d: parse error",
        ),
        (&["-help"], "flag: help requested"),
        (&["-h"], "flag: help requested"),
        (&["--help"], "flag: help requested"),
        (&["-f=bad"], "invalid value \"bad\" for flag -f: bad origin"),
        (
            &["-f", "bad"],
            "invalid value \"bad\" for flag -f: bad origin",
        ),
        (&["-bf"], "invalid boolean flag bf: no true"),
        (&["-bf=x"], "invalid boolean value \"x\" for -bf: no x"),
        (&["-s"], "flag needs an argument: -s"),
        (&["-f"], "flag needs an argument: -f"),
    ];
    for (name, header) in [("web", "Usage of web:\n"), ("", "Usage:\n")] {
        for (input, want_err) in cases {
            let (mut fs, buf) = error_set(name);
            let err: FlagError = fs.parse(&args(input)).unwrap_err();
            assert_eq!(err.message, *want_err, "{name:?} {input:?}");
            assert_eq!(err.to_string(), *want_err);
            let usage = format!("{header}{ERROR_SET_DEFAULTS}");
            let want_out = if err.is_help() {
                usage
            } else {
                format!("{want_err}\n{usage}")
            };
            assert_eq!(text(&buf), want_out, "{name:?} {input:?}");
        }
    }
}

/// `strconv.ParseInt(s, 0, 64)` through an int64 flag: base prefixes, underscores, signs,
/// range clamping (Go stores the clamped value even though `Parse` fails) and syntax errors.
/// Each row is (input, Go's value after parse, Go's error or "").
#[test]
fn int_flag_values_match_go_strconv() {
    let cases: &[(&str, i64, &str)] = &[
        ("0x23", 35, ""),
        ("0X23", 35, ""),
        ("0o17", 15, ""),
        ("017", 15, ""),
        ("0b101", 5, ""),
        ("1_000", 1000, ""),
        ("_1", 0, "parse error"),
        ("1__0", 0, "parse error"),
        ("0x_1F", 31, ""),
        ("0_7", 7, ""),
        ("1_", 0, "parse error"),
        ("-9223372036854775808", i64::MIN, ""),
        ("9223372036854775807", i64::MAX, ""),
        ("9223372036854775808", i64::MAX, "value out of range"),
        ("-9223372036854775809", i64::MIN, "value out of range"),
        ("18446744073709551616", i64::MAX, "value out of range"),
        ("+5", 5, ""),
        ("-0x10", -16, ""),
        ("0x", 0, "parse error"),
        ("0b", 0, "parse error"),
        ("08", 0, "parse error"),
        ("1e3", 0, "parse error"),
        ("", 0, "parse error"),
        ("-", 0, "parse error"),
        ("+", 0, "parse error"),
        ("0", 0, ""),
        ("00", 0, ""),
        ("0b2", 0, "parse error"),
        ("0xg", 0, "parse error"),
        (" 1", 0, "parse error"),
        ("１", 0, "parse error"),
    ];
    for (input, want, want_err) in cases {
        let mut fs = FlagSet::new("n");
        sink(&mut fs);
        let v = fs.int64("v", 99, "");
        let got_err = match fs.parse(&args(&[&format!("-v={input}")])) {
            Ok(()) => String::new(),
            Err(e) => e.message,
        };
        let want_err = if want_err.is_empty() {
            String::new()
        } else {
            format!(
                "invalid value {} for flag -v: {want_err}",
                gadk_platform::go_quote(input)
            )
        };
        assert_eq!((v.get(), got_err), (*want, want_err), "{input:?}");
    }
}

/// `go_parse_int` accepts Go `strconv.ParseInt(s, 0, 64)` syntax; failures read
/// `parsing "<s>" as an integer: <reason>` (the cause int flags print).
#[test]
fn go_parse_int_returns_parse_errors() {
    let cases: &[(&str, Result<i64, &str>)] = &[
        ("0x10", Ok(16)),
        ("010", Ok(8)),
        ("0b1", Ok(1)),
        ("1_000", Ok(1000)),
        ("-0x10", Ok(-16)),
        ("8080", Ok(8080)),
        ("x", Err(r#"parsing "x" as an integer: invalid syntax"#)),
        ("", Err(r#"parsing "" as an integer: invalid syntax"#)),
        ("1_", Err(r#"parsing "1_" as an integer: invalid syntax"#)),
        (
            "9223372036854775808",
            Err(r#"parsing "9223372036854775808" as an integer: value out of range"#),
        ),
        (
            "-9223372036854775809",
            Err(r#"parsing "-9223372036854775809" as an integer: value out of range"#),
        ),
    ];
    for (input, want) in cases {
        assert_eq!(
            go_parse_int(input),
            want.map_err(str::to_string),
            "{input:?}"
        );
    }
}

/// `go_parse_bool` accepts Go `strconv.ParseBool` spellings; failures read
/// `parsing "<s>" as a boolean: invalid syntax`.
#[test]
fn go_parse_bool_returns_parse_errors() {
    for s in ["1", "t", "T", "true", "TRUE", "True"] {
        assert_eq!(go_parse_bool(s), Ok(true), "{s:?}");
    }
    for s in ["0", "f", "F", "false", "FALSE", "False"] {
        assert_eq!(go_parse_bool(s), Ok(false), "{s:?}");
    }
    assert_eq!(
        go_parse_bool("yes"),
        Err(r#"parsing "yes" as a boolean: invalid syntax"#.to_string())
    );
    assert_eq!(
        go_parse_bool(""),
        Err(r#"parsing "" as a boolean: invalid syntax"#.to_string())
    );
}

/// `strconv.ParseBool` through `-v=x` on a bool flag defaulting to true (a failed parse
/// stores false, as Go does).
#[test]
fn bool_flag_values_match_go_strconv() {
    let cases: &[(&str, bool, bool)] = &[
        ("1", true, true),
        ("t", true, true),
        ("T", true, true),
        ("true", true, true),
        ("TRUE", true, true),
        ("True", true, true),
        ("0", false, true),
        ("f", false, true),
        ("F", false, true),
        ("false", false, true),
        ("FALSE", false, true),
        ("False", false, true),
        ("yes", false, false),
        ("tRUE", false, false),
        ("", false, false),
    ];
    for (input, want, ok) in cases {
        let mut fs = FlagSet::new("n");
        sink(&mut fs);
        let v = fs.bool("v", true, "");
        let res = fs.parse(&args(&[&format!("-v={input}")]));
        assert_eq!(v.get(), *want, "{input:?}");
        match res {
            Ok(()) => assert!(ok, "{input:?} parsed"),
            Err(e) => {
                assert!(!ok, "{input:?}: {e}");
                assert_eq!(
                    e.message,
                    format!(
                        "invalid boolean value {} for -v: parse error",
                        gadk_platform::go_quote(input)
                    )
                );
            }
        }
    }
}

/// Go `parseOne`'s argument rules: `--` ends the flags (and is consumed), parsing stops at
/// the first non-flag (a lone `-` is one), a bool flag never takes the next argument, a
/// non-bool flag takes the next argument even when it looks like a flag, and `--name` equals
/// `-name`.
#[test]
fn parse_stops_and_terminates_like_go() {
    let rows: &[(&[&str], &[&str])] = &[
        (&["-b", "--", "-s", "x"], &["-s", "x"]),
        (&["--", "--"], &["--"]),
        (&["-b", "pos", "-s", "x"], &["pos", "-s", "x"]),
        (&["-", "-b"], &["-", "-b"]),
        (&["-b", "false"], &["false"]),
        (&[], &[]),
    ];
    for (input, rest) in rows {
        let mut fs = FlagSet::new("t");
        sink(&mut fs);
        fs.bool("b", false, "");
        fs.string("s", "", "");
        fs.parse(&args(input)).unwrap();
        assert_eq!(fs.args(), args(rest), "{input:?}");
    }

    let mut fs = FlagSet::new("t");
    sink(&mut fs);
    let s = fs.string("s", "", "");
    let n = fs.int("n", 0, "");
    fs.parse(&args(&["-s", "-n", "--n=4"])).unwrap();
    assert_eq!((s.get().as_str(), n.get()), ("-n", 4));
    assert!(fs.is_set("s") && fs.is_set("n"));
    assert!(fs.args().is_empty());

    // `-s=` sets the empty string; a value may contain '='.
    let mut fs = FlagSet::new("t");
    sink(&mut fs);
    let s = fs.string("s", "dflt", "");
    fs.parse(&args(&["-s="])).unwrap();
    assert_eq!(s.get(), "");
    fs.parse(&args(&["--s=a=b"])).unwrap();
    assert_eq!(s.get(), "a=b");
}

/// A flag that fails to parse leaves the arguments after it unparsed and is not marked set;
/// flags before it are.
#[test]
fn a_failed_parse_reports_the_flags_set_before_it() {
    let mut fs = FlagSet::new("t");
    sink(&mut fs);
    let a = fs.int("a", 0, "");
    let b = fs.int("b", 0, "");
    let c = fs.int("c", 0, "");
    assert!(fs.parse(&args(&["-a=1", "-b=x", "-c=3"])).is_err());
    assert!(fs.parsed());
    assert_eq!((a.get(), b.get(), c.get()), (1, 0, 0));
    assert!(fs.is_set("a") && !fs.is_set("b") && !fs.is_set("c"));
    assert_eq!(fs.args(), args(&["-c=3"]));
}
