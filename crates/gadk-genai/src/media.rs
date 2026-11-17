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

//! File and image types.

use crate::serde_util::{base64_bytes, i64_string_opt, is_zero};
use bytes::Bytes;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeMap;

crate::string_enum!(
    /// Enum that controls the generation of people.
    PersonGeneration {
        DontAllow => "DONT_ALLOW",
        AllowAdult => "ALLOW_ADULT",
        AllowAll => "ALLOW_ALL",
    }
);

crate::string_enum!(
    /// Enum that controls the safety filter level for objectionable content.
    SafetyFilterLevel {
        BlockLowAndAbove => "BLOCK_LOW_AND_ABOVE",
        BlockMediumAndAbove => "BLOCK_MEDIUM_AND_ABOVE",
        BlockOnlyHigh => "BLOCK_ONLY_HIGH",
        BlockNone => "BLOCK_NONE",
    }
);

crate::string_enum!(
    /// Enum that specifies the language of the text in the prompt.
    ImagePromptLanguage {
        Auto => "auto",
        En => "en",
        Ja => "ja",
        Ko => "ko",
        Hi => "hi",
        Zh => "zh",
        Pt => "pt",
        Es => "es",
    }
);

crate::string_enum!(
    /// State for the lifecycle of a File.
    FileState {
        Unspecified => "STATE_UNSPECIFIED",
        Processing => "PROCESSING",
        Active => "ACTIVE",
        Failed => "FAILED",
    }
);

crate::string_enum!(
    /// Source of the File.
    FileSource {
        Unspecified => "SOURCE_UNSPECIFIED",
        Uploaded => "UPLOADED",
        Generated => "GENERATED",
        Registered => "REGISTERED",
    }
);

/// The configuration for generating images. You
/// can find API default values and more details at Gemini Enterprise Agent Platform:
/// <https://cloud.google.com/vertex-ai/generative-ai/docs/model-reference/imagen-api>.
/// GeminiAPI: <https://ai.google.dev/gemini-api/docs/imagen#imagen-model>
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GenerateImagesConfig {
    #[serde(default, skip_serializing_if = "is_zero")]
    pub output_gcs_uri: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub negative_prompt: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub number_of_images: i32,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub aspect_ratio: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guidance_scale: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub safety_filter_level: Option<SafetyFilterLevel>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub person_generation: Option<PersonGeneration>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub include_safety_attributes: bool,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub include_rai_reason: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<ImagePromptLanguage>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub output_mime_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_compression_quality: Option<i32>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub add_watermark: bool,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub image_size: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub enhance_prompt: bool,
}

/// An image.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Image {
    #[serde(default, skip_serializing_if = "is_zero")]
    pub gcs_uri: String,
    #[serde(
        default,
        skip_serializing_if = "Bytes::is_empty",
        with = "base64_bytes"
    )]
    pub image_bytes: Bytes,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub mime_type: String,
}

/// Safety attributes of a GeneratedImage or the user-provided prompt.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SafetyAttributes {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub categories: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scores: Vec<f32>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub content_type: String,
}

/// An output image.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GeneratedImage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<Image>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub rai_filtered_reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub safety_attributes: Option<SafetyAttributes>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub enhanced_prompt: String,
}

/// The output images response.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GenerateImagesResponse {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub generated_images: Vec<GeneratedImage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub positive_prompt_safety_attributes: Option<SafetyAttributes>,
}

/// Status of a File that uses a common error model.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileStatus {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub details: Vec<Map<String, Value>>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<i32>,
}

/// A file uploaded to the API.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct File {
    #[serde(default, skip_serializing_if = "is_zero")]
    pub name: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub display_name: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub mime_type: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "i64_string_opt"
    )]
    pub size_bytes: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub create_time: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expiration_time: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub update_time: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub sha256_hash: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub uri: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub download_uri: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<FileState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<FileSource>,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub video_metadata: Map<String, Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<FileStatus>,
}
