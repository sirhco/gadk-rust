# gadk-platform

Low-level seams and shared helpers used across the gadk workspace. Most applications never
depend on it directly; it is re-exported by the `gadk` facade as `gadk::platform`, mainly so
tests can make agent runs deterministic.

- `Platform` bundles the clock (`TimeProvider`), the ID source (`UuidProvider`) and the task
  runner (`TaskRunner`) that sessions, events and the runner use. `Platform::default()` uses the
  system clock, random v4 UUIDs and Tokio; `Platform::deterministic(seed)` starts the clock at
  2000-01-01T00:00:00Z plus `seed` seconds, advances it 1 ms per call and derives UUIDs from a
  counter. `with_time`, `with_uuid` and `with_task_runner` replace one part.
- `env` is environment-variable access that tests can inject (`EnvProvider`, `ProcessEnv`,
  `MapEnv`).
- `tracking_header()` is the `gadk-rust/<version> rust/<rustc version>` value sent as the
  `user-agent` / `x-goog-api-client` header.
- Helpers with exact, stable output formats that other crates rely on for wire and CLI
  compatibility: string quoting, float formatting, duration formatting and parsing, RFC 3339
  parsing, lexical path functions, safe resolution of untrusted relative paths
  (`safe_subpath`), URL parsing, a MIME table with content sniffing, and the command-line flag
  parser the launcher uses.

## Example

```rust
use gadk_platform::Platform;

let platform = Platform::deterministic(42);
let t0 = platform.now();
let t1 = platform.now(); // 1 ms later
let id = platform.new_uuid(); // the same sequence on every run with seed 42
```
