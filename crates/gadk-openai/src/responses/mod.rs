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

//! The Responses API, `POST {base}responses`, split three ways: [`request`] (contents,
//! messages, media, calls), [`generation_config`] (config translation,
//! thinking, service tiers) and [`schema`] (the JSON-schema text format).

pub(crate) mod consts;
pub(crate) mod generation_config;
pub(crate) mod model;
pub(crate) mod request;
pub(crate) mod response;
pub(crate) mod schema;
pub(crate) mod stream;
pub(crate) mod tools;
pub(crate) mod wire;
