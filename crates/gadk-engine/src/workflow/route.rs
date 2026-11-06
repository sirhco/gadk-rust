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

//! `Route`, `RouteSummary`, route kinds, `DEFAULT` and `Edge` (port of Go workflow/workflow.go
//! routes, the label logic of workflow/validation.go, and Go internal/workflowroute).

use std::any::Any;
use std::fmt;
use std::sync::{Arc, LazyLock};

use gadk_session::Event;
use parking_lot::RwLock;
use serde_json::Value;

use crate::error::WorkflowError;
use crate::workflow::Node;

/// A typed view of a route value, the input to Go validation.go summarizeOutgoingEdges
/// (internal/workflowroute.Value). `Multi` holds one summary per MultiRoute element; `Other`
/// carries Go's KindOther label (`%T` of a custom route or element, or `"<nil>"`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouteSummary {
    /// The `DEFAULT` route.
    Default,
    /// A string route value.
    String(String),
    /// An integer route value.
    Int(i64),
    /// A boolean route value.
    Bool(bool),
    /// A `MultiRoute`: one summary per element.
    Multi(Vec<RouteSummary>),
    /// Any other route value, described by a label.
    Other(String),
}

/// Port of Go workflow.Route: defines the interface for matching execution results to edges.
pub trait Route: Send + Sync {
    /// Reports whether `event` was routed along this route.
    fn matches(&self, event: &Event) -> bool;
    /// The default reproduces Go's `%T` fallback for custom routes (KindOther with the type name).
    fn summary(&self) -> RouteSummary {
        RouteSummary::Other(std::any::type_name::<Self>().to_string())
    }
}

/// Port of Go matchRoute (workflow.go:72).
fn match_route(route_value: &str, event: &Event) -> bool {
    event.routes.iter().any(|v| v == route_value)
}

/// Port of Go workflow.StringRoute: a route defined by a string value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StringRoute(pub String);

/// Port of Go workflow.IntRoute: a route defined by an integer value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IntRoute(pub i64);

/// Port of Go workflow.BoolRoute: a route defined by a boolean value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BoolRoute(pub bool);

/// Port of Go workflow.MultiRoute: matches any value within a list of allowed routes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MultiRoute<T>(pub Vec<T>);

impl fmt::Display for StringRoute {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Display for IntRoute {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl fmt::Display for BoolRoute {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl Route for StringRoute {
    /// Reports whether the event was routed to this string value.
    fn matches(&self, e: &Event) -> bool {
        match_route(&self.0, e)
    }
    fn summary(&self) -> RouteSummary {
        RouteSummary::String(self.0.clone())
    }
}

impl Route for IntRoute {
    /// Reports whether the event was routed to this integer value, compared by its decimal form.
    fn matches(&self, e: &Event) -> bool {
        match_route(&self.0.to_string(), e)
    }
    fn summary(&self) -> RouteSummary {
        RouteSummary::Int(self.0)
    }
}

impl Route for BoolRoute {
    /// Reports whether the event was routed to this boolean value ("true"/"false").
    fn matches(&self, e: &Event) -> bool {
        match_route(if self.0 { "true" } else { "false" }, e)
    }
    fn summary(&self) -> RouteSummary {
        RouteSummary::Bool(self.0)
    }
}

impl<T: fmt::Display + Send + Sync + 'static> Route for MultiRoute<T> {
    /// Go MultiRoute.Matches: any element's fmt.Sprint form is in event.routes.
    fn matches(&self, e: &Event) -> bool {
        self.0.iter().any(|el| match_route(&sprint(el), e))
    }
    /// Go MultiRoute.summarizeMultiRoute (validation.go:557); an empty list yields Multi(vec![]),
    /// which summarize_outgoing_edges renders as KindOther "[]".
    fn summary(&self) -> RouteSummary {
        RouteSummary::Multi(self.0.iter().map(element_summary).collect())
    }
}

/// Go fmt.Sprint(elem). serde_json::Value strings print raw (not JSON-quoted) and null prints "<nil>".
fn sprint<T: fmt::Display + 'static>(el: &T) -> String {
    if let Some(v) = (el as &dyn Any).downcast_ref::<Value>() {
        return match v {
            Value::String(s) => s.clone(),
            Value::Null => "<nil>".into(),
            other => other.to_string(),
        };
    }
    el.to_string()
}

/// Go summarizeMultiRoute's type switch, by downcast. Unquoted here: summarize_outgoing_edges applies `%q`.
/// (The `ints!` arms convert every integer type uniformly, including i64 → i64.)
#[allow(clippy::useless_conversion, clippy::unnecessary_fallible_conversions)]
fn element_summary<T: fmt::Display + 'static>(el: &T) -> RouteSummary {
    let a = el as &dyn Any;
    if let Some(s) = a.downcast_ref::<String>() {
        return RouteSummary::String(s.clone());
    }
    if let Some(s) = a.downcast_ref::<&'static str>() {
        return RouteSummary::String((*s).to_string());
    }
    if let Some(s) = a.downcast_ref::<StringRoute>() {
        return RouteSummary::String(s.0.clone());
    }
    if let Some(b) = a.downcast_ref::<bool>() {
        return RouteSummary::Bool(*b);
    }
    if let Some(b) = a.downcast_ref::<BoolRoute>() {
        return RouteSummary::Bool(b.0);
    }
    if let Some(i) = a.downcast_ref::<IntRoute>() {
        return RouteSummary::Int(i.0);
    }
    macro_rules! ints {
        ($($t:ty),*) => { $(
            if let Some(i) = a.downcast_ref::<$t>() {
                return i64::try_from(*i).map_or_else(|_| RouteSummary::Other(i.to_string()), RouteSummary::Int);
            }
        )* }
    }
    ints!(i8, i16, i32, i64, isize, u8, u16, u32, u64, usize);
    if let Some(v) = a.downcast_ref::<Value>() {
        return match v {
            Value::Null => RouteSummary::Other("<nil>".into()),
            Value::String(s) => RouteSummary::String(s.clone()),
            Value::Bool(b) => RouteSummary::Bool(*b),
            Value::Number(n) => n
                .as_i64()
                .map_or_else(|| RouteSummary::Other(n.to_string()), RouteSummary::Int),
            _ => RouteSummary::Other(std::any::type_name::<Value>().to_owned()),
        };
    }
    RouteSummary::Other(std::any::type_name::<T>().to_owned())
}

/// Port of Go defaultRoute.
struct DefaultRoute;

impl Route for DefaultRoute {
    fn matches(&self, _: &Event) -> bool {
        false
    }
    fn summary(&self) -> RouteSummary {
        RouteSummary::Default
    }
}

/// Port of Go workflow.Default: matches when no other concrete route matched (the scheduler decides).
pub static DEFAULT: LazyLock<Arc<dyn Route>> = LazyLock::new(|| Arc::new(DefaultRoute));

/// Go `route == Default` (identity).
pub(crate) fn is_default_route(r: &Arc<dyn Route>) -> bool {
    std::ptr::addr_eq(Arc::as_ptr(r), Arc::as_ptr(&*DEFAULT))
}

/// Port of Go workflow.Edge: a directed connection between nodes in the workflow graph.
#[derive(Clone)]
pub struct Edge {
    /// The source node.
    pub from: Arc<dyn Node>,
    /// The target node.
    pub to: Arc<dyn Node>,
    /// Routing condition; `None` makes the edge unconditional.
    pub route: Option<Arc<dyn Route>>,
}

/// Port of Go `strconv.Quote` (Go `%q` on a string); the faithful port lives in gadk-agent so
/// both crates format `%q` identically.
pub(crate) use gadk_agent::go_quote;

// ---------------------------------------------------------------------------------------------
// internal/workflowroute
// ---------------------------------------------------------------------------------------------

/// Port of Go workflowroute.Kind: classifies a concrete route value extracted from an edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RouteKind {
    /// A string route (StringRoute or string element in MultiRoute).
    String,
    /// A boolean route (BoolRoute or bool element in MultiRoute).
    Bool,
    /// Any other concrete route (IntRoute, an empty MultiRoute, or a custom Route).
    Other,
}

/// Port of Go workflowroute.Value: one concrete route value on an outgoing edge. `label` is a
/// quoted string for `String`, "true"/"false" for `Bool`, and a description otherwise.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RouteValue {
    pub(crate) kind: RouteKind,
    pub(crate) label: String,
}

/// Port of Go workflowroute.OutgoingSummary: all outgoing edges from a single node.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct OutgoingSummary {
    pub(crate) has_unrouted: bool,
    pub(crate) has_default: bool,
    pub(crate) routes: Vec<RouteValue>,
}

/// Port of Go workflowroute.Validator.
pub(crate) type RouteValidator =
    Arc<dyn Fn(&dyn Node, &OutgoingSummary) -> Result<(), WorkflowError> + Send + Sync>;

/// Go workflowroute's `validators` registry.
static VALIDATORS: LazyLock<RwLock<Vec<RouteValidator>>> =
    LazyLock::new(|| RwLock::new(Vec::new()));

/// Port of Go workflowroute.Register: `v` runs during workflow graph validation. Test-only: Go's
/// only production caller is the out-of-scope exp/classifier.
#[cfg(test)]
pub(crate) fn register_route_validator(v: RouteValidator) {
    VALIDATORS.write().push(v);
}

/// Port of Go workflowroute.Validate: runs every registered validator in registration order and
/// returns the first error.
pub(crate) fn validate_outgoing(
    node: &dyn Node,
    out: &OutgoingSummary,
) -> Result<(), WorkflowError> {
    let list = VALIDATORS.read().clone();
    for v in &list {
        v(node, out)?;
    }
    Ok(())
}
