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

//! Port of ADK-Go `internal/configurable/conformance/replayplugin/yaml_utils_test.go`.

use gadk_goyaml::{marshal_node, unmarshal_node};
use pretty_assertions::assert_eq;

use crate::conformance::replay::normalize::normalize_yaml_node;

/// Go `normalizeYAML`: the text re-marshalled through a node (the text itself when it does not
/// parse).
fn normalize_yaml(s: &str) -> String {
    let Ok(doc) = unmarshal_node(s.as_bytes()) else {
        return s.to_owned();
    };
    String::from_utf8(marshal_node(&doc, doc.root()).unwrap_or_default()).unwrap_or_default()
}

/// The body of both Go tests: unmarshal into a node, normalize, marshal, compare normalized.
fn run_cases(tests: &[(&str, &str, &str)]) {
    for (name, input, expected) in tests {
        let mut doc = unmarshal_node(input.as_bytes())
            .unwrap_or_else(|e| panic!("{name}: Unmarshal failed: {e}"));
        let root = doc.root();
        normalize_yaml_node(&mut doc, root);
        let output = marshal_node(&doc, doc.root())
            .unwrap_or_else(|e| panic!("{name}: Marshal failed: {e}"));
        let got_clean = normalize_yaml(&String::from_utf8(output).unwrap());
        let expected_clean = normalize_yaml(expected);
        assert_eq!(got_clean, expected_clean, "{name}: mismatch");
    }
}

#[test]
fn test_remove_underscores() {
    run_cases(&[
        (
            "strips underscores from standard keys",
            "\nfirst_name: \"John\"\nlast_name: \"Doe\"\n",
            "\nfirstname: \"John\"\nlastname: \"Doe\"\n",
        ),
        (
            "ignores fields specified in toIgnore",
            "\nthought_signature: \"SGVsbG8=\"\nhttp_options: \"options\"\nargs: \"arguments\"\nresponse: \"result\"\n",
            "\nthought_signature: \"SGVsbG8=\"\nhttp_options: \"options\"\nargs: \"arguments\"\nresponse: \"result\"\n",
        ),
        (
            "ignores but recurses fields specified in toIgnoreButRecurse",
            "\nuser_message_index: 0\nagent_name: \"test\"\nllm_recording:\n  nested_field_with_underscore: \"nested\"\n",
            "\nuser_message_index: 0\nagent_name: \"test\"\nllm_recording:\n  nestedfieldwithunderscore: \"nested\"\n",
        ),
    ]);
}

#[test]
fn test_fix_type_mismatches() {
    run_cases(&[
        (
            "wraps llmresponse mapping into sequence llm_responses",
            "\nllmresponse:\n  content:\n    role: \"model\"\n",
            "\nllm_responses:\n  - content:\n      role: \"model\"\n",
        ),
        (
            "wraps llm_response mapping into sequence llm_responses",
            "\nllm_response:\n  content:\n    role: \"model\"\n",
            "\nllm_responses:\n  - content:\n      role: \"model\"\n",
        ),
        (
            "normalizes systeminstruction scalar string to structured object",
            "\nsysteminstruction: \"You are a helpful assistant.\"\n",
            "\nsysteminstruction:\n  role: user\n  parts:\n    - text: You are a helpful assistant.\n",
        ),
        (
            "normalizes system_instruction scalar string to structured object",
            "\nsystem_instruction: \"You are a helpful assistant.\"\n",
            "\nsysteminstruction:\n  role: user\n  parts:\n    - text: You are a helpful assistant.\n",
        ),
    ]);
}
