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

//! Request body construction.
//!
//! Ports genai `models.go` `generateContentParametersToMldev` /
//! `generateContentParametersToVertex` with their config, content, part, tool, tool-config,
//! safety-setting, image-config and speech-config converters (genai v1.75.0), and ADK-Go
//! `model/gemini/gemini.go` `preserveEmptyTextThoughtSignatureParts`.

use gadk_genai::{Content, GenerateContentConfig};
use gadk_model::{Backend, ExtrasRequestProvider, ModelError};
use serde_json::{Map, Value};

type Obj = Map<String, Value>;

/// Where a request is going: the backend plus the Vertex AI project and location used to
/// expand resource names (genai `tResourceName`).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Target<'a> {
    pub backend: Backend,
    pub project: Option<&'a str>,
    pub location: Option<&'a str>,
}

/// Builds the JSON body of a `generateContent` request for `backend`, applies `extras`
/// (if any), then restores empty text on content-free thought-signature parts.
///
/// Vertex AI cached-content names are not expanded with a project/location here (there is
/// none); the model does that with its resolved configuration.
pub fn build_request_body(
    backend: Backend,
    contents: &[Content],
    cfg: &GenerateContentConfig,
    extras: Option<&ExtrasRequestProvider>,
) -> Result<Obj, ModelError> {
    let target = Target {
        backend,
        project: None,
        location: None,
    };
    build_body(target, contents, cfg, extras)
}

pub(crate) fn build_body(
    target: Target<'_>,
    contents: &[Content],
    cfg: &GenerateContentConfig,
    extras: Option<&ExtrasRequestProvider>,
) -> Result<Obj, ModelError> {
    let mut body = generate_content_parameters(target, contents, cfg)?;
    if let Some(provider) = extras {
        body = provider(body);
    }
    preserve_empty_text_thought_signature_parts(&mut body);
    Ok(body)
}

/// Port of gemini.go `preserveEmptyTextThoughtSignatureParts` (works around
/// googleapis/go-genai#931): every part with a `thoughtSignature` but no data field gets
/// `"text": ""`.
pub fn preserve_empty_text_thought_signature_parts(body: &mut Obj) {
    for content in maps_from_slice(body.get_mut("contents")) {
        for part in maps_from_slice(content.get_mut("parts")) {
            if !part.contains_key("thoughtSignature") || part_has_data(part) {
                continue;
            }
            part.insert("text".into(), Value::String(String::new()));
        }
    }
}

/// Port of gemini.go `mapsFromSlice`: the object entries of an array (others skipped).
fn maps_from_slice(value: Option<&mut Value>) -> impl Iterator<Item = &mut Obj> {
    let items: &mut [Value] = match value {
        Some(Value::Array(items)) => items,
        _ => &mut [],
    };
    items.iter_mut().filter_map(Value::as_object_mut)
}

/// Port of gemini.go `partHasData`.
fn part_has_data(part: &Obj) -> bool {
    part.keys().any(|field| {
        !matches!(
            field.as_str(),
            "audioTranscription"
                | "mediaProcessing"
                | "mediaResolution"
                | "partMetadata"
                | "speechMetadata"
                | "thought"
                | "thoughtSignature"
                | "videoMetadata"
        )
    })
}

// ---- genai converters ---------------------------------------------------------------

fn to_value<T: serde::Serialize>(v: &T) -> Result<Value, ModelError> {
    serde_json::to_value(v).map_err(|e| ModelError::Other(Box::new(e)))
}

/// The error genai returns for a field the backend does not support.
fn unsupported(field: &str, backend: Backend) -> ModelError {
    let msg = match backend {
        Backend::GeminiApi => format!(
            "{field} parameter is only supported in Gemini Enterprise Agent Platform mode, not in Gemini Developer API mode."
        ),
        Backend::VertexAi => format!(
            "{field} parameter is only supported in Gemini Developer API mode, not in Gemini Enterprise Agent Platform mode."
        ),
    };
    ModelError::Config(msg)
}

/// Fails if any of `fields` is present in `from`.
fn reject(from: &Obj, fields: &[&str], backend: Backend) -> Result<(), ModelError> {
    match fields.iter().find(|f| present(from, f)) {
        Some(f) => Err(unsupported(f, backend)),
        None => Ok(()),
    }
}

/// genai `getValueByPath(...) != nil`.
fn present(from: &Obj, field: &str) -> bool {
    from.get(field).is_some_and(|v| !v.is_null())
}

/// Copies each present `fields` entry from `from` into `to` unchanged.
fn copy(from: &Obj, to: &mut Obj, fields: &[&str]) {
    for f in fields {
        if let Some(v) = from.get(*f).filter(|v| !v.is_null()) {
            to.insert((*f).to_string(), v.clone());
        }
    }
}

/// genai `setValueByPath` for a single key: an existing object value is merged into.
fn set_merge(to: &mut Obj, key: &str, value: Value) {
    match (to.get_mut(key), value) {
        (Some(Value::Object(existing)), Value::Object(new)) => existing.extend(new),
        (_, value) => {
            to.insert(key.to_string(), value);
        }
    }
}

fn as_obj(v: &Value) -> Obj {
    v.as_object().cloned().unwrap_or_default()
}

/// Applies `f` to every object of the array at `from[field]` and stores the result in
/// `to[field]`.
fn convert_list(
    from: &Obj,
    to: &mut Obj,
    field: &str,
    mut f: impl FnMut(&Obj) -> Result<Obj, ModelError>,
) -> Result<(), ModelError> {
    if let Some(Value::Array(items)) = from.get(field) {
        let out = items
            .iter()
            .map(|item| f(&as_obj(item)).map(Value::Object))
            .collect::<Result<Vec<_>, _>>()?;
        to.insert(field.to_string(), Value::Array(out));
    }
    Ok(())
}

/// Applies `f` to the object at `from[field]` and stores the result in `to[field]`.
fn convert_obj(
    from: &Obj,
    to: &mut Obj,
    field: &str,
    f: impl FnOnce(&Obj) -> Result<Obj, ModelError>,
) -> Result<(), ModelError> {
    if let Some(v) = from.get(field).filter(|v| !v.is_null()) {
        let converted = f(&as_obj(v))?;
        to.insert(field.to_string(), Value::Object(converted));
    }
    Ok(())
}

/// Port of `generateContentParametersTo{Mldev,Vertex}` (minus the `_url.model`, which
/// the transport turns into the request path).
fn generate_content_parameters(
    target: Target<'_>,
    contents: &[Content],
    cfg: &GenerateContentConfig,
) -> Result<Obj, ModelError> {
    let mut to = Obj::new();
    // A nil Go slice marshals to null and is skipped; gadk has no nil/empty distinction.
    if !contents.is_empty() {
        let converted = contents
            .iter()
            .map(|c| content(&as_obj(&to_value(c)?), target.backend).map(Value::Object))
            .collect::<Result<Vec<_>, _>>()?;
        to.insert("contents".into(), Value::Array(converted));
    }
    let from = as_obj(&to_value(cfg)?);
    // ADK always passes a non-nil config, so genai always emits generationConfig.
    let generation_config = generate_content_config(target, &from, &mut to)?;
    to.insert("generationConfig".into(), Value::Object(generation_config));
    Ok(to)
}

/// Port of `generateContentConfigTo{Mldev,Vertex}`: returns the `generationConfig` object
/// and moves the request-level fields into `parent`.
fn generate_content_config(
    target: Target<'_>,
    from: &Obj,
    parent: &mut Obj,
) -> Result<Obj, ModelError> {
    let backend = target.backend;
    let vertex = backend == Backend::VertexAi;
    let mut to = Obj::new();

    if let Some(si) = from.get("systemInstruction").filter(|v| !v.is_null()) {
        parent.insert(
            "systemInstruction".into(),
            Value::Object(content(&as_obj(si), backend)?),
        );
    }

    copy(
        from,
        &mut to,
        &[
            "temperature",
            "topP",
            "topK",
            "candidateCount",
            "maxOutputTokens",
            "stopSequences",
            "responseLogprobs",
            "logprobs",
            "presencePenalty",
            "frequencyPenalty",
            "seed",
            "responseMimeType",
            "responseSchema",
            "responseJsonSchema",
        ],
    );

    if vertex {
        copy(from, &mut to, &["routingConfig"]);
        if let Some(v) = from.get("modelSelectionConfig").filter(|v| !v.is_null()) {
            to.insert("modelConfig".into(), v.clone());
        }
        copy(from, parent, &["safetySettings"]);
    } else {
        reject(from, &["routingConfig", "modelSelectionConfig"], backend)?;
        convert_list(from, parent, "safetySettings", |s| {
            reject(s, &["method"], backend)?;
            let mut out = Obj::new();
            copy(s, &mut out, &["category", "threshold"]);
            Ok(out)
        })?;
    }

    convert_list(from, parent, "tools", |t| tool(t, backend))?;
    convert_obj(from, parent, "toolConfig", |t| tool_config(t, backend))?;
    copy(from, parent, &["labels"]);
    if let Some(Value::String(name)) = from.get("cachedContent") {
        parent.insert(
            "cachedContent".into(),
            Value::String(cached_content_name(target, name)),
        );
    }

    copy(from, &mut to, &["responseModalities", "mediaResolution"]);
    if vertex {
        convert_obj(from, &mut to, "speechConfig", speech_config_vertex)?;
        copy(from, &mut to, &["audioTimestamp"]);
    } else {
        copy(from, &mut to, &["speechConfig"]);
        reject(from, &["audioTimestamp"], backend)?;
    }
    copy(from, &mut to, &["thinkingConfig"]);
    convert_obj(from, &mut to, "imageConfig", |c| image_config(c, backend))?;
    if vertex {
        reject(from, &["enableEnhancedCivicAnswers"], backend)?;
        copy(from, parent, &["modelArmorConfig"]);
    } else {
        copy(from, &mut to, &["enableEnhancedCivicAnswers"]);
        reject(from, &["modelArmorConfig"], backend)?;
    }
    copy(from, parent, &["serviceTier"]);
    copy(from, &mut to, &["audioTranscriptionConfig"]);
    copy(from, parent, &["continuationToken"]);
    Ok(to)
}

/// Port of `contentTo{Mldev,Vertex}`.
fn content(from: &Obj, backend: Backend) -> Result<Obj, ModelError> {
    let mut to = Obj::new();
    convert_list(from, &mut to, "parts", |p| part(p, backend))?;
    copy(from, &mut to, &["role"]);
    Ok(to)
}

/// Port of `partTo{Mldev,Vertex}`.
fn part(from: &Obj, backend: Backend) -> Result<Obj, ModelError> {
    let mut to = Obj::new();
    copy(
        from,
        &mut to,
        &[
            "mediaResolution",
            "codeExecutionResult",
            "executableCode",
            "fileData",
        ],
    );
    match backend {
        Backend::GeminiApi => convert_obj(from, &mut to, "functionCall", |fc| {
            let mut out = Obj::new();
            copy(fc, &mut out, &["id", "args", "name"]);
            reject(fc, &["partialArgs", "willContinue"], backend)?;
            Ok(out)
        })?,
        Backend::VertexAi => copy(from, &mut to, &["functionCall"]),
    }
    copy(
        from,
        &mut to,
        &[
            "functionResponse",
            "inlineData",
            "text",
            "thought",
            "thoughtSignature",
            "videoMetadata",
        ],
    );
    match backend {
        Backend::GeminiApi => copy(from, &mut to, &["toolCall", "toolResponse", "partMetadata"]),
        Backend::VertexAi => reject(from, &["toolCall", "toolResponse", "partMetadata"], backend)?,
    }
    copy(
        from,
        &mut to,
        &["audioTranscription", "mediaProcessing", "speechMetadata"],
    );
    Ok(to)
}

/// Port of `toolTo{Mldev,Vertex}` (with `googleSearchToMldev`, `googleMapsToMldev`,
/// `authConfigToMldev`, `mcpServerToVertex`).
fn tool(from: &Obj, backend: Backend) -> Result<Obj, ModelError> {
    let mut to = Obj::new();
    match backend {
        Backend::GeminiApi => {
            reject(from, &["retrieval"], backend)?;
            copy(from, &mut to, &["computerUse", "fileSearch"]);
            convert_obj(from, &mut to, "googleSearch", |gs| {
                let mut out = Obj::new();
                copy(gs, &mut out, &["searchTypes"]);
                reject(gs, &["blockingConfidence", "excludeDomains"], backend)?;
                copy(gs, &mut out, &["timeRangeFilter"]);
                Ok(out)
            })?;
            convert_obj(from, &mut to, "googleMaps", |gm| {
                let mut out = Obj::new();
                convert_obj(gm, &mut out, "authConfig", |ac| {
                    let mut out = Obj::new();
                    copy(ac, &mut out, &["apiKey"]);
                    reject(
                        ac,
                        &[
                            "apiKeyConfig",
                            "authType",
                            "googleServiceAccountConfig",
                            "httpBasicAuthConfig",
                            "oauthConfig",
                            "oidcConfig",
                        ],
                        backend,
                    )?;
                    Ok(out)
                })?;
                copy(gm, &mut out, &["enableWidget"]);
                reject(gm, &["groundingTypes"], backend)?;
                Ok(out)
            })?;
            copy(from, &mut to, &["codeExecution"]);
            reject(from, &["enterpriseWebSearch"], backend)?;
            copy(
                from,
                &mut to,
                &["functionDeclarations", "googleSearchRetrieval"],
            );
            reject(from, &["parallelAiSearch"], backend)?;
            copy(from, &mut to, &["urlContext", "mcpServers"]);
            reject(from, &["exaAiSearch"], backend)?;
        }
        Backend::VertexAi => {
            copy(from, &mut to, &["retrieval", "computerUse"]);
            reject(from, &["fileSearch"], backend)?;
            copy(
                from,
                &mut to,
                &[
                    "googleSearch",
                    "googleMaps",
                    "codeExecution",
                    "enterpriseWebSearch",
                    "functionDeclarations",
                    "googleSearchRetrieval",
                    "parallelAiSearch",
                    "urlContext",
                ],
            );
            convert_list(from, &mut to, "mcpServers", |m| {
                reject(m, &["name", "streamableHttpTransport"], backend)?;
                Ok(Obj::new())
            })?;
            copy(from, &mut to, &["exaAiSearch"]);
        }
    }
    Ok(to)
}

/// Port of `toolConfigTo{Mldev,Vertex}` (with `functionCallingConfigToMldev`).
fn tool_config(from: &Obj, backend: Backend) -> Result<Obj, ModelError> {
    let mut to = Obj::new();
    copy(from, &mut to, &["retrievalConfig"]);
    match backend {
        Backend::GeminiApi => {
            convert_obj(from, &mut to, "functionCallingConfig", |fcc| {
                let mut out = Obj::new();
                copy(fcc, &mut out, &["allowedFunctionNames", "mode"]);
                reject(fcc, &["streamFunctionCallArguments"], backend)?;
                Ok(out)
            })?;
            copy(from, &mut to, &["includeServerSideToolInvocations"]);
        }
        Backend::VertexAi => {
            copy(from, &mut to, &["functionCallingConfig"]);
            reject(from, &["includeServerSideToolInvocations"], backend)?;
        }
    }
    Ok(to)
}

/// Port of `imageConfigTo{Mldev,Vertex}`.
fn image_config(from: &Obj, backend: Backend) -> Result<Obj, ModelError> {
    let mut to = Obj::new();
    copy(from, &mut to, &["aspectRatio", "imageSize"]);
    match backend {
        Backend::GeminiApi => reject(
            from,
            &[
                "personGeneration",
                "prominentPeople",
                "outputMimeType",
                "outputCompressionQuality",
                "imageOutputOptions",
            ],
            backend,
        )?,
        Backend::VertexAi => {
            copy(from, &mut to, &["personGeneration", "prominentPeople"]);
            for (field, key) in [
                ("outputMimeType", "mimeType"),
                ("outputCompressionQuality", "compressionQuality"),
            ] {
                if let Some(v) = from.get(field).filter(|v| !v.is_null()) {
                    let mut nested = Obj::new();
                    nested.insert(key.into(), v.clone());
                    set_merge(&mut to, "imageOutputOptions", Value::Object(nested));
                }
            }
            if let Some(v) = from.get("imageOutputOptions").filter(|v| !v.is_null()) {
                set_merge(&mut to, "imageOutputOptions", v.clone());
            }
        }
    }
    Ok(to)
}

/// Port of `speechConfigToVertex` (with `voiceConfigToVertex`,
/// `multiSpeakerVoiceConfigToVertex`, `speakerVoiceConfigToVertex`,
/// `replicatedVoiceConfigToVertex`).
fn speech_config_vertex(from: &Obj) -> Result<Obj, ModelError> {
    let mut to = Obj::new();
    convert_obj(from, &mut to, "voiceConfig", voice_config_vertex)?;
    copy(from, &mut to, &["languageCode"]);
    convert_obj(from, &mut to, "multiSpeakerVoiceConfig", |m| {
        let mut out = Obj::new();
        convert_list(m, &mut out, "speakerVoiceConfigs", |s| {
            let mut out = Obj::new();
            copy(s, &mut out, &["speaker"]);
            convert_obj(s, &mut out, "voiceConfig", voice_config_vertex)?;
            Ok(out)
        })?;
        Ok(out)
    })?;
    Ok(to)
}

fn voice_config_vertex(from: &Obj) -> Result<Obj, ModelError> {
    let mut to = Obj::new();
    convert_obj(from, &mut to, "replicatedVoiceConfig", |r| {
        let mut out = Obj::new();
        copy(r, &mut out, &["mimeType", "voiceSampleAudio"]);
        reject(
            r,
            &["consentAudio", "voiceConsentSignature"],
            Backend::VertexAi,
        )?;
        Ok(out)
    })?;
    copy(from, &mut to, &["prebuiltVoiceConfig", "voice"]);
    Ok(to)
}

/// Port of genai `tCachedContentName` / `tResourceName(name, "cachedContents", 2)`.
fn cached_content_name(target: Target<'_>, name: &str) -> String {
    const COLLECTION: &str = "cachedContents";
    let full = format!("{COLLECTION}/{name}");
    let should_prepend = !name.starts_with("cachedContents/") && full.matches('/').count() + 1 == 2;
    let project = target.project.unwrap_or_default();
    let location = target.location.unwrap_or_default();
    match target.backend {
        Backend::VertexAi => {
            if name.starts_with("projects/") {
                name.to_string()
            } else if name.starts_with("locations/") && !project.is_empty() {
                format!("projects/{project}/{name}")
            } else if name.starts_with("cachedContents/")
                && !project.is_empty()
                && !location.is_empty()
            {
                format!("projects/{project}/locations/{location}/{name}")
            } else if should_prepend && !project.is_empty() && !location.is_empty() {
                format!("projects/{project}/locations/{location}/{full}")
            } else if should_prepend {
                full
            } else {
                name.to_string()
            }
        }
        Backend::GeminiApi => {
            if should_prepend {
                full
            } else {
                name.to_string()
            }
        }
    }
}
