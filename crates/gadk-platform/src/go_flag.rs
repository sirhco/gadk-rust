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

//! The parts of Go's `flag` package (`src/flag/flag.go`) the ADK-Go launcher uses: a
//! [`FlagSet`] with `ContinueOnError` handling and string, int, int64, bool, duration, func and
//! bool-func flags. Parsing (`parseOne`), the error texts, the usage message written on failure
//! and `PrintDefaults` reproduce Go byte for byte.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::io::Write;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use crate::{GoDuration, go_format_duration, go_parse_duration, go_quote};

/// Go `flag.ErrHelp`'s text.
const ERR_HELP: &str = "flag: help requested";
/// Go `flag.errParse`'s text.
const ERR_PARSE: &str = "parse error";
/// Go `flag.errRange`'s text.
const ERR_RANGE: &str = "value out of range";

/// A registered flag's current value, readable after `parse` (cheap clone; shared with the set).
#[derive(Clone)]
pub struct Flag<T> {
    value: Arc<Mutex<T>>,
}

impl<T: Clone> Flag<T> {
    /// The flag's current value: its default until `parse` sets it.
    pub fn get(&self) -> T {
        lock(&self.value).clone()
    }
}

impl<T: Clone + fmt::Debug> fmt::Debug for Flag<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Flag").field(&self.get()).finish()
    }
}

impl<T> Flag<T> {
    fn new(value: T) -> Flag<T> {
        Flag {
            value: Arc::new(Mutex::new(value)),
        }
    }

    fn set(&self, value: T) {
        *lock(&self.value) = value;
    }
}

/// A parse failure; `Display` is Go's text (the error `fs.Parse` returns).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlagError {
    pub message: String,
}

impl FlagError {
    /// Go `flag.ErrHelp` (`flag: help requested`).
    pub fn is_help(&self) -> bool {
        self.message == ERR_HELP
    }
}

impl fmt::Display for FlagError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for FlagError {}

/// A func flag's callback (Go `fs.Func`): `Err(text)` becomes `invalid value "v" for flag -name: text`.
pub type FlagFunc = Box<dyn FnMut(&str) -> Result<(), String> + Send>;

/// The value behind a flag: Go's `boolValue`, `intValue`, `int64Value`, `stringValue`,
/// `durationValue`, `funcValue` and `boolFuncValue`.
enum Value {
    Bool(Flag<bool>),
    Int(Flag<i64>),
    Int64(Flag<i64>),
    String(Flag<String>),
    Duration(Flag<GoDuration>),
    Func(FlagFunc),
    BoolFunc(FlagFunc),
}

impl Value {
    /// Go `Value.Set`; `Err` is the error's text.
    fn set(&mut self, s: &str) -> Result<(), String> {
        match self {
            Value::Bool(v) => {
                let parsed = parse_bool(s);
                v.set(parsed.unwrap_or(false));
                parsed.map(|_| ()).ok_or_else(|| ERR_PARSE.to_string())
            }
            Value::Int(v) | Value::Int64(v) => match parse_int(s) {
                Ok(n) => {
                    v.set(n);
                    Ok(())
                }
                Err(IntError::Syntax) => {
                    v.set(0);
                    Err(ERR_PARSE.to_string())
                }
                Err(IntError::Range(n)) => {
                    v.set(n);
                    Err(ERR_RANGE.to_string())
                }
            },
            Value::String(v) => {
                v.set(s.to_string());
                Ok(())
            }
            Value::Duration(v) => {
                let parsed = go_parse_duration(s);
                v.set(parsed.clone().unwrap_or_default());
                parsed.map(|_| ()).map_err(|_| ERR_PARSE.to_string())
            }
            Value::Func(f) | Value::BoolFunc(f) => f(s),
        }
    }

    /// Go `Value.String()`.
    fn string(&self) -> String {
        match self {
            Value::Bool(v) => v.get().to_string(),
            Value::Int(v) | Value::Int64(v) => v.get().to_string(),
            Value::String(v) => v.get(),
            Value::Duration(v) => go_format_duration(v.get()),
            Value::Func(_) | Value::BoolFunc(_) => String::new(),
        }
    }

    /// `String()` of the type's zero value (Go `isZeroValue`).
    fn zero_string(&self) -> &'static str {
        match self {
            Value::Bool(_) => "false",
            Value::Int(_) | Value::Int64(_) => "0",
            Value::Duration(_) => "0s",
            Value::String(_) | Value::Func(_) | Value::BoolFunc(_) => "",
        }
    }

    /// Go `boolFlag.IsBoolFlag()`.
    fn is_bool_flag(&self) -> bool {
        matches!(self, Value::Bool(_) | Value::BoolFunc(_))
    }

    /// The placeholder `UnquoteUsage` derives from the value's type.
    fn type_name(&self) -> &'static str {
        match self {
            Value::Bool(_) | Value::BoolFunc(_) => "",
            Value::Duration(_) => "duration",
            Value::Int(_) | Value::Int64(_) => "int",
            Value::String(_) => "string",
            Value::Func(_) => "value",
        }
    }
}

/// Go `flag.Flag`.
struct FlagDef {
    name: String,
    usage: String,
    value: Value,
    def_value: String,
}

/// Go `flag.FlagSet` with `ContinueOnError` (the only mode the launcher uses).
pub struct FlagSet {
    name: String,
    parsed: bool,
    actual: BTreeSet<String>,
    formal: BTreeMap<String, FlagDef>,
    args: Vec<String>,
    output: Arc<Mutex<dyn Write + Send>>,
}

impl fmt::Debug for FlagSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FlagSet")
            .field("name", &self.name)
            .field("parsed", &self.parsed)
            .field("flags", &self.formal.keys().collect::<Vec<_>>())
            .field("args", &self.args)
            .finish()
    }
}

impl FlagSet {
    /// Go `flag.NewFlagSet(name, flag.ContinueOnError)`; output goes to stderr.
    pub fn new(name: &str) -> FlagSet {
        FlagSet {
            name: name.to_string(),
            parsed: false,
            actual: BTreeSet::new(),
            formal: BTreeMap::new(),
            args: Vec::new(),
            output: Arc::new(Mutex::new(std::io::stderr())),
        }
    }

    /// Go `fs.Name()`.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Go `fs.String`.
    pub fn string(&mut self, name: &str, value: &str, usage: &str) -> Flag<String> {
        let flag = Flag::new(value.to_string());
        self.var(Value::String(flag.clone()), name, usage);
        flag
    }

    /// Go `fs.Int` (Go's `int` is 64 bits on every platform gadk supports).
    pub fn int(&mut self, name: &str, value: i64, usage: &str) -> Flag<i64> {
        let flag = Flag::new(value);
        self.var(Value::Int(flag.clone()), name, usage);
        flag
    }

    /// Go `fs.Int64`.
    pub fn int64(&mut self, name: &str, value: i64, usage: &str) -> Flag<i64> {
        let flag = Flag::new(value);
        self.var(Value::Int64(flag.clone()), name, usage);
        flag
    }

    /// Go `fs.Bool`.
    pub fn bool(&mut self, name: &str, value: bool, usage: &str) -> Flag<bool> {
        let flag = Flag::new(value);
        self.var(Value::Bool(flag.clone()), name, usage);
        flag
    }

    /// Go `fs.Duration`.
    pub fn duration(&mut self, name: &str, value: GoDuration, usage: &str) -> Flag<GoDuration> {
        let flag = Flag::new(value);
        self.var(Value::Duration(flag.clone()), name, usage);
        flag
    }

    /// Go `fs.Func`: `f` is called with the value each time the flag is seen.
    pub fn func(&mut self, name: &str, usage: &str, f: FlagFunc) {
        self.var(Value::Func(f), name, usage);
    }

    /// Go `fs.BoolFunc`: like [`FlagSet::func`], but the flag takes no value (`-name` calls `f`
    /// with `"true"`; `-name=v` with `v`).
    pub fn bool_func(&mut self, name: &str, usage: &str, f: FlagFunc) {
        self.var(Value::BoolFunc(f), name, usage);
    }

    /// Go `fs.Var`. Go panics on a name that begins with `-`, contains `=` or is already
    /// defined, after writing the message to the output; gadk writes the same message and
    /// ignores the definition (the returned handle then keeps its default).
    fn var(&mut self, value: Value, name: &str, usage: &str) {
        let problem = if name.starts_with('-') {
            Some(format!("flag {} begins with -", go_quote(name)))
        } else if name.contains('=') {
            Some(format!("flag {} contains =", go_quote(name)))
        } else if self.formal.contains_key(name) {
            Some(if self.name.is_empty() {
                format!("flag redefined: {name}")
            } else {
                format!("{} flag redefined: {name}", self.name)
            })
        } else {
            None
        };
        if let Some(msg) = problem {
            self.write(&format!("{msg}\n"));
            return;
        }
        let def_value = value.string();
        self.formal.insert(
            name.to_string(),
            FlagDef {
                name: name.to_string(),
                usage: usage.to_string(),
                value,
                def_value,
            },
        );
    }

    /// Go `fs.Parse(args)` with `ContinueOnError`: on failure writes the error line, then
    /// `Usage of <name>:` and `print_defaults()` to the output sink, and returns the error;
    /// `-h`/`-help` writes the usage and returns `flag: help requested`.
    pub fn parse(&mut self, args: &[String]) -> Result<(), FlagError> {
        self.parsed = true;
        self.args = args.to_vec();
        while self.parse_one()? {}
        Ok(())
    }

    /// Go `parseOne`: parses one flag and reports whether a flag was seen.
    fn parse_one(&mut self) -> Result<bool, FlagError> {
        let Some(s) = self.args.first().cloned() else {
            return Ok(false);
        };
        let bytes = s.as_bytes();
        if bytes.len() < 2 || bytes[0] != b'-' {
            return Ok(false);
        }
        let mut num_minuses = 1;
        if bytes[1] == b'-' {
            num_minuses += 1;
            if bytes.len() == 2 {
                // "--" terminates the flags.
                self.args.remove(0);
                return Ok(false);
            }
        }
        let mut name = &s[num_minuses..];
        if name.is_empty() || name.starts_with('-') || name.starts_with('=') {
            return Err(self.failf(format!("bad flag syntax: {s}")));
        }

        // It's a flag. Does it have an argument? ('=' cannot be first.)
        self.args.remove(0);
        let mut value: Option<String> = None;
        if let Some(i) = name.bytes().skip(1).position(|c| c == b'=') {
            let eq = i + 1;
            value = Some(name[eq + 1..].to_string());
            name = &name[..eq];
        }
        let name = name.to_string();

        let Some(mut flag) = self.formal.remove(&name) else {
            if name == "help" || name == "h" {
                // Special case for a nice help message.
                self.usage();
                return Err(FlagError {
                    message: ERR_HELP.to_string(),
                });
            }
            return Err(self.failf(format!("flag provided but not defined: -{name}")));
        };
        let result = if flag.value.is_bool_flag() {
            // Special case: doesn't need an argument.
            match value {
                Some(value) => flag.value.set(&value).map_err(|err| {
                    format!(
                        "invalid boolean value {} for -{name}: {err}",
                        go_quote(&value)
                    )
                }),
                None => flag
                    .value
                    .set("true")
                    .map_err(|err| format!("invalid boolean flag {name}: {err}")),
            }
        } else {
            // It must have a value, which might be the next argument.
            if value.is_none() && !self.args.is_empty() {
                value = Some(self.args.remove(0));
            }
            match value {
                None => Err(format!("flag needs an argument: -{name}")),
                Some(value) => flag.value.set(&value).map_err(|err| {
                    format!("invalid value {} for flag -{name}: {err}", go_quote(&value))
                }),
            }
        };
        self.formal.insert(name.clone(), flag);
        match result {
            Ok(()) => {
                self.actual.insert(name);
                Ok(true)
            }
            Err(msg) => Err(self.failf(msg)),
        }
    }

    /// Go `fs.Parsed()`.
    pub fn parsed(&self) -> bool {
        self.parsed
    }

    /// Go `fs.Args()`: the arguments after the flags.
    pub fn args(&self) -> Vec<String> {
        self.args.clone()
    }

    /// Go `fs.Visit` membership: whether the flag was set on the command line.
    pub fn is_set(&self, name: &str) -> bool {
        self.actual.contains(name)
    }

    /// Go `fs.SetOutput`; default stderr.
    pub fn set_output(&mut self, out: Arc<Mutex<dyn Write + Send>>) {
        self.output = out;
    }

    /// Go `fs.PrintDefaults()` captured as a string: one entry per flag in name order,
    /// `  -name placeholder` (the placeholder from a back-quoted word in the usage, else the
    /// value type; none for a bool), the usage on the same line after a tab for a one-letter
    /// bool flag and on the next line after four spaces and a tab otherwise, and
    /// ` (default v)` (`%q` for strings) when the default is not the type's zero value.
    pub fn print_defaults(&self) -> String {
        let mut out = String::new();
        for flag in self.formal.values() {
            let mut b = format!("  -{}", flag.name);
            let (name, usage) = unquote_usage(flag);
            if !name.is_empty() {
                b.push(' ');
                b.push_str(&name);
            }
            // Boolean flags of one ASCII letter are so common Go puts their usage on the same
            // line.
            if b.len() <= 4 {
                b.push('\t');
            } else {
                b.push_str("\n    \t");
            }
            b.push_str(&usage.replace('\n', "\n    \t"));
            if flag.def_value != flag.value.zero_string() {
                if matches!(flag.value, Value::String(_)) {
                    b.push_str(&format!(" (default {})", go_quote(&flag.def_value)));
                } else {
                    b.push_str(&format!(" (default {})", flag.def_value));
                }
            }
            out.push_str(&b);
            out.push('\n');
        }
        out
    }

    /// Go `failf`: writes the message and the usage, and returns the message as the error.
    fn failf(&self, message: String) -> FlagError {
        self.write(&format!("{message}\n"));
        self.usage();
        FlagError { message }
    }

    /// Go `defaultUsage`.
    fn usage(&self) {
        let header = if self.name.is_empty() {
            "Usage:\n".to_string()
        } else {
            format!("Usage of {}:\n", self.name)
        };
        self.write(&header);
        self.write(&self.print_defaults());
    }

    /// Writes to the output sink; like Go's `fmt.Fprint`, write errors are ignored.
    fn write(&self, s: &str) {
        let mut out = lock(&self.output);
        let _ = out.write_all(s.as_bytes());
        let _ = out.flush();
    }
}

/// Go `flag.UnquoteUsage`: the first back-quoted word of the usage as the placeholder (the
/// back quotes removed from the usage), else the value type's name.
fn unquote_usage(flag: &FlagDef) -> (String, String) {
    let usage = &flag.usage;
    // With only one back quote, the type name is used.
    if let Some(i) = usage.find('`')
        && let Some(len) = usage[i + 1..].find('`')
    {
        let j = i + 1 + len;
        let name = &usage[i + 1..j];
        return (
            name.to_string(),
            format!("{}{name}{}", &usage[..i], &usage[j + 1..]),
        );
    }
    (flag.value.type_name().to_string(), usage.clone())
}

/// Go `strconv.ParseBool`.
fn parse_bool(s: &str) -> Option<bool> {
    match s {
        "1" | "t" | "T" | "true" | "TRUE" | "True" => Some(true),
        "0" | "f" | "F" | "false" | "FALSE" | "False" => Some(false),
        _ => None,
    }
}

/// A `strconv.ParseInt` failure as `flag`'s `numError` sees it.
enum IntError {
    /// `strconv.ErrSyntax` (flag's `parse error`).
    Syntax,
    /// `strconv.ErrRange` (flag's `value out of range`), with the clamped value Go stores.
    Range(i64),
}

/// Go `strconv.ParseInt(s, 0, 64)`.
fn parse_int(s: &str) -> Result<i64, IntError> {
    let (neg, digits) = match s.as_bytes().first() {
        None => return Err(IntError::Syntax),
        Some(b'+') => (false, &s[1..]),
        Some(b'-') => (true, &s[1..]),
        Some(_) => (false, s),
    };
    let un = parse_uint(digits)?;
    const CUTOFF: u64 = 1 << 63;
    match un {
        None if neg => Err(IntError::Range(i64::MIN)),
        None => Err(IntError::Range(i64::MAX)),
        Some(un) if !neg && un >= CUTOFF => Err(IntError::Range(i64::MAX)),
        Some(un) if neg && un > CUTOFF => Err(IntError::Range(i64::MIN)),
        Some(un) if neg => Ok(0i64.wrapping_sub_unsigned(un)),
        // un < 2^63 here.
        Some(un) => i64::try_from(un).map_err(|_| IntError::Range(i64::MAX)),
    }
}

/// Go `strconv.ParseUint(s, 0, 64)`: `Ok(None)` is `ErrRange` (the value overflows `u64`).
fn parse_uint(s0: &str) -> Result<Option<u64>, IntError> {
    let b = s0.as_bytes();
    if b.is_empty() {
        return Err(IntError::Syntax);
    }
    let lower = |c: u8| c | (b'x' - b'X');
    // Look for an octal, hex or binary prefix.
    let (base, digits): (u64, &[u8]) = if b[0] == b'0' {
        match b.get(1).map(|&c| lower(c)) {
            Some(b'b') if b.len() >= 3 => (2, &b[2..]),
            Some(b'o') if b.len() >= 3 => (8, &b[2..]),
            Some(b'x') if b.len() >= 3 => (16, &b[2..]),
            _ => (8, &b[1..]),
        }
    } else {
        (10, b)
    };
    let cutoff = u64::MAX / base + 1;
    let mut underscores = false;
    let mut n: u64 = 0;
    for &c in digits {
        let d = match c {
            b'_' => {
                underscores = true;
                continue;
            }
            b'0'..=b'9' => c - b'0',
            _ if lower(c).is_ascii_lowercase() => lower(c) - b'a' + 10,
            _ => return Err(IntError::Syntax),
        };
        if u64::from(d) >= base {
            return Err(IntError::Syntax);
        }
        if n >= cutoff {
            // n * base overflows.
            return Ok(None);
        }
        n *= base;
        match n.checked_add(u64::from(d)) {
            Some(n1) => n = n1,
            None => return Ok(None),
        }
    }
    if underscores && !underscore_ok(s0) {
        return Err(IntError::Syntax);
    }
    Ok(Some(n))
}

/// Go `strconv.underscoreOK`: underscores only between digits (or after the base prefix).
fn underscore_ok(s: &str) -> bool {
    let mut s = s.as_bytes();
    let lower = |c: u8| c | (b'x' - b'X');
    // saw: b'^' beginning of number, b'0' digit or base prefix, b'_' underscore, b'!' other.
    let mut saw = b'^';
    let mut i = 0;
    if let Some(b'-' | b'+') = s.first() {
        s = &s[1..];
    }
    let mut hex = false;
    if s.len() >= 2 && s[0] == b'0' && matches!(lower(s[1]), b'b' | b'o' | b'x') {
        i = 2;
        saw = b'0';
        hex = lower(s[1]) == b'x';
    }
    while i < s.len() {
        let c = s[i];
        i += 1;
        if c.is_ascii_digit() || hex && (b'a'..=b'f').contains(&lower(c)) {
            saw = b'0';
            continue;
        }
        if c == b'_' {
            if saw != b'0' {
                return false;
            }
            saw = b'_';
            continue;
        }
        if saw == b'_' {
            return false;
        }
        saw = b'!';
    }
    saw != b'_'
}

/// Locks `m`, recovering the data of a poisoned lock.
fn lock<T: ?Sized>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}
