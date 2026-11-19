# gadk-genai

Gemini-compatible data types for gadk: the messages, generation settings, tool declarations and
responses that agents, models, sessions and tools exchange. The types mirror the JSON schema of
the Gemini API and Vertex AI REST APIs (camelCase field names, zero values omitted, bytes as
base64), so they serialize straight onto the wire.

This crate holds types only; it has no HTTP client. The model crates (`gadk-gemini`,
`gadk-vertexai`, and the Anthropic and OpenAI adapters) send and receive these types, and the
rest of gadk uses them as its common message format. Most users reach it through the `gadk`
facade as `gadk::genai` rather than depending on it directly.

Main types:

- `Content` and `Part`: a message and its pieces (text, inline or file data, function calls and
  responses, executable code, thoughts). `Part::from_text`, `Content::from_text` and the other
  `from_*` constructors build them.
- `GenerateContentConfig`: sampling, safety, tools, structured output (`Schema`), thinking,
  speech and image settings for a request.
- `Tool`, `FunctionDeclaration` and `ToolConfig`: function calling and built-in tools such as
  Google Search, code execution and URL context.
- `GenerateContentResponse` and `Candidate`: model output with finish reasons, safety ratings,
  grounding and usage metadata.
- `LiveConnectConfig` and `LiveServerMessage`: Live API (bidirectional streaming) session setup
  and server messages.

String enums such as `FinishReason` or `HarmCategory` keep values this version does not know in
an `Unknown(String)` variant, so newer API values round-trip unchanged.

## Example

```rust
use gadk::genai::{
    Content, FunctionDeclaration, GenerateContentConfig, Schema, Tool, Type, ROLE_USER,
};

let user = Content::from_text("What's the weather in Paris?", ROLE_USER);

let config = GenerateContentConfig {
    system_instruction: Some(Content::from_text("Answer briefly.", ROLE_USER)),
    temperature: Some(0.2),
    tools: vec![Tool {
        function_declarations: vec![FunctionDeclaration {
            name: "get_weather".into(),
            description: "Current weather for a city.".into(),
            parameters: Some(Schema {
                r#type: Some(Type::Object),
                properties: [("city".to_string(), Schema {
                    r#type: Some(Type::String),
                    ..Default::default()
                })]
                .into(),
                required: vec!["city".into()],
                ..Default::default()
            }),
            ..Default::default()
        }],
        ..Default::default()
    }],
    ..Default::default()
};

let json = serde_json::to_string(&config)?; // the config's wire JSON
```

## Notes

- `serde_util` has the serde helpers the types use (base64 bytes, duration strings, 64-bit
  integers as quoted strings).
- Some fields apply only to the Gemini API or only to Vertex AI; their docs say so where the
  difference matters.
