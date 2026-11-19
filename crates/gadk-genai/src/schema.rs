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

//! OpenAPI-subset schema.

use crate::serde_util::is_zero;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

crate::string_enum!(
    /// The JSON type of a [`Schema`] (OpenAPI data type).
    Type {
        /// Not specified.
        Unspecified => "TYPE_UNSPECIFIED",
        /// A string.
        String => "STRING",
        /// A floating-point number.
        Number => "NUMBER",
        /// An integer.
        Integer => "INTEGER",
        /// A boolean.
        Boolean => "BOOLEAN",
        /// An array; see [`Schema::items`].
        Array => "ARRAY",
        /// An object; see [`Schema::properties`].
        Object => "OBJECT",
        /// JSON `null`.
        Null => "NULL",
    }
);

/// An OpenAPI-subset schema describing the shape of a function parameter or response.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Schema {
    /// The value must match at least one of these schemas.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub any_of: Vec<Schema>,
    /// An arbitrary JSON value; a JSON `null` decodes to `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<Value>,
    /// Description of the value; the model reads it to understand the field.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub description: String,
    /// Allowed values of a `STRING` schema.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub r#enum: Vec<String>,
    /// An example value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub example: Option<Value>,
    /// Format of the value, such as `"int32"`, `"double"`, `"date-time"` or `"enum"`.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub format: String,
    /// Schema of the elements of an `ARRAY`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub items: Option<Box<Schema>>,
    /// Maximum number of elements of an `ARRAY`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_items: Option<i64>,
    /// Maximum length of a `STRING`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_length: Option<i64>,
    /// Maximum number of properties of an `OBJECT`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_properties: Option<i64>,
    /// Maximum value of a `NUMBER` or `INTEGER`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub maximum: Option<f64>,
    /// Minimum number of elements of an `ARRAY`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_items: Option<i64>,
    /// Minimum length of a `STRING`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_length: Option<i64>,
    /// Minimum number of properties of an `OBJECT`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_properties: Option<i64>,
    /// Minimum value of a `NUMBER` or `INTEGER`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minimum: Option<f64>,
    /// Whether the value may be `null`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nullable: Option<bool>,
    /// Regular expression a `STRING` must match.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub pattern: String,
    /// Schemas of the properties of an `OBJECT`, by name.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub properties: BTreeMap<String, Schema>,
    /// The order in which the model should produce the properties of an `OBJECT`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub property_ordering: Vec<String>,
    /// Names of the required properties of an `OBJECT`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required: Vec<String>,
    /// Title of the schema.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub title: String,
    /// The data type of the value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r#type: Option<Type>,
}
