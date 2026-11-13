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

// recordingCase is one authored recording-corpus case (SCENARIO.md
// "Recording corpus"). recordings is a JSON array of case recordings (the
// input.json "recordings" shape); existing is the YAML text of an already
// present recordings file, or existingFrom names an earlier case whose Go
// output is that file.
type recordingCase struct {
	name         string
	mode         string // "none" or "sse"
	index        int    // the invocation's user_message_index
	existing     string
	existingFrom string
	recordings   string
}

// userHi is the user turn most cases start with.
const userHi = `{"role": "user", "parts": [{"text": "Hi"}]}`

var authoredCases = []recordingCase{
	{name: "text", recordings: `[
  {"user_message_index": 0, "agent_name": "root_agent", "llm_recording": {
    "llm_request": {"model": "gemini-2.5-flash", "contents": [` + userHi + `], "config": {}},
    "llm_responses": [{"content": {"role": "model", "parts": [{"text": "Hello! How can I help?"}]}, "finishReason": "STOP"}]
  }}
]`},
	{name: "system-instruction", recordings: `[
  {"user_message_index": 0, "agent_name": "root_agent", "llm_recording": {
    "llm_request": {"model": "gemini-2.5-flash", "contents": [` + userHi + `], "config": {
      "systemInstruction": {"role": "user", "parts": [{"text": "You are a helpful agent.\nAnswer briefly.\n"}, {"text": "second part"}]}
    }},
    "llm_responses": [{"content": {"role": "model", "parts": [{"text": "OK"}]}}]
  }},
  {"user_message_index": 0, "agent_name": "single_line", "llm_recording": {
    "llm_request": {"model": "gemini-2.5-flash", "contents": [` + userHi + `], "config": {
      "systemInstruction": {"parts": [{"text": "One line: with a colon"}]}
    }},
    "llm_responses": [{"content": {"role": "model", "parts": [{"text": "OK"}]}}]
  }},
  {"user_message_index": 0, "agent_name": "no_text", "llm_recording": {
    "llm_request": {"model": "gemini-2.5-flash", "contents": [` + userHi + `], "config": {
      "systemInstruction": {"role": "user", "parts": [{"inlineData": {"mimeType": "text/plain", "data": "aGk="}}]}
    }},
    "llm_responses": [{"content": {"role": "model", "parts": [{"text": "OK"}]}}]
  }}
]`},
	{name: "function-call-args", recordings: `[
  {"user_message_index": 0, "agent_name": "root_agent", "llm_recording": {
    "llm_request": {"model": "gemini-2.5-flash", "contents": [{"role": "user", "parts": [{"text": "Weather in Paris?"}]}]},
    "llm_responses": [{"content": {"role": "model", "parts": [{"functionCall": {"id": "adk-1", "name": "get_weather", "args": {
      "city": "Paris", "days": 3, "ratio": 1.5, "big": 12345678901, "flag": false, "on": true, "zero": 0, "empty": "",
      "nothing": null, "list": [], "items": [1, "two", 3.25, null, {"k": "v"}], "obj": {}, "nested": {"a": {"b": [0, false, ""]}},
      "text_multi": "line1\nline2", "quoted": "yes", "num_str": "42", "colon": "a: b"
    }}}]}, "finishReason": "STOP"}]
  }}
]`},
	{name: "function-call-empty-args", recordings: `[
  {"user_message_index": 0, "agent_name": "root_agent", "llm_recording": {
    "llm_request": {"model": "gemini-2.5-flash", "contents": [` + userHi + `]},
    "llm_responses": [{"content": {"role": "model", "parts": [
      {"functionCall": {"name": "get_time"}},
      {"functionCall": {"id": "adk-2", "name": "get_date", "args": {}}}
    ]}}]
  }}
]`},
	{name: "function-response", recordings: `[
  {"user_message_index": 0, "agent_name": "root_agent", "llm_recording": {
    "llm_request": {"model": "gemini-2.5-flash", "contents": [
      ` + userHi + `,
      {"role": "model", "parts": [{"functionCall": {"id": "adk-1", "name": "get_weather", "args": {"city": "Paris"}}}]},
      {"role": "user", "parts": [
        {"functionResponse": {"id": "adk-1", "name": "get_weather", "response": {"temp": 21.5, "ok": false, "unit": "", "count": 0, "tags": [], "meta": {}}}},
        {"functionResponse": {"name": "noop", "response": {}}},
        {"functionResponse": {"id": "adk-3", "name": "image", "response": {"result": "see part"}, "willContinue": false, "scheduling": "SILENT",
          "parts": [{"inlineData": {"mimeType": "image/png", "data": "iVBORw0K"}}]}}
      ]}
    ]},
    "llm_responses": [{"content": {"role": "model", "parts": [{"text": "It is 21.5C."}]}}]
  }}
]`},
	{name: "thought-signature", recordings: `[
  {"user_message_index": 0, "agent_name": "root_agent", "llm_recording": {
    "llm_request": {"model": "gemini-2.5-flash", "contents": [
      ` + userHi + `,
      {"role": "model", "parts": [{"text": "thinking", "thought": true, "thoughtSignature": "c2lnbmF0dXJl"},
        {"functionCall": {"name": "f", "args": {"a": 1}}, "thoughtSignature": "AAEC/w=="}]}
    ]},
    "llm_responses": [{"content": {"role": "model", "parts": [{"text": "done", "thoughtSignature": "eA=="}]}}]
  }}
]`},
	{name: "inline-data", recordings: `[
  {"user_message_index": 0, "agent_name": "root_agent", "llm_recording": {
    "llm_request": {"model": "gemini-2.5-flash", "contents": [{"role": "user", "parts": [
      {"text": "What is this?"},
      {"inlineData": {"mimeType": "image/png", "data": "aGVsbG8=", "displayName": "hello.png"}},
      {"inlineData": {"mimeType": "application/octet-stream", "data": ""}},
      {"fileData": {"fileUri": "gs://bucket/file.pdf", "mimeType": "application/pdf"}}
    ]}]},
    "llm_responses": [{"content": {"role": "model", "parts": [{"text": "A greeting."}]}}]
  }}
]`},
	{name: "config-function-schema", recordings: `[
  {"user_message_index": 0, "agent_name": "root_agent", "llm_recording": {
    "llm_request": {"model": "gemini-2.5-flash", "contents": [` + userHi + `], "config": {"tools": [{"functionDeclarations": [
      {"name": "lookup_order", "description": "Looks up an order.", "parameters": {"type": "OBJECT",
        "properties": {
          "order_id": {"type": "STRING", "description": "The order id."},
          "include_items": {"type": "BOOLEAN"},
          "filters": {"type": "OBJECT", "properties": {"min_total": {"type": "NUMBER", "minimum": 0}, "status": {"type": "STRING", "enum": ["open", "closed"]}}},
          "tags": {"type": "ARRAY", "items": {"type": "STRING"}, "maxItems": 5},
          "titled": {"type": "STRING", "title": "Kept Title"}
        },
        "required": ["order_id"], "propertyOrdering": ["order_id", "include_items"]},
        "response": {"type": "OBJECT", "properties": {"status": {"type": "STRING"}}}},
      {"name": "no_params", "description": "Takes nothing."},
      {"name": "titled_params", "parameters": {"type": "OBJECT", "title": "Explicit", "nullable": true, "properties": {"x": {"type": "INTEGER", "format": "int32", "default": 3, "example": 4}}}}
    ]}]}},
    "llm_responses": [{"content": {"role": "model", "parts": [{"text": "OK"}]}}]
  }}
]`},
	{name: "config-json-schema", recordings: `[
  {"user_message_index": 0, "agent_name": "root_agent", "llm_recording": {
    "llm_request": {"model": "gemini-2.5-flash", "contents": [` + userHi + `], "config": {"tools": [{"functionDeclarations": [
      {"name": "book_trip", "description": "Books a trip.", "parametersJsonSchema": {
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$defs": {"Leg": {"type": "object", "properties": {"from": {"type": "string"}, "to": {"type": "string"}}, "required": ["from", "to"], "additionalProperties": false}},
        "type": "object",
        "properties": {
          "legs": {"type": "array", "items": {"$ref": "#/$defs/Leg"}, "minItems": 1},
          "class": {"type": "string", "enum": ["economy", "business"], "default": "economy"},
          "passengers": {"type": "integer", "minimum": 1, "maximum": 9, "default": 1},
          "note": {"type": ["null", "string"], "description": "Optional note."},
          "flex": {"anyOf": [{"type": "boolean"}, {"const": 3}]},
          "budget": {"type": "number", "exclusiveMinimum": 0, "multipleOf": 0.01}
        },
        "required": ["legs"],
        "additionalProperties": false
      },
      "responseJsonSchema": {"type": "object", "properties": {"ok": {"type": "boolean"}}}},
      {"name": "true_schema", "parametersJsonSchema": true},
      {"name": "empty_schema", "parametersJsonSchema": {}},
      {"name": "false_props", "parametersJsonSchema": {"type": "object", "properties": {"x": false, "y": true}}}
    ]}], "responseJsonSchema": {"type": "object"}}},
    "llm_responses": [{"content": {"role": "model", "parts": [{"text": "OK"}]}}]
  }}
]`},
	{name: "config-fields", recordings: `[
  {"user_message_index": 0, "agent_name": "root_agent", "llm_recording": {
    "llm_request": {"model": "gemini-2.5-flash", "contents": [` + userHi + `], "config": {
      "temperature": 0.7, "topP": 0.95, "topK": 40, "candidateCount": 1, "maxOutputTokens": 1024,
      "stopSequences": ["END", ""], "responseLogprobs": true, "logprobs": 0, "presencePenalty": 0, "seed": 42,
      "responseMimeType": "application/json",
      "responseSchema": {"type": "OBJECT", "properties": {"answer": {"type": "STRING"}}},
      "safetySettings": [{"category": "HARM_CATEGORY_HARASSMENT", "threshold": "BLOCK_ONLY_HIGH"}],
      "toolConfig": {"functionCallingConfig": {"mode": "ANY", "allowedFunctionNames": ["f"]}},
      "labels": {"team": "adk", "empty": "", "zero": "0"},
      "responseModalities": ["TEXT"],
      "thinkingConfig": {"includeThoughts": true, "thinkingBudget": 0},
      "enableEnhancedCivicAnswers": false
    }},
    "llm_responses": [{"content": {"role": "model", "parts": [{"text": "{\"answer\": \"hi\"}"}]}}]
  }}
]`},
	{name: "google-search", recordings: `[
  {"user_message_index": 0, "agent_name": "root_agent", "llm_recording": {
    "llm_request": {"model": "gemini-2.5-flash", "contents": [` + userHi + `], "config": {"tools": [
      {"googleSearch": {}}, {"codeExecution": {}}, {"retrieval": {}}, {"urlContext": {}}
    ]}},
    "llm_responses": [{"content": {"role": "model", "parts": [{"text": "OK"}]}}]
  }}
]`},
	{name: "google-search-time-range", recordings: `[
  {"user_message_index": 0, "agent_name": "root_agent", "llm_recording": {
    "llm_request": {"model": "gemini-2.5-flash", "contents": [` + userHi + `], "config": {"tools": [
      {"googleSearch": {"timeRangeFilter": {"startTime": "2024-01-02T03:04:05.123456789Z"}, "excludeDomains": ["example.com"]}}
    ]}},
    "llm_responses": [{"content": {"role": "model", "parts": [{"text": "OK"}]}}]
  }}
]`},
	{name: "sanitize-http-options", recordings: `[
  {"user_message_index": 0, "agent_name": "root_agent", "llm_recording": {
    "llm_request": {"model": "gemini-2.5-flash", "contents": [` + userHi + `], "config": {
      "httpOptions": {"baseUrl": "https://example.com", "apiVersion": "v1", "headers": {"X-A": ["b"]}}, "temperature": 0.5
    }},
    "llm_responses": [{"content": {"role": "model", "parts": [{"text": "OK"}]}}]
  }}
]`},
	{name: "usage-metadata", recordings: `[
  {"user_message_index": 0, "agent_name": "root_agent", "llm_recording": {
    "llm_request": {"model": "gemini-2.5-flash", "contents": [` + userHi + `]},
    "llm_responses": [{"content": {"role": "model", "parts": [{"text": "Hello"}]}, "usageMetadata": {
      "promptTokenCount": 10, "candidatesTokenCount": 5, "totalTokenCount": 15, "thoughtsTokenCount": 0,
      "cachedContentTokenCount": 2, "toolUsePromptTokenCount": 3,
      "promptTokensDetails": [{"modality": "TEXT", "tokenCount": 10}, {"modality": "IMAGE"}],
      "candidatesTokensDetails": [{"modality": "TEXT", "tokenCount": 5}],
      "trafficType": "ON_DEMAND"
    }, "modelVersion": "gemini-2.5-flash-001"}]
  }}
]`},
	{name: "grounding-metadata", recordings: `[
  {"user_message_index": 0, "agent_name": "root_agent", "llm_recording": {
    "llm_request": {"model": "gemini-2.5-flash", "contents": [` + userHi + `]},
    "llm_responses": [{"content": {"role": "model", "parts": [{"text": "Paris is the capital."}]}, "groundingMetadata": {
      "groundingChunks": [{"web": {"uri": "https://example.com/paris", "title": "Paris", "domain": "example.com"}}, {"web": {}}],
      "groundingSupports": [{"segment": {"startIndex": 0, "endIndex": 21, "text": "Paris is the capital."}, "groundingChunkIndices": [0, 1], "confidenceScores": [0.9, 0.1, 0]}],
      "retrievalMetadata": {"googleSearchDynamicRetrievalScore": 0.25},
      "searchEntryPoint": {"renderedContent": "<div>search</div>", "sdkBlob": "AQID"},
      "webSearchQueries": ["capital of france"],
      "googleMapsWidgetContextToken": "tok"
    }}]
  }}
]`},
	{name: "citation-dates", recordings: `[
  {"user_message_index": 0, "agent_name": "root_agent", "llm_recording": {
    "llm_request": {"model": "gemini-2.5-flash", "contents": [` + userHi + `]},
    "llm_responses": [{"content": {"role": "model", "parts": [{"text": "Quoted."}]}, "citationMetadata": {"citations": [
      {"startIndex": 0, "endIndex": 7, "uri": "https://example.com/a", "title": "A", "license": "mit", "publicationDate": {"year": 2024, "month": 1, "day": 2}},
      {"startIndex": 1, "endIndex": 3, "uri": "https://example.com/b"}
    ]}}]
  }}
]`},
	{name: "citation-set-date", recordings: `[
  {"user_message_index": 0, "agent_name": "root_agent", "llm_recording": {
    "llm_request": {"model": "gemini-2.5-flash", "contents": [` + userHi + `]},
    "llm_responses": [{"content": {"role": "model", "parts": [{"text": "Quoted."}]}, "citationMetadata": {"citations": [
      {"uri": "https://example.com/a", "publicationDate": {"year": 1999, "month": 12, "day": 31}}
    ]}}]
  }}
]`},
	{name: "citation-partial-date", recordings: `[
  {"user_message_index": 0, "agent_name": "root_agent", "llm_recording": {
    "llm_request": {"model": "gemini-2.5-flash", "contents": [` + userHi + `]},
    "llm_responses": [{"content": {"role": "model", "parts": [{"text": "Quoted."}]}, "citationMetadata": {"citations": [
      {"uri": "https://example.com/a", "publicationDate": {"year": 2024}}
    ]}}]
  }}
]`},
	{name: "sse-partial", mode: "sse", recordings: `[
  {"user_message_index": 0, "agent_name": "root_agent", "llm_recording": {
    "llm_request": {"model": "gemini-2.5-flash", "contents": [` + userHi + `]},
    "llm_responses": [
      {"content": {"role": "model", "parts": [{"text": "Hel"}]}, "partial": true},
      {"content": {"role": "model", "parts": [{"text": "lo"}]}, "partial": true},
      {"content": {"role": "model", "parts": [{"text": "Hello"}]}, "turnComplete": true, "finishReason": "STOP"}
    ]
  }}
]`},
	{name: "finish-reason-error", recordings: `[
  {"user_message_index": 0, "agent_name": "root_agent", "llm_recording": {
    "llm_request": {"model": "gemini-2.5-flash", "contents": [` + userHi + `]},
    "llm_responses": [{"finishReason": "MAX_TOKENS", "errorCode": "MAX_TOKENS", "errorMessage": "too long", "interrupted": true, "sessionResumptionHandle": "h1"}]
  }}
]`},
	{name: "logprobs", recordings: `[
  {"user_message_index": 0, "agent_name": "root_agent", "llm_recording": {
    "llm_request": {"model": "gemini-2.5-flash", "contents": [` + userHi + `]},
    "llm_responses": [{"content": {"role": "model", "parts": [{"text": "Hi"}]}, "avgLogprobs": -0.123456789, "logprobsResult": {
      "chosenCandidates": [{"token": "Hi", "tokenId": 17, "logProbability": -0.1}, {"token": "!", "logProbability": 0}],
      "topCandidates": [{"candidates": [{"token": "Hi", "logProbability": -0.1}, {"token": "Hey", "logProbability": -2.3025851}]}],
      "logProbabilitySum": -0.33333334
    }}]
  }}
]`},
	{name: "custom-metadata", recordings: `[
  {"user_message_index": 0, "agent_name": "root_agent", "llm_recording": {
    "llm_request": {"model": "gemini-2.5-flash", "contents": [` + userHi + `]},
    "llm_responses": [{"content": {"role": "model", "parts": [{"text": "Hi"}]}, "customMetadata": {"a": 0, "b": "", "c": "x", "d": {}, "e": [1], "f": false, "g": 2.5}}]
  }}
]`},
	{name: "code-execution", recordings: `[
  {"user_message_index": 0, "agent_name": "root_agent", "llm_recording": {
    "llm_request": {"model": "gemini-2.5-flash", "contents": [` + userHi + `]},
    "llm_responses": [{"content": {"role": "model", "parts": [
      {"executableCode": {"language": "PYTHON", "code": "print(1)"}},
      {"codeExecutionResult": {"outcome": "OUTCOME_OK", "output": "1\n"}},
      {"text": "1", "partMetadata": {"k": "v", "n": 0}}
    ]}}]
  }}
]`},
	{name: "transcription-durations", recordings: `[
  {"user_message_index": 0, "agent_name": "root_agent", "llm_recording": {
    "llm_request": {"model": "gemini-2.5-flash", "contents": [` + userHi + `]},
    "llm_responses": [
      {"inputTranscription": {"text": "hello"}, "outputTranscription": {"text": "hi there", "finished": true, "startOffset": "1.5s", "endOffset": "90s"}},
      {"outputTranscription": {"text": "x", "startOffset": "0s", "endOffset": "3600s"}}
    ]
  }}
]`},
	{name: "video-metadata", recordings: `[
  {"user_message_index": 0, "agent_name": "root_agent", "llm_recording": {
    "llm_request": {"model": "gemini-2.5-flash", "contents": [{"role": "user", "parts": [
      {"fileData": {"fileUri": "gs://b/v.mp4", "mimeType": "video/mp4"}, "videoMetadata": {"startOffset": "5s", "endOffset": "65.5s", "fps": 2}},
      {"fileData": {"fileUri": "gs://b/w.mp4", "mimeType": "video/mp4"}, "videoMetadata": {"fps": 1}},
      {"fileData": {"fileUri": "gs://b/x.mp4", "mimeType": "video/mp4"}, "videoMetadata": {"endOffset": "10s"}}
    ]}]},
    "llm_responses": [{"content": {"role": "model", "parts": [{"text": "A video."}]}}]
  }}
]`},
	{name: "empty-request", recordings: `[
  {"user_message_index": 0, "agent_name": "", "llm_recording": {
    "llm_request": {"model": ""}
  }}
]`},
	{name: "tool-empty-id-name", recordings: `[
  {"user_message_index": 0, "agent_name": "root_agent", "tool_recording": {
    "tool_call": {}, "tool_response": {}
  }},
  {"user_message_index": 0, "agent_name": "root_agent", "tool_recording": {
    "tool_call": {"args": {}}, "tool_response": {"response": {}}
  }}
]`},
	{name: "tool-args", recordings: `[
  {"user_message_index": 0, "agent_name": "root_agent", "tool_recording": {
    "tool_call": {"id": "adk-7", "name": "get_weather", "args": {"city": "Paris", "days": 3, "metric": true, "ratio": 0.5,
      "empty": "", "zero": 0, "off": false, "nothing": null, "list": [], "obj": {}, "nested": {"z": 0, "e": "", "n": null}}},
    "tool_response": {"id": "adk-7", "name": "get_weather", "response": {"temp": 21.5, "ok": false, "error": null, "result": {"items": [], "count": 0}},
      "willContinue": false, "scheduling": "SILENT"}
  }},
  {"user_message_index": 0, "agent_name": "root_agent", "tool_recording": {
    "tool_call": {"id": "adk-8", "name": "long_op", "willContinue": true, "partialArgs": [{"jsonPath": "$.a", "stringValue": "x"}]},
    "tool_response": {"id": "adk-8", "name": "long_op", "parts": [{"inlineData": {"mimeType": "text/plain", "data": "aGk="}}]}
  }}
]`},
	{name: "mixed-sequence", recordings: `[
  {"user_message_index": 0, "agent_name": "root_agent", "llm_recording": {
    "llm_request": {"model": "gemini-2.5-flash", "contents": [{"role": "user", "parts": [{"text": "Weather?"}]}], "config": {
      "systemInstruction": {"parts": [{"text": "Use tools."}]},
      "tools": [{"functionDeclarations": [{"name": "get_weather", "description": "Gets weather.", "parameters": {"type": "OBJECT", "properties": {"city": {"type": "STRING"}}}}]}]
    }},
    "llm_responses": [{"content": {"role": "model", "parts": [{"functionCall": {"id": "adk-1", "name": "get_weather", "args": {"city": "Paris"}}}]},
      "usageMetadata": {"promptTokenCount": 7, "totalTokenCount": 9, "candidatesTokenCount": 2}}]
  }},
  {"user_message_index": 0, "agent_name": "root_agent", "tool_recording": {
    "tool_call": {"id": "adk-1", "name": "get_weather", "args": {"city": "Paris"}},
    "tool_response": {"id": "adk-1", "name": "get_weather", "response": {"temp": 21}}
  }},
  {"user_message_index": 0, "agent_name": "root_agent", "llm_recording": {
    "llm_request": {"model": "gemini-2.5-flash", "contents": [
      {"role": "user", "parts": [{"text": "Weather?"}]},
      {"role": "model", "parts": [{"functionCall": {"id": "adk-1", "name": "get_weather", "args": {"city": "Paris"}}}]},
      {"role": "user", "parts": [{"functionResponse": {"id": "adk-1", "name": "get_weather", "response": {"temp": 21}}}]}
    ], "config": {"systemInstruction": {"parts": [{"text": "Use tools."}]}}},
    "llm_responses": [{"content": {"role": "model", "parts": [{"text": "21 degrees."}]}, "finishReason": "STOP"}]
  }},
  {"user_message_index": 0, "agent_name": "helper_agent", "llm_recording": {
    "llm_request": {"model": "gemini-2.5-flash", "contents": [{"role": "user", "parts": [{"text": "Weather?"}]}]},
    "llm_responses": [{"content": {"role": "model", "parts": [{"text": "Delegated."}]}}]
  }}
]`},
	{name: "merge-second-message", index: 1, existingFrom: "mixed-sequence", recordings: `[
  {"user_message_index": 1, "agent_name": "root_agent", "llm_recording": {
    "llm_request": {"model": "gemini-2.5-flash", "contents": [{"role": "user", "parts": [{"text": "Thanks"}]}]},
    "llm_responses": [{"content": {"role": "model", "parts": [{"text": "You are welcome."}]}}]
  }}
]`},
	{name: "merge-replace-message", index: 0, existingFrom: "merge-second-message", recordings: `[
  {"user_message_index": 0, "agent_name": "root_agent", "llm_recording": {
    "llm_request": {"model": "gemini-2.5-flash", "contents": [{"role": "user", "parts": [{"text": "Redo"}]}]},
    "llm_responses": [{"content": {"role": "model", "parts": [{"text": "Redone."}]}}]
  }}
]`},
	{name: "merge-corrupt-existing", index: 0, existing: "recordings: [unclosed\n", recordings: `[
  {"user_message_index": 0, "agent_name": "root_agent", "llm_recording": {
    "llm_request": {"model": "gemini-2.5-flash", "contents": [` + userHi + `]},
    "llm_responses": [{"content": {"role": "model", "parts": [{"text": "Fresh."}]}}]
  }}
]`},
	{name: "merge-go-key-existing", index: 2, existing: `recordings:
  - user_message_index: 0
    agent_name: a
    llm_recording:
      llm_request:
        model: m
        contents:
          - role: user
            parts:
              - text: hi
              - functioncall:
                  name: f
                  args: {x: 1}
                thoughtsignature: [1, 2]
        config:
          systeminstruction:
            parts:
              - text: sys
          temperature: 0.5
      llm_responses:
        - content:
            role: model
            parts:
              - text: ok
          usagemetadata:
            totaltokencount: 3
          finishreason: STOP
  - user_message_index: 1
    agent_name: b
    tool_recording:
      tool_call:
        name: t
        args: {}
      tool_response:
        name: t
        response: {r: 1}
  - user_message_index: 2
    agent_name: dropped
`, recordings: `[
  {"user_message_index": 2, "agent_name": "root_agent", "tool_recording": {
    "tool_call": {"id": "c", "name": "t", "args": {"y": 2}},
    "tool_response": {"id": "c", "name": "t", "response": {"r": 2}}
  }}
]`},
	{name: "merge-type-error-existing", index: 1, existing: `recordings:
  - user_message_index: 0
    agent_name: a
    llm_recording:
      llm_request:
        model: m
        config:
          temperature: hot
      llm_responses:
        - content:
            parts: not-a-list
  - user_message_index: oops
    agent_name: b
`, recordings: `[
  {"user_message_index": 1, "agent_name": "root_agent", "llm_recording": {
    "llm_request": {"model": "gemini-2.5-flash", "contents": [` + userHi + `]},
    "llm_responses": [{"content": {"role": "model", "parts": [{"text": "OK"}]}}]
  }}
]`},
	{name: "merge-self-written", index: 1, existingFrom: "function-call-args", recordings: `[]`},
}
