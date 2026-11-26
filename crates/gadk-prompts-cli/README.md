# gadk-prompts-cli

`gadk-prompts`, the command-line tool for managing `gadk-prompts` versions and labels in a file
or Firestore store.

```sh
cargo install gadk-prompts-cli

STORE=file://./prompts            # or firestore://<project>/<database>[/<root>]
gadk-prompts --store $STORE push support-triage --template triage.txt -m "Tighten tone"
gadk-prompts --store $STORE promote support-triage 3 --label production --expect 2
gadk-prompts --store $STORE rollout support-triage --label production 4 --percent 10
gadk-prompts --store $STORE rollout support-triage --label production --promote
gadk-prompts --store $STORE show support-triage --label production
gadk-prompts --store $STORE diff support-triage production latest
gadk-prompts --store $STORE list --prefix support-
gadk-prompts --store $STORE unlabel support-triage --label staging
```

| Command | Does |
|---|---|
| `push <name> [--template <file>\|-] [--config <json file>] [-m <msg>] [--author <a>]` | Creates a new version from the template (stdin by default); a no-op when unchanged. The author defaults to `git config user.email`. |
| `promote <name> <version> --label <l> [--expect <v>\|none]` | Points a label at a version; `--expect` makes it conditional on the label's current version. |
| `unlabel <name> --label <l>` | Removes a label (and its rollout). |
| `rollout <name> --label <l> <version> --percent <p>` | Starts or replaces the label's rollout: `<p>`% of sessions (1-99) are served `<version>`, the rest the label's version. Prints `<name> <l> -> v<label version> (v<version> at <p>%)`. |
| `rollout <name> --label <l> --promote` | Moves the label to its rollout version and ends the rollout in one atomic write (`promote_rollout_if`), on condition that neither the label nor its rollout (version and percent) changed since they were read; prints `<name> <l> -> v<n>`. Exit code 2 when the label has no rollout, 3 when the label or the rollout changed. |
| `rollout <name> --label <l> --abort` | Removes the label's rollout (no error when there is none); prints `<name> <l> rollout aborted`. |
| `list [--prefix <p>]` | Lists prompts: `<name>`, `v<latest>` and the labels, a label with a rollout as `production=v3(v4@10%)`. |
| `show <name> [--label <l> \| --version <n>] [--json]` | Shows a version (latest by default). With `--label` and a rollout on that label, a last line `rollout: v<n> at <p>%` follows the template (not with `--json`). |
| `diff <name> <from> <to>` | Unified diff of two versions; each is `v<n>`, a label or `latest`. |

`--store` is required: `file://<dir>` or `firestore://<project>/<database>[/<root>]`
(Application Default Credentials; `FIRESTORE_EMULATOR_HOST` for the emulator). Firestore support
is the default feature `firestore`.

Exit codes: 0 ok, 1 usage or invalid input, 2 not found, 3 conflict, 4 store or IO error.
