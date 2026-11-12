# gadk-cli

`gadk`, the gadk deploy CLI: a port of ADK-Go's `adkgo` (`cmd/adkgo`) and its helper package
`internal/cli/util`. It deploys a gadk agent, a Cargo binary target, to Cloud Run or to Vertex AI
Agent Engine with ADK-Go's flags, checks, generated Dockerfile `CMD`, `gcloud` invocations,
Reasoning Engine requests and console output. Where gadk differs from ADK-Go, the difference is
listed in `docs/parity/6b.md`.

## Install

```sh
cargo install --path crates/gadk-cli    # installs the `gadk` binary
```

## Usage

```text
gadk deploy cloudrun    [flags]   # Cloud Run, built remotely by Cloud Build
gadk deploy agentengine [flags]   # Vertex AI Agent Engine (Reasoning Engine)
gadk deploy <command> --help      # every flag with its default
```

The entry point is a Cargo package: `-e/--entry_point_path` names its directory or its
`Cargo.toml` (default: the current directory), and `--bin` picks the binary target when the
package has more than one. The package's Cargo workspace root must hold a `Cargo.lock`; the
image builds with `cargo build --release --locked`. A `path` dependency outside the staged
directory is rejected before anything is staged.

The builder image is `rust:<version>-bookworm`, where the version is the
`rust-toolchain.toml` `[toolchain] channel` of the workspace root, else the package's
`rust-version`, else the rustc `gadk` was built with, else `rust:bookworm`. The runtime image is
`gcr.io/distroless/cc-debian12`.

### Cloud Run

```sh
gadk deploy cloudrun -r us-central1 -p my-project -s my-agent -e path/to/agent
```

Steps: compute flags and create a temp dir (`-t`, default the system temp dir) → stage the
Cargo workspace root into it → write the two-stage `Dockerfile` → `gcloud run deploy <service>
--source . …` in the temp dir → remove the temp dir → `gcloud run services proxy <service>
--port <proxy_port> …` (a local authenticating proxy on `127.0.0.1:<proxy_port>`, default
8081). `gcloud` must be on `PATH` and authenticated (`gcloud auth login`); the service reads
`GOOGLE_API_KEY` from Secret Manager (`--set-secrets=GOOGLE_API_KEY=GOOGLE_API_KEY:latest`).
`--api`, `--debug_api`, `--webui`, `--a2a`, `--pubsub` and `--eventarc` choose the sub-launchers
the container starts.

### Agent Engine

```sh
gadk deploy agentengine -r us-central1 -p my-project -s my-agent -e path/to/agent
```

Steps: compute flags and create a temp dir → write the `Dockerfile` → archive the source
directory (`-d`, default the Cargo workspace root; it must be the root or an ancestor of it)
with the `Dockerfile` as a `.tar.gz` → create a Reasoning Engine, or update the one named by
`--agent_engine_id`, and wait for the operation (at most 45 minutes) → remove the temp dir.
Credentials are Application Default Credentials (`gcloud auth application-default login`).
`--mem_deploy` adds a Memory Bank (`--mem_ttl`, `--mem_model`).

## Notes and limitations

- **Unix only (Linux and macOS).** `gadk` uses Unix file permissions and symbolic links when
  staging; it does not build on Windows.
- **Cloud Build timeout.** Cloud Run images are built remotely, from source, on Cloud Build.
  A cold release build of a gadk agent can take longer than Cloud Build's default timeout
  (10 minutes). Raise it once for your gcloud configuration:

  ```sh
  gcloud config set builds/timeout 3600
  ```

  `gadk` does not change your gcloud configuration.
- **`.gcloudignore`.** `gcloud run deploy --source` uploads the staged directory filtered by its
  `.gcloudignore`. A `.gcloudignore` at your workspace root is staged unchanged, so its
  exclusions apply (do not exclude `Cargo.lock` or sources the build needs). Without one, `gadk`
  writes a one-comment `.gcloudignore`, so gcloud does not fall back to your `.gitignore`
  (which may list `Cargo.lock`).
- **Excluded directories.** Staging (Cloud Run) and the source archive (Agent Engine) leave out
  every file or directory named `.git` or `target`, at any depth, and the temp dir when it lies
  inside the workspace. A source directory that is itself named `target` (for example a module
  `src/target/`) is therefore not deployed; rename it.
- **`build.target`.** The builder runs `cargo build --target-dir /gadk-target` and copies
  `/gadk-target/release/<bin>`. A staged `.cargo/config.toml` that sets `build.target` puts the
  binary under `/gadk-target/<triple>/release/` instead, and the image build fails at that
  `COPY` (visible in the Cloud Build log). Remove `build.target` from the staged config.
- A root `Dockerfile` in your workspace is replaced by the generated one in the staged copy
  (Cloud Run) and left out of the archive (Agent Engine).
- No local build: `gadk` does not cross-compile; the image is always built remotely.
