// Copyright 2026 Chris Olson
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

//! The reshaping of a JSON-schema `serde_json::Value` (what gadk holds where Go holds a
//! `*jsonschema.Schema`, jsonschema-go v0.4.3) into the yaml.v3 reflection of jsonschema-go's
//! `Schema` struct: Go field keys (`$ref` → `ref`, `$defs` → `defs`, …), Go field order, Go's
//! emitted zero fields, sorted `properties`, `default` as a `[]byte` sequence, and the boolean
//! schema `false` as `{not: {}}`. Task 5 owns this file; nothing outside `gonode` uses its
//! items directly.
