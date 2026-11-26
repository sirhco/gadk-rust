# gadk-prompts-firestore

A `gadk-prompts` store in Google Cloud Firestore, over the Firestore REST API (v1) with
Application Default Credentials. `{root}/prompts/{id}` holds a prompt's name, labels, latest
version and, when any label has a rollout, `rollouts` (label → `{version, percent}`; the field is
absent when there are none), and `{root}/prompts/{id}/versions/{n}` holds each version; every
write is one Firestore transaction. Each write of a prompt document rewrites all its fields, so
upgrade every writer (CLI, console, services) before starting rollouts: an older writer drops the
`rollouts` field when it changes that prompt. Re-exported by the `gadk` façade as
`gadk::prompts_firestore` (feature `prompts-firestore`, not in `default`; it enables `prompts`).

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

Each registry refresh calls `load_all_with_rollouts`. It makes one `runQuery` over the `prompts`
collection under the root (one document read per prompt; there is no collection-group scan of
`versions`), then reads by name only the version documents it has not read before, each new
version once. The rollouts come from the same prompt documents, so they add no reads, and
`load_all` costs the same. The store keeps the versions it has read in memory (they are
immutable), so a poll of an unchanged store costs about one read per prompt; the first call after
a restart reads every version once.

`load_prompt(name)` reads only that prompt's document (one read) and then only its versions not
yet read, from the same in-memory versions; a name that fails `validate_name` reads nothing. The
`gadk-prompts` CLI's `show`, `diff` and `rollout`, `gadk-eval guard` and `guards`, and the
console's prompt page use it, so in a fresh process they read one prompt and its versions instead
of every version of every prompt.

Use a dedicated non-empty `root` (e.g. `envs/prod`) and set
`PromptRegistryBuilder::refresh_interval` to suit the number of prompts.

## Testing

The store's REST semantics (paths, transactions, retries, error mapping) are verified against an
in-process fake Firestore server, which also runs the `PromptStore` contract suites
(`testsuite::run`, `testsuite::run_rollouts` and `testsuite::run_rollout_cas`, the last covering
the compare-and-set `set_rollout_if` and the atomic promote `promote_rollout_if`, each run in one
transaction). `tests/live.rs` (ignored by default) runs the same three
suites against the Firestore emulator when `FIRESTORE_EMULATOR_HOST` is set. The store reads
inside transactions with `batchGet` (the emulator does not answer transactional single-document
GETs):

```sh
FIRESTORE_EMULATOR_HOST=localhost:8080 cargo test -p gadk-prompts-firestore --test live live_emulator_contract -- --ignored
```

`live_project_contract` runs the same suites against a real project with Application Default
Credentials (it refuses to run while `FIRESTORE_EMULATOR_HOST` is set;
`GADK_FIRESTORE_DATABASE` defaults to `(default)`):

```sh
GADK_FIRESTORE_PROJECT=my-project cargo test -p gadk-prompts-firestore --test live live_project_contract -- --ignored
```

It leaves documents under `runs/gadk-test-*` (each scenario's root is printed as `root: …`);
delete them afterwards.
