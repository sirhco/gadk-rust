# Copyright 2026 Chris Olson — Apache-2.0 (see LICENSE)
"""A small Rust lexer for the comment checkers.

`segments(src)` splits Rust source into ("code" | "str" | "comment", start, end) spans that cover
the whole text. It knows nested block comments, line comments, string and byte strings, raw strings
(`r#"…"#`, `br"…"`), C strings, char and byte-char literals (told apart from lifetimes and labels)
and raw identifiers (`r#type`). `mask(src)` returns the source with comment and literal contents
replaced by spaces (newlines kept), so offsets, line numbers and brace matching stay valid.
"""
import re

_IDENT = re.compile(r"[A-Za-z0-9_]")
_CHAR = re.compile(r"'(?:\\(?:x[0-9a-fA-F]{2}|u\{[0-9a-fA-F_]{1,6}\}|[^\n])|[^\\'\n])'")


class LexError(Exception):
    """Unterminated comment or literal."""


def _ident_before(src, i):
    return i > 0 and _IDENT.match(src[i - 1]) is not None


def _line(src, i):
    return src.count("\n", 0, i) + 1


def segments(src):
    out = []
    n = len(src)
    i = 0
    code_start = 0

    def flush(end):
        if end > code_start:
            out.append(("code", code_start, end))

    while i < n:
        c = src[i]
        two = src[i:i + 2]
        if two == "//":
            flush(i)
            j = src.find("\n", i)
            j = n if j < 0 else j
            out.append(("comment", i, j))
            i = code_start = j
            continue
        if two == "/*":
            flush(i)
            depth, j = 1, i + 2
            while depth:
                if j >= n:
                    raise LexError(f"unterminated block comment at line {_line(src, i)}")
                if src.startswith("/*", j):
                    depth, j = depth + 1, j + 2
                elif src.startswith("*/", j):
                    depth, j = depth - 1, j + 2
                else:
                    j += 1
            out.append(("comment", i, j))
            i = code_start = j
            continue
        if not _ident_before(src, i):
            m = re.compile(r'(?:br|cr|r)(#*)"').match(src, i)
            if m:
                close = '"' + m.group(1)
                j = src.find(close, m.end())
                if j < 0:
                    raise LexError(f"unterminated raw string at line {_line(src, i)}")
                flush(i)
                out.append(("str", i, j + len(close)))
                i = code_start = j + len(close)
                continue
            m = re.compile(r'[bc]?"').match(src, i)
            if m:
                j = m.end()
                while True:
                    if j >= n:
                        raise LexError(f"unterminated string at line {_line(src, i)}")
                    if src[j] == "\\":
                        j += 2
                    elif src[j] == '"':
                        j += 1
                        break
                    else:
                        j += 1
                flush(i)
                out.append(("str", i, j))
                i = code_start = j
                continue
            start = i + 1 if src.startswith("b'", i) else i
            if src[start:start + 1] == "'":
                m = _CHAR.match(src, start)
                if m:
                    flush(i)
                    out.append(("str", i, m.end()))
                    i = code_start = m.end()
                    continue
        i += 1
    flush(n)
    return out


def mask(src, segs=None):
    segs = segments(src) if segs is None else segs
    parts = []
    for kind, s, e in segs:
        t = src[s:e]
        parts.append(t if kind == "code" else re.sub(r"[^\n]", " ", t))
    return "".join(parts)
