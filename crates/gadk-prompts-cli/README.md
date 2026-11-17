# gadk-prompts-cli

`gadk-prompts`, the command-line tool for managing `gadk-prompts` versions and labels in a file
or Firestore store.

```sh
cargo install gadk-prompts-cli

STORE=file://./prompts            # or firestore://<project>/<database>[/<root>]
gadk-prompts --store $STORE push support-triage --template triage.txt -m "Tighten tone"
gadk-prompts --store $STORE promote support-triage 3 --label production --expect 2
gadk-prompts --store $STORE show support-triage --label production
gadk-prompts --store $STORE diff support-triage production latest
gadk-prompts --store $STORE list --prefix support-
gadk-prompts --store $STORE unlabel support-triage --label staging
```

| Command | Does |
|---|---|
| `push <name> [--template <file>\|-] [--config <json file>] [-m <msg>] [--author <a>]` | Creates a new version from the template (stdin by default); a no-op when unchanged. The author defaults to `git config user.email`. |
| `promote <name> <version> --label <l> [--expect <v>\|none]` | Points a label at a version; `--expect` makes it conditional on the label's current version. |
| `unlabel <name> --label <l>` | Removes a label. |
| `list [--prefix <p>]` | Lists prompts. |
| `show <name> [--label <l> \| --version <n>] [--json]` | Shows a version (latest by default). |
| `diff <name> <from> <to>` | Unified diff of two versions; each is `v<n>`, a label or `latest`. |

`--store` is required: `file://<dir>` or `firestore://<project>/<database>[/<root>]`
(Application Default Credentials; `FIRESTORE_EMULATOR_HOST` for the emulator). Firestore support
is the default feature `firestore`.

Exit codes: 0 ok, 1 usage or invalid input, 2 not found, 3 conflict, 4 store or IO error.
