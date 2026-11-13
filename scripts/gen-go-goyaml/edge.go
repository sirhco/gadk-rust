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

package main

// The spec R11 edge corpus, the reduced limit_test.go inputs, the alias-ratio
// boundary cases and the two conformance-style recordings files.

import (
	"fmt"
	"strings"
	"unicode/utf16"
)

// edgeCorpus returns every hand-written parse case, in this order: edge-*,
// limit-*, alias-*, recordings-*.
func edgeCorpus() []corpusCase {
	var cases []corpusCase
	add := func(name, data string) { cases = append(cases, corpusCase{name, []byte(data)}) }

	// Quoting-sensitive strings: each value plain in a file of its own
	// (edge-plain-NN: resolved by the parser, or a syntax error), then all of
	// them double-quoted and single-quoted.
	sensitive := []string{
		"true", "True", "TRUE", "false", "yes", "no", "on", "off", "y", "n",
		"null", "Null", "NULL", "~", "", " lead", "trail ", "a: b", "a #b", "#x",
		"- x", "? x", "@at", "`tick", "|", ">", "%p", "!t", "&a", "*a",
		"[x", "{x", "]", "}", ",", "0o17", "017", "0x1F", "0b101", "1e3",
		"1_000", "+1", "-0", ".5", "1.", "-.inf", ".NaN", "+.INF", "2001-12-14",
		"2001-12-14t21:59:43.10-05:00", "1:20", "<<", "=", "-", "--", "---", "...",
		"a\\b", "tab\there", "x:y", "x: ", "'", "\"", "it's", "say \"hi\"",
	}
	var dq, sq strings.Builder
	for i, s := range sensitive {
		add(fmt.Sprintf("edge-plain-%02d", i), fmt.Sprintf("k: %s\n", s))
		fmt.Fprintf(&dq, "k%02d: %q\n", i, s)
		fmt.Fprintf(&sq, "k%02d: '%s'\n", i, strings.ReplaceAll(s, "'", "''"))
	}
	add("edge-quote-double", dq.String())
	add("edge-quote-single", sq.String())
	add("edge-quote-flow", "[true, 'true', \"true\", null, '', \"\", ~, 1, '1', 0x10, \"a, b\", 'c: d', \"e\\tf\"]\n")
	add("edge-quote-keys", "true: a\n'true': b\n1: c\n'1': d\n~: e\n\"\": f\n? |\n  multi\n  key\n: g\n")

	// Multi-line scalars.
	add("edge-multiline-literal", "a: |\n  one\n  two\n\nb: |-\n  one\n  two\n\nc: |+\n  one\n  two\n\nd: |2\n    indented\n  base\n")
	add("edge-multiline-folded", "a: >\n  one\n  two\n\n  three\n    more\n  four\nb: >-\n  x\n  y\nc: >+\n  keep\n\n")
	add("edge-multiline-plain", "a: one\n  two\n\n  three\nb: - not a seq\n  continued\n")
	add("edge-multiline-double", "a: \"one\n  two\\\n  three\n\n  four\"\nb: \"trailing  \\\n  space\"\n")
	add("edge-multiline-single", "a: 'one\n  two\n\n  three'\n")
	add("edge-multiline-trailing-space", "a: |\n  x  \n  y\t\n")
	add("edge-multiline-in-seq", "- |\n  a\n  b\n- >\n  c\n  d\n- \"e\\nf\"\n")
	add("edge-multiline-doc", "--- |\n  literal document\n  body\n")

	// Unicode.
	add("edge-unicode", "é: café\n世界: こんにちは\nemoji: 😀\ncombining: e\u0301\nrtl: שלום\nwide: \"\\u00e9\\U0001F600\\x41\"\n")
	add("edge-unicode-escapes", "a: \"\\0\\a\\b\\t\\n\\v\\f\\r\\e\\ \\\"\\/\\\\\\N\\_\\L\\P\"\nb: \"\\x85\\u2028\\u2029\\ufeff\"\n")
	add("edge-unicode-bom", "\ufeffa: 1\n")
	add("edge-unicode-nonprintable", "a: \"\u0085\u00a0\u2028\"\nb: \u00a0x\n")
	add("edge-utf16le-bom", utf16Bytes("a: é\nb: [1, 😀]\n", false))
	add("edge-utf16be-bom", utf16Bytes("a: é\nb: [1, 😀]\n", true))

	// Non-UTF-8 and control characters.
	add("edge-nonutf8-invalid", "a: \xc3\x28\n")
	add("edge-nonutf8-leading", "a: \x80\n")
	add("edge-nonutf8-truncated", "a: \xe2\x82")
	add("edge-nonutf8-overlong", "a: \xc0\xaf\n")
	add("edge-nonutf8-surrogate", "a: \xed\xa0\x80\n")
	add("edge-control-char", "a: \x01\n")
	add("edge-control-del", "a: x\x7fy\n")
	add("edge-binary-nonutf8", "a: !!binary gIGC\nb: !!binary |\n  /+7/\n")
	add("edge-binary-utf8", "a: !!binary aGVsbG8=\nb: !!binary \"\"\n")
	add("edge-binary-invalid", "a: !!binary 'not base64!'\n")

	// Very long lines (the default width is unlimited).
	add("edge-long-plain", "a: "+strings.Repeat("word ", 2000)+"end\n")
	add("edge-long-double", "a: \""+strings.Repeat("x", 10000)+"\"\n")
	add("edge-long-spaced-double", "a: \""+strings.Repeat("two words ", 1000)+"\"\n")
	add("edge-long-single", "a: '"+strings.Repeat("ab cd ", 1500)+"'\n")
	add("edge-long-key-ok", strings.Repeat("k", 1000)+": v\n")
	add("edge-long-key-too-long", strings.Repeat("k", 1100)+": v\n")
	add("edge-long-flow", "["+strings.Repeat("item, ", 2000)+"last]\n")

	// Comments in every position.
	add("edge-comment-head-doc", "# head of document\n\n# head of a\na: 1\n")
	add("edge-comment-line", "a: 1 # line a\nb: # line b key\n  c: 2 # line c\n")
	add("edge-comment-foot", "a:\n  b: 1\n  # foot of b\n\n# foot of a\nc: 2\n")
	add("edge-comment-seq", "# head\n- a # line a\n# between\n- b\n  # foot b\n- c\n# tail\n")
	add("edge-comment-flow", "a: [1, # one\n  2] # after\nb: {x: 1, # x\n  y: 2}\n")
	add("edge-comment-doc-markers", "# before\n--- # on start\na: 1\n... # on end\n# after end\n")
	add("edge-comment-only", "# only a comment\n")
	add("edge-comment-block-scalar", "a: | # on indicator\n  text\n# after scalar\nb: > # folded\n  more\n")
	add("edge-comment-nested-foot", "a:\n  b:\n    c: 1\n    # foot c\n  # foot b\n# foot a\n")
	add("edge-comment-indented", "a:\n    # indented head\n  b: 1\n")
	add("edge-comment-key-value-split", "a:\n  # head of value\n  b\n")
	add("edge-comment-empty-lines", "a: 1\n\n\n# after blanks\n\nb: 2\n")
	add("edge-comment-hash-in-value", "a: b#c\nd: 'e # f'\ng: h # i\n")
	add("edge-comment-seq-of-maps", "- a: 1 # one\n  # inner foot\n  b: 2\n# outer\n- c: 3\n")

	// Anchors, aliases and merges.
	add("edge-anchor-scalar", "a: &x 1\nb: *x\n")
	add("edge-anchor-collections", "a: &m {k: v}\nb: &s [1, 2]\nc: *m\nd: *s\n")
	add("edge-anchor-key", "&k key: value\n*k : other\n")
	add("edge-anchor-redefined", "a: &x 1\nb: *x\nc: &x 2\nd: *x\n")
	add("edge-anchor-self-map", "a: &a\n  b: *a\n")
	add("edge-anchor-self-seq", "a: &a [1, *a]\n")
	add("edge-anchor-unknown", "a: *nope\n")
	add("edge-anchor-unused", "a: &unused 1\n")
	add("edge-anchor-on-doc-value", "--- &top\na: 1\n")
	add("edge-merge-single", "base: &b {x: 1, y: 2}\nderived:\n  <<: *b\n  y: 3\n")
	add("edge-merge-list", "a: &a {x: 1}\nb: &b {x: 2, y: 2}\nc:\n  <<: [*a, *b]\n  z: 3\n")
	add("edge-merge-inline", "a:\n  <<: {x: 1}\n  y: 2\n")
	add("edge-merge-invalid", "a: &a [1]\nb:\n  <<: *a\n")
	add("edge-merge-scalar", "b:\n  <<: 1\n")
	add("edge-merge-tagged", "a: &a {x: 1}\nb:\n  !!merge <<: *a\n")
	add("edge-alias-in-seq", "- &a x\n- *a\n- [*a, *a]\n")

	// Flow and block styles.
	add("edge-flow-nested", "a: {b: [1, 2, {c: d}], e: {}}\nf: []\n")
	add("edge-flow-trailing-comma", "a: [1, 2, ]\nb: {x: 1, }\n")
	add("edge-flow-single-pair", "a: [x: 1, y: 2]\n")
	add("edge-flow-explicit-key", "{? a : b, ? c}\n")
	add("edge-flow-multiline", "a: [\n  1,\n  2\n]\nb: {\n  x: 1\n}\n")
	add("edge-flow-quoted-keys", "{\"a\": 1, 'b': 2, c: 3}\n")
	add("edge-flow-root-seq", "[a, b, c]\n")
	add("edge-block-explicit-key", "? a\n: b\n? - c\n  - d\n: e\n")
	add("edge-block-empty-values", "a:\nb: ~\nc:\n- \n-\n")
	add("edge-block-seq-unindented", "a:\n- 1\n- 2\nb: 3\n")
	add("edge-block-seq-of-seqs", "- - a\n  - b\n- - c\n")
	add("edge-block-deep", nestedBlock(20))
	add("edge-flow-deep", strings.Repeat("[", 20)+"x"+strings.Repeat("]", 20)+"\n")
	add("edge-block-indent-3", "a:\n   b:\n      c: 1\n   d: 2\n")
	add("edge-complex-key", "? [a, b]\n: c\n? {x: 1}\n: d\n")

	// Multi-document streams.
	add("edge-stream-two", "a: 1\n---\nb: 2\n")
	add("edge-stream-explicit", "---\na: 1\n...\n---\nb: 2\n...\n")
	add("edge-stream-empty-docs", "---\n---\n...\n---\n")
	add("edge-stream-directives", "%YAML 1.1\n---\na: 1\n...\n%YAML 1.1\n---\nb: 2\n")
	add("edge-stream-tag-directive", "%TAG !e! tag:example.com,2000:\n---\na: !e!foo bar\n")
	add("edge-stream-error-second", "a: 1\n---\n[\n")
	add("edge-stream-scalars", "--- one\n--- two\n--- |\n  three\n")
	add("edge-stream-comments", "# c1\na: 1\n# c2\n---\n# c3\nb: 2\n")
	add("edge-doc-start-inline", "--- {a: 1}\n")
	add("edge-doc-end-only", "...\n")

	// Tags and resolution.
	add("edge-tags", "a: !!str 1\nb: !!int \"2\"\nc: !custom x\nd: !<tag:yaml.org,2002:str> y\ne: !!float 1\nf: !!null ''\ng: !!map {}\nh: !!seq []\ni: !local {k: v}\n")
	add("edge-tag-bad-bool", "a: !!bool yes\n")
	add("edge-tag-bad-int", "a: !!int abc\n")
	add("edge-tag-timestamp", "a: !!timestamp 2001-12-14\nb: !!timestamp '2001-12-14T21:59:43Z'\n")
	add("edge-tag-unknown-handle", "a: !x!y z\n")
	add("edge-ints", "a: 0\nb: -17\nc: 0x1F\nd: 0o17\ne: 017\nf: 0b101\ng: 1_000\nh: 9223372036854775807\ni: 9223372036854775808\nj: 18446744073709551615\nk: 18446744073709551616\nl: -9223372036854775808\nm: -9223372036854775809\nn: +12\no: -0x1A\np: 0B11\n")
	add("edge-floats", "a: 1.5\nb: -0.0\nc: 1e3\nd: 1.\ne: .5\nf: +.inf\ng: -.Inf\nh: .NAN\ni: 6.8523015e+5\nj: 685.230_15e+03\nk: 190:20:30.15\nl: 1e400\nm: 4.9e-324\nn: 0.1\n")
	add("edge-bools-nulls", "a: true\nb: True\nc: TRUE\nd: false\ne: ~\nf: null\ng: NULL\nh:\ni: yes\nj: Off\n")
	add("edge-timestamps", "a: 2001-12-14\nb: 2001-12-14t21:59:43.10-05:00\nc: 2001-12-14 21:59:43.10 -5\nd: 2001-12-15T02:59:43.1Z\ne: 2001-12-15 2:59:43.10\nf: 2002-12-14T21:59:43.123456789+08:00\ng: 2001-1-1\nh: 2001-12-14T21:59\n")

	// Line endings and whitespace.
	add("edge-crlf", "a: 1\r\nb:\r\n  - x\r\n  - y\r\n")
	add("edge-cr-only", "a: 1\rb: 2\r")
	add("edge-no-final-newline", "a: 1\nb: 2")
	add("edge-trailing-spaces", "a: 1   \nb: x  \n")
	add("edge-only-whitespace", "   \n\n  \n")
	add("edge-empty", "")

	// Duplicate keys and odd keys.
	add("edge-duplicate-key", "a: 1\na: 2\n")
	add("edge-duplicate-key-nested", "x:\n  a: 1\n  b: 2\n  a: 3\n")
	add("edge-mixed-keys", "1: int\n1.5: float\ntrue: bool\n~: null\ns: str\n")
	add("edge-null-key-only", "~: v\n")

	// Invalid inputs.
	add("edge-invalid-tab-indent", "a:\n\tb: 1\n")
	add("edge-invalid-unclosed-double", "a: \"open\n")
	add("edge-invalid-unclosed-single", "a: 'open\n")
	add("edge-invalid-nested-mapping", "a: b: c\n")
	add("edge-invalid-bad-indent", "a:\n  b: 1\n c: 2\n")
	add("edge-invalid-unclosed-flow-map", "{a: 1\n")
	add("edge-invalid-unclosed-flow-seq", "[a, b\n")
	add("edge-invalid-stray-bracket", "]\n")
	add("edge-invalid-stray-brace", "a: }\n")
	add("edge-invalid-escape", "a: \"\\q\"\n")
	add("edge-invalid-reserved-at", "@a\n")
	add("edge-invalid-reserved-backtick", "a: `b\n")
	add("edge-invalid-directive", "%FOO bar\n---\na: 1\n")
	add("edge-invalid-yaml-version", "%YAML 2.0\n---\na: 1\n")
	add("edge-invalid-duplicate-yaml-directive", "%YAML 1.1\n%YAML 1.1\n---\na: 1\n")
	add("edge-invalid-anchor-empty", "a: & x\n")
	add("edge-invalid-alias-empty", "a: *\n")
	add("edge-invalid-block-in-flow", "[a, - b]\n")
	add("edge-invalid-seq-after-map", "a: 1\n- b\n")
	add("edge-invalid-literal-indent", "a: |0\n  x\n")
	add("edge-invalid-tag-uri", "a: !<> x\n")
	add("edge-invalid-doc-content-after-end", "a: 1\n... b\n")
	add("edge-invalid-key-no-value-flow", "{a: 1, b}\n")
	add("edge-invalid-question-in-plain", "a: ?\n")
	add("edge-invalid-colon-flow-key", "[a:b, c]\n")

	// limit_test.go inputs: "1kb of maps" verbatim, and reduced variants of
	// the depth limits (one past 10000 instead of 1000kb) and of the 100
	// aliases row (a 31-element list, so the expanded value dump stays near
	// 1 MB); the multi-megabyte limitTests rows are not in the corpus.
	add("limit-1kb-of-maps", `a: &a [{a}`+strings.Repeat(`,{a}`, 1*1024/4-1)+`]`)
	add("limit-deep-slices-10001", strings.Repeat(`[`, 10001))
	add("limit-deep-maps-10001", "x: "+strings.Repeat(`{`, 10001))
	add("limit-deep-indents-10001", strings.Repeat(`- `, 10001))
	add("limit-100-aliases", `{a: &a [{a}`+strings.Repeat(`,{a}`, 30)+`], b: &b [*a`+strings.Repeat(`,*a`, 99)+`]}`)

	// Alias-ratio boundary (decode.go allowedAliasRatio): level 0 is a
	// sequence of w scalars, level k a sequence of w aliases to level k-1.
	for _, wl := range [][2]int{{10, 2}, {10, 3}, {10, 4}, {3, 6}, {3, 7}, {2, 10}, {2, 11}} {
		add(fmt.Sprintf("alias-w%02d-l%02d", wl[0], wl[1]), aliasLevels(wl[0], wl[1]))
	}

	add("recordings-llm", recordingsLLM)
	add("recordings-tools", recordingsTools)
	return cases
}

func utf16Bytes(s string, bigEndian bool) string {
	units := append([]uint16{0xfeff}, utf16.Encode([]rune(s))...)
	b := make([]byte, 0, 2*len(units))
	for _, u := range units {
		if bigEndian {
			b = append(b, byte(u>>8), byte(u))
		} else {
			b = append(b, byte(u), byte(u>>8))
		}
	}
	return string(b)
}

func nestedBlock(depth int) string {
	var b strings.Builder
	for i := 0; i < depth; i++ {
		fmt.Fprintf(&b, "%sk%d:\n", strings.Repeat("  ", i), i)
	}
	fmt.Fprintf(&b, "%sleaf\n", strings.Repeat("  ", depth))
	return b.String()
}

func aliasLevels(w, levels int) string {
	var b strings.Builder
	items := make([]string, w)
	for i := range items {
		items[i] = "x"
	}
	fmt.Fprintf(&b, "l0: &l0 [%s]\n", strings.Join(items, ", "))
	for k := 1; k < levels; k++ {
		for i := range items {
			items[i] = fmt.Sprintf("*l%d", k-1)
		}
		fmt.Fprintf(&b, "l%d: &l%d [%s]\n", k, k, strings.Join(items, ", "))
	}
	return b.String()
}

// recordingsLLM is shaped like ADK-Go's conformance recordings
// (internal/configurable/conformance, recording.Recordings written by the
// record plugin): an LLM recording and a tool recording.
const recordingsLLM = `recordings:
  - user_message_index: 0
    agent_name: root_agent
    llm_recording:
      llm_request:
        model: gemini-2.5-flash
        contents:
          - role: user
            parts:
              - text: "What's the weather in Paris?"
        config:
          system_instruction:
            role: user
            parts:
              - text: |-
                  You are a helpful weather assistant.
                  Answer in one sentence.
          tools:
            - function_declarations:
                - name: get_weather
                  description: Returns the weather for a city.
                  parameters:
                    type: OBJECT
                    properties:
                      city:
                        type: STRING
                        description: "The city name, e.g. \"Paris\"."
                    required:
                      - city
          labels: {}
      llm_responses:
        - content:
            role: model
            parts:
              - function_call:
                  id: adk-0001
                  name: get_weather
                  args:
                    city: Paris
                thought_signature: "SGVsbG8="
          usage_metadata:
            prompt_token_count: 42
            candidates_token_count: 7
            total_token_count: 49
  - user_message_index: 0
    agent_name: root_agent
    tool_recording:
      tool_call:
        id: adk-0001
        name: get_weather
        args:
          city: Paris
      tool_response:
        id: adk-0001
        name: get_weather
        response:
          result: "sunny, 21°C"
          ok: true
          count: 0
  - user_message_index: 0
    agent_name: root_agent
    llm_recording:
      llm_request:
        model: gemini-2.5-flash
        contents:
          - role: user
            parts:
              - text: "What's the weather in Paris?"
          - role: model
            parts:
              - function_call:
                  id: adk-0001
                  name: get_weather
                  args:
                    city: Paris
          - role: user
            parts:
              - function_response:
                  id: adk-0001
                  name: get_weather
                  response:
                    result: "sunny, 21°C"
        config:
          temperature: 0.7
          top_p: 0.95
          max_output_tokens: 1024
      llm_responses:
        - content:
            role: model
            parts:
              - text: It is sunny and 21°C in Paris.
          finish_reason: STOP
`

// recordingsTools is a second recordings file: several user messages, a
// streamed (partial) response, thoughts, empty and null fields, a long
// line, comments and flow collections.
const recordingsTools = `# Recordings for the conformance test "multi_tool".
# Generated by the record plugin; edit with care.
recordings:
  - user_message_index: 0
    agent_name: coordinator
    llm_recording:
      llm_request:
        model: gemini-2.5-pro
        contents:
          - role: user
            parts:
              - text: 'Add 2 and 3, then look up the capital of France.'
        config:
          system_instruction:
            role: user
            parts:
              - text: "You coordinate tools.\nAlways explain your reasoning."
          tools:
            - function_declarations:
                - name: add
                  parameters:
                    type: OBJECT
                    properties:
                      a: {type: INTEGER}
                      b: {type: INTEGER}
                    required: [a, b]
                - name: capital_of
                  parameters_json_schema:
                    type: object
                    properties:
                      country:
                        type: string
                        enum: [France, Spain, "N/A"]
          thinking_config:
            include_thoughts: true
            thinking_budget: -1
      llm_responses:
        - content:
            role: model
            parts:
              - text: I should call add first.
                thought: true
          partial: true
        - content:
            role: model
            parts:
              - function_call:
                  id: call-1
                  name: add
                  args: {a: 2, b: 3}
              - function_call:
                  id: call-2
                  name: capital_of
                  args:
                    country: France
          usage_metadata:
            prompt_token_count: 120
            thoughts_token_count: 15
  - user_message_index: 0
    agent_name: coordinator
    tool_recording:
      tool_call: {id: call-1, name: add, args: {a: 2, b: 3}}
      tool_response:
        id: call-1
        name: add
        response: {result: 5}
  - user_message_index: 0
    agent_name: coordinator
    tool_recording:
      tool_call:
        id: call-2
        name: capital_of
        args:
          country: France
      tool_response:
        id: call-2
        name: capital_of
        response:
          result: Paris
          sources: []
          notes: ~
  # The second user message.
  - user_message_index: 1
    agent_name: coordinator
    llm_recording:
      llm_request:
        model: gemini-2.5-pro
        contents:
          - role: user
            parts:
              - text: >-
                  Summarize everything so far in a single, deliberately long sentence that keeps going well past eighty columns so that line width matters.
      llm_responses:
        - content:
            role: model
            parts:
              - text: "2 + 3 = 5, and the capital of France is Paris — both answers came from tools: add and capital_of."
          finish_reason: STOP
          avg_logprobs: -0.125
`
