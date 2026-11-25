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

//! Shared pieces of the pages that show tokens and cost.

use gadk_analytics::cost::Usage;
use maud::{Markup, html};

use crate::prices::thousands;

/// The note shown when the console has no price file.
pub(crate) const NO_PRICES: &str =
    "No price file: start the console with --prices <file> to see costs.";
/// The note shown under a cost marked `*`.
pub(crate) const PARTIAL_NOTE: &str = "* Costs exclude calls to models without a price.";

/// Four `td class="num"` cells: input, cached, output and reasoning tokens.
pub(crate) fn token_cells(u: &Usage) -> Markup {
    html! {
        td class="num" { (thousands(u.input)) }
        td class="num" { (thousands(u.cached)) }
        td class="num" { (thousands(u.output)) }
        td class="num" { (thousands(u.reasoning)) }
    }
}
