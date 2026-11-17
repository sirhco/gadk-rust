#!/usr/bin/env python3
# Copyright 2026 Chris Olson — Apache-2.0 (see LICENSE)
"""Fails unless the working tree differs from BASE only in comments and documentation.

- A changed `.rs` file must have the same code and literals once comments are removed (whitespace
  in code is collapsed; literals are compared verbatim). Added or changed comment lines must be at
  most 100 characters.
- A changed `Cargo.toml` may change only `#` comments.
- Other changed paths must be `README.md` files, `CHANGELOG.md`, `docs/**`, or the checker files
  themselves (`scripts/rust_lex.py`, `scripts/check-go-refs.py`, `scripts/check-comment-only.py`,
  `scripts/check_go_refs_test.py`, `scripts/go-refs-allow/*.txt`, `.github/workflows/ci.yml`).

- Paths in `CODE_EXEMPT` (a one-off, user-approved exemption) are skipped entirely.

Usage: check-comment-only.py BASE [--root DIR]. Exit 0: ok; 1: violations; 2: error.
"""
import argparse, pathlib, re, subprocess, sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import rust_lex  # noqa: E402

ALLOWED = {
    "CHANGELOG.md", "scripts/rust_lex.py", "scripts/check-go-refs.py", "scripts/check-comment-only.py",
    "scripts/check_go_refs_test.py", ".github/workflows/ci.yml",
}
MAX_COLS = 100
# Paths whose code may change, with the reason (user-approved one-offs).
CODE_EXEMPT = {
    "crates/gadk-configurable/tests/recordings_differential.rs":
        "string literals of the generated shapes.rs header (user-approved one-off)",
}


def _git(root, *args):
    r = subprocess.run(["git", "-C", str(root), *args], capture_output=True, text=True)
    if r.returncode != 0:
        raise RuntimeError(r.stderr.strip())
    return r.stdout


def code_signature(src):
    """Code and literal tokens with comments dropped and code whitespace collapsed."""
    out, run = [], []
    for kind, s, e in rust_lex.segments(src):
        if kind == "str":
            out.append(re.sub(r"\s+", " ", "".join(run)).strip())
            out.append(src[s:e])
            run = []
        else:
            run.append(src[s:e] if kind == "code" else " ")
    out.append(re.sub(r"\s+", " ", "".join(run)).strip())
    return "\x00".join(out)


def comment_lines(src):
    """Set of full source lines that contain (part of) a comment."""
    lines = src.split("\n")
    found = set()
    for kind, s, e in rust_lex.segments(src):
        if kind == "comment":
            first = src.count("\n", 0, s)
            for k in range(first, first + src[s:e].count("\n") + 1):
                found.add(lines[k])
    return found


def toml_signature(src):
    """Non-comment lines of a TOML file; a `#` inside a string is not a comment."""
    out, multi = [], None
    for line in src.split("\n"):
        quote, cut, k = multi, len(line), 0
        multi_here = multi is not None
        while k < len(line):
            ch = line[k]
            if quote:
                if ch == "\\" and quote in ('"', '"""'):
                    k += 2
                    continue
                if line.startswith(quote, k):
                    k += len(quote)
                    quote = None
                    continue
            elif line.startswith(('"""', "'''"), k):
                quote = line[k:k + 3]
                k += 3
                continue
            elif ch in "\"'":
                quote = ch
            elif ch == "#":
                cut = k
                break
            k += 1
        multi = quote if quote in ('"""', "'''") else None
        code = line[:cut].rstrip()
        if code or multi_here:
            out.append(code)
    return out


def check(root, base):
    root = pathlib.Path(root).resolve()
    changed = set(_git(root, "diff", "--name-only", "-z", base).split("\0"))
    changed |= set(_git(root, "ls-files", "-z", "--others", "--exclude-standard").split("\0"))
    changed.discard("")
    problems = []
    for rel in sorted(changed):
        path = root / rel
        if (rel in ALLOWED or rel in CODE_EXEMPT or rel.startswith(("docs/", "scripts/go-refs-allow/"))
                or pathlib.PurePosixPath(rel).name == "README.md"):
            continue
        try:
            old = _git(root, "show", f"{base}:{rel}")
        except RuntimeError:
            old = None
        new = path.read_text(encoding="utf-8") if path.is_file() else None
        if rel.endswith(".rs"):
            if old is None or new is None:
                problems.append(f"{rel}: added or deleted Rust file")
                continue
            if code_signature(old) != code_signature(new):
                problems.append(f"{rel}: code changed (not only comments)")
            before, commented = set(old.split("\n")), comment_lines(new)
            for n, line in enumerate(new.split("\n"), 1):
                if line in commented and line not in before and len(line) > MAX_COLS:
                    problems.append(f"{rel}:{n}: comment line is {len(line)} > {MAX_COLS} characters")
        elif pathlib.PurePosixPath(rel).name == "Cargo.toml":
            if old is None or new is None or toml_signature(old) != toml_signature(new):
                problems.append(f"{rel}: non-comment change")
        else:
            problems.append(f"{rel}: file outside the comment-only scope changed")
    return problems


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("base")
    ap.add_argument("--root", default=pathlib.Path(__file__).resolve().parent.parent)
    args = ap.parse_args()
    try:
        problems = check(args.root, args.base)
    except (RuntimeError, rust_lex.LexError, OSError) as e:
        print(f"error: {e}")
        return 2
    for p in problems:
        print(p)
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
