// Copyright 2026 Chris Olson
// Portions derived from gopkg.in/yaml.v3 (https://github.com/go-yaml/yaml), Copyright 2011-2019 Canonical Ltd.
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

//! Port of yaml.v3 v3.0.1 `node_test.go` (Task 10).
//!
//! Go's `yaml.Node` literals are [`N`] values: a static tree with Go's field values whose
//! `alias` is the index path (through `content`, from the table node) of the alias target —
//! the node Go shares by pointer (`saveNode`/`dropNode`). [`build`] turns one into a
//! [`Document`]; [`canon`] turns a `Document` back into a comparable tree (Go's `DeepEquals`;
//! an alias compares by its target's path). Go's `TZ=UTC` setup is not needed (no timestamps
//! are involved) and `fprintComments` only feeds `c.Logf`, so neither is ported.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::{
    Document, Encoder, GoValue, Kind, NodeData, NodeId, Options, Style, from_node, marshal,
    marshal_node, to_node, unmarshal_node,
};

const TAGGED: u32 = Style::TAGGED.bits();
const DOUBLE: u32 = Style::DOUBLE_QUOTED.bits();
const SINGLE: u32 = Style::SINGLE_QUOTED.bits();
const LITERAL: u32 = Style::LITERAL.bits();
const FOLDED: u32 = Style::FOLDED.bits();
const FLOW: u32 = Style::FLOW.bits();

/// A Go `yaml.Node` literal.
#[derive(Clone, Copy, Debug)]
struct N {
    kind: Kind,
    style: u32,
    tag: &'static str,
    value: &'static str,
    anchor: &'static str,
    /// The path (content indexes from the table node) of the alias target.
    alias: Option<&'static [usize]>,
    head: &'static str,
    line_comment: &'static str,
    foot: &'static str,
    line: usize,
    column: usize,
    content: &'static [N],
}

/// Go's zero `yaml.Node{}` (the base of every [`N`] literal).
const Z: N = N {
    kind: Kind::Zero,
    style: 0,
    tag: "",
    value: "",
    anchor: "",
    alias: None,
    head: "",
    line_comment: "",
    foot: "",
    line: 0,
    column: 0,
    content: &[],
};

fn node_data(n: &N) -> NodeData {
    NodeData {
        kind: n.kind,
        style: Style::from_bits(n.style),
        tag: n.tag.to_string(),
        value: n.value.to_string(),
        anchor: n.anchor.to_string(),
        alias: None,
        content: Vec::new(),
        head_comment: n.head.to_string(),
        line_comment: n.line_comment.to_string(),
        foot_comment: n.foot.to_string(),
        line: n.line,
        column: n.column,
    }
}

/// The [`Document`] of a Go node literal (its root is the literal's top node).
fn build(n: &N) -> Document {
    fn add(
        doc: &mut Document,
        id: NodeId,
        n: &N,
        path: &mut Vec<usize>,
        ids: &mut HashMap<Vec<usize>, NodeId>,
    ) {
        ids.insert(path.clone(), id);
        for (i, c) in n.content.iter().enumerate() {
            let child = doc.push(node_data(c));
            doc[id].content.push(child);
            path.push(i);
            add(doc, child, c, path, ids);
            path.pop();
        }
    }
    fn link(doc: &mut Document, id: NodeId, n: &N, ids: &HashMap<Vec<usize>, NodeId>) {
        if let Some(p) = n.alias {
            doc[id].alias = Some(ids[p]);
        }
        let content = doc[id].content.clone();
        for (c, cn) in content.into_iter().zip(n.content) {
            link(doc, c, cn, ids);
        }
    }
    let mut doc = Document::new(node_data(n));
    let root = doc.root();
    let mut ids = HashMap::new();
    add(&mut doc, root, n, &mut Vec::new(), &mut ids);
    link(&mut doc, root, n, &ids);
    doc
}

/// A node tree in comparable form: Go's `DeepEquals` over `yaml.Node`, with an alias compared
/// by its target's path (pointer identity within the tree).
#[derive(Debug, PartialEq)]
struct Canon {
    kind: Kind,
    style: u32,
    tag: String,
    value: String,
    anchor: String,
    alias: Option<Vec<usize>>,
    head: String,
    line_comment: String,
    foot: String,
    line: usize,
    column: usize,
    content: Vec<Canon>,
}

/// The comparable tree of `doc` from its root.
fn canon(doc: &Document) -> Canon {
    fn paths(
        doc: &Document,
        id: NodeId,
        path: &mut Vec<usize>,
        out: &mut HashMap<NodeId, Vec<usize>>,
    ) {
        if out.contains_key(&id) {
            return;
        }
        out.insert(id, path.clone());
        for (i, &c) in doc[id].content.iter().enumerate() {
            path.push(i);
            paths(doc, c, path, out);
            path.pop();
        }
    }
    fn walk(doc: &Document, id: NodeId, paths: &HashMap<NodeId, Vec<usize>>) -> Canon {
        let n = &doc[id];
        Canon {
            kind: n.kind,
            style: n.style.bits(),
            tag: n.tag.clone(),
            value: n.value.clone(),
            anchor: n.anchor.clone(),
            alias: n.alias.map(|a| {
                paths
                    .get(&a)
                    .cloned()
                    .unwrap_or_else(|| panic!("alias target {a:?} is outside the tree"))
            }),
            head: n.head_comment.clone(),
            line_comment: n.line_comment.clone(),
            foot: n.foot_comment.clone(),
            line: n.line,
            column: n.column,
            content: n.content.iter().map(|&c| walk(doc, c, paths)).collect(),
        }
    }
    let mut p = HashMap::new();
    paths(doc, doc.root(), &mut Vec::new(), &mut p);
    walk(doc, doc.root(), &p)
}

/// Go: `yaml.NewEncoder(&buf)`, `SetIndent(2)`, `Encode(node)`, `Close()`.
fn encode_indent2(doc: &Document) -> String {
    let mut buf = Vec::new();
    let mut enc = Encoder::new(&mut buf);
    enc.set_indent(2);
    enc.encode_node(doc, doc.root()).expect("Encode");
    enc.close().expect("Close");
    String::from_utf8(buf).expect("UTF-8 output")
}

/// One `nodeTests` row: the YAML (`[decode]`/`[encode]` prefix = one direction only) and its
/// node.
struct Row {
    /// The row's index in Go's table.
    go: usize,
    yaml: &'static str,
    node: N,
}

/// Go: `nodeTests`.
static NODE_TESTS: &[Row] = &[
    Row {
        go: 0,
        yaml: "null\n",
        node: N {
            kind: Kind::Document,
            line: 1,
            column: 1,
            content: &[N {
                kind: Kind::Scalar,
                tag: "!!null",
                value: "null",
                line: 1,
                column: 1,
                ..Z
            }],
            ..Z
        },
    },
    Row {
        go: 1,
        yaml: "[encode]null\n",
        node: Z,
    },
    Row {
        go: 2,
        yaml: "foo\n",
        node: N {
            kind: Kind::Document,
            line: 1,
            column: 1,
            content: &[N {
                kind: Kind::Scalar,
                tag: "!!str",
                value: "foo",
                line: 1,
                column: 1,
                ..Z
            }],
            ..Z
        },
    },
    Row {
        go: 3,
        yaml: "\"foo\"\n",
        node: N {
            kind: Kind::Document,
            line: 1,
            column: 1,
            content: &[N {
                kind: Kind::Scalar,
                style: DOUBLE,
                tag: "!!str",
                value: "foo",
                line: 1,
                column: 1,
                ..Z
            }],
            ..Z
        },
    },
    Row {
        go: 4,
        yaml: "'foo'\n",
        node: N {
            kind: Kind::Document,
            line: 1,
            column: 1,
            content: &[N {
                kind: Kind::Scalar,
                style: SINGLE,
                tag: "!!str",
                value: "foo",
                line: 1,
                column: 1,
                ..Z
            }],
            ..Z
        },
    },
    Row {
        go: 5,
        yaml: "!!str 123\n",
        node: N {
            kind: Kind::Document,
            line: 1,
            column: 1,
            content: &[N {
                kind: Kind::Scalar,
                style: TAGGED,
                tag: "!!str",
                value: "123",
                line: 1,
                column: 1,
                ..Z
            }],
            ..Z
        },
    },
    // Although the node isn't TaggedStyle, dropping the tag would change the value.
    Row {
        go: 6,
        yaml: "[encode]!!binary gIGC\n",
        node: N {
            kind: Kind::Document,
            line: 1,
            column: 1,
            content: &[N {
                kind: Kind::Scalar,
                tag: "!!binary",
                value: "gIGC",
                line: 1,
                column: 1,
                ..Z
            }],
            ..Z
        },
    },
    // Item doesn't have a tag, but needs to be binary encoded due to its content.
    // Skipped nodeTests[7] ("[encode]!!binary gIGC\n", a scalar with `Value: "\x80\x81\x82"`
    // and no tag): the value is not valid UTF-8, and `NodeData::value` is a Rust `String`
    // (Divergence, ledger 6c-1).

    // Same, but with strings we can just quote them.
    Row {
        go: 8,
        yaml: "[encode]\"123\"\n",
        node: N {
            kind: Kind::Document,
            line: 1,
            column: 1,
            content: &[N {
                kind: Kind::Scalar,
                tag: "!!str",
                value: "123",
                line: 1,
                column: 1,
                ..Z
            }],
            ..Z
        },
    },
    Row {
        go: 9,
        yaml: "!tag:something 123\n",
        node: N {
            kind: Kind::Document,
            line: 1,
            column: 1,
            content: &[N {
                kind: Kind::Scalar,
                style: TAGGED,
                tag: "!tag:something",
                value: "123",
                line: 1,
                column: 1,
                ..Z
            }],
            ..Z
        },
    },
    Row {
        go: 10,
        yaml: "[encode]!tag:something 123\n",
        node: N {
            kind: Kind::Document,
            line: 1,
            column: 1,
            content: &[N {
                kind: Kind::Scalar,
                tag: "!tag:something",
                value: "123",
                line: 1,
                column: 1,
                ..Z
            }],
            ..Z
        },
    },
    Row {
        go: 11,
        yaml: "!tag:something {}\n",
        node: N {
            kind: Kind::Document,
            line: 1,
            column: 1,
            content: &[N {
                kind: Kind::Mapping,
                style: TAGGED | FLOW,
                tag: "!tag:something",
                line: 1,
                column: 1,
                ..Z
            }],
            ..Z
        },
    },
    Row {
        go: 12,
        yaml: "[encode]!tag:something {}\n",
        node: N {
            kind: Kind::Document,
            line: 1,
            column: 1,
            content: &[N {
                kind: Kind::Mapping,
                style: FLOW,
                tag: "!tag:something",
                line: 1,
                column: 1,
                ..Z
            }],
            ..Z
        },
    },
    Row {
        go: 13,
        yaml: "!tag:something []\n",
        node: N {
            kind: Kind::Document,
            line: 1,
            column: 1,
            content: &[N {
                kind: Kind::Sequence,
                style: TAGGED | FLOW,
                tag: "!tag:something",
                line: 1,
                column: 1,
                ..Z
            }],
            ..Z
        },
    },
    Row {
        go: 14,
        yaml: "[encode]!tag:something []\n",
        node: N {
            kind: Kind::Document,
            line: 1,
            column: 1,
            content: &[N {
                kind: Kind::Sequence,
                style: FLOW,
                tag: "!tag:something",
                line: 1,
                column: 1,
                ..Z
            }],
            ..Z
        },
    },
    Row {
        go: 15,
        yaml: "''\n",
        node: N {
            kind: Kind::Document,
            line: 1,
            column: 1,
            content: &[N {
                kind: Kind::Scalar,
                style: SINGLE,
                tag: "!!str",
                line: 1,
                column: 1,
                ..Z
            }],
            ..Z
        },
    },
    Row {
        go: 16,
        yaml: "|\n  foo\n  bar\n",
        node: N {
            kind: Kind::Document,
            line: 1,
            column: 1,
            content: &[N {
                kind: Kind::Scalar,
                style: LITERAL,
                tag: "!!str",
                value: "foo\nbar\n",
                line: 1,
                column: 1,
                ..Z
            }],
            ..Z
        },
    },
    Row {
        go: 17,
        yaml: "true\n",
        node: N {
            kind: Kind::Document,
            line: 1,
            column: 1,
            content: &[N {
                kind: Kind::Scalar,
                tag: "!!bool",
                value: "true",
                line: 1,
                column: 1,
                ..Z
            }],
            ..Z
        },
    },
    Row {
        go: 18,
        yaml: "-10\n",
        node: N {
            kind: Kind::Document,
            line: 1,
            column: 1,
            content: &[N {
                kind: Kind::Scalar,
                tag: "!!int",
                value: "-10",
                line: 1,
                column: 1,
                ..Z
            }],
            ..Z
        },
    },
    Row {
        go: 19,
        yaml: "4294967296\n",
        node: N {
            kind: Kind::Document,
            line: 1,
            column: 1,
            content: &[N {
                kind: Kind::Scalar,
                tag: "!!int",
                value: "4294967296",
                line: 1,
                column: 1,
                ..Z
            }],
            ..Z
        },
    },
    Row {
        go: 20,
        yaml: "0.1000\n",
        node: N {
            kind: Kind::Document,
            line: 1,
            column: 1,
            content: &[N {
                kind: Kind::Scalar,
                tag: "!!float",
                value: "0.1000",
                line: 1,
                column: 1,
                ..Z
            }],
            ..Z
        },
    },
    Row {
        go: 21,
        yaml: "-.inf\n",
        node: N {
            kind: Kind::Document,
            line: 1,
            column: 1,
            content: &[N {
                kind: Kind::Scalar,
                tag: "!!float",
                value: "-.inf",
                line: 1,
                column: 1,
                ..Z
            }],
            ..Z
        },
    },
    Row {
        go: 22,
        yaml: ".nan\n",
        node: N {
            kind: Kind::Document,
            line: 1,
            column: 1,
            content: &[N {
                kind: Kind::Scalar,
                tag: "!!float",
                value: ".nan",
                line: 1,
                column: 1,
                ..Z
            }],
            ..Z
        },
    },
    Row {
        go: 23,
        yaml: "{}\n",
        node: N {
            kind: Kind::Document,
            line: 1,
            column: 1,
            content: &[N {
                kind: Kind::Mapping,
                style: FLOW,
                tag: "!!map",
                line: 1,
                column: 1,
                ..Z
            }],
            ..Z
        },
    },
    Row {
        go: 24,
        yaml: "a: b c\n",
        node: N {
            kind: Kind::Document,
            line: 1,
            column: 1,
            content: &[N {
                kind: Kind::Mapping,
                tag: "!!map",
                line: 1,
                column: 1,
                content: &[
                    N {
                        kind: Kind::Scalar,
                        tag: "!!str",
                        value: "a",
                        line: 1,
                        column: 1,
                        ..Z
                    },
                    N {
                        kind: Kind::Scalar,
                        tag: "!!str",
                        value: "b c",
                        line: 1,
                        column: 4,
                        ..Z
                    },
                ],
                ..Z
            }],
            ..Z
        },
    },
    Row {
        go: 25,
        yaml: "a:\n  b: c\n  d: e\n",
        node: N {
            kind: Kind::Document,
            line: 1,
            column: 1,
            content: &[N {
                kind: Kind::Mapping,
                tag: "!!map",
                line: 1,
                column: 1,
                content: &[
                    N {
                        kind: Kind::Scalar,
                        tag: "!!str",
                        value: "a",
                        line: 1,
                        column: 1,
                        ..Z
                    },
                    N {
                        kind: Kind::Mapping,
                        tag: "!!map",
                        line: 2,
                        column: 3,
                        content: &[
                            N {
                                kind: Kind::Scalar,
                                tag: "!!str",
                                value: "b",
                                line: 2,
                                column: 3,
                                ..Z
                            },
                            N {
                                kind: Kind::Scalar,
                                tag: "!!str",
                                value: "c",
                                line: 2,
                                column: 6,
                                ..Z
                            },
                            N {
                                kind: Kind::Scalar,
                                tag: "!!str",
                                value: "d",
                                line: 3,
                                column: 3,
                                ..Z
                            },
                            N {
                                kind: Kind::Scalar,
                                tag: "!!str",
                                value: "e",
                                line: 3,
                                column: 6,
                                ..Z
                            },
                        ],
                        ..Z
                    },
                ],
                ..Z
            }],
            ..Z
        },
    },
    Row {
        go: 26,
        yaml: "a:\n  - b: c\n    d: e\n",
        node: N {
            kind: Kind::Document,
            line: 1,
            column: 1,
            content: &[N {
                kind: Kind::Mapping,
                tag: "!!map",
                line: 1,
                column: 1,
                content: &[
                    N {
                        kind: Kind::Scalar,
                        tag: "!!str",
                        value: "a",
                        line: 1,
                        column: 1,
                        ..Z
                    },
                    N {
                        kind: Kind::Sequence,
                        tag: "!!seq",
                        line: 2,
                        column: 3,
                        content: &[N {
                            kind: Kind::Mapping,
                            tag: "!!map",
                            line: 2,
                            column: 5,
                            content: &[
                                N {
                                    kind: Kind::Scalar,
                                    tag: "!!str",
                                    value: "b",
                                    line: 2,
                                    column: 5,
                                    ..Z
                                },
                                N {
                                    kind: Kind::Scalar,
                                    tag: "!!str",
                                    value: "c",
                                    line: 2,
                                    column: 8,
                                    ..Z
                                },
                                N {
                                    kind: Kind::Scalar,
                                    tag: "!!str",
                                    value: "d",
                                    line: 3,
                                    column: 5,
                                    ..Z
                                },
                                N {
                                    kind: Kind::Scalar,
                                    tag: "!!str",
                                    value: "e",
                                    line: 3,
                                    column: 8,
                                    ..Z
                                },
                            ],
                            ..Z
                        }],
                        ..Z
                    },
                ],
                ..Z
            }],
            ..Z
        },
    },
    Row {
        go: 27,
        yaml: "a: # AI\n  - b\nc:\n  - d\n",
        node: N {
            kind: Kind::Document,
            line: 1,
            column: 1,
            content: &[N {
                kind: Kind::Mapping,
                tag: "!!map",
                line: 1,
                column: 1,
                content: &[
                    N {
                        kind: Kind::Scalar,
                        tag: "!!str",
                        value: "a",
                        line_comment: "# AI",
                        line: 1,
                        column: 1,
                        ..Z
                    },
                    N {
                        kind: Kind::Sequence,
                        tag: "!!seq",
                        line: 2,
                        column: 3,
                        content: &[N {
                            kind: Kind::Scalar,
                            tag: "!!str",
                            value: "b",
                            line: 2,
                            column: 5,
                            ..Z
                        }],
                        ..Z
                    },
                    N {
                        kind: Kind::Scalar,
                        tag: "!!str",
                        value: "c",
                        line: 3,
                        column: 1,
                        ..Z
                    },
                    N {
                        kind: Kind::Sequence,
                        tag: "!!seq",
                        line: 4,
                        column: 3,
                        content: &[N {
                            kind: Kind::Scalar,
                            tag: "!!str",
                            value: "d",
                            line: 4,
                            column: 5,
                            ..Z
                        }],
                        ..Z
                    },
                ],
                ..Z
            }],
            ..Z
        },
    },
    Row {
        go: 28,
        yaml: "[decode]a:\n  # HM\n  - # HB1\n    # HB2\n    b: # IB\n      c # IC\n",
        node: N {
            kind: Kind::Document,
            line: 1,
            column: 1,
            content: &[N {
                kind: Kind::Mapping,
                tag: "!!map",
                line: 1,
                column: 1,
                content: &[
                    N {
                        kind: Kind::Scalar,
                        tag: "!!str",
                        value: "a",
                        line: 1,
                        column: 1,
                        ..Z
                    },
                    N {
                        kind: Kind::Sequence,
                        tag: "!!seq",
                        line: 3,
                        column: 3,
                        content: &[N {
                            kind: Kind::Mapping,
                            tag: "!!map",
                            head: "# HM",
                            line: 5,
                            column: 5,
                            content: &[
                                N {
                                    kind: Kind::Scalar,
                                    tag: "!!str",
                                    value: "b",
                                    head: "# HB1\n# HB2",
                                    line_comment: "# IB",
                                    line: 5,
                                    column: 5,
                                    ..Z
                                },
                                N {
                                    kind: Kind::Scalar,
                                    tag: "!!str",
                                    value: "c",
                                    line_comment: "# IC",
                                    line: 6,
                                    column: 7,
                                    ..Z
                                },
                            ],
                            ..Z
                        }],
                        ..Z
                    },
                ],
                ..Z
            }],
            ..Z
        },
    },
    // When encoding the value above, it loses b's inline comment.
    Row {
        go: 29,
        yaml: "[encode]a:\n  # HM\n  - # HB1\n    # HB2\n    b: c # IC\n",
        node: N {
            kind: Kind::Document,
            line: 1,
            column: 1,
            content: &[N {
                kind: Kind::Mapping,
                tag: "!!map",
                line: 1,
                column: 1,
                content: &[
                    N {
                        kind: Kind::Scalar,
                        tag: "!!str",
                        value: "a",
                        line: 1,
                        column: 1,
                        ..Z
                    },
                    N {
                        kind: Kind::Sequence,
                        tag: "!!seq",
                        line: 3,
                        column: 3,
                        content: &[N {
                            kind: Kind::Mapping,
                            tag: "!!map",
                            head: "# HM",
                            line: 5,
                            column: 5,
                            content: &[
                                N {
                                    kind: Kind::Scalar,
                                    tag: "!!str",
                                    value: "b",
                                    head: "# HB1\n# HB2",
                                    line_comment: "# IB",
                                    line: 5,
                                    column: 5,
                                    ..Z
                                },
                                N {
                                    kind: Kind::Scalar,
                                    tag: "!!str",
                                    value: "c",
                                    line_comment: "# IC",
                                    line: 6,
                                    column: 7,
                                    ..Z
                                },
                            ],
                            ..Z
                        }],
                        ..Z
                    },
                ],
                ..Z
            }],
            ..Z
        },
    },
    // Multiple cases of comment inlining next to mapping keys.
    Row {
        go: 30,
        yaml: "a: | # IA\n  str\nb: >- # IB\n  str\nc: # IC\n  - str\nd: # ID\n  str:\n",
        node: N {
            kind: Kind::Document,
            line: 1,
            column: 1,
            content: &[N {
                kind: Kind::Mapping,
                tag: "!!map",
                line: 1,
                column: 1,
                content: &[
                    N {
                        kind: Kind::Scalar,
                        tag: "!!str",
                        value: "a",
                        line: 1,
                        column: 1,
                        ..Z
                    },
                    N {
                        kind: Kind::Scalar,
                        style: LITERAL,
                        tag: "!!str",
                        value: "str\n",
                        line_comment: "# IA",
                        line: 1,
                        column: 4,
                        ..Z
                    },
                    N {
                        kind: Kind::Scalar,
                        tag: "!!str",
                        value: "b",
                        line: 3,
                        column: 1,
                        ..Z
                    },
                    N {
                        kind: Kind::Scalar,
                        style: FOLDED,
                        tag: "!!str",
                        value: "str",
                        line_comment: "# IB",
                        line: 3,
                        column: 4,
                        ..Z
                    },
                    N {
                        kind: Kind::Scalar,
                        tag: "!!str",
                        value: "c",
                        line_comment: "# IC",
                        line: 5,
                        column: 1,
                        ..Z
                    },
                    N {
                        kind: Kind::Sequence,
                        tag: "!!seq",
                        line: 6,
                        column: 3,
                        content: &[N {
                            kind: Kind::Scalar,
                            tag: "!!str",
                            value: "str",
                            line: 6,
                            column: 5,
                            ..Z
                        }],
                        ..Z
                    },
                    N {
                        kind: Kind::Scalar,
                        tag: "!!str",
                        value: "d",
                        line_comment: "# ID",
                        line: 7,
                        column: 1,
                        ..Z
                    },
                    N {
                        kind: Kind::Mapping,
                        tag: "!!map",
                        line: 8,
                        column: 3,
                        content: &[
                            N {
                                kind: Kind::Scalar,
                                tag: "!!str",
                                value: "str",
                                line: 8,
                                column: 3,
                                ..Z
                            },
                            N {
                                kind: Kind::Scalar,
                                tag: "!!null",
                                line: 8,
                                column: 7,
                                ..Z
                            },
                        ],
                        ..Z
                    },
                ],
                ..Z
            }],
            ..Z
        },
    },
    // Indentless sequence.
    Row {
        go: 31,
        yaml: "[decode]a:\n# HM\n- # HB1\n  # HB2\n  b: # IB\n    c # IC\n",
        node: N {
            kind: Kind::Document,
            line: 1,
            column: 1,
            content: &[N {
                kind: Kind::Mapping,
                tag: "!!map",
                line: 1,
                column: 1,
                content: &[
                    N {
                        kind: Kind::Scalar,
                        tag: "!!str",
                        value: "a",
                        line: 1,
                        column: 1,
                        ..Z
                    },
                    N {
                        kind: Kind::Sequence,
                        tag: "!!seq",
                        line: 3,
                        column: 1,
                        content: &[N {
                            kind: Kind::Mapping,
                            tag: "!!map",
                            head: "# HM",
                            line: 5,
                            column: 3,
                            content: &[
                                N {
                                    kind: Kind::Scalar,
                                    tag: "!!str",
                                    value: "b",
                                    head: "# HB1\n# HB2",
                                    line_comment: "# IB",
                                    line: 5,
                                    column: 3,
                                    ..Z
                                },
                                N {
                                    kind: Kind::Scalar,
                                    tag: "!!str",
                                    value: "c",
                                    line_comment: "# IC",
                                    line: 6,
                                    column: 5,
                                    ..Z
                                },
                            ],
                            ..Z
                        }],
                        ..Z
                    },
                ],
                ..Z
            }],
            ..Z
        },
    },
    Row {
        go: 32,
        yaml: "- a\n- b\n",
        node: N {
            kind: Kind::Document,
            line: 1,
            column: 1,
            content: &[N {
                kind: Kind::Sequence,
                tag: "!!seq",
                line: 1,
                column: 1,
                content: &[
                    N {
                        kind: Kind::Scalar,
                        tag: "!!str",
                        value: "a",
                        line: 1,
                        column: 3,
                        ..Z
                    },
                    N {
                        kind: Kind::Scalar,
                        tag: "!!str",
                        value: "b",
                        line: 2,
                        column: 3,
                        ..Z
                    },
                ],
                ..Z
            }],
            ..Z
        },
    },
    Row {
        go: 33,
        yaml: "- a\n- - b\n  - c\n",
        node: N {
            kind: Kind::Document,
            line: 1,
            column: 1,
            content: &[N {
                kind: Kind::Sequence,
                tag: "!!seq",
                line: 1,
                column: 1,
                content: &[
                    N {
                        kind: Kind::Scalar,
                        tag: "!!str",
                        value: "a",
                        line: 1,
                        column: 3,
                        ..Z
                    },
                    N {
                        kind: Kind::Sequence,
                        tag: "!!seq",
                        line: 2,
                        column: 3,
                        content: &[
                            N {
                                kind: Kind::Scalar,
                                tag: "!!str",
                                value: "b",
                                line: 2,
                                column: 5,
                                ..Z
                            },
                            N {
                                kind: Kind::Scalar,
                                tag: "!!str",
                                value: "c",
                                line: 3,
                                column: 5,
                                ..Z
                            },
                        ],
                        ..Z
                    },
                ],
                ..Z
            }],
            ..Z
        },
    },
    Row {
        go: 34,
        yaml: "[a, b]\n",
        node: N {
            kind: Kind::Document,
            line: 1,
            column: 1,
            content: &[N {
                kind: Kind::Sequence,
                style: FLOW,
                tag: "!!seq",
                line: 1,
                column: 1,
                content: &[
                    N {
                        kind: Kind::Scalar,
                        tag: "!!str",
                        value: "a",
                        line: 1,
                        column: 2,
                        ..Z
                    },
                    N {
                        kind: Kind::Scalar,
                        tag: "!!str",
                        value: "b",
                        line: 1,
                        column: 5,
                        ..Z
                    },
                ],
                ..Z
            }],
            ..Z
        },
    },
    Row {
        go: 35,
        yaml: "- a\n- [b, c]\n",
        node: N {
            kind: Kind::Document,
            line: 1,
            column: 1,
            content: &[N {
                kind: Kind::Sequence,
                tag: "!!seq",
                line: 1,
                column: 1,
                content: &[
                    N {
                        kind: Kind::Scalar,
                        tag: "!!str",
                        value: "a",
                        line: 1,
                        column: 3,
                        ..Z
                    },
                    N {
                        kind: Kind::Sequence,
                        style: FLOW,
                        tag: "!!seq",
                        line: 2,
                        column: 3,
                        content: &[
                            N {
                                kind: Kind::Scalar,
                                tag: "!!str",
                                value: "b",
                                line: 2,
                                column: 4,
                                ..Z
                            },
                            N {
                                kind: Kind::Scalar,
                                tag: "!!str",
                                value: "c",
                                line: 2,
                                column: 7,
                                ..Z
                            },
                        ],
                        ..Z
                    },
                ],
                ..Z
            }],
            ..Z
        },
    },
    Row {
        go: 36,
        yaml: "a: &x 1\nb: &y 2\nc: *x\nd: *y\n",
        node: N {
            kind: Kind::Document,
            line: 1,
            column: 1,
            content: &[N {
                kind: Kind::Mapping,
                tag: "!!map",
                line: 1,
                column: 1,
                content: &[
                    N {
                        kind: Kind::Scalar,
                        tag: "!!str",
                        value: "a",
                        line: 1,
                        column: 1,
                        ..Z
                    },
                    N {
                        kind: Kind::Scalar,
                        tag: "!!int",
                        value: "1",
                        anchor: "x",
                        line: 1,
                        column: 4,
                        ..Z
                    },
                    N {
                        kind: Kind::Scalar,
                        tag: "!!str",
                        value: "b",
                        line: 2,
                        column: 1,
                        ..Z
                    },
                    N {
                        kind: Kind::Scalar,
                        tag: "!!int",
                        value: "2",
                        anchor: "y",
                        line: 2,
                        column: 4,
                        ..Z
                    },
                    N {
                        kind: Kind::Scalar,
                        tag: "!!str",
                        value: "c",
                        line: 3,
                        column: 1,
                        ..Z
                    },
                    N {
                        kind: Kind::Alias,
                        value: "x",
                        alias: Some(&[0, 1]),
                        line: 3,
                        column: 4,
                        ..Z
                    },
                    N {
                        kind: Kind::Scalar,
                        tag: "!!str",
                        value: "d",
                        line: 4,
                        column: 1,
                        ..Z
                    },
                    N {
                        kind: Kind::Alias,
                        value: "y",
                        alias: Some(&[0, 3]),
                        line: 4,
                        column: 4,
                        ..Z
                    },
                ],
                ..Z
            }],
            ..Z
        },
    },
    Row {
        go: 37,
        yaml: "# One\n# Two\ntrue # Three\n# Four\n# Five\n",
        node: N {
            kind: Kind::Document,
            line: 3,
            column: 1,
            content: &[N {
                kind: Kind::Scalar,
                tag: "!!bool",
                value: "true",
                head: "# One\n# Two",
                line_comment: "# Three",
                foot: "# Four\n# Five",
                line: 3,
                column: 1,
                ..Z
            }],
            ..Z
        },
    },
    Row {
        go: 38,
        yaml: "# \u{161}\ntrue # \u{161}\n",
        node: N {
            kind: Kind::Document,
            line: 2,
            column: 1,
            content: &[N {
                kind: Kind::Scalar,
                tag: "!!bool",
                value: "true",
                head: "# \u{161}",
                line_comment: "# \u{161}",
                line: 2,
                column: 1,
                ..Z
            }],
            ..Z
        },
    },
    Row {
        go: 39,
        yaml: "[decode]\n# One\n\n# Two\n\n# Three\ntrue # Four\n# Five\n\n# Six\n\n# Seven\n",
        node: N {
            kind: Kind::Document,
            head: "# One\n\n# Two",
            foot: "# Six\n\n# Seven",
            line: 7,
            column: 1,
            content: &[N {
                kind: Kind::Scalar,
                tag: "!!bool",
                value: "true",
                head: "# Three",
                line_comment: "# Four",
                foot: "# Five",
                line: 7,
                column: 1,
                ..Z
            }],
            ..Z
        },
    },
    // Write out the pound character if missing from comments.
    Row {
        go: 40,
        yaml: "[encode]# One\n# Two\ntrue # Three\n# Four\n# Five\n",
        node: N {
            kind: Kind::Document,
            line: 3,
            column: 1,
            content: &[N {
                kind: Kind::Scalar,
                tag: "!!bool",
                value: "true",
                head: "One\nTwo\n",
                line_comment: "Three\n",
                foot: "Four\nFive\n",
                line: 3,
                column: 1,
                ..Z
            }],
            ..Z
        },
    },
    Row {
        go: 41,
        yaml: "[encode]#   One\n#   Two\ntrue #   Three\n#   Four\n#   Five\n",
        node: N {
            kind: Kind::Document,
            line: 3,
            column: 1,
            content: &[N {
                kind: Kind::Scalar,
                tag: "!!bool",
                value: "true",
                head: "  One\n  Two",
                line_comment: "  Three",
                foot: "  Four\n  Five",
                line: 3,
                column: 1,
                ..Z
            }],
            ..Z
        },
    },
    Row {
        go: 42,
        yaml: "# DH1\n\n# DH2\n\n# H1\n# H2\ntrue # I\n# F1\n# F2\n\n# DF1\n\n# DF2\n",
        node: N {
            kind: Kind::Document,
            head: "# DH1\n\n# DH2",
            foot: "# DF1\n\n# DF2",
            line: 7,
            column: 1,
            content: &[N {
                kind: Kind::Scalar,
                tag: "!!bool",
                value: "true",
                head: "# H1\n# H2",
                line_comment: "# I",
                foot: "# F1\n# F2",
                line: 7,
                column: 1,
                ..Z
            }],
            ..Z
        },
    },
    Row {
        go: 43,
        yaml: "# DH1\n\n# DH2\n\n# HA1\n# HA2\nka: va # IA\n# FA1\n# FA2\n\n# HB1\n# HB2\nkb: vb # IB\n# FB1\n# FB2\n\n# DF1\n\n# DF2\n",
        node: N {
            kind: Kind::Document,
            head: "# DH1\n\n# DH2",
            foot: "# DF1\n\n# DF2",
            line: 7,
            column: 1,
            content: &[N {
                kind: Kind::Mapping,
                tag: "!!map",
                line: 7,
                column: 1,
                content: &[
                    N {
                        kind: Kind::Scalar,
                        tag: "!!str",
                        value: "ka",
                        head: "# HA1\n# HA2",
                        foot: "# FA1\n# FA2",
                        line: 7,
                        column: 1,
                        ..Z
                    },
                    N {
                        kind: Kind::Scalar,
                        tag: "!!str",
                        value: "va",
                        line_comment: "# IA",
                        line: 7,
                        column: 5,
                        ..Z
                    },
                    N {
                        kind: Kind::Scalar,
                        tag: "!!str",
                        value: "kb",
                        head: "# HB1\n# HB2",
                        foot: "# FB1\n# FB2",
                        line: 13,
                        column: 1,
                        ..Z
                    },
                    N {
                        kind: Kind::Scalar,
                        tag: "!!str",
                        value: "vb",
                        line_comment: "# IB",
                        line: 13,
                        column: 5,
                        ..Z
                    },
                ],
                ..Z
            }],
            ..Z
        },
    },
    Row {
        go: 44,
        yaml: "# DH1\n\n# DH2\n\n# HA1\n# HA2\n- la # IA\n# FA1\n# FA2\n\n# HB1\n# HB2\n- lb # IB\n# FB1\n# FB2\n\n# DF1\n\n# DF2\n",
        node: N {
            kind: Kind::Document,
            head: "# DH1\n\n# DH2",
            foot: "# DF1\n\n# DF2",
            line: 7,
            column: 1,
            content: &[N {
                kind: Kind::Sequence,
                tag: "!!seq",
                line: 7,
                column: 1,
                content: &[
                    N {
                        kind: Kind::Scalar,
                        tag: "!!str",
                        value: "la",
                        head: "# HA1\n# HA2",
                        line_comment: "# IA",
                        foot: "# FA1\n# FA2",
                        line: 7,
                        column: 3,
                        ..Z
                    },
                    N {
                        kind: Kind::Scalar,
                        tag: "!!str",
                        value: "lb",
                        head: "# HB1\n# HB2",
                        line_comment: "# IB",
                        foot: "# FB1\n# FB2",
                        line: 13,
                        column: 3,
                        ..Z
                    },
                ],
                ..Z
            }],
            ..Z
        },
    },
    Row {
        go: 45,
        yaml: "# DH1\n\n- la # IA\n# HB1\n- lb\n",
        node: N {
            kind: Kind::Document,
            head: "# DH1",
            line: 3,
            column: 1,
            content: &[N {
                kind: Kind::Sequence,
                tag: "!!seq",
                line: 3,
                column: 1,
                content: &[
                    N {
                        kind: Kind::Scalar,
                        tag: "!!str",
                        value: "la",
                        line_comment: "# IA",
                        line: 3,
                        column: 3,
                        ..Z
                    },
                    N {
                        kind: Kind::Scalar,
                        tag: "!!str",
                        value: "lb",
                        head: "# HB1",
                        line: 5,
                        column: 3,
                        ..Z
                    },
                ],
                ..Z
            }],
            ..Z
        },
    },
    Row {
        go: 46,
        yaml: "- la # IA\n- lb # IB\n- lc # IC\n",
        node: N {
            kind: Kind::Document,
            line: 1,
            column: 1,
            content: &[N {
                kind: Kind::Sequence,
                tag: "!!seq",
                line: 1,
                column: 1,
                content: &[
                    N {
                        kind: Kind::Scalar,
                        tag: "!!str",
                        value: "la",
                        line_comment: "# IA",
                        line: 1,
                        column: 3,
                        ..Z
                    },
                    N {
                        kind: Kind::Scalar,
                        tag: "!!str",
                        value: "lb",
                        line_comment: "# IB",
                        line: 2,
                        column: 3,
                        ..Z
                    },
                    N {
                        kind: Kind::Scalar,
                        tag: "!!str",
                        value: "lc",
                        line_comment: "# IC",
                        line: 3,
                        column: 3,
                        ..Z
                    },
                ],
                ..Z
            }],
            ..Z
        },
    },
    Row {
        go: 47,
        yaml: "# DH1\n\n# HL1\n- - la\n  # HB1\n  - lb\n",
        node: N {
            kind: Kind::Document,
            head: "# DH1",
            line: 4,
            column: 1,
            content: &[N {
                kind: Kind::Sequence,
                tag: "!!seq",
                line: 4,
                column: 1,
                content: &[N {
                    kind: Kind::Sequence,
                    tag: "!!seq",
                    head: "# HL1",
                    line: 4,
                    column: 3,
                    content: &[
                        N {
                            kind: Kind::Scalar,
                            tag: "!!str",
                            value: "la",
                            line: 4,
                            column: 5,
                            ..Z
                        },
                        N {
                            kind: Kind::Scalar,
                            tag: "!!str",
                            value: "lb",
                            head: "# HB1",
                            line: 6,
                            column: 5,
                            ..Z
                        },
                    ],
                    ..Z
                }],
                ..Z
            }],
            ..Z
        },
    },
    Row {
        go: 48,
        yaml: "# DH1\n\n# HL1\n- # HA1\n  - la\n  # HB1\n  - lb\n",
        node: N {
            kind: Kind::Document,
            head: "# DH1",
            line: 4,
            column: 1,
            content: &[N {
                kind: Kind::Sequence,
                tag: "!!seq",
                line: 4,
                column: 1,
                content: &[N {
                    kind: Kind::Sequence,
                    tag: "!!seq",
                    head: "# HL1",
                    line: 5,
                    column: 3,
                    content: &[
                        N {
                            kind: Kind::Scalar,
                            tag: "!!str",
                            value: "la",
                            head: "# HA1",
                            line: 5,
                            column: 5,
                            ..Z
                        },
                        N {
                            kind: Kind::Scalar,
                            tag: "!!str",
                            value: "lb",
                            head: "# HB1",
                            line: 7,
                            column: 5,
                            ..Z
                        },
                    ],
                    ..Z
                }],
                ..Z
            }],
            ..Z
        },
    },
    Row {
        go: 49,
        yaml: "[decode]# DH1\n\n# HL1\n- # HA1\n\n  - la\n  # HB1\n  - lb\n",
        node: N {
            kind: Kind::Document,
            head: "# DH1",
            line: 4,
            column: 1,
            content: &[N {
                kind: Kind::Sequence,
                tag: "!!seq",
                line: 4,
                column: 1,
                content: &[N {
                    kind: Kind::Sequence,
                    tag: "!!seq",
                    head: "# HL1",
                    line: 6,
                    column: 3,
                    content: &[
                        N {
                            kind: Kind::Scalar,
                            tag: "!!str",
                            value: "la",
                            head: "# HA1\n",
                            line: 6,
                            column: 5,
                            ..Z
                        },
                        N {
                            kind: Kind::Scalar,
                            tag: "!!str",
                            value: "lb",
                            head: "# HB1",
                            line: 8,
                            column: 5,
                            ..Z
                        },
                    ],
                    ..Z
                }],
                ..Z
            }],
            ..Z
        },
    },
    Row {
        go: 50,
        yaml: "# DH1\n\n# HA1\nka:\n  # HB1\n  kb:\n    # HC1\n    # HC2\n    - lc # IC\n    # FC1\n    # FC2\n\n    # HD1\n    - ld # ID\n    # FD1\n\n# DF1\n",
        node: N {
            kind: Kind::Document,
            head: "# DH1",
            foot: "# DF1",
            line: 4,
            column: 1,
            content: &[N {
                kind: Kind::Mapping,
                tag: "!!map",
                line: 4,
                column: 1,
                content: &[
                    N {
                        kind: Kind::Scalar,
                        tag: "!!str",
                        value: "ka",
                        head: "# HA1",
                        line: 4,
                        column: 1,
                        ..Z
                    },
                    N {
                        kind: Kind::Mapping,
                        tag: "!!map",
                        line: 6,
                        column: 3,
                        content: &[
                            N {
                                kind: Kind::Scalar,
                                tag: "!!str",
                                value: "kb",
                                head: "# HB1",
                                line: 6,
                                column: 3,
                                ..Z
                            },
                            N {
                                kind: Kind::Sequence,
                                tag: "!!seq",
                                line: 9,
                                column: 5,
                                content: &[
                                    N {
                                        kind: Kind::Scalar,
                                        tag: "!!str",
                                        value: "lc",
                                        head: "# HC1\n# HC2",
                                        line_comment: "# IC",
                                        foot: "# FC1\n# FC2",
                                        line: 9,
                                        column: 7,
                                        ..Z
                                    },
                                    N {
                                        kind: Kind::Scalar,
                                        tag: "!!str",
                                        value: "ld",
                                        head: "# HD1",
                                        line_comment: "# ID",
                                        foot: "# FD1",
                                        line: 14,
                                        column: 7,
                                        ..Z
                                    },
                                ],
                                ..Z
                            },
                        ],
                        ..Z
                    },
                ],
                ..Z
            }],
            ..Z
        },
    },
    Row {
        go: 51,
        yaml: "# DH1\n\n# HA1\nka:\n  # HB1\n  kb:\n    # HC1\n    # HC2\n    - lc # IC\n    # FC1\n    # FC2\n\n    # HD1\n    - ld # ID\n    # FD1\nke: ve\n\n# DF1\n",
        node: N {
            kind: Kind::Document,
            head: "# DH1",
            foot: "# DF1",
            line: 4,
            column: 1,
            content: &[N {
                kind: Kind::Mapping,
                tag: "!!map",
                line: 4,
                column: 1,
                content: &[
                    N {
                        kind: Kind::Scalar,
                        tag: "!!str",
                        value: "ka",
                        head: "# HA1",
                        line: 4,
                        column: 1,
                        ..Z
                    },
                    N {
                        kind: Kind::Mapping,
                        tag: "!!map",
                        line: 6,
                        column: 3,
                        content: &[
                            N {
                                kind: Kind::Scalar,
                                tag: "!!str",
                                value: "kb",
                                head: "# HB1",
                                line: 6,
                                column: 3,
                                ..Z
                            },
                            N {
                                kind: Kind::Sequence,
                                tag: "!!seq",
                                line: 9,
                                column: 5,
                                content: &[
                                    N {
                                        kind: Kind::Scalar,
                                        tag: "!!str",
                                        value: "lc",
                                        head: "# HC1\n# HC2",
                                        line_comment: "# IC",
                                        foot: "# FC1\n# FC2",
                                        line: 9,
                                        column: 7,
                                        ..Z
                                    },
                                    N {
                                        kind: Kind::Scalar,
                                        tag: "!!str",
                                        value: "ld",
                                        head: "# HD1",
                                        line_comment: "# ID",
                                        foot: "# FD1",
                                        line: 14,
                                        column: 7,
                                        ..Z
                                    },
                                ],
                                ..Z
                            },
                        ],
                        ..Z
                    },
                    N {
                        kind: Kind::Scalar,
                        tag: "!!str",
                        value: "ke",
                        line: 16,
                        column: 1,
                        ..Z
                    },
                    N {
                        kind: Kind::Scalar,
                        tag: "!!str",
                        value: "ve",
                        line: 16,
                        column: 5,
                        ..Z
                    },
                ],
                ..Z
            }],
            ..Z
        },
    },
    Row {
        go: 52,
        yaml: "# DH1\n\n# DH2\n\n# HA1\n# HA2\nka:\n  # HB1\n  # HB2\n  kb:\n    # HC1\n    # HC2\n    kc:\n      # HD1\n      # HD2\n      kd: vd\n      # FD1\n      # FD2\n    # FC1\n    # FC2\n  # FB1\n  # FB2\n# FA1\n# FA2\n\n# HE1\n# HE2\nke: ve\n# FE1\n# FE2\n\n# DF1\n\n# DF2\n",
        node: N {
            kind: Kind::Document,
            head: "# DH1\n\n# DH2",
            foot: "# DF1\n\n# DF2",
            line: 7,
            column: 1,
            content: &[N {
                kind: Kind::Mapping,
                tag: "!!map",
                line: 7,
                column: 1,
                content: &[
                    N {
                        kind: Kind::Scalar,
                        tag: "!!str",
                        value: "ka",
                        head: "# HA1\n# HA2",
                        foot: "# FA1\n# FA2",
                        line: 7,
                        column: 1,
                        ..Z
                    },
                    N {
                        kind: Kind::Mapping,
                        tag: "!!map",
                        line: 10,
                        column: 3,
                        content: &[
                            N {
                                kind: Kind::Scalar,
                                tag: "!!str",
                                value: "kb",
                                head: "# HB1\n# HB2",
                                foot: "# FB1\n# FB2",
                                line: 10,
                                column: 3,
                                ..Z
                            },
                            N {
                                kind: Kind::Mapping,
                                tag: "!!map",
                                line: 13,
                                column: 5,
                                content: &[
                                    N {
                                        kind: Kind::Scalar,
                                        tag: "!!str",
                                        value: "kc",
                                        head: "# HC1\n# HC2",
                                        foot: "# FC1\n# FC2",
                                        line: 13,
                                        column: 5,
                                        ..Z
                                    },
                                    N {
                                        kind: Kind::Mapping,
                                        tag: "!!map",
                                        line: 16,
                                        column: 7,
                                        content: &[
                                            N {
                                                kind: Kind::Scalar,
                                                tag: "!!str",
                                                value: "kd",
                                                head: "# HD1\n# HD2",
                                                foot: "# FD1\n# FD2",
                                                line: 16,
                                                column: 7,
                                                ..Z
                                            },
                                            N {
                                                kind: Kind::Scalar,
                                                tag: "!!str",
                                                value: "vd",
                                                line: 16,
                                                column: 11,
                                                ..Z
                                            },
                                        ],
                                        ..Z
                                    },
                                ],
                                ..Z
                            },
                        ],
                        ..Z
                    },
                    N {
                        kind: Kind::Scalar,
                        tag: "!!str",
                        value: "ke",
                        head: "# HE1\n# HE2",
                        foot: "# FE1\n# FE2",
                        line: 28,
                        column: 1,
                        ..Z
                    },
                    N {
                        kind: Kind::Scalar,
                        tag: "!!str",
                        value: "ve",
                        line: 28,
                        column: 5,
                        ..Z
                    },
                ],
                ..Z
            }],
            ..Z
        },
    },
    // Same as above but indenting ke in so it's also part of ka's value.
    Row {
        go: 53,
        yaml: "# DH1\n\n# DH2\n\n# HA1\n# HA2\nka:\n  # HB1\n  # HB2\n  kb:\n    # HC1\n    # HC2\n    kc:\n      # HD1\n      # HD2\n      kd: vd\n      # FD1\n      # FD2\n    # FC1\n    # FC2\n  # FB1\n  # FB2\n\n  # HE1\n  # HE2\n  ke: ve\n  # FE1\n  # FE2\n# FA1\n# FA2\n\n# DF1\n\n# DF2\n",
        node: N {
            kind: Kind::Document,
            head: "# DH1\n\n# DH2",
            foot: "# DF1\n\n# DF2",
            line: 7,
            column: 1,
            content: &[N {
                kind: Kind::Mapping,
                tag: "!!map",
                line: 7,
                column: 1,
                content: &[
                    N {
                        kind: Kind::Scalar,
                        tag: "!!str",
                        value: "ka",
                        head: "# HA1\n# HA2",
                        foot: "# FA1\n# FA2",
                        line: 7,
                        column: 1,
                        ..Z
                    },
                    N {
                        kind: Kind::Mapping,
                        tag: "!!map",
                        line: 10,
                        column: 3,
                        content: &[
                            N {
                                kind: Kind::Scalar,
                                tag: "!!str",
                                value: "kb",
                                head: "# HB1\n# HB2",
                                foot: "# FB1\n# FB2",
                                line: 10,
                                column: 3,
                                ..Z
                            },
                            N {
                                kind: Kind::Mapping,
                                tag: "!!map",
                                line: 13,
                                column: 5,
                                content: &[
                                    N {
                                        kind: Kind::Scalar,
                                        tag: "!!str",
                                        value: "kc",
                                        head: "# HC1\n# HC2",
                                        foot: "# FC1\n# FC2",
                                        line: 13,
                                        column: 5,
                                        ..Z
                                    },
                                    N {
                                        kind: Kind::Mapping,
                                        tag: "!!map",
                                        line: 16,
                                        column: 7,
                                        content: &[
                                            N {
                                                kind: Kind::Scalar,
                                                tag: "!!str",
                                                value: "kd",
                                                head: "# HD1\n# HD2",
                                                foot: "# FD1\n# FD2",
                                                line: 16,
                                                column: 7,
                                                ..Z
                                            },
                                            N {
                                                kind: Kind::Scalar,
                                                tag: "!!str",
                                                value: "vd",
                                                line: 16,
                                                column: 11,
                                                ..Z
                                            },
                                        ],
                                        ..Z
                                    },
                                ],
                                ..Z
                            },
                            N {
                                kind: Kind::Scalar,
                                tag: "!!str",
                                value: "ke",
                                head: "# HE1\n# HE2",
                                foot: "# FE1\n# FE2",
                                line: 26,
                                column: 3,
                                ..Z
                            },
                            N {
                                kind: Kind::Scalar,
                                tag: "!!str",
                                value: "ve",
                                line: 26,
                                column: 7,
                                ..Z
                            },
                        ],
                        ..Z
                    },
                ],
                ..Z
            }],
            ..Z
        },
    },
    // Decode only due to lack of newline at the end.
    Row {
        go: 54,
        yaml: "[decode]# HA1\nka:\n  # HB1\n  kb: vb\n  # FB1\n# FA1",
        node: N {
            kind: Kind::Document,
            line: 2,
            column: 1,
            content: &[N {
                kind: Kind::Mapping,
                tag: "!!map",
                line: 2,
                column: 1,
                content: &[
                    N {
                        kind: Kind::Scalar,
                        tag: "!!str",
                        value: "ka",
                        head: "# HA1",
                        foot: "# FA1",
                        line: 2,
                        column: 1,
                        ..Z
                    },
                    N {
                        kind: Kind::Mapping,
                        tag: "!!map",
                        line: 4,
                        column: 3,
                        content: &[
                            N {
                                kind: Kind::Scalar,
                                tag: "!!str",
                                value: "kb",
                                head: "# HB1",
                                foot: "# FB1",
                                line: 4,
                                column: 3,
                                ..Z
                            },
                            N {
                                kind: Kind::Scalar,
                                tag: "!!str",
                                value: "vb",
                                line: 4,
                                column: 7,
                                ..Z
                            },
                        ],
                        ..Z
                    },
                ],
                ..Z
            }],
            ..Z
        },
    },
    // Same as above, but with newline at the end.
    Row {
        go: 55,
        yaml: "# HA1\nka:\n  # HB1\n  kb: vb\n  # FB1\n# FA1\n",
        node: N {
            kind: Kind::Document,
            line: 2,
            column: 1,
            content: &[N {
                kind: Kind::Mapping,
                tag: "!!map",
                line: 2,
                column: 1,
                content: &[
                    N {
                        kind: Kind::Scalar,
                        tag: "!!str",
                        value: "ka",
                        head: "# HA1",
                        foot: "# FA1",
                        line: 2,
                        column: 1,
                        ..Z
                    },
                    N {
                        kind: Kind::Mapping,
                        tag: "!!map",
                        line: 4,
                        column: 3,
                        content: &[
                            N {
                                kind: Kind::Scalar,
                                tag: "!!str",
                                value: "kb",
                                head: "# HB1",
                                foot: "# FB1",
                                line: 4,
                                column: 3,
                                ..Z
                            },
                            N {
                                kind: Kind::Scalar,
                                tag: "!!str",
                                value: "vb",
                                line: 4,
                                column: 7,
                                ..Z
                            },
                        ],
                        ..Z
                    },
                ],
                ..Z
            }],
            ..Z
        },
    },
    // Same as above, but without FB1.
    Row {
        go: 56,
        yaml: "# HA1\nka:\n  # HB1\n  kb: vb\n# FA1\n",
        node: N {
            kind: Kind::Document,
            line: 2,
            column: 1,
            content: &[N {
                kind: Kind::Mapping,
                tag: "!!map",
                line: 2,
                column: 1,
                content: &[
                    N {
                        kind: Kind::Scalar,
                        tag: "!!str",
                        value: "ka",
                        head: "# HA1",
                        foot: "# FA1",
                        line: 2,
                        column: 1,
                        ..Z
                    },
                    N {
                        kind: Kind::Mapping,
                        tag: "!!map",
                        line: 4,
                        column: 3,
                        content: &[
                            N {
                                kind: Kind::Scalar,
                                tag: "!!str",
                                value: "kb",
                                head: "# HB1",
                                line: 4,
                                column: 3,
                                ..Z
                            },
                            N {
                                kind: Kind::Scalar,
                                tag: "!!str",
                                value: "vb",
                                line: 4,
                                column: 7,
                                ..Z
                            },
                        ],
                        ..Z
                    },
                ],
                ..Z
            }],
            ..Z
        },
    },
    // Same as above, but with two newlines at the end. Decode-only for that.
    Row {
        go: 57,
        yaml: "[decode]# HA1\nka:\n  # HB1\n  kb: vb\n  # FB1\n# FA1\n\n",
        node: N {
            kind: Kind::Document,
            line: 2,
            column: 1,
            content: &[N {
                kind: Kind::Mapping,
                tag: "!!map",
                line: 2,
                column: 1,
                content: &[
                    N {
                        kind: Kind::Scalar,
                        tag: "!!str",
                        value: "ka",
                        head: "# HA1",
                        foot: "# FA1",
                        line: 2,
                        column: 1,
                        ..Z
                    },
                    N {
                        kind: Kind::Mapping,
                        tag: "!!map",
                        line: 4,
                        column: 3,
                        content: &[
                            N {
                                kind: Kind::Scalar,
                                tag: "!!str",
                                value: "kb",
                                head: "# HB1",
                                foot: "# FB1",
                                line: 4,
                                column: 3,
                                ..Z
                            },
                            N {
                                kind: Kind::Scalar,
                                tag: "!!str",
                                value: "vb",
                                line: 4,
                                column: 7,
                                ..Z
                            },
                        ],
                        ..Z
                    },
                ],
                ..Z
            }],
            ..Z
        },
    },
    // Similar to above, but make HB1 look more like a footer of ka.
    Row {
        go: 58,
        yaml: "[decode]# HA1\nka:\n# HB1\n\n  kb: vb\n# FA1\n",
        node: N {
            kind: Kind::Document,
            line: 2,
            column: 1,
            content: &[N {
                kind: Kind::Mapping,
                tag: "!!map",
                line: 2,
                column: 1,
                content: &[
                    N {
                        kind: Kind::Scalar,
                        tag: "!!str",
                        value: "ka",
                        head: "# HA1",
                        foot: "# FA1",
                        line: 2,
                        column: 1,
                        ..Z
                    },
                    N {
                        kind: Kind::Mapping,
                        tag: "!!map",
                        line: 5,
                        column: 3,
                        content: &[
                            N {
                                kind: Kind::Scalar,
                                tag: "!!str",
                                value: "kb",
                                head: "# HB1\n",
                                line: 5,
                                column: 3,
                                ..Z
                            },
                            N {
                                kind: Kind::Scalar,
                                tag: "!!str",
                                value: "vb",
                                line: 5,
                                column: 7,
                                ..Z
                            },
                        ],
                        ..Z
                    },
                ],
                ..Z
            }],
            ..Z
        },
    },
    Row {
        go: 59,
        yaml: "ka:\n  kb: vb\n# FA1\n\nkc: vc\n",
        node: N {
            kind: Kind::Document,
            line: 1,
            column: 1,
            content: &[N {
                kind: Kind::Mapping,
                tag: "!!map",
                line: 1,
                column: 1,
                content: &[
                    N {
                        kind: Kind::Scalar,
                        tag: "!!str",
                        value: "ka",
                        foot: "# FA1",
                        line: 1,
                        column: 1,
                        ..Z
                    },
                    N {
                        kind: Kind::Mapping,
                        tag: "!!map",
                        line: 2,
                        column: 3,
                        content: &[
                            N {
                                kind: Kind::Scalar,
                                tag: "!!str",
                                value: "kb",
                                line: 2,
                                column: 3,
                                ..Z
                            },
                            N {
                                kind: Kind::Scalar,
                                tag: "!!str",
                                value: "vb",
                                line: 2,
                                column: 7,
                                ..Z
                            },
                        ],
                        ..Z
                    },
                    N {
                        kind: Kind::Scalar,
                        tag: "!!str",
                        value: "kc",
                        line: 5,
                        column: 1,
                        ..Z
                    },
                    N {
                        kind: Kind::Scalar,
                        tag: "!!str",
                        value: "vc",
                        line: 5,
                        column: 5,
                        ..Z
                    },
                ],
                ..Z
            }],
            ..Z
        },
    },
    Row {
        go: 60,
        yaml: "ka:\n  kb: vb\n# HC1\nkc: vc\n",
        node: N {
            kind: Kind::Document,
            line: 1,
            column: 1,
            content: &[N {
                kind: Kind::Mapping,
                tag: "!!map",
                line: 1,
                column: 1,
                content: &[
                    N {
                        kind: Kind::Scalar,
                        tag: "!!str",
                        value: "ka",
                        line: 1,
                        column: 1,
                        ..Z
                    },
                    N {
                        kind: Kind::Mapping,
                        tag: "!!map",
                        line: 2,
                        column: 3,
                        content: &[
                            N {
                                kind: Kind::Scalar,
                                tag: "!!str",
                                value: "kb",
                                line: 2,
                                column: 3,
                                ..Z
                            },
                            N {
                                kind: Kind::Scalar,
                                tag: "!!str",
                                value: "vb",
                                line: 2,
                                column: 7,
                                ..Z
                            },
                        ],
                        ..Z
                    },
                    N {
                        kind: Kind::Scalar,
                        tag: "!!str",
                        value: "kc",
                        head: "# HC1",
                        line: 4,
                        column: 1,
                        ..Z
                    },
                    N {
                        kind: Kind::Scalar,
                        tag: "!!str",
                        value: "vc",
                        line: 4,
                        column: 5,
                        ..Z
                    },
                ],
                ..Z
            }],
            ..Z
        },
    },
    // Decode only due to empty line before HC1.
    Row {
        go: 61,
        yaml: "[decode]ka:\n  kb: vb\n\n# HC1\nkc: vc\n",
        node: N {
            kind: Kind::Document,
            line: 1,
            column: 1,
            content: &[N {
                kind: Kind::Mapping,
                tag: "!!map",
                line: 1,
                column: 1,
                content: &[
                    N {
                        kind: Kind::Scalar,
                        tag: "!!str",
                        value: "ka",
                        line: 1,
                        column: 1,
                        ..Z
                    },
                    N {
                        kind: Kind::Mapping,
                        tag: "!!map",
                        line: 2,
                        column: 3,
                        content: &[
                            N {
                                kind: Kind::Scalar,
                                tag: "!!str",
                                value: "kb",
                                line: 2,
                                column: 3,
                                ..Z
                            },
                            N {
                                kind: Kind::Scalar,
                                tag: "!!str",
                                value: "vb",
                                line: 2,
                                column: 7,
                                ..Z
                            },
                        ],
                        ..Z
                    },
                    N {
                        kind: Kind::Scalar,
                        tag: "!!str",
                        value: "kc",
                        head: "# HC1",
                        line: 5,
                        column: 1,
                        ..Z
                    },
                    N {
                        kind: Kind::Scalar,
                        tag: "!!str",
                        value: "vc",
                        line: 5,
                        column: 5,
                        ..Z
                    },
                ],
                ..Z
            }],
            ..Z
        },
    },
    // Decode-only due to empty lines around HC1.
    Row {
        go: 62,
        yaml: "[decode]ka:\n  kb: vb\n\n# HC1\n\nkc: vc\n",
        node: N {
            kind: Kind::Document,
            line: 1,
            column: 1,
            content: &[N {
                kind: Kind::Mapping,
                tag: "!!map",
                line: 1,
                column: 1,
                content: &[
                    N {
                        kind: Kind::Scalar,
                        tag: "!!str",
                        value: "ka",
                        line: 1,
                        column: 1,
                        ..Z
                    },
                    N {
                        kind: Kind::Mapping,
                        tag: "!!map",
                        line: 2,
                        column: 3,
                        content: &[
                            N {
                                kind: Kind::Scalar,
                                tag: "!!str",
                                value: "kb",
                                line: 2,
                                column: 3,
                                ..Z
                            },
                            N {
                                kind: Kind::Scalar,
                                tag: "!!str",
                                value: "vb",
                                line: 2,
                                column: 7,
                                ..Z
                            },
                        ],
                        ..Z
                    },
                    N {
                        kind: Kind::Scalar,
                        tag: "!!str",
                        value: "kc",
                        head: "# HC1\n",
                        line: 6,
                        column: 1,
                        ..Z
                    },
                    N {
                        kind: Kind::Scalar,
                        tag: "!!str",
                        value: "vc",
                        line: 6,
                        column: 5,
                        ..Z
                    },
                ],
                ..Z
            }],
            ..Z
        },
    },
    Row {
        go: 63,
        yaml: "ka: # IA\n  kb: # IB\n",
        node: N {
            kind: Kind::Document,
            line: 1,
            column: 1,
            content: &[N {
                kind: Kind::Mapping,
                tag: "!!map",
                line: 1,
                column: 1,
                content: &[
                    N {
                        kind: Kind::Scalar,
                        tag: "!!str",
                        value: "ka",
                        line_comment: "# IA",
                        line: 1,
                        column: 1,
                        ..Z
                    },
                    N {
                        kind: Kind::Mapping,
                        tag: "!!map",
                        line: 2,
                        column: 3,
                        content: &[
                            N {
                                kind: Kind::Scalar,
                                tag: "!!str",
                                value: "kb",
                                line_comment: "# IB",
                                line: 2,
                                column: 3,
                                ..Z
                            },
                            N {
                                kind: Kind::Scalar,
                                tag: "!!null",
                                line: 2,
                                column: 6,
                                ..Z
                            },
                        ],
                        ..Z
                    },
                ],
                ..Z
            }],
            ..Z
        },
    },
    Row {
        go: 64,
        yaml: "# HA1\nka:\n  # HB1\n  kb: vb\n  # FB1\n# HC1\n# HC2\nkc: vc\n# FC1\n# FC2\n",
        node: N {
            kind: Kind::Document,
            line: 2,
            column: 1,
            content: &[N {
                kind: Kind::Mapping,
                tag: "!!map",
                line: 2,
                column: 1,
                content: &[
                    N {
                        kind: Kind::Scalar,
                        tag: "!!str",
                        value: "ka",
                        head: "# HA1",
                        line: 2,
                        column: 1,
                        ..Z
                    },
                    N {
                        kind: Kind::Mapping,
                        tag: "!!map",
                        line: 4,
                        column: 3,
                        content: &[
                            N {
                                kind: Kind::Scalar,
                                tag: "!!str",
                                value: "kb",
                                head: "# HB1",
                                foot: "# FB1",
                                line: 4,
                                column: 3,
                                ..Z
                            },
                            N {
                                kind: Kind::Scalar,
                                tag: "!!str",
                                value: "vb",
                                line: 4,
                                column: 7,
                                ..Z
                            },
                        ],
                        ..Z
                    },
                    N {
                        kind: Kind::Scalar,
                        tag: "!!str",
                        value: "kc",
                        head: "# HC1\n# HC2",
                        foot: "# FC1\n# FC2",
                        line: 8,
                        column: 1,
                        ..Z
                    },
                    N {
                        kind: Kind::Scalar,
                        tag: "!!str",
                        value: "vc",
                        line: 8,
                        column: 5,
                        ..Z
                    },
                ],
                ..Z
            }],
            ..Z
        },
    },
    // Same as above, but decode only due to empty line between ka's value and kc's headers.
    Row {
        go: 65,
        yaml: "[decode]# HA1\nka:\n  # HB1\n  kb: vb\n  # FB1\n\n# HC1\n# HC2\nkc: vc\n# FC1\n# FC2\n",
        node: N {
            kind: Kind::Document,
            line: 2,
            column: 1,
            content: &[N {
                kind: Kind::Mapping,
                tag: "!!map",
                line: 2,
                column: 1,
                content: &[
                    N {
                        kind: Kind::Scalar,
                        tag: "!!str",
                        value: "ka",
                        head: "# HA1",
                        line: 2,
                        column: 1,
                        ..Z
                    },
                    N {
                        kind: Kind::Mapping,
                        tag: "!!map",
                        line: 4,
                        column: 3,
                        content: &[
                            N {
                                kind: Kind::Scalar,
                                tag: "!!str",
                                value: "kb",
                                head: "# HB1",
                                foot: "# FB1",
                                line: 4,
                                column: 3,
                                ..Z
                            },
                            N {
                                kind: Kind::Scalar,
                                tag: "!!str",
                                value: "vb",
                                line: 4,
                                column: 7,
                                ..Z
                            },
                        ],
                        ..Z
                    },
                    N {
                        kind: Kind::Scalar,
                        tag: "!!str",
                        value: "kc",
                        head: "# HC1\n# HC2",
                        foot: "# FC1\n# FC2",
                        line: 9,
                        column: 1,
                        ..Z
                    },
                    N {
                        kind: Kind::Scalar,
                        tag: "!!str",
                        value: "vc",
                        line: 9,
                        column: 5,
                        ..Z
                    },
                ],
                ..Z
            }],
            ..Z
        },
    },
    Row {
        go: 66,
        yaml: "# H1\n[la, lb] # I\n# F1\n",
        node: N {
            kind: Kind::Document,
            line: 2,
            column: 1,
            content: &[N {
                kind: Kind::Sequence,
                style: FLOW,
                tag: "!!seq",
                head: "# H1",
                line_comment: "# I",
                foot: "# F1",
                line: 2,
                column: 1,
                content: &[
                    N {
                        kind: Kind::Scalar,
                        tag: "!!str",
                        value: "la",
                        line: 2,
                        column: 2,
                        ..Z
                    },
                    N {
                        kind: Kind::Scalar,
                        tag: "!!str",
                        value: "lb",
                        line: 2,
                        column: 6,
                        ..Z
                    },
                ],
                ..Z
            }],
            ..Z
        },
    },
    Row {
        go: 67,
        yaml: "# DH1\n\n# SH1\n[\n  # HA1\n  la, # IA\n  # FA1\n\n  # HB1\n  lb, # IB\n  # FB1\n]\n# SF1\n\n# DF1\n",
        node: N {
            kind: Kind::Document,
            head: "# DH1",
            foot: "# DF1",
            line: 4,
            column: 1,
            content: &[N {
                kind: Kind::Sequence,
                style: FLOW,
                tag: "!!seq",
                head: "# SH1",
                foot: "# SF1",
                line: 4,
                column: 1,
                content: &[
                    N {
                        kind: Kind::Scalar,
                        tag: "!!str",
                        value: "la",
                        head: "# HA1",
                        line_comment: "# IA",
                        foot: "# FA1",
                        line: 6,
                        column: 3,
                        ..Z
                    },
                    N {
                        kind: Kind::Scalar,
                        tag: "!!str",
                        value: "lb",
                        head: "# HB1",
                        line_comment: "# IB",
                        foot: "# FB1",
                        line: 10,
                        column: 3,
                        ..Z
                    },
                ],
                ..Z
            }],
            ..Z
        },
    },
    // Same as above, but with extra newlines before FB1 and FB2
    Row {
        go: 68,
        yaml: "[decode]# DH1\n\n# SH1\n[\n  # HA1\n  la, # IA\n  # FA1\n\n  # HB1\n  lb, # IB\n\n\n  # FB1\n\n# FB2\n]\n# SF1\n\n# DF1\n",
        node: N {
            kind: Kind::Document,
            head: "# DH1",
            foot: "# DF1",
            line: 4,
            column: 1,
            content: &[N {
                kind: Kind::Sequence,
                style: FLOW,
                tag: "!!seq",
                head: "# SH1",
                foot: "# SF1",
                line: 4,
                column: 1,
                content: &[
                    N {
                        kind: Kind::Scalar,
                        tag: "!!str",
                        value: "la",
                        head: "# HA1",
                        line_comment: "# IA",
                        foot: "# FA1",
                        line: 6,
                        column: 3,
                        ..Z
                    },
                    N {
                        kind: Kind::Scalar,
                        tag: "!!str",
                        value: "lb",
                        head: "# HB1",
                        line_comment: "# IB",
                        foot: "# FB1\n\n# FB2",
                        line: 10,
                        column: 3,
                        ..Z
                    },
                ],
                ..Z
            }],
            ..Z
        },
    },
    Row {
        go: 69,
        yaml: "# DH1\n\n# SH1\n[\n  # HA1\n  la,\n  # FA1\n\n  # HB1\n  lb,\n  # FB1\n]\n# SF1\n\n# DF1\n",
        node: N {
            kind: Kind::Document,
            head: "# DH1",
            foot: "# DF1",
            line: 4,
            column: 1,
            content: &[N {
                kind: Kind::Sequence,
                style: FLOW,
                tag: "!!seq",
                head: "# SH1",
                foot: "# SF1",
                line: 4,
                column: 1,
                content: &[
                    N {
                        kind: Kind::Scalar,
                        tag: "!!str",
                        value: "la",
                        head: "# HA1",
                        foot: "# FA1",
                        line: 6,
                        column: 3,
                        ..Z
                    },
                    N {
                        kind: Kind::Scalar,
                        tag: "!!str",
                        value: "lb",
                        head: "# HB1",
                        foot: "# FB1",
                        line: 10,
                        column: 3,
                        ..Z
                    },
                ],
                ..Z
            }],
            ..Z
        },
    },
    Row {
        go: 70,
        yaml: "ka:\n  kb: [\n    # HA1\n    la,\n    # FA1\n\n    # HB1\n    lb,\n    # FB1\n  ]\n",
        node: N {
            kind: Kind::Document,
            line: 1,
            column: 1,
            content: &[N {
                kind: Kind::Mapping,
                tag: "!!map",
                line: 1,
                column: 1,
                content: &[
                    N {
                        kind: Kind::Scalar,
                        tag: "!!str",
                        value: "ka",
                        line: 1,
                        column: 1,
                        ..Z
                    },
                    N {
                        kind: Kind::Mapping,
                        tag: "!!map",
                        line: 2,
                        column: 3,
                        content: &[
                            N {
                                kind: Kind::Scalar,
                                tag: "!!str",
                                value: "kb",
                                line: 2,
                                column: 3,
                                ..Z
                            },
                            N {
                                kind: Kind::Sequence,
                                style: FLOW,
                                tag: "!!seq",
                                line: 2,
                                column: 7,
                                content: &[
                                    N {
                                        kind: Kind::Scalar,
                                        tag: "!!str",
                                        value: "la",
                                        head: "# HA1",
                                        foot: "# FA1",
                                        line: 4,
                                        column: 5,
                                        ..Z
                                    },
                                    N {
                                        kind: Kind::Scalar,
                                        tag: "!!str",
                                        value: "lb",
                                        head: "# HB1",
                                        foot: "# FB1",
                                        line: 8,
                                        column: 5,
                                        ..Z
                                    },
                                ],
                                ..Z
                            },
                        ],
                        ..Z
                    },
                ],
                ..Z
            }],
            ..Z
        },
    },
    Row {
        go: 71,
        yaml: "# DH1\n\n# MH1\n{\n  # HA1\n  ka: va, # IA\n  # FA1\n\n  # HB1\n  kb: vb, # IB\n  # FB1\n}\n# MF1\n\n# DF1\n",
        node: N {
            kind: Kind::Document,
            head: "# DH1",
            foot: "# DF1",
            line: 4,
            column: 1,
            content: &[N {
                kind: Kind::Mapping,
                style: FLOW,
                tag: "!!map",
                head: "# MH1",
                foot: "# MF1",
                line: 4,
                column: 1,
                content: &[
                    N {
                        kind: Kind::Scalar,
                        tag: "!!str",
                        value: "ka",
                        head: "# HA1",
                        foot: "# FA1",
                        line: 6,
                        column: 3,
                        ..Z
                    },
                    N {
                        kind: Kind::Scalar,
                        tag: "!!str",
                        value: "va",
                        line_comment: "# IA",
                        line: 6,
                        column: 7,
                        ..Z
                    },
                    N {
                        kind: Kind::Scalar,
                        tag: "!!str",
                        value: "kb",
                        head: "# HB1",
                        foot: "# FB1",
                        line: 10,
                        column: 3,
                        ..Z
                    },
                    N {
                        kind: Kind::Scalar,
                        tag: "!!str",
                        value: "vb",
                        line_comment: "# IB",
                        line: 10,
                        column: 7,
                        ..Z
                    },
                ],
                ..Z
            }],
            ..Z
        },
    },
    Row {
        go: 72,
        yaml: "# DH1\n\n# MH1\n{\n  # HA1\n  ka: va,\n  # FA1\n\n  # HB1\n  kb: vb,\n  # FB1\n}\n# MF1\n\n# DF1\n",
        node: N {
            kind: Kind::Document,
            head: "# DH1",
            foot: "# DF1",
            line: 4,
            column: 1,
            content: &[N {
                kind: Kind::Mapping,
                style: FLOW,
                tag: "!!map",
                head: "# MH1",
                foot: "# MF1",
                line: 4,
                column: 1,
                content: &[
                    N {
                        kind: Kind::Scalar,
                        tag: "!!str",
                        value: "ka",
                        head: "# HA1",
                        foot: "# FA1",
                        line: 6,
                        column: 3,
                        ..Z
                    },
                    N {
                        kind: Kind::Scalar,
                        tag: "!!str",
                        value: "va",
                        line: 6,
                        column: 7,
                        ..Z
                    },
                    N {
                        kind: Kind::Scalar,
                        tag: "!!str",
                        value: "kb",
                        head: "# HB1",
                        foot: "# FB1",
                        line: 10,
                        column: 3,
                        ..Z
                    },
                    N {
                        kind: Kind::Scalar,
                        tag: "!!str",
                        value: "vb",
                        line: 10,
                        column: 7,
                        ..Z
                    },
                ],
                ..Z
            }],
            ..Z
        },
    },
    Row {
        go: 73,
        yaml: "# DH1\n\n# DH2\n\n# HA1\n# HA2\n- &x la # IA\n# FA1\n# FA2\n\n# HB1\n# HB2\n- *x # IB\n# FB1\n# FB2\n\n# DF1\n\n# DF2\n",
        node: N {
            kind: Kind::Document,
            head: "# DH1\n\n# DH2",
            foot: "# DF1\n\n# DF2",
            line: 7,
            column: 1,
            content: &[N {
                kind: Kind::Sequence,
                tag: "!!seq",
                line: 7,
                column: 1,
                content: &[
                    N {
                        kind: Kind::Scalar,
                        tag: "!!str",
                        value: "la",
                        anchor: "x",
                        head: "# HA1\n# HA2",
                        line_comment: "# IA",
                        foot: "# FA1\n# FA2",
                        line: 7,
                        column: 3,
                        ..Z
                    },
                    N {
                        kind: Kind::Alias,
                        value: "x",
                        alias: Some(&[0, 0]),
                        head: "# HB1\n# HB2",
                        line_comment: "# IB",
                        foot: "# FB1\n# FB2",
                        line: 13,
                        column: 3,
                        ..Z
                    },
                ],
                ..Z
            }],
            ..Z
        },
    },
];

// parity: adapted: nodeTests[7] is skipped (its node value is not valid UTF-8; NodeData::value is a String)
#[test]
fn test_node_roundtrip() {
    // Go: node_test.go TestNodeRoundtrip
    for item in NODE_TESTS {
        let i = item.go;
        let mut decode = true;
        let mut encode = true;

        let mut test_yaml = item.yaml;
        if let Some(s) = test_yaml.strip_prefix("[decode]") {
            encode = false;
            test_yaml = s;
        }
        if let Some(s) = test_yaml.strip_prefix("[encode]") {
            decode = false;
            test_yaml = s;
        }

        let expected = build(&item.node);
        if decode {
            let node = unmarshal_node(test_yaml.as_bytes())
                .unwrap_or_else(|e| panic!("test {i}: {test_yaml:?}: {e}"));
            assert_eq!(canon(&node), canon(&expected), "test {i}: {test_yaml:?}");
        }
        if encode {
            let node = expected.clone();
            assert_eq!(
                encode_indent2(&node),
                test_yaml,
                "test {i}: {:?}",
                item.yaml
            );

            // Ensure there were no mutations to the tree.
            assert_eq!(node, expected, "test {i}: {:?}", item.yaml);
        }
    }
}

/// One `setStringTests` row.
struct SetStringRow {
    str: &'static str,
    yaml: &'static str,
    node: N,
}

/// Go: `setStringTests`.
static SET_STRING_TESTS: &[SetStringRow] = &[
    SetStringRow {
        str: "something simple",
        yaml: "something simple\n",
        node: N {
            kind: Kind::Scalar,
            value: "something simple",
            tag: "!!str",
            ..Z
        },
    },
    SetStringRow {
        str: "\"quoted value\"",
        yaml: "'\"quoted value\"'\n",
        node: N {
            kind: Kind::Scalar,
            value: "\"quoted value\"",
            tag: "!!str",
            ..Z
        },
    },
    SetStringRow {
        str: "multi\nline",
        yaml: "|-\n  multi\n  line\n",
        node: N {
            kind: Kind::Scalar,
            value: "multi\nline",
            tag: "!!str",
            style: LITERAL,
            ..Z
        },
    },
    SetStringRow {
        str: "123",
        yaml: "\"123\"\n",
        node: N {
            kind: Kind::Scalar,
            value: "123",
            tag: "!!str",
            ..Z
        },
    },
    SetStringRow {
        str: "multi\nline\n",
        yaml: "|\n  multi\n  line\n",
        node: N {
            kind: Kind::Scalar,
            value: "multi\nline\n",
            tag: "!!str",
            style: LITERAL,
            ..Z
        },
    },
    // Skipped ("\x80\x81\x82" → "!!binary gIGC\n", `Value: "gIGC", Tag: "!!binary"`): the
    // input is not valid UTF-8, and `NodeData::set_string` takes a Rust `&str`, so Go's
    // `!!binary` branch of `SetString` cannot occur (Divergence, ledger 6c-1).
];

// parity: adapted: setStringTests[5] is skipped (its input is not valid UTF-8; set_string takes a &str)
#[test]
fn test_set_string() {
    // Go: node_test.go TestSetString
    for (i, item) in SET_STRING_TESTS.iter().enumerate() {
        let mut node = NodeData::default();

        node.set_string(item.str);

        assert_eq!(node, node_data(&item.node), "test {i}: {:?}", item.str);

        assert_eq!(
            encode_indent2(&build(&item.node)),
            item.yaml,
            "test {i}: {:?}",
            item.str
        );

        unmarshal_node(item.yaml.as_bytes())
            .unwrap_or_else(|e| panic!("test {i}: {:?}: {e}", item.yaml));

        let node = Document::new(node);
        let str: String = from_node(&node, node.root(), &Options::default())
            .unwrap_or_else(|e| panic!("test {i}: {:?}: {e}", item.str));
        assert_eq!(str, item.str, "test {i}");
    }
}

/// One `nodeEncodeDecodeTests` row (`yaml` is unused by the Go test, kept as in Go).
struct EncodeDecodeRow {
    value: GoValue,
    #[allow(dead_code)] // Go's table carries it; TestNodeEncodeDecode never reads it.
    yaml: &'static str,
    node: N,
}

/// Go: `nodeEncodeDecodeTests` (a function: `GoValue` literals are not `const`).
fn node_encode_decode_tests() -> Vec<EncodeDecodeRow> {
    vec![
        EncodeDecodeRow {
            value: GoValue::String("something simple".to_string()),
            yaml: "something simple\n",
            node: N {
                kind: Kind::Scalar,
                value: "something simple",
                tag: "!!str",
                ..Z
            },
        },
        EncodeDecodeRow {
            value: GoValue::String("\"quoted value\"".to_string()),
            yaml: "'\"quoted value\"'\n",
            node: N {
                kind: Kind::Scalar,
                style: SINGLE,
                value: "\"quoted value\"",
                tag: "!!str",
                ..Z
            },
        },
        EncodeDecodeRow {
            value: GoValue::Int(123),
            yaml: "123",
            node: N {
                kind: Kind::Scalar,
                value: "123",
                tag: "!!int",
                ..Z
            },
        },
        EncodeDecodeRow {
            value: GoValue::Seq(vec![GoValue::Int(1), GoValue::Int(2)]),
            yaml: "[1, 2]",
            node: N {
                kind: Kind::Sequence,
                tag: "!!seq",
                content: &[
                    N {
                        kind: Kind::Scalar,
                        value: "1",
                        tag: "!!int",
                        ..Z
                    },
                    N {
                        kind: Kind::Scalar,
                        value: "2",
                        tag: "!!int",
                        ..Z
                    },
                ],
                ..Z
            },
        },
        EncodeDecodeRow {
            value: GoValue::Map(vec![("a".to_string(), GoValue::String("b".to_string()))]),
            yaml: "a: b",
            node: N {
                kind: Kind::Mapping,
                tag: "!!map",
                content: &[
                    N {
                        kind: Kind::Scalar,
                        value: "a",
                        tag: "!!str",
                        ..Z
                    },
                    N {
                        kind: Kind::Scalar,
                        value: "b",
                        tag: "!!str",
                        ..Z
                    },
                ],
                ..Z
            },
        },
    ]
}

#[test]
fn test_node_encode_decode() {
    // Go: node_test.go TestNodeEncodeDecode
    for (i, item) in node_encode_decode_tests().into_iter().enumerate() {
        let node = build(&item.node);
        let v: GoValue = from_node(&node, node.root(), &Options::default())
            .unwrap_or_else(|e| panic!("test {i}: {e}"));
        assert_eq!(v, item.value, "test {i}");

        let n =
            to_node(&item.value, &Options::default()).unwrap_or_else(|e| panic!("test {i}: {e}"));
        assert_eq!(canon(&n), canon(&node), "test {i}");
    }
}

/// Go's `struct{}` (the target of `var v *struct{}`).
#[derive(Debug, Deserialize)]
struct Empty {}

#[test]
fn test_node_zero_encode_decode() {
    // Go: node_test.go TestNodeZeroEncodeDecode
    // Zero node value behaves as nil when encoding...
    let mut n = Document::default();
    let data = marshal_node(&n, n.root()).expect("Marshal");
    assert_eq!(String::from_utf8(data).expect("UTF-8"), "null\n");

    // ... and decoding.
    let v: Option<Empty> = from_node(&n, n.root(), &Options::default()).expect("Decode");
    assert!(v.is_none(), "{v:?}");

    // ... and even when looking for its tag.
    assert_eq!(n.short_tag(n.root()), "!!null");

    // Kind zero is still unknown, though.
    let root = n.root();
    n[root].line = 1;
    let err = marshal_node(&n, n.root()).expect_err("Marshal of a kind-0 node");
    assert_eq!(
        err.to_string(),
        "yaml: cannot encode node with unknown kind 0"
    );
    let err = from_node::<Option<Empty>>(&n, n.root(), &Options::default())
        .expect_err("Decode of a kind-0 node");
    assert_eq!(
        err.to_string(),
        "yaml: cannot decode node with unknown kind 0"
    );
}

/// Go's `struct { A int; B yaml.Node ",omitempty" }`.
#[derive(Serialize)]
struct OmitEmpty {
    a: i64,
    #[serde(skip_serializing_if = "node_is_zero")]
    b: Document,
}

/// Go's `(*Node).IsZero`, which `,omitempty` calls through the `IsZeroer` interface.
fn node_is_zero(doc: &Document) -> bool {
    doc[doc.root()].is_zero()
}

// parity: adapted: `,omitempty` on the yaml.Node field is `skip_serializing_if` calling Node.IsZero (Go reaches it through the IsZeroer interface)
#[test]
fn test_node_omit_empty() {
    // Go: node_test.go TestNodeOmitEmpty
    let mut v = OmitEmpty {
        a: 1,
        b: Document::default(),
    };
    let data = marshal(&v, &Options::default()).expect("Marshal");
    assert_eq!(String::from_utf8(data).expect("UTF-8"), "a: 1\n");

    let root = v.b.root();
    v.b[root].line = 1;
    let err = marshal(&v, &Options::default()).expect_err("Marshal of a kind-0 node");
    assert_eq!(
        err.to_string(),
        "yaml: cannot encode node with unknown kind 0"
    );
}
