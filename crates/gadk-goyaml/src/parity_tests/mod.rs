// Copyright 2026 Chris Olson
// Portions derived from gopkg.in/yaml.v3 (https://github.com/go-yaml/yaml), Copyright 2011-2019 Canonical Ltd.
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

//! Ports of yaml.v3 v3.0.1's test files (ledger `docs/parity/6c-1.md`): one module per Go
//! file, `yaml_v3_<file>`, holding `fn test_<snake of TestName>` (and
//! `example_unmarshal_embedded`).

mod yaml_v3_decode;
mod yaml_v3_encode;
mod yaml_v3_example_embedded;
mod yaml_v3_limit;
mod yaml_v3_node;
