#!/usr/bin/env python3
# Copyright 2026 Chris Olson — Apache-2.0 (see LICENSE)
"""Reports Go references left in gadk's shipped comments and READMEs.

Scans comments in non-test Rust code (`crates/*/src`, `crates/*/examples`, `crates/*/build.rs`),
`#` comments in `Cargo.toml` files, and every line of `README.md` and `crates/*/README.md`. Test
code (`parity_tests/`, `tests/`, `testdata/`, `fixtures/`, test modules), `@generated` files and
licence headers are skipped. A hit is allowed only by an allowlist entry that matches exactly one
hit line. Allowlists live in `scripts/go-refs-allow/`: `<crate>.txt` (entries for `crates/<crate>/`,
at most 3) and `root.txt` (entries for the root `README.md`, at most 10); each non-comment line is
`path<TAB>substring<TAB>reason`.

Usage: check-go-refs.py [--root DIR] [--paths PREFIX ...]. With --paths, only hits and allowlist
entries whose path starts with one of the prefixes are checked.
Exit 0: clean; 1: hits or bad allowlist entries; 2: error.
"""
import argparse, pathlib, re, sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import rust_lex  # noqa: E402

PATTERN = re.compile(
    r"\bADK-Go\b|\bGo\b|golang|go-genai|(?i:\bport(?:ed)? (?:of|from)\b)|\bDivergences?\b"
    r"|\bledger\b|goroutine"
    r"|(?<![\w.])[a-z][a-z0-9_]*\.[A-Z][A-Za-z0-9_]*"
)
ALLOWLIST_DIR = "scripts/go-refs-allow"
LIMITS = {"root.txt": 10}
DEFAULT_LIMIT = 3
SKIP_DIRS = {"parity_tests", "tests", "testdata", "fixtures"}


class CheckError(Exception):
    pass


# ---- test modules -------------------------------------------------------------------------------

def _cfg_requires_test(expr):
    """True when a cfg predicate can only hold under `test` (`test`, `all(test, …)`)."""
    expr = expr.strip()
    if expr == "test":
        return True
    m = re.fullmatch(r"(all|any|not)\s*\((.*)\)", expr, re.S)
    if not m:
        return False
    args, depth, cur = [], 0, ""
    for ch in m.group(2):
        if ch == "," and depth == 0:
            args.append(cur)
            cur = ""
            continue
        depth += ch == "("
        depth -= ch == ")"
        cur += ch
    if cur.strip():
        args.append(cur)
    if m.group(1) == "all":
        return any(_cfg_requires_test(a) for a in args)
    if m.group(1) == "any":
        return bool(args) and all(_cfg_requires_test(a) for a in args)
    return False


def _attrs_before(masked, pos):
    """The outer attributes ([(start, end)] spans of `#[…]`) directly before `pos`."""
    spans = []
    j = pos
    while True:
        k = j - 1
        while k >= 0 and masked[k].isspace():
            k -= 1
        if k < 0 or masked[k] != "]":
            return spans
        depth = 0
        while k >= 0:
            if masked[k] == "]":
                depth += 1
            elif masked[k] == "[":
                depth -= 1
                if depth == 0:
                    break
            k -= 1
        h = k - 1
        while h >= 0 and masked[h].isspace():
            h -= 1
        if h < 0 or masked[h] != "#":
            return spans
        spans.append((h, j))
        j = h


def _match_brace(masked, open_pos):
    depth = 0
    for k in range(open_pos, len(masked)):
        if masked[k] == "{":
            depth += 1
        elif masked[k] == "}":
            depth -= 1
            if depth == 0:
                return k
    raise CheckError("unbalanced braces")


MOD_RE = re.compile(r"\b(?:pub(?:\s*\([^)]*\))?\s+)?mod\s+([A-Za-z_][A-Za-z0-9_]*)\s*([;{])")


def test_modules(path, src, masked):
    """(inline line ranges, files of test file-modules) for one Rust file."""
    ranges, files = [], []
    for m in MOD_RE.finditer(masked):
        is_test, path_attr = False, None
        for s, e in _attrs_before(masked, m.start()):
            body = masked[s:e]
            inner = re.fullmatch(r"#\s*\[\s*(.*)\]\s*", body, re.S)
            if not inner:
                continue
            cfg = re.fullmatch(r"cfg\s*\((.*)\)", inner.group(1).strip(), re.S)
            if cfg and _cfg_requires_test(cfg.group(1)):
                is_test = True
            if re.match(r"#\s*\[\s*path\s*=", body):
                lit = re.match(r'#\s*\[\s*path\s*=\s*"((?:[^"\\]|\\.)*)"', src[s:e])
                path_attr = lit.group(1) if lit else None
        if not is_test:
            continue
        if m.group(2) == "{":
            close = _match_brace(masked, m.end() - 1)
            ranges.append((masked.count("\n", 0, m.start()) + 1, masked.count("\n", 0, close) + 1))
        else:
            files.append(_resolve(path, m.group(1), path_attr))
    return ranges, files


def _resolve(path, name, path_attr):
    base = path.parent
    if path_attr is not None:
        target = (base / path_attr).resolve()
        if not target.exists():
            raise CheckError(f"{path}: #[path = {path_attr!r}] not found")
        return target
    if path.name not in ("lib.rs", "main.rs", "mod.rs"):
        base = base / path.stem
    for cand in (base / f"{name}.rs", base / name / "mod.rs"):
        if cand.exists():
            return cand.resolve()
    raise CheckError(f"{path}: cannot resolve test module `{name}`")


def _module_dir(f):
    return f.parent if f.name == "mod.rs" else f.parent / f.stem


# ---- scanning -----------------------------------------------------------------------------------

def _licence_end(lines):
    """Number of leading licence-header lines (0 when none)."""
    run = []
    for ln in lines:
        if ln.startswith("//") and not ln.startswith(("///", "//!")):
            run.append(ln)
        else:
            break
    if not any("Copyright" in ln for ln in run):
        return 0
    for i, ln in enumerate(run):
        if ln.rstrip().endswith("limitations under the License.") or ln.rstrip().endswith("SOFTWARE."):
            return i + 1
    return 0


def _rust_hits(path, src, excluded_ranges):
    lines = src.split("\n")
    skip_to = _licence_end(lines)
    hits = {}
    for kind, s, e in rust_lex.segments(src):
        if kind != "comment":
            continue
        first = src.count("\n", 0, s) + 1
        for off, text in enumerate(src[s:e].split("\n")):
            ln = first + off
            if ln <= skip_to or any(a <= ln <= b for a, b in excluded_ranges):
                continue
            if PATTERN.search(text):
                hits[ln] = lines[ln - 1]
    return sorted(hits.items())


def _toml_hits(src):
    out = []
    for n, line in enumerate(src.split("\n"), 1):
        quote, skip = None, False
        for k, ch in enumerate(line):
            if skip:
                skip = False
                continue
            if quote:
                if ch == "\\" and quote == '"':
                    skip = True
                elif ch == quote:
                    quote = None
            elif ch in "\"'":
                quote = ch
            elif ch == "#":
                if PATTERN.search(line[k:]):
                    out.append((n, line))
                break
    return out


def _rust_files(root):
    crates = root / "crates"
    files = []
    for c in sorted(crates.iterdir()) if crates.is_dir() else []:
        for sub in ("src", "examples"):
            if (c / sub).is_dir():
                files += sorted((c / sub).rglob("*.rs"))
        if (c / "build.rs").is_file():
            files.append(c / "build.rs")
    return [f.resolve() for f in files if not (SKIP_DIRS & set(f.relative_to(crates).parts))]


def scan(root):
    """[(relative path, line number, line text)] of every in-scope hit."""
    root = pathlib.Path(root).resolve()
    files = _rust_files(root)
    sources, ranges, test_files = {}, {}, set()
    for f in files:
        try:
            src = f.read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError) as e:
            raise CheckError(f"{f}: {e}")
        sources[f] = src
        try:
            masked = rust_lex.mask(src)
        except rust_lex.LexError as e:
            raise CheckError(f"{f}: {e}")
        ranges[f], tf = test_modules(f, src, masked)
        test_files.update(tf)
    test_dirs = {_module_dir(f) for f in test_files}

    def is_test_file(f):
        return f in test_files or any(d in f.parents for d in test_dirs)

    def read(p):
        try:
            return p.read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError) as e:
            raise CheckError(f"{p}: {e}")

    hits = []
    for f in files:
        src = sources[f]
        if is_test_file(f) or "@generated" in src.split("\n", 1)[0]:
            continue
        rel = f.relative_to(root).as_posix()
        hits += [(rel, n, t) for n, t in _rust_hits(f, src, ranges[f])]
    for t in [root / "Cargo.toml"] + sorted((root / "crates").glob("*/Cargo.toml")):
        if t.is_file():
            rel = t.relative_to(root).as_posix()
            hits += [(rel, n, l) for n, l in _toml_hits(read(t))]
    for r in [root / "README.md"] + sorted((root / "crates").glob("*/README.md")):
        if r.is_file():
            rel = r.relative_to(root).as_posix()
            for n, line in enumerate(read(r).split("\n"), 1):
                if PATTERN.search(line):
                    hits.append((rel, n, line))
    return hits


def load_allowlist(root):
    """([(file:line, path, substring)], problems) from scripts/go-refs-allow/*.txt."""
    d = pathlib.Path(root) / ALLOWLIST_DIR
    entries, problems = [], []
    for f in sorted(d.glob("*.txt")) if d.is_dir() else []:
        rel = f"{ALLOWLIST_DIR}/{f.name}"
        prefix = "README.md" if f.name == "root.txt" else f"crates/{f.stem}/"
        count = 0
        try:
            text = f.read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError) as e:
            raise CheckError(f"{rel}: {e}")
        for n, line in enumerate(text.split("\n"), 1):
            if not line.strip() or line.startswith("#"):
                continue
            parts = line.split("\t")
            if len(parts) != 3 or not all(x.strip() for x in parts):
                raise CheckError(f"{rel}:{n}: want path<TAB>substring<TAB>reason")
            if not parts[0].startswith(prefix):
                problems.append(f"{rel}:{n}: path {parts[0]!r} does not belong in {f.name}")
            count += 1
            entries.append((f"{rel}:{n}", parts[0], parts[1]))
        limit = LIMITS.get(f.name, DEFAULT_LIMIT)
        if count > limit:
            problems.append(f"{rel}: {count} entries, limit {limit}")
    return entries, problems


def check(root, paths=None):
    """(problem lines, exit code). `paths`: optional path prefixes to restrict the check to."""
    try:
        hits = scan(root)
        entries, problems = load_allowlist(root)
    except CheckError as e:
        return [f"error: {e}"], 2
    if paths:
        keep = lambda p: any(p.startswith(x) for x in paths)  # noqa: E731
        hits = [h for h in hits if keep(h[0])]
        entries = [e for e in entries if keep(e[1])]
    allowed = set()
    for where, path, sub in entries:
        matched = [h for h in hits if h[0] == path and sub in h[2]]
        if len(matched) != 1:
            what = "matches no hit line" if not matched else f"matches {len(matched)} hit lines"
            problems.append(f"{where}: entry {path!r} {sub!r} {what}")
        allowed.update(matched)
    problems += [f"{p}:{n}: {t.strip()}" for p, n, t in hits if (p, n, t) not in allowed]
    return problems, 1 if problems else 0


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--root", default=pathlib.Path(__file__).resolve().parent.parent)
    ap.add_argument("--paths", nargs="*", default=None)
    args = ap.parse_args()
    problems, code = check(args.root, args.paths)
    for p in problems:
        print(p)
    if code == 1:
        print(f"{len(problems)} Go reference(s) outside the allowlist", file=sys.stderr)
    return code


if __name__ == "__main__":
    sys.exit(main())
