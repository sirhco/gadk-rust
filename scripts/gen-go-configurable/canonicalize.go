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

package main

// canonicalizeInputs feed the vendored modifyString and
// canonicalizeJSONSubstrings (SCENARIO.md "Canonicalize table").
var canonicalizeInputs = []string{
	``,
	`no json here`,
	`{"b": 1, "a": 2}`,
	`  {"a" : [ ] }  `,
	`[1, 2, 3]`,
	`text {} and []`,
	`prefix {"z": [1, 2], "a": {"y": 1, "x": 2}} suffix`,
	`{"a": 1} and {"b": 2}`,
	`{"a": 1} and [2]`,
	"{\"a\":\n 1}",
	"line one {\"b\": 1, \"a\": 2}\nline two {\"d\": 1, \"c\": 2}",
	`{not json}`,
	`{"a": NaN}`,
	`{"dup": 1, "dup": 2}`,
	`{"n": 1.0, "big": 12345678901234567890, "e": 1e3, "neg": -0.0, "small": 1e-7, "f": 0.1}`,
	`{"html": "<b>&</b>"}`,
	`{"line": "a` + "\u2028" + `b", "ctl": "\u0001", "esc": "\/"}`,
	`unicode {"é": "ü", "emoji": "😀", "cjk": "漢字"}`,
	`{"a": "it's"}`,
	`parameters: {'a': 'x', 'b': None, 'c': True, 'd': False}`,
	`result: [True, False, None]`,
	`Parameters: {'k': 1}`,
	`RESULT:   {'a': 'b'}`,
	`result: {"x": null}`,
	`parameters:{"b":2,"a":1}`,
	`parameters: {'a': 1} result: {'b': 2}`,
	`parameters: not json {'a': 1}`,
	`parameters: {'a': 'O'Malley'}`,
	`O'Malley said {'a': 'b'}`,
	`TrueValue None_x {"a": True, "b": None}`,
	`{'k': 'éTrue', 'm': 'ÄNone', 'n': 'éNone'}`,
	`{"k": "xTrue", "m": "_None", "n": "True_"}`,
	"parameters:\u00a0{'a': 1}",
	"parameters:\t{'a': 1}",
	`{'nested': {'list': [{'x': None}, True]}, 'num': 1.50}`,
	`For context: [agent] called tool ` + "`get_weather`" + ` with parameters: {'city': 'Paris', 'days': 3}`,
	`For context: [agent] ` + "`get_weather`" + ` tool returned result: {'temp': 21.5, 'ok': True}`,
	`{"a": "\ud83d\ude00", "b": "\u00e9"}`,
	`{"a": "x"}}`,
	`[[{"a": 1}]]`,
}

type canonicalizeRow struct {
	Input  string `json:"input"`
	Modify string `json:"modify"`
	Output string `json:"output"`
}

func writeCanonicalize(path string) (int, error) {
	rows := make([]canonicalizeRow, 0, len(canonicalizeInputs))
	for _, in := range canonicalizeInputs {
		rows = append(rows, canonicalizeRow{in, modifyString(in), canonicalizeJSONSubstrings(in)})
	}
	return len(rows), writeJSON(path, rows)
}
