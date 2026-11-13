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

//! Port of ADK-Go `internal/configurable/conformance/recordplugin/yaml_clean_test.go` (Task 7).

use gadk_goyaml::{marshal_node, unmarshal_node};
use pretty_assertions::assert_eq;

use crate::conformance::record::clean::clean_yaml_node;

struct Case {
    name: &'static str,
    input: &'static str,
    expected: &'static str,
}

#[test]
fn test_clean_yaml_node() {
    // Go: yaml_clean_test.go TestCleanYAMLNode
    let tests = [
        Case {
            name: "prunes null fields",
            input: r#"
field1: null
field2: ~
field3: "value"
"#,
            expected: r#"field3: "value"
"#,
        },
        Case {
            name: "prunes default zero values and false booleans outside tool args",
            input: r#"
candidate_count: 0
max_output_tokens: 0
thought: false
text: "keep me"
"#,
            expected: r#"text: "keep me"
"#,
        },
        Case {
            name: "retains zero values and false booleans inside tool args",
            input: r#"
tool_call:
  name: "my_tool"
  args:
    param1: 0
    param2: false
"#,
            expected: r#"tool_call:
  name: "my_tool"
  args:
    param1: 0
    param2: false
"#,
        },
        Case {
            name: "retains zero values and false booleans inside tool responses",
            input: r#"
tool_response:
  response:
    success: false
    count: 0
"#,
            expected: r#"tool_response:
  response:
    success: false
    count: 0
"#,
        },
        Case {
            name: "prunes empty sequences but retains empty mappings",
            input: r#"
thought_signature: []
google_search: {}
text: "test"
"#,
            expected: r#"google_search: {}
text: "test"
"#,
        },
        Case {
            name: "retains thought_signature base64 scalar as string",
            input: r#"
text: "test"
thought_signature: "SGVsbG8="
"#,
            expected: r#"text: "test"
thought_signature: "SGVsbG8="
"#,
        },
        Case {
            name: "retains thoughtsignature base64 scalar as string",
            input: r#"
text: "test"
thoughtsignature: "SGVsbG8="
"#,
            expected: r#"text: "test"
thought_signature: "SGVsbG8="
"#,
        },
        Case {
            name: "converts thought_signature sequence of integers to base64 string",
            input: r#"
text: "test"
thought_signature:
  - 72
  - 101
  - 108
  - 108
  - 111
"#,
            expected: r#"text: "test"
thought_signature: "SGVsbG8="
"#,
        },
        Case {
            name: "converts thoughtsignature sequence of integers to base64 string",
            input: r#"
text: "test"
thoughtsignature:
  - 72
  - 101
  - 108
  - 108
  - 111
"#,
            expected: r#"text: "test"
thought_signature: "SGVsbG8="
"#,
        },
        Case {
            name: "deep nested pruning",
            input: r#"
recordings:
  - user_message_index: 0
    agent_name: "search"
    llm_recording:
      llm_request:
        model: "gemini"
        contents:
          - parts:
              - media_resolution: null
                text: "hello"
                thought_signature: []
"#,
            expected: r#"recordings:
  - user_message_index: 0
    agent_name: "search"
    llm_recording:
      llm_request:
        model: "gemini"
        contents:
          - parts:
              - text: "hello"
"#,
        },
        Case {
            name: "cleans bloated parameters schema and translates keys",
            input: r#"
functiondeclarations:
  - name: "validate_email"
    description: "checks email"
    parametersjsonschema:
      id: ""
      schema: ""
      ref: ""
      comment: ""
      defs: {}
      definitions: {}
      dependentrequired: {}
      extra: {}
      type: "object"
      required:
        - "email"
      properties:
        email:
          id: ""
          type: "string"
          description: "the email address"
          dependentrequired: {}
    responsejsonschema:
      type: "boolean"
"#,
            expected: r#"
function_declarations:
  - name: "validate_email"
    description: "checks email"
    parameters_json_schema:
      title: "validate_emailParams"
      type: "object"
      required:
        - "email"
      properties:
        email:
          title: "Email"
          type: "string"
          description: "the email address"
"#,
        },
        Case {
            name: "cleans schema parameters mapping additionalproperties and propertyorder, and pruning unsupported openapi properties",
            input: r#"
functiondeclarations:
  - name: "validate_email"
    parametersjsonschema:
      type: "object"
      required:
        - "email"
      properties:
        email:
          type: "string"
      additionalproperties:
        not: {}
      propertyorder:
        - "email"
      deprecated: false
      readonly: false
      writeonly: false
      uniqueitems: false
"#,
            expected: r#"
function_declarations:
  - name: "validate_email"
    parameters_json_schema:
      title: "validate_emailParams"
      type: "object"
      required:
        - "email"
      properties:
        email:
          title: "Email"
          type: "string"
"#,
        },
        Case {
            name: "prunes empty strings for general fields but retains them inside tool args",
            input: r#"
llm_recording:
  llm_responses:
    - content:
        parts:
          - text: "done"
      session_resumption_handle: ""
      error_code: ""
      error_message: ""
tool_call:
  name: "my_tool"
  args:
    param_empty_str: ""
"#,
            expected: r#"
llm_recording:
  llm_responses:
    - content:
        parts:
          - text: "done"
tool_call:
  name: "my_tool"
  args:
    param_empty_str: ""
"#,
        },
        Case {
            name: "prunes empty GenerateContentConfig and google search properties",
            input: r#"
llm_recording:
  llm_request:
    config:
      response_mime_type: ""
      cached_content: ""
      media_resolution: ""
      service_tier: ""
      tools:
        - google_search:
            blocking_confidence: ""
      labels: {}
"#,
            expected: r#"
llm_recording:
  llm_request:
    config:
      tools:
        - google_search: {}
"#,
        },
        Case {
            name: "normalizes raw string system_instruction to structured object",
            input: r#"
llm_request:
  config:
    system_instruction: "You are a booking assistant."
"#,
            expected: r#"
llm_request:
  config:
    system_instruction: "You are a booking assistant."
"#,
        },
        Case {
            name: "injects part_metadata to contents parts",
            input: r#"
llm_request:
  contents:
    - role: "user"
      parts:
        - text: "Hello"
"#,
            expected: r#"
llm_request:
  contents:
    - role: "user"
      parts:
        - text: "Hello"
"#,
        },
        Case {
            name: "preserves existing part_metadata if already present",
            input: r#"
llm_request:
  contents:
    - role: "user"
      parts:
        - text: "Hello"
          part_metadata:
            some_key: "some_value"
"#,
            expected: r#"
llm_request:
  contents:
    - role: "user"
      parts:
        - text: "Hello"
          part_metadata:
            some_key: "some_value"
"#,
        },
    ];

    for tc in tests {
        let mut node = unmarshal_node(tc.input.as_bytes())
            .unwrap_or_else(|err| panic!("{}: failed to unmarshal test input: {err}", tc.name));

        let root = node.root();
        clean_yaml_node(&mut node, root, false);

        let output = marshal_node(&node, root)
            .unwrap_or_else(|err| panic!("{}: failed to marshal cleaned node: {err}", tc.name));

        let got_clean = normalize_yaml(&String::from_utf8(output).expect("UTF-8 output"));
        let expected_clean = normalize_yaml(tc.expected);
        assert_eq!(got_clean, expected_clean, "{}: mismatch", tc.name);
    }
}

/// Go `normalizeYAML`: the text re-marshalled through a node (the text itself when it does
/// not parse; Go ignores the marshal error, so a failed marshal is `""`).
fn normalize_yaml(s: &str) -> String {
    // Go: yaml_clean_test.go normalizeYAML
    let Ok(node) = unmarshal_node(s.as_bytes()) else {
        return s.to_string();
    };
    let out = marshal_node(&node, node.root()).unwrap_or_default();
    String::from_utf8(out).expect("UTF-8 output")
}

/// `yaml.Marshal(node)` after Go's `node.Encode(decl)` of the test's `genai.FunctionDeclaration`
/// (captured from Go: every Go field, zero values included).
const ENCODED_DECL: &str = "description: checks email
name: validate_email
parameters:
    anyof: []
    default: null
    description: \"\"
    enum: []
    example: null
    format: \"\"
    items: null
    maxitems: null
    maxlength: null
    maxproperties: null
    maximum: null
    minitems: null
    minlength: null
    minproperties: null
    minimum: null
    nullable: null
    pattern: \"\"
    properties:
        email:
            anyof: []
            default: null
            description: the email address
            enum: []
            example: null
            format: \"\"
            items: null
            maxitems: null
            maxlength: null
            maxproperties: null
            maximum: null
            minitems: null
            minlength: null
            minproperties: null
            minimum: null
            nullable: null
            pattern: \"\"
            properties: {}
            propertyordering: []
            required: []
            title: \"\"
            type: STRING
    propertyordering: []
    required:
        - email
    title: \"\"
    type: OBJECT
parametersjsonschema: null
response: null
responsejsonschema: null
behavior: \"\"
";

/// What Go's test logs (`Serialized:`) for the cleaned declaration.
const SERIALIZED_DECL: &str = "description: checks email
name: validate_email
parameters:
    properties:
        email:
            description: the email address
            title: \"Email\"
            type: STRING
    required:
        - email
    title: \"validate_emailParams\"
    type: OBJECT
";

// parity: adapted: gadk has no Go-shaped yaml encoding of a bare genai.FunctionDeclaration (T5's gonode encodes only the recording roots), so the node is Go's node.Encode(decl) output re-read with unmarshal_node (node.Encode re-parses its own text; the mapping is the document's content); Go only logs the result, the port asserts the logged text
#[test]
fn test_function_declaration_title_injection() {
    // Go: yaml_clean_test.go TestFunctionDeclarationTitleInjection
    let mut doc = unmarshal_node(ENCODED_DECL.as_bytes()).expect("failed to encode");
    let node = doc[doc.root()].content[0];

    clean_yaml_node(&mut doc, node, false);

    let output = marshal_node(&doc, node).expect("failed to marshal");
    let output = String::from_utf8(output).expect("UTF-8 output");
    println!("Serialized:\n{output}");
    assert_eq!(output, SERIALIZED_DECL);
}
