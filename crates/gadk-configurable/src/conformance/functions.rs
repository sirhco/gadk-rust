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

//! The conformance agents' function tools (`validate_email`, `get_user_id`, `create_booking`,
//! `search_flights`, `calculate_trip_cost`, `reimburse`, `ask_for_approval`), registered as
//! tool factories under `tools_agent_00N.tools.*`.
//!
//! The argument structs use the conformance agents' JSON field names. Their schemars schemas
//! are shaped to match the declarations in ADK's conformance recordings (every field
//! required, an optional field is `{"type": ["null", T]}`), so the tools' declarations match
//! those recordings. The structs have no doc comments on purpose: schemars would turn them
//! into `description`s, which the recorded declarations do not have.

use std::sync::{Arc, LazyLock};

use futures::FutureExt;
use gadk_agent::{Context, Tool, ToolError};
use gadk_engine::tool::{FunctionTool, FunctionToolConfig};
use regex::Regex;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::{ConfigError, Registry, ToolFactory};

/// A JSON object result.
type GoResult = Map<String, Value>;

/// Schema of an optional field: `{"type": ["null", T]}` (the recordings put `"null"` first;
/// schemars writes `[T, "null"]` and leaves an `Option` out of `required`, so these fields are
/// also marked `#[schemars(required)]`, which keeps the bare `T` schema).
fn go_pointer(schema: &mut schemars::Schema) {
    if let Some(t) = schema.get("type").cloned() {
        schema.insert("type".into(), json!(["null", t]));
    }
}

// Arguments of `validate_email`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub(crate) struct ValidateEmailArgs {
    pub(crate) email: String,
}

// The email format `validate_email` accepts.
static EMAIL_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    #[allow(clippy::expect_used)] // a constant pattern
    Regex::new(r"^[a-zA-Z0-9._%+-]+@[a-zA-Z0-9.-]+\.[a-zA-Z]{2,}$").expect("valid email regex")
});

/// Whether the email matches the accepted format.
pub(crate) async fn validate_email(
    _ctx: Context,
    args: ValidateEmailArgs,
) -> Result<bool, ToolError> {
    Ok(EMAIL_REGEX.is_match(&args.email))
}

/// 32-bit FNV-1a of the data.
fn fnv32a(data: &[u8]) -> u32 {
    data.iter().fold(0x811c_9dc5_u32, |h, &b| {
        (h ^ u32::from(b)).wrapping_mul(0x0100_0193)
    })
}

/// `invalid email format provided`, else FNV-1a 32 of the email modulo 10000.
pub(crate) async fn get_user_id(ctx: Context, args: ValidateEmailArgs) -> Result<i64, ToolError> {
    let email = args.email.clone();
    let valid = validate_email(ctx, args).await?;
    if !valid {
        return Err(ToolError::Execution("invalid email format provided".into()));
    }
    Ok(i64::from(fnv32a(email.as_bytes()) % 10000))
}

// Arguments of `create_booking`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub(crate) struct CreateBookingArgs {
    pub(crate) user_id: i64,
    pub(crate) is_confirmed: bool,
    pub(crate) details: String,
}

/// Echoes the arguments with their type names (`int`, `bool`, `string`).
pub(crate) async fn create_booking(
    _ctx: Context,
    args: CreateBookingArgs,
) -> Result<GoResult, ToolError> {
    Ok(go_map(json!({
        "user_id": args.user_id,
        "is_confirmed": args.is_confirmed,
        "details": args.details,
        "user_id_type": "int",
        "is_confirmed_type": "bool",
        "details_type": "string",
    })))
}

// Flight preferences of `search_flights`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub(crate) struct FlightPreferences {
    pub(crate) cabin_class: String,
    pub(crate) max_stops: i64,
    #[schemars(required, transform = go_pointer)]
    pub(crate) preferred_airline: Option<String>,
    pub(crate) flexible_dates: bool,
}

// Trip details of `search_flights`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub(crate) struct TripDetails {
    pub(crate) origin: String,
    pub(crate) destination: String,
    pub(crate) departure_date: String,
    pub(crate) return_date: String,
}

// Arguments of `search_flights`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub(crate) struct SearchFlightsArgs {
    pub(crate) trip: TripDetails,
    #[schemars(required, transform = go_pointer)]
    pub(crate) preferences: Option<FlightPreferences>,
}

/// Default preferences (`economy`, 1 stop) when absent, a one-way or round-trip summary and
/// the `available_flights` lines.
pub(crate) async fn search_flights(
    _ctx: Context,
    args: SearchFlightsArgs,
) -> Result<GoResult, ToolError> {
    let trip = args.trip;
    let preferences = args.preferences.unwrap_or_else(|| FlightPreferences {
        cabin_class: "economy".to_owned(),
        max_stops: 1,
        flexible_dates: false,
        ..Default::default()
    });

    let trip_type = if trip.return_date.is_empty() {
        "one-way"
    } else {
        "round-trip"
    };

    let mut result = go_map(json!({
        "trip_type": trip_type,
        "route": format!("{} to {}", trip.origin, trip.destination),
        "departure_date": trip.departure_date,
        "return_date": trip.return_date,
        "cabin_class": preferences.cabin_class,
        "max_stops": preferences.max_stops,
        "preferred_airline": preferences.preferred_airline,
        "flexible_dates": preferences.flexible_dates,
        "search_status": "completed",
    }));

    let airline = match &preferences.preferred_airline {
        Some(a) if !a.is_empty() => a.as_str(),
        _ => "Various Airlines",
    };

    let stops_desc = if preferences.max_stops > 0 {
        format!("up to {} stops", preferences.max_stops)
    } else {
        "direct".to_owned()
    };

    let mut flights = vec![
        format!(
            "{airline} - {trip_type} {} flight with {stops_desc}",
            preferences.cabin_class
        ),
        format!("Departure: {}", trip.departure_date),
    ];

    if !trip.return_date.is_empty() {
        flights.push(format!("Return: {}", trip.return_date));
    }

    result.insert("available_flights".to_owned(), json!(flights));

    Ok(result)
}

// Arguments of `calculate_trip_cost`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub(crate) struct CalculateTripCostArgs {
    pub(crate) base_fare: f64,
    pub(crate) num_passengers: i64,
    pub(crate) insurance: bool,
    #[schemars(required, transform = go_pointer)]
    pub(crate) baggage_count: Option<i64>,
}

/// At least one passenger, 10% insurance, $35 per checked bag after the first, per passenger.
#[allow(clippy::cast_precision_loss)] // integer counts to f64
pub(crate) async fn calculate_trip_cost(
    _ctx: Context,
    mut args: CalculateTripCostArgs,
) -> Result<GoResult, ToolError> {
    if args.num_passengers <= 0 {
        args.num_passengers = 1;
    }

    let subtotal = args.base_fare * args.num_passengers as f64;

    let insurance_cost = if args.insurance { subtotal * 0.1 } else { 0.0 };

    let mut baggage_cost = 0.0;
    let mut display_baggage = Value::Null;

    if let Some(count) = args.baggage_count {
        display_baggage = json!(count);

        // The first bag is free; the integer subtraction wraps, then the count is clamped at 0.
        let chargeable_bags = f64::max(0.0, count.wrapping_sub(1) as f64);
        baggage_cost = chargeable_bags * 35.0 * args.num_passengers as f64;
    }

    let total = subtotal + insurance_cost + baggage_cost;

    Ok(go_map(json!({
        "base_fare": args.base_fare,
        "num_passengers": args.num_passengers,
        "subtotal": subtotal,
        "insurance_included": args.insurance,
        "insurance_cost": insurance_cost,
        "baggage_count": display_baggage,
        "baggage_cost": baggage_cost,
        "total_cost": total,
    })))
}

// Arguments of `reimburse`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub(crate) struct ReimburseArgs {
    pub(crate) purpose: String,
    pub(crate) amount: f64,
}

/// `{"status": "ok"}`.
pub(crate) async fn reimburse(_ctx: Context, _args: ReimburseArgs) -> Result<GoResult, ToolError> {
    Ok(go_map(json!({ "status": "ok" })))
}

// Arguments of `ask_for_approval`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub(crate) struct AskForApprovalArgs {
    pub(crate) purpose: String,
    pub(crate) amount: f64,
}

/// A pending ticket for the amount.
pub(crate) async fn ask_for_approval(
    _ctx: Context,
    args: AskForApprovalArgs,
) -> Result<GoResult, ToolError> {
    Ok(go_map(json!({
        "status": "pending",
        "amount": args.amount,
        "ticketId": "reimbursement-ticket-001",
    })))
}

/// The object of a `json!({...})` literal.
fn go_map(v: Value) -> GoResult {
    match v {
        Value::Object(m) => m,
        _ => Map::new(),
    }
}

const CREATE_BOOKING_DESCRIPTION: &str = "Creates a booking for a user.

Args:
  user_id: The unique identifier for the user.
  is_confirmed: Whether the booking is confirmed.
  details: Any additional details for the booking.

Returns:
  A dictionary containing the booking information and the types of the
  received arguments.";

const SEARCH_FLIGHTS_DESCRIPTION: &str = "Search for flights based on trip details and preferences.

This function demonstrates advanced parameter handling:
- Pydantic models as parameters (trip, preferences)
- Optional/nullable parameters (preferences, return_date, preferred_airline)
- Default values (cabin_class, max_stops, flexible_dates)

Args:
  trip: Core trip information including origin, destination, and dates.
  preferences: Optional flight preferences. If not provided, uses defaults.

Returns:
  A dictionary containing search results and parameters received.
";

const CALCULATE_TRIP_COST_DESCRIPTION: &str =
    "Calculate total trip cost with various optional charges.

This function demonstrates:
- Mix of required and optional parameters
- Default values for common cases
- Nullable parameter that affects calculation logic

Args:
  base_fare: Base ticket price per passenger.
  num_passengers: Number of passengers (default: 1).
  insurance: Whether to add travel insurance (default: False).
  baggage_count: Number of checked bags per passenger, or None for carry-on
    only.

Returns:
  A dictionary with cost breakdown.
";

/// Builds a [`FunctionTool`] from its name, description and function, wrapping a failure as
/// `{context}: {cause}`.
fn new_tool<Args, Out, F, Fut>(
    name: &str,
    description: &str,
    is_long_running: bool,
    f: F,
    context: &str,
) -> Result<Arc<dyn Tool>, ConfigError>
where
    Args: serde::de::DeserializeOwned + JsonSchema + Send + 'static,
    Out: Serialize + JsonSchema + Send + 'static,
    F: Fn(Context, Args) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<Out, ToolError>> + Send + 'static,
{
    let cfg = FunctionToolConfig {
        is_long_running,
        ..FunctionToolConfig::new(name, description)
    };
    FunctionTool::new(cfg, f).map_err(|e| ConfigError::wrap(context, ConfigError::other(e)))
}

/// A factory that returns the given tool, ignoring the registry, context and args.
fn constant_factory(t: Arc<dyn Tool>) -> ToolFactory {
    Arc::new(move |_, _, _| {
        let t = t.clone();
        async move { Ok(t) }.boxed()
    })
}

/// Builds the seven function tools (`error creating validate email tool: {cause}`, …) and
/// registers a factory returning each (`error registering validate email tool: {cause}`, …).
pub fn register_functions(registry: &Registry) -> Result<(), ConfigError> {
    let validate_email_tool = new_tool(
        "validate_email",
        "Checks if the provided string is a valid email format.",
        false,
        validate_email,
        "error creating validate email tool",
    )?;
    let get_user_id_tool = new_tool(
        "get_user_id",
        "Retrieves a user ID based on their email.",
        false,
        get_user_id,
        "error creating get user ID tool",
    )?;
    let create_booking_tool = new_tool(
        "create_booking",
        CREATE_BOOKING_DESCRIPTION,
        false,
        create_booking,
        "error creating create booking tool",
    )?;

    let search_flights_tool = new_tool(
        "search_flights",
        SEARCH_FLIGHTS_DESCRIPTION,
        false,
        search_flights,
        "error creating search flights tool",
    )?;

    let calculate_trip_cost_tool = new_tool(
        "calculate_trip_cost",
        CALCULATE_TRIP_COST_DESCRIPTION,
        false,
        calculate_trip_cost,
        "error creating calculate trip cost tool",
    )?;

    let reimburse_tool = new_tool(
        "reimburse",
        "Reimburse the amount of money to the employee.",
        false,
        reimburse,
        "error creating reimburse tool",
    )?;

    let ask_for_approval_tool = new_tool(
        "ask_for_approval",
        "Ask for approval for the reimbursement.",
        true,
        ask_for_approval,
        "error creating ask for approval tool",
    )?;

    let registrations = [
        (
            "tools_agent_002.tools.validate_email",
            validate_email_tool,
            "error registering validate email tool",
        ),
        (
            "tools_agent_002.tools.get_user_id",
            get_user_id_tool,
            "error registering get user ID tool",
        ),
        (
            "tools_agent_002.tools.create_booking",
            create_booking_tool,
            "error registering create booking tool",
        ),
        (
            "tools_agent_004.tools.search_flights",
            search_flights_tool,
            "error registering search flights tool",
        ),
        (
            "tools_agent_004.tools.calculate_trip_cost",
            calculate_trip_cost_tool,
            "error registering calculate trip cost tool",
        ),
        (
            "tools_agent_009.tools.reimburse",
            reimburse_tool,
            "error registering reimburse tool",
        ),
        (
            "tools_agent_009.tools.ask_for_approval",
            ask_for_approval_tool,
            "error registering ask for approval tool",
        ),
    ];
    for (name, tool, context) in registrations {
        registry
            .register_tool_factory(name, constant_factory(tool))
            .map_err(|e| ConfigError::wrap(context, e))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use gadk_agent::InvocationContext;
    use gadk_genai::{Content, GenerateContentConfig, Tool as GenaiTool};
    use gadk_model::{LlmRequest, LlmResponse};
    use pretty_assertions::assert_eq;

    use super::*;
    use crate::ToolCtx;
    use crate::conformance::gonode::to_go_node;
    use crate::conformance::record::clean::clean_yaml_node;
    use crate::conformance::recording::{LlmRecording, LlmRequestView, Recording, Recordings};
    use crate::tests::CfgErr;

    /// `(registered name, tool name)` in Go's registration order.
    const TOOLS: [(&str, &str); 7] = [
        ("tools_agent_002.tools.validate_email", "validate_email"),
        ("tools_agent_002.tools.get_user_id", "get_user_id"),
        ("tools_agent_002.tools.create_booking", "create_booking"),
        ("tools_agent_004.tools.search_flights", "search_flights"),
        (
            "tools_agent_004.tools.calculate_trip_cost",
            "calculate_trip_cost",
        ),
        ("tools_agent_009.tools.reimburse", "reimburse"),
        ("tools_agent_009.tools.ask_for_approval", "ask_for_approval"),
    ];

    fn registered() -> Registry {
        let r = Registry::empty();
        register_functions(&r).unwrap();
        r
    }

    async fn tool(r: &Registry, name: &str) -> Arc<dyn Tool> {
        let (tool, toolset) = r
            .resolve_tool_reference(&ToolCtx::default(), name, None)
            .await
            .unwrap();
        assert!(toolset.is_none());
        tool.unwrap()
    }

    async fn run(tool_name: &str, args: Value) -> Result<Value, ToolError> {
        let r = registered();
        let (name, _) = TOOLS.iter().find(|(_, t)| *t == tool_name).unwrap();
        let t = tool(&r, name).await;
        let ctx = InvocationContext::builder().build();
        t.as_runnable()
            .unwrap()
            .run(ctx, args)
            .await
            .map(Value::Object)
    }

    fn case_dir(tool_name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/recordings")
            .join(format!("conformance-{tool_name}"))
    }

    /// The Go probe's `conformance-<tool>` case: one LLM recording whose request carries the
    /// tool's `Declaration()`, encoded, cleaned and marshaled as the record plugin does.
    fn record_declaration(decl: gadk_genai::FunctionDeclaration) -> Vec<u8> {
        let req = LlmRequest {
            model: "gemini-2.5-flash".to_owned(),
            contents: vec![Content::from_text("Hi", "user")],
            config: Some(GenerateContentConfig {
                tools: vec![GenaiTool {
                    function_declarations: vec![decl],
                    ..Default::default()
                }],
                ..Default::default()
            }),
            ..Default::default()
        };
        let recs = Recordings {
            recordings: vec![Recording {
                user_message_index: 0,
                agent_name: "tools_agent".to_owned(),
                llm_recording: Some(LlmRecording {
                    llm_request: Some(LlmRequestView::from(&req)),
                    llm_responses: vec![LlmResponse {
                        content: Some(Content::from_text("done", "model")),
                        ..Default::default()
                    }],
                }),
                tool_recording: None,
                index: 0,
            }],
        };
        let mut doc = to_go_node(&recs).unwrap();
        let root = doc.root();
        clean_yaml_node(&mut doc, root, false);
        gadk_goyaml::marshal_node(&doc, doc.root()).unwrap()
    }

    #[tokio::test]
    async fn declarations_record_byte_identical_to_go() {
        let r = registered();
        for (name, tool_name) in TOOLS {
            let decl = tool(&r, name).await.declaration().unwrap();
            let golden =
                std::fs::read(case_dir(tool_name).join("generated-recordings.yaml")).unwrap();
            assert_eq!(
                String::from_utf8(record_declaration(decl)).unwrap(),
                String::from_utf8(golden).unwrap(),
                "{tool_name}"
            );
        }
    }

    #[tokio::test]
    async fn declarations_equal_go_declaration_json() {
        let r = registered();
        for (name, tool_name) in TOOLS {
            let decl = tool(&r, name).await.declaration().unwrap();
            let input: Value = serde_json::from_slice(
                &std::fs::read(case_dir(tool_name).join("input.json")).unwrap(),
            )
            .unwrap();
            let go = &input["recordings"][0]["llm_recording"]["llm_request"]["config"]["tools"][0]
                ["functionDeclarations"][0];
            // serde_json object equality ignores key order; arrays (`required`, `type`) keep
            // Go's order.
            assert_eq!(serde_json::to_value(&decl).unwrap(), *go, "{tool_name}");
        }
    }

    #[tokio::test]
    async fn factories_return_the_same_tool_and_long_running_is_set() {
        let r = registered();
        for (name, tool_name) in TOOLS {
            let a = tool(&r, name).await;
            let b = tool(&r, name).await;
            assert!(Arc::ptr_eq(&a, &b), "{name}");
            assert_eq!(a.name(), tool_name);
            assert_eq!(a.is_long_running(), tool_name == "ask_for_approval");
        }
    }

    #[test]
    fn register_functions_twice_fails_with_go_text() {
        let r = registered();
        assert_eq!(
            register_functions(&r).cfg_err().to_string(),
            "error registering validate email tool: RegisterToolFactory called twice for tool \
             tools_agent_002.tools.validate_email"
        );
    }

    #[test]
    fn register_functions_reports_the_failing_registration() {
        let r = Registry::empty();
        r.register_tool_factory(
            "tools_agent_009.tools.ask_for_approval",
            constant_factory(
                FunctionTool::new(FunctionToolConfig::new("x", ""), reimburse).unwrap(),
            ),
        )
        .unwrap();
        assert_eq!(
            register_functions(&r).cfg_err().to_string(),
            "error registering ask for approval tool: RegisterToolFactory called twice for tool \
             tools_agent_009.tools.ask_for_approval"
        );
        // Go registers in order and stops at the first failure: the earlier six are in.
        let g = r.inner.read();
        assert!(g.tools.contains_key("tools_agent_009.tools.reimburse"));
        assert_eq!(g.tools.len(), 7);
    }

    // ---- Function outputs (expected values from Go: a `go test -overlay` probe calling the
    // functions of functions.go at ADK-Go `0990f9e`) ----

    #[tokio::test]
    async fn validate_email_and_get_user_id() {
        for (email, valid, id) in [
            ("test@example.com", true, Some(4312)),
            ("alice@example.org", true, Some(8641)),
            ("not-an-email", false, None),
            ("a@b.c", false, None),
        ] {
            assert_eq!(
                run("validate_email", json!({ "email": email }))
                    .await
                    .unwrap(),
                json!({ "result": valid }),
                "{email}"
            );
            let got = run("get_user_id", json!({ "email": email })).await;
            match id {
                Some(id) => assert_eq!(got.unwrap(), json!({ "result": id }), "{email}"),
                None => assert_eq!(
                    got.unwrap_err().to_string(),
                    "invalid email format provided",
                    "{email}"
                ),
            }
        }
    }

    #[test]
    fn fnv32a_matches_go_hash_fnv() {
        assert_eq!(fnv32a(b""), 0x811c_9dc5);
        assert_eq!(fnv32a(b"test@example.com"), 2_982_314_312);
        assert_eq!(fnv32a(b"alice@example.org"), 2_173_618_641);
    }

    #[tokio::test]
    async fn create_booking_echoes_args_and_go_types() {
        assert_eq!(
            run(
                "create_booking",
                json!({ "user_id": 42, "is_confirmed": true, "details": "window seat" })
            )
            .await
            .unwrap(),
            json!({
                "details": "window seat", "details_type": "string", "is_confirmed": true,
                "is_confirmed_type": "bool", "user_id": 42, "user_id_type": "int",
            })
        );
    }

    #[tokio::test]
    async fn search_flights_default_preferences_one_way() {
        assert_eq!(
            run(
                "search_flights",
                json!({
                    "trip": { "origin": "SFO", "destination": "JFK",
                              "departure_date": "2025-01-01", "return_date": "" },
                    "preferences": null,
                })
            )
            .await
            .unwrap(),
            json!({
                "available_flights": [
                    "Various Airlines - one-way economy flight with up to 1 stops",
                    "Departure: 2025-01-01",
                ],
                "cabin_class": "economy", "departure_date": "2025-01-01",
                "flexible_dates": false, "max_stops": 1, "preferred_airline": null,
                "return_date": "", "route": "SFO to JFK", "search_status": "completed",
                "trip_type": "one-way",
            })
        );
    }

    #[tokio::test]
    async fn search_flights_round_trip_with_airline_direct() {
        assert_eq!(
            run(
                "search_flights",
                json!({
                    "trip": { "origin": "SFO", "destination": "JFK",
                              "departure_date": "2025-01-01", "return_date": "2025-01-10" },
                    "preferences": { "cabin_class": "business", "max_stops": 0,
                                     "preferred_airline": "United", "flexible_dates": true },
                })
            )
            .await
            .unwrap(),
            json!({
                "available_flights": [
                    "United - round-trip business flight with direct",
                    "Departure: 2025-01-01",
                    "Return: 2025-01-10",
                ],
                "cabin_class": "business", "departure_date": "2025-01-01",
                "flexible_dates": true, "max_stops": 0, "preferred_airline": "United",
                "return_date": "2025-01-10", "route": "SFO to JFK", "search_status": "completed",
                "trip_type": "round-trip",
            })
        );
    }

    #[tokio::test]
    async fn search_flights_empty_airline_uses_various() {
        assert_eq!(
            run(
                "search_flights",
                json!({
                    "trip": { "origin": "A", "destination": "B",
                              "departure_date": "d", "return_date": "" },
                    "preferences": { "cabin_class": "economy", "max_stops": 2,
                                     "preferred_airline": "", "flexible_dates": false },
                })
            )
            .await
            .unwrap(),
            json!({
                "available_flights": [
                    "Various Airlines - one-way economy flight with up to 2 stops",
                    "Departure: d",
                ],
                "cabin_class": "economy", "departure_date": "d", "flexible_dates": false,
                "max_stops": 2, "preferred_airline": "", "return_date": "", "route": "A to B",
                "search_status": "completed", "trip_type": "one-way",
            })
        );
    }

    #[tokio::test]
    async fn search_flights_rejects_missing_required_args() {
        // Go's schema requires every field (no `omitempty`); validation fails before the call.
        let err = run("search_flights", json!({ "trip": {} }))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidArgument(_)), "{err}");
    }

    #[tokio::test]
    async fn calculate_trip_cost_cases() {
        let cases = [
            (
                json!({ "base_fare": 199.99, "num_passengers": 3, "insurance": true,
                        "baggage_count": 2 }),
                json!({
                    "baggage_cost": 105.0, "baggage_count": 2, "base_fare": 199.99,
                    "insurance_cost": 59.997_000_000_000_01, "insurance_included": true,
                    "num_passengers": 3, "subtotal": 599.97, "total_cost": 764.967,
                }),
            ),
            (
                json!({ "base_fare": 100.0, "num_passengers": 0, "insurance": false,
                        "baggage_count": null }),
                json!({
                    "baggage_cost": 0.0, "baggage_count": null, "base_fare": 100.0,
                    "insurance_cost": 0.0, "insurance_included": false, "num_passengers": 1,
                    "subtotal": 100.0, "total_cost": 100.0,
                }),
            ),
            (
                json!({ "base_fare": 50.0, "num_passengers": 2, "insurance": false,
                        "baggage_count": 0 }),
                json!({
                    "baggage_cost": 0.0, "baggage_count": 0, "base_fare": 50.0,
                    "insurance_cost": 0.0, "insurance_included": false, "num_passengers": 2,
                    "subtotal": 100.0, "total_cost": 100.0,
                }),
            ),
        ];
        for (args, want) in cases {
            assert_eq!(
                run("calculate_trip_cost", args.clone()).await.unwrap(),
                want,
                "{args}"
            );
        }
    }

    #[tokio::test]
    async fn reimburse_and_ask_for_approval() {
        assert_eq!(
            run("reimburse", json!({ "purpose": "x", "amount": 1.0 }))
                .await
                .unwrap(),
            json!({ "status": "ok" })
        );
        assert_eq!(
            run(
                "ask_for_approval",
                json!({ "purpose": "travel", "amount": 250.5 })
            )
            .await
            .unwrap(),
            json!({ "amount": 250.5, "status": "pending", "ticketId": "reimbursement-ticket-001" })
        );
    }
}
