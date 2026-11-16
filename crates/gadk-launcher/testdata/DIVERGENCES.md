# Accepted launcher divergences from ADK-Go

Every `divergence` (and `json_body`) a fixture's `<name>.deltas.json` declares must be one of the
`- ` lines below, verbatim (checked by `tests/wire_replay.rs`,
`every_delta_cites_a_ledger_divergence`).

- An existing global `tracing` subscriber is kept and the launcher logs that its telemetry layers were not installed (Go always replaces the global OpenTelemetry providers).
- A2A answers are a2a-rs's JSON (compact, a2a-rs's key order; Go: a2a-go's, the card pretty-printed), and the card endpoint adds `Cache-Control: max-age=300` and an `ETag` (the card version).
- The A2A 0.3 endpoint (`/a2a/invoke`) is not served: gorilla's 404, which the request logger does not log; the agent card has none of its 0.3 union fields (top-level `url`, `protocolVersion`, `preferredTransport`, the 0.3 interface, `capabilities.pushNotifications`).
- A2A task results lack Go's merged status-update metadata (`adk_app_name`, `adk_session_id`, `adk_user_id`, the ADK extension key): a2a-rs's task store keeps only a `Task` event's own metadata (ledger 5b).
- A2A v1 requests without an `A2A-Version` header are refused with a2a-rs's JSON-RPC error message `version not supported: 0.3` (a2a-go v2.7.0: `this version is not supported: requested "0.3", served "1.0"`).
- Request-body JSON syntax and type errors keep serde's text after Go's prefix (ledger 5a; Go's `encoding/json` words them differently, e.g. `unexpected EOF`).
