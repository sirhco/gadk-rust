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
- `root` — an optional document path (e.g. `envs/prod`) under which the `prompts` collection
  lives.
- `endpoint` — `None` uses `http://$FIRESTORE_EMULATOR_HOST` when that is set, else
  `https://firestore.googleapis.com`. Requests to the emulator or any plain `http://` endpoint
  carry no credentials.
- `credentials`, `http_client`, `env` — override the credentials, the HTTP client and where the
  environment is read.
