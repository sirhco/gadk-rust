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

// Verbatim copies of ADK-Go's
// internal/configurable/conformance/replayplugin/{yaml_utils.go,replay_plugin.go}
// declarations (scripts/check-fixtures.sh diffs them against ADK-Go).

import (
	"encoding/json"
	"fmt"
	"regexp"
	"strings"

	"gopkg.in/yaml.v3"
)

var toIgnore = map[string]struct{}{
	"thought_signature": {},
	"http_options":      {},
	"args":              {},
	"response":          {},
}

var toIgnoreButRecurse = map[string]struct{}{
	"user_message_index": {},
	"agent_name":         {},
	"llm_recording":      {},
	"llm_request":        {},
	"llm_responses":      {},
	"tool_recording":     {},
	"tool_call":          {},
	"tool_response":      {},
}

// normalizeYAMLNode standardizes the recorded YAML AST in a single tree traversal.
// It strips underscores from structural Go field keys to match case-insensitive fields,
// while preserving user freeform tool arguments/responses, and resolves type mismatches.
func normalizeYAMLNode(node *yaml.Node) {
	switch node.Kind {
	case yaml.DocumentNode, yaml.SequenceNode:
		for _, child := range node.Content {
			normalizeYAMLNode(child)
		}
	case yaml.MappingNode:
		for i := 0; i < len(node.Content); i += 2 {
			keyNode := node.Content[i]
			valueNode := node.Content[i+1]

			// 1. Fix known type mismatches and singular/plural fields
			switch keyNode.Value {
			case "systeminstruction", "system_instruction":
				if valueNode.Kind == yaml.ScalarNode {
					val := valueNode.Value
					valueNode.Kind = yaml.MappingNode
					valueNode.Tag = "!!map"
					valueNode.Value = ""
					valueNode.Content = []*yaml.Node{
						{Kind: yaml.ScalarNode, Value: "role"},
						{Kind: yaml.ScalarNode, Tag: "!!str", Value: "user"},
						{Kind: yaml.ScalarNode, Value: "parts"},
						{
							Kind: yaml.SequenceNode,
							Tag:  "!!seq",
							Content: []*yaml.Node{
								{
									Kind: yaml.MappingNode,
									Tag:  "!!map",
									Content: []*yaml.Node{
										{Kind: yaml.ScalarNode, Value: "text"},
										{Kind: yaml.ScalarNode, Tag: "!!str", Value: val},
									},
								},
							},
						},
					}
				}
			case "llmresponse", "llm_response":
				if valueNode.Kind == yaml.MappingNode {
					origCopy := *valueNode
					valueNode.Kind = yaml.SequenceNode
					valueNode.Tag = "!!seq"
					valueNode.Value = ""
					valueNode.Content = []*yaml.Node{&origCopy}
				}
				keyNode.Value = "llm_responses"
			}

			// 2. Skip processing/recursion for ignored freeform keys (like tool args or response payload)
			if _, ok := toIgnore[keyNode.Value]; ok {
				continue
			}

			// 3. For standard metadata fields, preserve snake_case but recurse into value
			if _, ok := toIgnoreButRecurse[keyNode.Value]; ok {
				normalizeYAMLNode(valueNode)
				continue
			}

			// 4. Strip underscores to match camelCase Go struct field names case-insensitively
			keyNode.Value = strings.ReplaceAll(keyNode.Value, "_", "")

			// 5. Recurse to process nested fields
			normalizeYAMLNode(valueNode)
		}
	}
}

var (
	// Matches either "parameters: " or "result: " followed by a JSON-like object/array
	dataBlockRegex = regexp.MustCompile(`(?i)(parameters|result):\s*([\{\[].*[\}\]])`)
	// Matches any standalone or embedded JSON-like object/array
	jsonCandidateRegex = regexp.MustCompile(`([\{\[].*[\}\]])`)
	// Matches 'key' or 'value' but ignores apostrophes inside words like O'Malley
	quoteRegex = regexp.MustCompile(`'([^']*)'`)
	// Matches Python/Pseudo-JSON constants specifically as values
	nullRegex = regexp.MustCompile(`\bNone\b`)
	boolRegex = regexp.MustCompile(`\b(True|False)\b`)
)

func modifyString(input string) string {
	// We use ReplaceAllStringFunc to process ONLY the captured data parts
	return dataBlockRegex.ReplaceAllStringFunc(input, func(fullMatch string) string {
		// Split label (e.g., "parameters:") from the data (e.g., "{'a': 1}")
		parts := dataBlockRegex.FindStringSubmatch(fullMatch)
		if len(parts) < 3 {
			return fullMatch
		}

		label := parts[1]
		rawData := parts[2]

		// Normalize Python-isms to JSON-isms
		// Replace single quotes with double quotes
		normalized := quoteRegex.ReplaceAllString(rawData, `"$1"`)
		// Replace None -> null
		normalized = nullRegex.ReplaceAllString(normalized, "null")
		// Replace True/False -> true/false
		normalized = boolRegex.ReplaceAllStringFunc(normalized, func(m string) string {
			return strings.ToLower(m)
		})

		// Round-trip through JSON to validate and clean up
		var parsed any
		if err := json.Unmarshal([]byte(normalized), &parsed); err != nil {
			// If it's still not valid JSON, return the original match to avoid corruption
			return fullMatch
		}

		// Marshal back to a clean, standard JSON string
		fixedJSON, err := json.Marshal(parsed)
		if err != nil {
			return fullMatch
		}

		return fmt.Sprintf("%s: %s", label, string(fixedJSON))
	})
}

// canonicalizeJSONSubstrings searches for standalone or embedded JSON candidates within a string and re-marshals
// them canonically to eliminate key-ordering, whitespace, and quote differences between Python and Go serializers.
func canonicalizeJSONSubstrings(input string) string {
	s := modifyString(input)
	return jsonCandidateRegex.ReplaceAllStringFunc(s, func(rawData string) string {
		normalized := quoteRegex.ReplaceAllString(rawData, `"$1"`)
		normalized = nullRegex.ReplaceAllString(normalized, "null")
		normalized = boolRegex.ReplaceAllStringFunc(normalized, func(m string) string {
			return strings.ToLower(m)
		})

		var parsed any
		if err := json.Unmarshal([]byte(normalized), &parsed); err != nil {
			return rawData
		}
		fixedJSON, err := json.Marshal(parsed)
		if err != nil {
			return rawData
		}
		return string(fixedJSON)
	})
}
