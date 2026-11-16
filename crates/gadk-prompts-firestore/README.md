# gadk-prompts-firestore

A `gadk-prompts` store in Google Cloud Firestore, over the Firestore REST API (v1) with
Application Default Credentials. `{root}/prompts/{id}` holds a prompt's name, labels and latest
version, and `{root}/prompts/{id}/versions/{n}` holds each version; every write is one Firestore
transaction. Re-exported by the `gadk` façade as `gadk::prompts_firestore` (feature
`prompts-firestore`, not in `default`; it enables `prompts`).

```rust,ignore
use std::sync::Arc;
use gadk_prompts::PromptRegistry;
use gadk_prompts_firestore::{FirestoreConfig, FirestoreStore};

let store = FirestoreStore::new(FirestoreConfig {
    project: "my-project".into(),
    ..FirestoreConfig::default()
})?;
let registry = PromptRegistry::builder(Arc::new(store)).load().await?;
```

`FirestoreConfig`:

- `project` — the Google Cloud project; `database` — the database ID (`(default)` by default).
- `root` — an optional document path (e.g. `envs/prod`; segments `[A-Za-z0-9._-]+`, not `.` or
  `..`) under which the `prompts` collection lives.
- `endpoint` — `None` uses `http://$FIRESTORE_EMULATOR_HOST` when that is set, else
  `https://firestore.googleapis.com`. Requests to the emulator or any plain `http://` endpoint
  carry no credentials.
- `credentials`, `http_client`, `env` — override the credentials, the HTTP client and where the
  environment is read.

## Read cost

Each registry refresh calls `load_all`, which re-reads the whole prompt set: about one
document read per prompt plus one per version of every prompt, on every poll. With an empty
`root`, the versions query (a collection-group query over `versions`) scans every `versions`
collection in the database, including ones that are not prompts, and drops them by path only
after the reads are billed. Use a dedicated non-empty `root` (e.g. `envs/prod`) and set
`PromptRegistryBuilder::refresh_interval` to suit the size of the prompt set: for P prompts
with V versions in total, a poll every interval costs roughly P + V reads.

## Testing

The store's REST semantics (paths, transactions, retries, error mapping) are verified against an
in-process fake Firestore server. `tests/live.rs` (ignored by default) runs the `PromptStore`
contract suite against the Firestore emulator when `FIRESTORE_EMULATOR_HOST` is set. The store
reads inside transactions with `batchGet` (the emulator does not answer transactional
single-document GETs):

```sh
FIRESTORE_EMULATOR_HOST=localhost:8080 cargo test -p gadk-prompts-firestore --test live live_emulator_contract -- --ignored
```

`live_project_contract` runs the same suite against a real project with Application Default
Credentials (it refuses to run while `FIRESTORE_EMULATOR_HOST` is set;
`GADK_FIRESTORE_DATABASE` defaults to `(default)`):

```sh
GADK_FIRESTORE_PROJECT=my-project cargo test -p gadk-prompts-firestore --test live live_project_contract -- --ignored
```

It leaves documents under `runs/gadk-test-*` (each scenario's root is printed as `root: …`);
delete them afterwards.
