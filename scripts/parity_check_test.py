import contextlib, importlib.machinery, importlib.util, io, os, pathlib, tempfile, unittest

HERE = pathlib.Path(__file__).resolve().parent
loader = importlib.machinery.SourceFileLoader("parity_check", str(HERE / "parity-check"))
spec = importlib.util.spec_from_loader("parity_check", loader)
pc = importlib.util.module_from_spec(spec)
loader.exec_module(pc)


class ConvTest(unittest.TestCase):
    def test_examples(self):
        cases = {
            "TestNewUUIDDefaultIsRandomAndValid": "test_new_uuid_default_is_random_and_valid",
            "TestIsGemini25OrLower": "test_is_gemini25_or_lower",
            "TestNewLLMSingleMatch": "test_new_llm_single_match",
            "TestModel_Generate": "test_model_generate",
            "TestContinuation_GenerateResumedBySDK": "test_continuation_generate_resumed_by_sdk",
            "Test_inMemoryService": "test_in_memory_service",
            "TestMetadataVertexAISSEStream": "test_metadata_vertex_aisse_stream",
            "TestOAuth2TokenSourceError": "test_o_auth2_token_source_error",
            "TestMCPTool_EmptyTextResponse": "test_mcp_tool_empty_text_response",
            "TestNewToolSet_RequireConfirmationProvider_Validation":
                "test_new_tool_set_require_confirmation_provider_validation",
            "TestMCPToolSetConfirmation": "test_mcp_tool_set_confirmation",
            "FuzzRedact": "fuzz_redact",
            "FuzzRedact_Seeds": "fuzz_redact_seeds",
            "TestSamplePNG1x1_DecodesAsValidPNG": "test_sample_png1x1_decodes_as_valid_png",
            "TestNewModel_VertexAndModelNames": "test_new_model_vertex_and_model_names",
            "TestExecutableCodeAndURISchemePrivacy": "test_executable_code_and_uri_scheme_privacy",
        }
        for go, rust in cases.items():
            self.assertEqual(pc.go_test_to_rust(go), rust, go)

    def test_other_prefixes_raise(self):
        for name in ("BenchmarkFoo", "ExampleFoo", "testFoo", "fuzzFoo"):
            with self.assertRaises(ValueError, msg=name):
                pc.go_test_to_rust(name)

    def test_go_test_regex_finds_test_and_fuzz_only(self):
        src = (
            "func TestA(t *testing.T) {}\nfunc FuzzB(f *testing.F) {}\n"
            "func BenchmarkC(b *testing.B) {}\nfunc ExampleD() {}\nfunc helperFuzz() {}\n"
        )
        self.assertEqual(pc.GO_TEST.findall(src), ["TestA", "FuzzB"])


    def test_target_name(self):
        self.assertEqual(pc.target_name("agent/llmagent/llmagent_test.go"), "agent_llmagent_llmagent")
        self.assertEqual(pc.target_name("internal/utils/utils_test.go"), "internal_utils_utils")


class CheckTest(unittest.TestCase):
    def test_missing_row_fails_and_complete_passes(self):
        with tempfile.TemporaryDirectory() as d:
            d = pathlib.Path(d)
            (d / "go/model").mkdir(parents=True)
            (d / "go/model/llm_test.go").write_text("package model\nfunc TestA(t *testing.T) {}\nfunc TestB(t *testing.T) {}\n")
            (d / "rs").mkdir()
            (d / "rs/a.rs").write_text("#[test]\nfn test_a() {}\n")
            ledger = d / "1a.md"
            ledger.write_text("| Go file | Go test | status | Rust file | Rust fn / reason |\n|---|---|---|---|---|\n"
                              "| model/llm_test.go | TestA | ported | rs/a.rs | test_a |\n")
            errs = pc.check(go_dir=d / "go", repo_dir=d, ledger=ledger, in_scope=["model/llm_test.go"])
            self.assertTrue(any("TestB" in e for e in errs))
            ledger.write_text(ledger.read_text() + "| model/llm_test.go | TestB | skipped | - | reason |\n")
            self.assertEqual(pc.check(go_dir=d / "go", repo_dir=d, ledger=ledger, in_scope=["model/llm_test.go"]), [])
            (d / "rs/a.rs").write_text("")
            errs = pc.check(go_dir=d / "go", repo_dir=d, ledger=ledger, in_scope=["model/llm_test.go"])
            self.assertTrue(any("test_a" in e for e in errs))

    def test_1a_in_scope_from_header(self):
        with tempfile.TemporaryDirectory() as d:
            d = pathlib.Path(d)
            (d / "go/model").mkdir(parents=True)
            (d / "go/model/llm_test.go").write_text("package model\nfunc TestA(t *testing.T) {}\n")
            ledger = d / "1a.md"
            ledger.write_text("<!-- in-scope:\nmodel/llm_test.go\n-->\n\n" + pc.TABLE_HEADER + "\n" + pc.TABLE_SEP + "\n")
            self.assertEqual(pc.parse_header(ledger), (["model/llm_test.go"], {}))
            self.assertEqual(pc.check(d / "go", d, ledger), ["missing ledger row: model/llm_test.go TestA"])


GO_SRC = """package agent

func TestPorted(t *testing.T) {}
func TestAdapted(t *testing.T) {}
func TestSkipped(t *testing.T) {}
func helper() {}
"""

RUST_OK = """//! target
#[test]
fn test_ported() {}

// parity: adapted: Go mock replaced by ContextBuilder
#[tokio::test]
async fn test_adapted() {}

// parity: skipped TestSkipped: YAML loader is out of scope
"""

LEDGER_1B = (
    "# Parity ledger — sub-project 1b\n\n"
    "<!-- in-scope:\nagent/agent_test.go\nagent/helpers_test.go\n-->\n\n"
    "<!-- deferred:\nrunner/live_runner_test.go: live mode (sub-project 1b-live)\n-->\n\n"
    + pc.TABLE_HEADER + "\n" + pc.TABLE_SEP + "\n"
)

TARGET = "crates/gadk-agent/src/parity_tests/agent_agent.rs"


class OneBTest(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        d = pathlib.Path(self._tmp.name)
        self.go, self.repo = d / "go", d / "repo"
        (self.go / "agent").mkdir(parents=True)
        (self.go / "runner").mkdir(parents=True)
        (self.go / "agent/agent_test.go").write_text(GO_SRC)
        (self.go / "agent/helpers_test.go").write_text("package agent\nfunc newThing() {}\n")
        (self.go / "runner/live_runner_test.go").write_text("package runner\nfunc TestLive(t *testing.T) {}\n")
        (self.repo / "crates/gadk-agent/src/parity_tests").mkdir(parents=True)
        (self.repo / "docs/parity").mkdir(parents=True)
        self.target = self.repo / TARGET
        self.target.write_text(RUST_OK)
        self.ledger = self.repo / "docs/parity/1b.md"
        self.ledger.write_text(LEDGER_1B)

    def tearDown(self):
        self._tmp.cleanup()

    def scan(self, text):
        return pc.scan_target("agent/agent_test.go", ["TestPorted", "TestAdapted", "TestSkipped"], TARGET, text)

    def test_scan_ported_adapted_skipped(self):
        rows, errs = self.scan(RUST_OK)
        self.assertEqual(errs, [])
        self.assertEqual(rows, [
            ("agent/agent_test.go", "TestPorted", "ported", TARGET, "test_ported"),
            ("agent/agent_test.go", "TestAdapted", "ported (adapted: Go mock replaced by ContextBuilder)", TARGET, "test_adapted"),
            ("agent/agent_test.go", "TestSkipped", "skipped", TARGET, "YAML loader is out of scope"),
        ])

    def test_scan_adapted_window_is_three_lines_and_stops_at_previous_item(self):
        far = "// parity: adapted: too far\n#[a]\n#[b]\n#[c]\nfn test_adapted() {}\n"
        rows, _ = self.scan(RUST_OK.replace("// parity: adapted: Go mock replaced by ContextBuilder\n#[tokio::test]\nasync fn test_adapted() {}\n", far))
        self.assertEqual(rows[1][2], "ported")
        prev = "// parity: adapted: belongs to test_ported\nfn test_ported() {}\nfn test_adapted() {}\n"
        rows, _ = self.scan(prev + "// parity: skipped TestSkipped: r\n")
        self.assertEqual(rows[0][2], "ported (adapted: belongs to test_ported)")
        self.assertEqual(rows[1][2], "ported")

    def test_scan_missing(self):
        _, errs = self.scan(RUST_OK.replace("fn test_ported() {}", ""))
        self.assertEqual(len(errs), 1)
        self.assertIn("TestPorted: missing", errs[0])

    def test_scan_duplicate_fn_and_duplicate_skip(self):
        _, errs = self.scan(RUST_OK + "fn test_ported() {}\n")
        self.assertTrue(any("defined 2 times" in e for e in errs), errs)
        _, errs = self.scan(RUST_OK + "// parity: skipped TestSkipped: again\n")
        self.assertTrue(any("2 skip annotations" in e for e in errs), errs)

    def test_scan_both_unknown_empty_and_pipe(self):
        _, errs = self.scan(RUST_OK + "// parity: skipped TestPorted: dup\n")
        self.assertTrue(any("both fn test_ported" in e for e in errs), errs)
        _, errs = self.scan(RUST_OK + "// parity: skipped TestNope: typo\n")
        self.assertTrue(any("unknown Go test TestNope" in e for e in errs), errs)
        _, errs = self.scan(RUST_OK.replace("YAML loader is out of scope", ""))
        self.assertTrue(any("empty parity reason" in e for e in errs), errs)
        _, errs = self.scan(RUST_OK.replace("YAML loader", "a | b"))
        self.assertTrue(any("contains '|'" in e for e in errs), errs)

    def test_find_target_none_and_multiple(self):
        self.target.unlink()
        _, errs = pc.find_target(self.repo, "agent/agent_test.go")
        self.assertIn("no Rust target agent_agent.rs", errs[0])
        (self.repo / "crates/gadk-agent/tests").mkdir(parents=True)
        (self.repo / "crates/gadk-agent/tests/agent_agent.rs").write_text(RUST_OK)
        self.assertEqual(pc.find_target(self.repo, "agent/agent_test.go"), ("crates/gadk-agent/tests/agent_agent.rs", []))
        self.target.write_text(RUST_OK)
        _, errs = pc.find_target(self.repo, "agent/agent_test.go")
        self.assertIn("multiple Rust targets", errs[0])

    def test_files_mode_scans_only_named_files(self):
        self.assertEqual(pc.check_1b(self.go, self.repo, self.ledger, files=["agent/agent_test.go"]), [])
        self.assertEqual(pc.check_1b(self.go, self.repo, self.ledger, files=["agent/helpers_test.go"]), [])
        errs = pc.check_1b(self.go, self.repo, self.ledger, files=["runner/live_runner_test.go"])
        self.assertEqual(errs, ["not in the 1b in-scope list: runner/live_runner_test.go"])
        self.target.write_text("")
        errs = pc.check_1b(self.go, self.repo, self.ledger, files=["agent/agent_test.go"])
        self.assertEqual(len(errs), 3)

    def test_full_check_requires_generated_rows_and_generate_is_idempotent(self):
        errs = pc.check_1b(self.go, self.repo, self.ledger)
        self.assertTrue(any("ledger row missing or stale" in e for e in errs), errs)
        self.assertEqual(pc.generate(self.go, self.repo, self.ledger), [])
        once = self.ledger.read_text()
        self.assertTrue(once.startswith(LEDGER_1B))
        self.assertEqual(once.count("\n| agent/agent_test.go |"), 3)
        self.assertEqual(pc.check_1b(self.go, self.repo, self.ledger), [])
        self.assertEqual(pc.generate(self.go, self.repo, self.ledger), [])
        self.assertEqual(self.ledger.read_text(), once)
        # A hand edit or a removed row is rejected.
        self.ledger.write_text(once.replace("| ported |", "| skipped |", 1))
        self.assertNotEqual(pc.check_1b(self.go, self.repo, self.ledger), [])
        self.ledger.write_text("\n".join(once.splitlines()[:-1]) + "\n")
        self.assertNotEqual(pc.check_1b(self.go, self.repo, self.ledger), [])

    def test_generate_refuses_on_scan_errors(self):
        self.target.write_text("")
        self.assertNotEqual(pc.generate(self.go, self.repo, self.ledger), [])
        self.assertEqual(self.ledger.read_text(), LEDGER_1B)

    def test_header_checks(self):
        (self.go / "runner/live_runner_test.go").unlink()
        errs = pc.check_1b(self.go, self.repo, self.ledger)
        self.assertTrue(any("deferred file missing in ADK-Go: runner/live_runner_test.go" in e for e in errs), errs)

    def test_cli_count_and_files(self):
        def run(*args):
            out, err = io.StringIO(), io.StringIO()
            with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
                rc = pc.main(["--adk-go", str(self.go), "--repo", str(self.repo), *args])
            return rc, out.getvalue(), err.getvalue()
        self.assertEqual(run("--ledger", "1b", "--count"), (0, "3\n", ""))
        self.assertEqual(run("--ledger", "1b", "--count", "--files", "agent/helpers_test.go"), (0, "0\n", ""))
        self.assertEqual(run("--ledger", "1b", "--files", "agent/agent_test.go")[0], 0)
        self.assertEqual(run("--ledger", "1b")[0], 1)
        self.assertEqual(run("--ledger", "1b", "--generate")[0], 0)
        self.assertEqual(run("--ledger", "1b")[0], 0)
        with self.assertRaises(SystemExit):
            run("--ledger", "all", "--count")


LEDGER_1B_LIVE = (
    "# Parity ledger — sub-project 1b-live\n\n"
    "<!-- in-scope:\nrunner/live_runner_test.go\n-->\n\n"
    + pc.TABLE_HEADER + "\n" + pc.TABLE_SEP + "\n"
)

LIVE_TARGET = "crates/gadk-engine/src/parity_tests/runner_live_runner.rs"


class OneBLiveTest(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        d = pathlib.Path(self._tmp.name)
        self.go, self.repo = d / "go", d / "repo"
        (self.go / "runner").mkdir(parents=True)
        (self.go / "runner/live_runner_test.go").write_text(
            "package runner\nfunc TestLive(t *testing.T) {}\nfunc TestLiveNil(t *testing.T) {}\n")
        (self.repo / "crates/gadk-engine/src/parity_tests").mkdir(parents=True)
        (self.repo / "docs/parity").mkdir(parents=True)
        (self.repo / LIVE_TARGET).write_text(
            "#[tokio::test]\nasync fn test_live() {}\n\n// parity: skipped TestLiveNil: no nil events in Rust\n")
        self.ledger = self.repo / "docs/parity/1b-live.md"
        self.ledger.write_text(LEDGER_1B_LIVE)

    def tearDown(self):
        self._tmp.cleanup()

    def run_cli(self, *args):
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            rc = pc.main(["--adk-go", str(self.go), "--repo", str(self.repo), *args])
        return rc, out.getvalue(), err.getvalue()

    def test_count_files_generate_and_full_check(self):
        self.assertEqual(self.run_cli("--ledger", "1b-live", "--count"), (0, "2\n", ""))
        self.assertEqual(self.run_cli("--ledger", "1b-live", "--files", "runner/live_runner_test.go")[0], 0)
        rc, _, err = self.run_cli("--ledger", "1b-live")
        self.assertEqual(rc, 1)
        self.assertIn("run: python3 scripts/parity-check --ledger 1b-live --generate", err)
        self.assertEqual(self.run_cli("--ledger", "1b-live", "--generate")[0], 0)
        self.assertEqual(self.run_cli("--ledger", "1b-live")[0], 0)
        self.assertIn(
            "| runner/live_runner_test.go | TestLive | ported | " + LIVE_TARGET + " | test_live |",
            self.ledger.read_text())
        self.assertIn(
            "| runner/live_runner_test.go | TestLiveNil | skipped | " + LIVE_TARGET + " | no nil events in Rust |",
            self.ledger.read_text())

    def test_files_outside_the_live_in_scope_list_are_rejected(self):
        errs = pc.check_1b(self.go, self.repo, self.ledger, files=["agent/agent_test.go"], name="1b-live")
        self.assertEqual(errs, ["not in the 1b-live in-scope list: agent/agent_test.go"])

    def test_files_and_generate_still_refuse_1a_and_all(self):
        with self.assertRaises(SystemExit):
            self.run_cli("--ledger", "1a", "--generate")
        with self.assertRaises(SystemExit):
            self.run_cli("--ledger", "all", "--files", "runner/live_runner_test.go")


LEDGER_1C = (
    "# Parity ledger — sub-project 1c\n\n"
    "<!-- in-scope:\nsession/compaction/llm_summarizer_test.go\n-->\n\n"
    "<!-- deferred:\nsession/database/compaction_test.go: database session service (sub-project 4)\n-->\n\n"
    + pc.TABLE_HEADER + "\n" + pc.TABLE_SEP + "\n"
)

COMPACTION_TARGET = "crates/gadk-session/src/parity_tests/session_compaction_llm_summarizer.rs"


class OneCTest(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        d = pathlib.Path(self._tmp.name)
        self.go, self.repo = d / "go", d / "repo"
        (self.go / "session/compaction").mkdir(parents=True)
        (self.go / "session/database").mkdir(parents=True)
        (self.go / "session/compaction/llm_summarizer_test.go").write_text(
            "package compaction\nfunc TestNewLLMSummarizer(t *testing.T) {}\n"
            "func TestFormatEventsToleratesNilParts(t *testing.T) {}\n")
        (self.go / "session/database/compaction_test.go").write_text(
            "package database\nfunc TestDB(t *testing.T) {}\n")
        (self.repo / "crates/gadk-session/src/parity_tests").mkdir(parents=True)
        (self.repo / "docs/parity").mkdir(parents=True)
        (self.repo / COMPACTION_TARGET).write_text(
            "#[test]\nfn test_new_llm_summarizer() {}\n\n"
            "// parity: skipped TestFormatEventsToleratesNilParts: Content::parts holds no nil part\n")
        self.ledger = self.repo / "docs/parity/1c.md"
        self.ledger.write_text(LEDGER_1C)

    def tearDown(self):
        self._tmp.cleanup()

    def run_cli(self, *args):
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            rc = pc.main(["--adk-go", str(self.go), "--repo", str(self.repo), *args])
        return rc, out.getvalue(), err.getvalue()

    def test_count_files_generate_and_full_check(self):
        self.assertEqual(self.run_cli("--ledger", "1c", "--count"), (0, "2\n", ""))
        self.assertEqual(
            self.run_cli("--ledger", "1c", "--files", "session/compaction/llm_summarizer_test.go")[0], 0)
        rc, _, err = self.run_cli("--ledger", "1c")
        self.assertEqual(rc, 1)
        self.assertIn("run: python3 scripts/parity-check --ledger 1c --generate", err)
        self.assertEqual(self.run_cli("--ledger", "1c", "--generate")[0], 0)
        self.assertEqual(self.run_cli("--ledger", "1c")[0], 0)
        text = self.ledger.read_text()
        self.assertIn(
            "| session/compaction/llm_summarizer_test.go | TestNewLLMSummarizer | ported | "
            + COMPACTION_TARGET + " | test_new_llm_summarizer |", text)
        self.assertIn(
            "| session/compaction/llm_summarizer_test.go | TestFormatEventsToleratesNilParts | skipped | "
            + COMPACTION_TARGET + " | Content::parts holds no nil part |", text)
        # Deferred files get no rows.
        self.assertNotIn("TestDB", text)

    def test_files_outside_the_1c_in_scope_list_are_rejected(self):
        errs = pc.check_1b(self.go, self.repo, self.ledger,
                           files=["session/database/compaction_test.go"], name="1c")
        self.assertEqual(errs, ["not in the 1c in-scope list: session/database/compaction_test.go"])

    def test_missing_target_is_reported(self):
        (self.repo / COMPACTION_TARGET).unlink()
        errs = pc.check_1b(self.go, self.repo, self.ledger,
                           files=["session/compaction/llm_summarizer_test.go"], name="1c")
        self.assertEqual(errs, [
            "session/compaction/llm_summarizer_test.go: no Rust target session_compaction_llm_summarizer.rs"
            " under crates/*/tests/ or crates/*/src/parity_tests/"])

    def test_files_and_generate_still_refuse_1a_and_all(self):
        with self.assertRaises(SystemExit):
            self.run_cli("--ledger", "1a", "--generate")
        with self.assertRaises(SystemExit):
            self.run_cli("--ledger", "all", "--files", "session/compaction/llm_summarizer_test.go")


LEDGER_2A = (
    "# Parity ledger — sub-project 2a\n\n"
    "<!-- in-scope:\nplugin/functioncallmodifier/plugin_test.go\ntool/exampletool/tool_test.go\n-->\n\n"
    + pc.TABLE_HEADER + "\n" + pc.TABLE_SEP + "\n"
)

EXAMPLE_TARGET = "crates/gadk-tools/src/parity_tests/tool_exampletool_tool.rs"
FCM_TARGET = "crates/gadk-engine/tests/plugin_functioncallmodifier_plugin.rs"


class TwoATest(unittest.TestCase):
    """2a: a gadk-tools white-box target and a gadk-engine tests target; the ledger starts with
    a header and no rows, and the full check fails until every target is annotated."""

    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        d = pathlib.Path(self._tmp.name)
        self.go, self.repo = d / "go", d / "repo"
        (self.go / "tool/exampletool").mkdir(parents=True)
        (self.go / "plugin/functioncallmodifier").mkdir(parents=True)
        (self.go / "tool/exampletool/tool_test.go").write_text(
            "package exampletool\nfunc TestExampleTool_ProcessRequest(t *testing.T) {}\n"
            "func TestExampleTool_Interface(t *testing.T) {}\n")
        (self.go / "plugin/functioncallmodifier/plugin_test.go").write_text(
            "package functioncallmodifier_test\nfunc TestPluginCallbacks(t *testing.T) {}\n")
        (self.repo / "crates/gadk-tools/src/parity_tests").mkdir(parents=True)
        (self.repo / "crates/gadk-engine/tests").mkdir(parents=True)
        (self.repo / "docs/parity").mkdir(parents=True)
        # Placeholders, as the 2a scaffold leaves them.
        (self.repo / EXAMPLE_TARGET).write_text("//! Port of Go tool/exampletool/tool_test.go.\n")
        (self.repo / FCM_TARGET).write_text("// license header only\n")
        self.ledger = self.repo / "docs/parity/2a.md"
        self.ledger.write_text(LEDGER_2A)

    def tearDown(self):
        self._tmp.cleanup()

    def run_cli(self, *args):
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            rc = pc.main(["--adk-go", str(self.go), "--repo", str(self.repo), *args])
        return rc, out.getvalue(), err.getvalue()

    def fill_targets(self):
        (self.repo / EXAMPLE_TARGET).write_text(
            "// parity: adapted: nil-request cases dropped\n"
            "#[tokio::test]\nasync fn test_example_tool_process_request() {}\n\n"
            "#[test]\nfn test_example_tool_interface() {}\n")
        (self.repo / FCM_TARGET).write_text("#[tokio::test]\nasync fn test_plugin_callbacks() {}\n")

    def test_header_only_ledger_counts_and_reports_placeholders(self):
        self.assertEqual(self.run_cli("--ledger", "2a", "--count"), (0, "3\n", ""))
        rc, _, err = self.run_cli("--ledger", "2a")
        self.assertEqual(rc, 1)
        self.assertIn(
            "tool/exampletool/tool_test.go TestExampleTool_Interface: missing (no fn"
            " test_example_tool_interface", err)
        self.assertIn(
            "plugin/functioncallmodifier/plugin_test.go TestPluginCallbacks: missing", err)
        rc, _, _ = self.run_cli("--ledger", "2a", "--generate")
        self.assertEqual(rc, 1, "generate refuses while targets are placeholders")
        self.assertEqual(self.ledger.read_text(), LEDGER_2A)

    def test_files_generate_and_full_check(self):
        self.fill_targets()
        self.assertEqual(
            self.run_cli("--ledger", "2a", "--files", "tool/exampletool/tool_test.go")[0], 0)
        rc, _, err = self.run_cli("--ledger", "2a")
        self.assertEqual(rc, 1)
        self.assertIn("run: python3 scripts/parity-check --ledger 2a --generate", err)
        self.assertEqual(self.run_cli("--ledger", "2a", "--generate")[0], 0)
        self.assertEqual(self.run_cli("--ledger", "2a")[0], 0)
        text = self.ledger.read_text()
        self.assertIn(
            "| tool/exampletool/tool_test.go | TestExampleTool_ProcessRequest | ported (adapted: nil-request"
            " cases dropped) | " + EXAMPLE_TARGET + " | test_example_tool_process_request |", text)
        self.assertIn(
            "| plugin/functioncallmodifier/plugin_test.go | TestPluginCallbacks | ported | "
            + FCM_TARGET + " | test_plugin_callbacks |", text)

    def test_files_outside_the_2a_in_scope_list_are_rejected(self):
        errs = pc.check_1b(self.go, self.repo, self.ledger,
                           files=["tool/toolutils/toolutils_test.go"], name="2a")
        self.assertEqual(errs, ["not in the 2a in-scope list: tool/toolutils/toolutils_test.go"])

    def test_files_and_generate_still_refuse_1a_and_all(self):
        with self.assertRaises(SystemExit):
            self.run_cli("--ledger", "1a", "--generate")
        with self.assertRaises(SystemExit):
            self.run_cli("--ledger", "all", "--files", "tool/exampletool/tool_test.go")


LEDGER_2B = (
    "# Parity ledger — sub-project 2b\n\n"
    "<!-- in-scope:\nauth/credential_test.go\nauth/gcp/redact_fuzz_test.go\n-->\n\n"
    + pc.TABLE_HEADER + "\n" + pc.TABLE_SEP + "\n"
)

CRED_TARGET = "crates/gadk-auth/tests/auth_credential.rs"
FUZZ_TARGET = "crates/gadk-auth/src/parity_tests/auth_gcp_redact_fuzz.rs"


class TwoBTest(unittest.TestCase):
    """2b: a `Fuzz*` Go test gets a row like a `Test*` one (`FuzzRedact` -> `fn fuzz_redact`);
    the ledger starts with a header and no rows, as the 2b scaffold leaves it."""

    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        d = pathlib.Path(self._tmp.name)
        self.go, self.repo = d / "go", d / "repo"
        (self.go / "auth/gcp").mkdir(parents=True)
        (self.go / "auth/credential_test.go").write_text(
            "package auth_test\nfunc TestCredentialApply(t *testing.T) {}\n"
            "func TestOAuth2TokenSourceError(t *testing.T) {}\n")
        (self.go / "auth/gcp/redact_fuzz_test.go").write_text(
            "package gcp\nfunc FuzzRedact(f *testing.F) {}\nfunc BenchmarkRedact(b *testing.B) {}\n")
        (self.repo / "crates/gadk-auth/tests").mkdir(parents=True)
        (self.repo / "crates/gadk-auth/src/parity_tests").mkdir(parents=True)
        (self.repo / "docs/parity").mkdir(parents=True)
        (self.repo / CRED_TARGET).write_text(
            "// parity: adapted: two cases cannot be built\n"
            "#[tokio::test]\nasync fn test_credential_apply() {}\n\n"
            "#[tokio::test]\nasync fn test_o_auth2_token_source_error() {}\n")
        # The placeholder the 2b scaffold leaves for Task 5.
        (self.repo / FUZZ_TARGET).write_text("//! Port of Go `auth/gcp/redact_fuzz_test.go` (Task 5).\n")
        self.ledger = self.repo / "docs/parity/2b.md"
        self.ledger.write_text(LEDGER_2B)

    def tearDown(self):
        self._tmp.cleanup()

    def run_cli(self, *args):
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            rc = pc.main(["--adk-go", str(self.go), "--repo", str(self.repo), *args])
        return rc, out.getvalue(), err.getvalue()

    def test_fuzz_counts_and_a_placeholder_reports_it_missing(self):
        self.assertEqual(self.run_cli("--ledger", "2b", "--count"), (0, "3\n", ""))
        self.assertEqual(self.run_cli("--ledger", "2b", "--files", "auth/credential_test.go")[0], 0)
        rc, _, err = self.run_cli("--ledger", "2b")
        self.assertEqual(rc, 1)
        self.assertIn(
            "auth/gcp/redact_fuzz_test.go FuzzRedact: missing (no fn fuzz_redact and no"
            " `// parity: skipped FuzzRedact:` in " + FUZZ_TARGET + ")", err)
        self.assertEqual(self.run_cli("--ledger", "2b", "--generate")[0], 1)
        self.assertEqual(self.ledger.read_text(), LEDGER_2B)

    def test_fuzz_port_generates_a_row(self):
        (self.repo / FUZZ_TARGET).write_text(
            "// parity: adapted: deterministic corpus\n#[test]\nfn fuzz_redact() {}\n")
        self.assertEqual(self.run_cli("--ledger", "2b", "--generate")[0], 0)
        self.assertEqual(self.run_cli("--ledger", "2b")[0], 0)
        text = self.ledger.read_text()
        self.assertIn(
            "| auth/gcp/redact_fuzz_test.go | FuzzRedact | ported (adapted: deterministic corpus) | "
            + FUZZ_TARGET + " | fuzz_redact |", text)
        self.assertIn(
            "| auth/credential_test.go | TestOAuth2TokenSourceError | ported | " + CRED_TARGET
            + " | test_o_auth2_token_source_error |", text)
        self.assertNotIn("BenchmarkRedact", text)

    def test_fuzz_skip_annotation_is_accepted(self):
        (self.repo / FUZZ_TARGET).write_text("// parity: skipped FuzzRedact: no fuzzer\n")
        self.assertEqual(self.run_cli("--ledger", "2b", "--generate")[0], 0)
        self.assertIn(
            "| auth/gcp/redact_fuzz_test.go | FuzzRedact | skipped | " + FUZZ_TARGET
            + " | no fuzzer |", self.ledger.read_text())

    def test_files_and_generate_still_refuse_1a_and_all(self):
        with self.assertRaises(SystemExit):
            self.run_cli("--ledger", "1a", "--generate")
        with self.assertRaises(SystemExit):
            self.run_cli("--ledger", "all", "--files", "auth/credential_test.go")


LEDGER_2C = (
    "# Parity ledger — sub-project 2c\n\n"
    "<!-- in-scope:\ntool/mcptoolset/tool_test.go\ntool/mcptoolset/set_test.go\n-->\n\n"
    + pc.TABLE_HEADER + "\n" + pc.TABLE_SEP + "\n"
)

MCP_TOOL_TARGET = "crates/gadk-mcp/src/parity_tests/tool_mcptoolset_tool.rs"
MCP_SET_TARGET = "crates/gadk/tests/tool_mcptoolset_set.rs"


class TwoCTest(unittest.TestCase):
    """2c: a gadk-mcp white-box target and a façade tests target; the ledger starts with a
    header and no rows, and the scaffold's placeholder target reports its tests missing."""

    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        d = pathlib.Path(self._tmp.name)
        self.go, self.repo = d / "go", d / "repo"
        (self.go / "tool/mcptoolset").mkdir(parents=True)
        (self.go / "tool/mcptoolset/tool_test.go").write_text(
            "package mcptoolset\nfunc TestMCPToolRunContent(t *testing.T) {}\n"
            "func TestHasMIMECharsetParameter(t *testing.T) {}\n")
        (self.go / "tool/mcptoolset/set_test.go").write_text(
            "package mcptoolset_test\nfunc TestMCPToolSet(t *testing.T) {}\n")
        (self.repo / "crates/gadk-mcp/src/parity_tests").mkdir(parents=True)
        (self.repo / "crates/gadk/tests").mkdir(parents=True)
        (self.repo / "docs/parity").mkdir(parents=True)
        # The placeholder the 2c scaffold leaves for Task 3.
        (self.repo / MCP_TOOL_TARGET).write_text(
            "//! Port of Go `tool/mcptoolset/tool_test.go` (package `mcptoolset`; Task 3).\n")
        (self.repo / MCP_SET_TARGET).write_text("#[tokio::test]\nasync fn test_mcp_tool_set() {}\n")
        self.ledger = self.repo / "docs/parity/2c.md"
        self.ledger.write_text(LEDGER_2C)

    def tearDown(self):
        self._tmp.cleanup()

    def run_cli(self, *args):
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            rc = pc.main(["--adk-go", str(self.go), "--repo", str(self.repo), *args])
        return rc, out.getvalue(), err.getvalue()

    def test_placeholder_reports_missing_and_generate_refuses(self):
        self.assertEqual(self.run_cli("--ledger", "2c", "--count"), (0, "3\n", ""))
        self.assertEqual(
            self.run_cli("--ledger", "2c", "--files", "tool/mcptoolset/set_test.go")[0], 0)
        rc, _, err = self.run_cli("--ledger", "2c")
        self.assertEqual(rc, 1)
        self.assertIn(
            "tool/mcptoolset/tool_test.go TestHasMIMECharsetParameter: missing (no fn"
            " test_has_mime_charset_parameter and no `// parity: skipped"
            " TestHasMIMECharsetParameter:` in " + MCP_TOOL_TARGET + ")", err)
        self.assertEqual(self.run_cli("--ledger", "2c", "--generate")[0], 1)
        self.assertEqual(self.ledger.read_text(), LEDGER_2C)

    def test_ports_generate_rows(self):
        (self.repo / MCP_TOOL_TARGET).write_text(
            "// parity: adapted: nil content cases are skipped\n#[test]\nfn test_mcp_tool_run_content() {}\n"
            "\n#[test]\nfn test_has_mime_charset_parameter() {}\n")
        self.assertEqual(self.run_cli("--ledger", "2c", "--generate")[0], 0)
        self.assertEqual(self.run_cli("--ledger", "2c")[0], 0)
        text = self.ledger.read_text()
        self.assertIn(
            "| tool/mcptoolset/tool_test.go | TestMCPToolRunContent | ported (adapted: nil content"
            " cases are skipped) | " + MCP_TOOL_TARGET + " | test_mcp_tool_run_content |", text)
        self.assertIn(
            "| tool/mcptoolset/set_test.go | TestMCPToolSet | ported | " + MCP_SET_TARGET
            + " | test_mcp_tool_set |", text)


LEDGER_3A = (
    "# Parity ledger — sub-project 3a\n\n"
    "<!-- in-scope:\nmodel/anthropicmodel/anthropicmodel_test.go\nmodel/apigee/apigee_test.go\n-->\n\n"
    + pc.TABLE_HEADER + "\n" + pc.TABLE_SEP + "\n"
)

ANTHROPIC_DIR = "crates/gadk-anthropic/tests/model_anthropicmodel_anthropicmodel/"
APIGEE_TARGET = "crates/gadk-gemini/tests/model_apigee_apigee.rs"
RUNNER_SKIP = (
    "// parity: skipped TestAgentRunner_MultiTurnImageAndTool: ported in"
    " crates/gadk/tests/model_anthropicmodel_agent_runner.rs (needs the runner)\n"
)


class ThreeATest(unittest.TestCase):
    """3a: a directory target (`tests/<target>/main.rs` plus module files) and a file target."""

    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        d = pathlib.Path(self._tmp.name)
        self.go, self.repo = d / "go", d / "repo"
        (self.go / "model/anthropicmodel").mkdir(parents=True)
        (self.go / "model/apigee").mkdir(parents=True)
        (self.go / "model/anthropicmodel/anthropicmodel_test.go").write_text(
            "package anthropicmodel_test\nfunc TestGenerate_Anthropic(t *testing.T) {}\n"
            "func TestAgentRunner_MultiTurnImageAndTool(t *testing.T) {}\n"
            "func TestPromptCaching(t *testing.T) {}\n")
        (self.go / "model/apigee/apigee_test.go").write_text(
            "package apigee\nfunc TestParseModelName(t *testing.T) {}\n")
        self.dir = self.repo / ANTHROPIC_DIR
        self.dir.mkdir(parents=True)
        (self.repo / "crates/gadk-gemini/tests").mkdir(parents=True)
        (self.repo / "docs/parity").mkdir(parents=True)
        # The scaffold: main.rs with the runner skip line, empty module files.
        (self.dir / "main.rs").write_text("//! Parity target.\n\n" + RUNNER_SKIP + "\nmod generate;\nmod tools_thinking;\n")
        (self.dir / "generate.rs").write_text("//! Task 8.\n")
        (self.dir / "tools_thinking.rs").write_text("//! Task 10.\n")
        (self.repo / APIGEE_TARGET).write_text("#[tokio::test]\nasync fn test_parse_model_name() {}\n")
        self.ledger = self.repo / "docs/parity/3a.md"
        self.ledger.write_text(LEDGER_3A)

    def tearDown(self):
        self._tmp.cleanup()

    def run_cli(self, *args):
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            rc = pc.main(["--adk-go", str(self.go), "--repo", str(self.repo), *args])
        return rc, out.getvalue(), err.getvalue()

    def test_find_target_accepts_a_test_directory(self):
        self.assertEqual(
            pc.find_target(self.repo, "model/anthropicmodel/anthropicmodel_test.go"),
            (ANTHROPIC_DIR + "main.rs", []))

    def test_scaffold_reports_missing_with_the_directory_and_accepts_the_skip(self):
        self.assertEqual(self.run_cli("--ledger", "3a", "--count"), (0, "4\n", ""))
        rc, _, err = self.run_cli("--ledger", "3a")
        self.assertEqual(rc, 1)
        self.assertEqual(err.splitlines(), [
            "model/anthropicmodel/anthropicmodel_test.go TestGenerate_Anthropic: missing (no fn"
            " test_generate_anthropic and no `// parity: skipped TestGenerate_Anthropic:` in "
            + ANTHROPIC_DIR + ")",
            "model/anthropicmodel/anthropicmodel_test.go TestPromptCaching: missing (no fn"
            " test_prompt_caching and no `// parity: skipped TestPromptCaching:` in "
            + ANTHROPIC_DIR + ")",
        ])
        self.assertEqual(self.run_cli("--ledger", "3a", "--files", "model/apigee/apigee_test.go")[0], 0)

    def test_rows_name_the_file_that_holds_the_fn_or_the_skip(self):
        (self.dir / "generate.rs").write_text("#[tokio::test]\nasync fn test_generate_anthropic() {}\n")
        (self.dir / "tools_thinking.rs").write_text(
            "// parity: adapted: tracing replaces slog\n#[tokio::test]\nasync fn test_prompt_caching() {}\n")
        self.assertEqual(self.run_cli("--ledger", "3a", "--files",
                                      "model/anthropicmodel/anthropicmodel_test.go")[0], 0)
        self.assertEqual(self.run_cli("--ledger", "3a", "--generate")[0], 0)
        self.assertEqual(self.run_cli("--ledger", "3a"), (0, "", ""))
        rows = self.ledger.read_text().splitlines()[-4:]
        self.assertEqual(rows, [
            "| model/anthropicmodel/anthropicmodel_test.go | TestGenerate_Anthropic | ported | "
            + ANTHROPIC_DIR + "generate.rs | test_generate_anthropic |",
            "| model/anthropicmodel/anthropicmodel_test.go | TestAgentRunner_MultiTurnImageAndTool |"
            " skipped | " + ANTHROPIC_DIR + "main.rs | ported in"
            " crates/gadk/tests/model_anthropicmodel_agent_runner.rs (needs the runner) |",
            "| model/anthropicmodel/anthropicmodel_test.go | TestPromptCaching | ported (adapted:"
            " tracing replaces slog) | " + ANTHROPIC_DIR + "tools_thinking.rs | test_prompt_caching |",
            "| model/apigee/apigee_test.go | TestParseModelName | ported | " + APIGEE_TARGET
            + " | test_parse_model_name |",
        ])

    def test_duplicates_across_files_and_unknown_skips_are_errors(self):
        (self.dir / "generate.rs").write_text(
            "fn test_generate_anthropic() {}\n// parity: skipped TestNope: x\n")
        (self.dir / "tools_thinking.rs").write_text("fn test_generate_anthropic() {}\nfn test_prompt_caching() {}\n")
        rc, _, err = self.run_cli("--ledger", "3a")
        self.assertEqual(rc, 1)
        self.assertEqual(err.splitlines(), [
            ANTHROPIC_DIR + "generate.rs: skip annotation for unknown Go test TestNope (not in"
            " model/anthropicmodel/anthropicmodel_test.go)",
            "model/anthropicmodel/anthropicmodel_test.go TestGenerate_Anthropic: fn"
            " test_generate_anthropic defined 2 times in " + ANTHROPIC_DIR,
        ])

    def test_nested_module_files_are_scanned(self):
        (self.dir / "nested").mkdir()
        (self.dir / "nested/deep.rs").write_text("fn test_generate_anthropic() {}\nfn test_prompt_caching() {}\n")
        rows, errs = pc.generate_rows(self.go, self.repo, ["model/anthropicmodel/anthropicmodel_test.go"])
        self.assertEqual(errs, [])
        self.assertEqual([r[3] for r in rows], [
            ANTHROPIC_DIR + "nested/deep.rs", ANTHROPIC_DIR + "main.rs", ANTHROPIC_DIR + "nested/deep.rs"])


LEDGER_3B = (
    "# Parity ledger — sub-project 3b\n\n"
    "<!-- in-scope:\nmodel/openaimodel/internal/responses/request_test.go\n"
    "model/openaimodel/internal/shared/tools_test.go\n-->\n\n"
    + pc.TABLE_HEADER + "\n" + pc.TABLE_SEP + "\n"
)

REQUEST_DIR = "crates/gadk-openai/src/parity_tests/model_openaimodel_internal_responses_request/"
TOOLS_TARGET = "crates/gadk-openai/src/parity_tests/model_openaimodel_internal_shared_tools.rs"


class ThreeBTest(unittest.TestCase):
    """3b: an in-crate module directory target (`src/parity_tests/<target>/mod.rs` plus module
    files) beside a file target under `src/parity_tests/`."""

    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        d = pathlib.Path(self._tmp.name)
        self.go, self.repo = d / "go", d / "repo"
        (self.go / "model/openaimodel/internal/responses").mkdir(parents=True)
        (self.go / "model/openaimodel/internal/shared").mkdir(parents=True)
        (self.go / "model/openaimodel/internal/responses/request_test.go").write_text(
            "package responses\nfunc TestBuildParams_Text(t *testing.T) {}\n"
            "func TestApplyGenerationConfig(t *testing.T) {}\n"
            "func FuzzFailedResponseError(f *testing.F) {}\n")
        (self.go / "model/openaimodel/internal/shared/tools_test.go").write_text(
            "package shared\nfunc TestEnsureFunctionToolOnly(t *testing.T) {}\n")
        self.dir = self.repo / REQUEST_DIR
        self.dir.mkdir(parents=True)
        (self.repo / "docs/parity").mkdir(parents=True)
        # The scaffold: mod.rs declaring the sub-modules, empty module files.
        (self.dir / "mod.rs").write_text("//! Parity target.\n\nmod build_params;\nmod generation_config;\n")
        (self.dir / "build_params.rs").write_text("//! Task 5.\n")
        (self.dir / "generation_config.rs").write_text("//! Task 5.\n")
        (self.repo / TOOLS_TARGET).write_text("#[test]\nfn test_ensure_function_tool_only() {}\n")
        self.ledger = self.repo / "docs/parity/3b.md"
        self.ledger.write_text(LEDGER_3B)

    def tearDown(self):
        self._tmp.cleanup()

    def run_cli(self, *args):
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            rc = pc.main(["--adk-go", str(self.go), "--repo", str(self.repo), *args])
        return rc, out.getvalue(), err.getvalue()

    def test_find_target_accepts_a_parity_tests_module_directory(self):
        self.assertEqual(
            pc.find_target(self.repo, "model/openaimodel/internal/responses/request_test.go"),
            (REQUEST_DIR + "mod.rs", []))
        self.assertEqual(
            pc.find_target(self.repo, "model/openaimodel/internal/shared/tools_test.go"),
            (TOOLS_TARGET, []))

    def test_target_files_scans_every_module_file(self):
        label, files = pc.target_files(self.repo, REQUEST_DIR + "mod.rs")
        self.assertEqual(label, REQUEST_DIR)
        self.assertEqual([p for p, _ in files], [
            REQUEST_DIR + "build_params.rs", REQUEST_DIR + "generation_config.rs", REQUEST_DIR + "mod.rs"])
        label, files = pc.target_files(self.repo, TOOLS_TARGET)
        self.assertEqual((label, [p for p, _ in files]), (TOOLS_TARGET, [TOOLS_TARGET]))

    def test_scaffold_reports_only_missing_tests(self):
        self.assertEqual(self.run_cli("--ledger", "3b", "--count"), (0, "4\n", ""))
        rc, _, err = self.run_cli("--ledger", "3b")
        self.assertEqual(rc, 1)
        self.assertEqual(err.splitlines(), [
            "model/openaimodel/internal/responses/request_test.go " + name + ": missing (no fn "
            + rust + " and no `// parity: skipped " + name + ":` in " + REQUEST_DIR + ")"
            for name, rust in [
                ("TestBuildParams_Text", "test_build_params_text"),
                ("TestApplyGenerationConfig", "test_apply_generation_config"),
                ("FuzzFailedResponseError", "fuzz_failed_response_error"),
            ]
        ])
        self.assertEqual(self.run_cli("--ledger", "3b", "--files",
                                      "model/openaimodel/internal/shared/tools_test.go")[0], 0)

    def test_rows_name_the_module_file_that_holds_the_fn_or_the_skip(self):
        (self.dir / "build_params.rs").write_text(
            "#[test]\nfn test_build_params_text() {}\n"
            "// parity: skipped FuzzFailedResponseError: covered by its seed table\n")
        (self.dir / "generation_config.rs").write_text(
            "// parity: adapted: HttpOptions travels beside the config\n#[test]\nfn test_apply_generation_config() {}\n")
        self.assertEqual(self.run_cli("--ledger", "3b", "--generate")[0], 0)
        self.assertEqual(self.run_cli("--ledger", "3b"), (0, "", ""))
        rows = self.ledger.read_text().splitlines()[-4:]
        self.assertEqual(rows, [
            "| model/openaimodel/internal/responses/request_test.go | TestBuildParams_Text | ported | "
            + REQUEST_DIR + "build_params.rs | test_build_params_text |",
            "| model/openaimodel/internal/responses/request_test.go | TestApplyGenerationConfig | ported"
            " (adapted: HttpOptions travels beside the config) | " + REQUEST_DIR
            + "generation_config.rs | test_apply_generation_config |",
            "| model/openaimodel/internal/responses/request_test.go | FuzzFailedResponseError | skipped | "
            + REQUEST_DIR + "build_params.rs | covered by its seed table |",
            "| model/openaimodel/internal/shared/tools_test.go | TestEnsureFunctionToolOnly | ported | "
            + TOOLS_TARGET + " | test_ensure_function_tool_only |",
        ])

    def test_a_file_and_a_directory_target_for_one_go_file_is_an_error(self):
        (self.dir.parent / "model_openaimodel_internal_responses_request.rs").write_text("")
        target, errs = pc.find_target(self.repo, "model/openaimodel/internal/responses/request_test.go")
        self.assertIsNone(target)
        self.assertEqual(len(errs), 1)
        self.assertIn("multiple Rust targets", errs[0])

    def test_ledger_3b_is_registered(self):
        self.assertIn("3b", pc.GENERATED_LEDGERS)
        err = io.StringIO()
        with contextlib.redirect_stderr(err), self.assertRaises(SystemExit):
            pc.main(["--adk-go", str(self.go), "--repo", str(self.repo), "--ledger", "all", "--count"])
        self.assertIn("--count requires --ledger 1a, 1b, 1b-live, 1c, 2a, 2b, 2c, 3a, 3b, 4a, 4b, 4c, 5a, 5b, 5c or 6", err.getvalue())


LEDGER_4A = (
    "# Parity ledger — sub-project 4a\n\n"
    "<!-- in-scope:\nsession/database/compaction_test.go\nsession/database/session_test.go\n-->\n\n"
    + pc.TABLE_HEADER + "\n" + pc.TABLE_SEP + "\n"
)

ENGINE_TARGET = "crates/gadk-engine/src/parity_tests/session_database_compaction.rs"
SESSION_TARGET = "crates/gadk-session-database/src/parity_tests/session_database_session.rs"


LEDGER_4B = (
    "# Parity ledger — sub-project 4b\n\n"
    "<!-- in-scope:\nartifact/gcsartifact/gcs_client_test.go\nartifact/gcsartifact/gcs_test.go\n-->\n\n"
    + pc.TABLE_HEADER + "\n" + pc.TABLE_SEP + "\n"
)

GCS_TARGET = "crates/gadk-artifact-gcs/src/parity_tests/artifact_gcsartifact_gcs.rs"
GCS_CLIENT_TARGET = "crates/gadk-artifact-gcs/src/parity_tests/artifact_gcsartifact_gcs_client.rs"


class FourBTest(unittest.TestCase):
    """4b: one Go package whose two test files land in one crate's `src/parity_tests/`."""

    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        d = pathlib.Path(self._tmp.name)
        self.go, self.repo = d / "go", d / "repo"
        (self.go / "artifact/gcsartifact").mkdir(parents=True)
        (self.go / "artifact/gcsartifact/gcs_test.go").write_text(
            "package gcsartifact\nfunc TestSleepContext(t *testing.T) {}\n")
        (self.go / "artifact/gcsartifact/gcs_client_test.go").write_text(
            "package gcsartifact\nfunc TestObjectWrapperIfNotExist(t *testing.T) {}\n")
        (self.repo / GCS_TARGET).parent.mkdir(parents=True)
        (self.repo / GCS_TARGET).write_text("#[tokio::test]\nasync fn test_sleep_context() {}\n")
        (self.repo / GCS_CLIENT_TARGET).write_text(
            "// parity: adapted: no unconditional upload\n#[tokio::test]\nasync fn test_object_wrapper_if_not_exist() {}\n")
        (self.repo / "docs/parity").mkdir(parents=True)
        self.ledger = self.repo / "docs/parity/4b.md"
        self.ledger.write_text(LEDGER_4B)

    def tearDown(self):
        self._tmp.cleanup()

    def run_cli(self, *args):
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            rc = pc.main(["--adk-go", str(self.go), "--repo", str(self.repo), *args])
        return rc, out.getvalue(), err.getvalue()

    def test_ledger_4b_is_registered(self):
        self.assertIn("4b", pc.GENERATED_LEDGERS)

    def test_targets_generate_and_check(self):
        self.assertEqual(self.run_cli("--ledger", "4b", "--count"), (0, "2\n", ""))
        self.assertEqual(self.run_cli("--ledger", "4b", "--generate")[0], 0)
        self.assertEqual(self.run_cli("--ledger", "4b"), (0, "", ""))
        rows = self.ledger.read_text().splitlines()[-2:]
        self.assertEqual(rows, [
            "| artifact/gcsartifact/gcs_client_test.go | TestObjectWrapperIfNotExist"
            " | ported (adapted: no unconditional upload) | " + GCS_CLIENT_TARGET
            + " | test_object_wrapper_if_not_exist |",
            "| artifact/gcsartifact/gcs_test.go | TestSleepContext | ported | " + GCS_TARGET
            + " | test_sleep_context |",
        ])



LEDGER_4C = (
    "# Parity ledger — sub-project 4c\n\n"
    "<!-- in-scope:\nsession/vertexai/filter_test.go\nsession/vertexai/vertexai_test.go\n-->\n\n"
    + pc.TABLE_HEADER + "\n" + pc.TABLE_SEP + "\n"
)

FILTER_TARGET = "crates/gadk-vertexai/src/parity_tests/session_vertexai_filter.rs"
VERTEXAI_DIR = "crates/gadk-vertexai/src/parity_tests/session_vertexai_vertexai"


class FourCTest(unittest.TestCase):
    """4c: one Go package whose test files land in one crate's `src/parity_tests/`, one of them
    (`vertexai_test.go`) in a module directory split into `convert.rs` and `client.rs`."""

    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        d = pathlib.Path(self._tmp.name)
        self.go, self.repo = d / "go", d / "repo"
        (self.go / "session/vertexai").mkdir(parents=True)
        (self.go / "session/vertexai/filter_test.go").write_text(
            "package vertexai\nfunc TestQuoteFilterLiteral(t *testing.T) {}\n")
        (self.go / "session/vertexai/vertexai_test.go").write_text(
            "package vertexai\nfunc TestToStructPB(t *testing.T) {}\n"
            "func TestGetReasoningEngineID(t *testing.T) {}\n"
            "func TestAiplatformToGenaiContentPreservesFunctionIDs(t *testing.T) {}\n")
        (self.repo / VERTEXAI_DIR).mkdir(parents=True)
        (self.repo / FILTER_TARGET).write_text("#[test]\nfn test_quote_filter_literal() {}\n")
        (self.repo / VERTEXAI_DIR / "mod.rs").write_text("mod client;\nmod convert;\n")
        (self.repo / VERTEXAI_DIR / "convert.rs").write_text(
            "#[test]\nfn test_to_struct_pb() {}\n\n"
            "#[test]\nfn test_aiplatform_to_genai_content_preserves_function_i_ds() {}\n")
        (self.repo / VERTEXAI_DIR / "client.rs").write_text(
            "// parity: adapted: REST resource names\n#[test]\nfn test_get_reasoning_engine_id() {}\n")
        (self.repo / "docs/parity").mkdir(parents=True)
        self.ledger = self.repo / "docs/parity/4c.md"
        self.ledger.write_text(LEDGER_4C)

    def tearDown(self):
        self._tmp.cleanup()

    def run_cli(self, *args):
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            rc = pc.main(["--adk-go", str(self.go), "--repo", str(self.repo), *args])
        return rc, out.getvalue(), err.getvalue()

    def test_ledger_4c_is_registered(self):
        self.assertIn("4c", pc.GENERATED_LEDGERS)

    def test_function_ids_name(self):
        self.assertEqual(pc.go_test_to_rust("TestAiplatformToGenaiContentPreservesFunctionIDs"),
                         "test_aiplatform_to_genai_content_preserves_function_i_ds")

    def test_header_only_ledger_fails_until_targets_hold_tests(self):
        (self.repo / FILTER_TARGET).write_text("//! placeholder\n")
        rc, out, err = self.run_cli("--ledger", "4c")
        self.assertEqual((rc, out), (1, ""))
        self.assertIn(
            "session/vertexai/filter_test.go TestQuoteFilterLiteral: missing (no fn test_quote_filter_literal"
            " and no `// parity: skipped TestQuoteFilterLiteral:` in " + FILTER_TARGET + ")", err)

    def test_targets_generate_and_check(self):
        self.assertEqual(self.run_cli("--ledger", "4c", "--count"), (0, "4\n", ""))
        self.assertEqual(self.run_cli("--ledger", "4c", "--generate")[0], 0)
        self.assertEqual(self.run_cli("--ledger", "4c"), (0, "", ""))
        rows = self.ledger.read_text().splitlines()[-4:]
        self.assertEqual(rows, [
            "| session/vertexai/filter_test.go | TestQuoteFilterLiteral | ported | " + FILTER_TARGET
            + " | test_quote_filter_literal |",
            "| session/vertexai/vertexai_test.go | TestToStructPB | ported | " + VERTEXAI_DIR
            + "/convert.rs | test_to_struct_pb |",
            "| session/vertexai/vertexai_test.go | TestGetReasoningEngineID"
            " | ported (adapted: REST resource names) | " + VERTEXAI_DIR
            + "/client.rs | test_get_reasoning_engine_id |",
            "| session/vertexai/vertexai_test.go | TestAiplatformToGenaiContentPreservesFunctionIDs"
            " | ported | " + VERTEXAI_DIR + "/convert.rs | test_aiplatform_to_genai_content_preserves_function_i_ds |",
        ])


LEDGER_5A = (
    "# Parity ledger — sub-project 5a\n\n"
    "<!-- in-scope:\ninternal/compactionvalidate/validate_test.go\n"
    "server/adkrest/controllers/version_test.go\n"
    "internal/telemetry/functionaltest/telemetry_schema_test.go\n-->\n\n"
    + pc.TABLE_HEADER + "\n" + pc.TABLE_SEP + "\n"
)

VALIDATE_TARGET = "crates/gadk-engine/src/parity_tests/internal_compactionvalidate_validate.rs"
VERSION_TARGET = "crates/gadk-server/tests/server_adkrest_controllers_version.rs"
SCHEMA_DIR = "crates/gadk-engine/tests/internal_telemetry_functionaltest_telemetry_schema"


class FiveATest(unittest.TestCase):
    """5a: Go test files spread over two crates, in `src/parity_tests/` (internal packages), in
    `tests/` (public API) and in a test directory (`functionaltest`)."""

    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        d = pathlib.Path(self._tmp.name)
        self.go, self.repo = d / "go", d / "repo"
        for f, body in (
            ("internal/compactionvalidate/validate_test.go",
             "func TestAgainstAgentsRefusesAConfigNoAgentCanServe(t *testing.T) {}\n"),
            ("server/adkrest/controllers/version_test.go",
             "func TestVersionHandler(t *testing.T) {}\nfunc TestVersionIsPublic(t *testing.T) {}\n"),
            ("internal/telemetry/functionaltest/telemetry_schema_test.go",
             "func TestTelemetrySchema(t *testing.T) {}\n"),
        ):
            (self.go / f).parent.mkdir(parents=True, exist_ok=True)
            (self.go / f).write_text("package x\n" + body)
        for f in (VALIDATE_TARGET, VERSION_TARGET, SCHEMA_DIR + "/main.rs"):
            (self.repo / f).parent.mkdir(parents=True, exist_ok=True)
        (self.repo / VALIDATE_TARGET).write_text(
            "#[test]\nfn test_against_agents_refuses_a_config_no_agent_can_serve() {}\n")
        (self.repo / VERSION_TARGET).write_text(
            "#[tokio::test]\nasync fn test_version_handler() {}\n"
            "// parity: skipped TestVersionIsPublic: covered by the router\n")
        (self.repo / SCHEMA_DIR / "main.rs").write_text("#[test]\nfn test_telemetry_schema() {}\n")
        (self.repo / "docs/parity").mkdir(parents=True)
        self.ledger = self.repo / "docs/parity/5a.md"
        self.ledger.write_text(LEDGER_5A)

    def tearDown(self):
        self._tmp.cleanup()

    def run_cli(self, *args):
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            rc = pc.main(["--adk-go", str(self.go), "--repo", str(self.repo), *args])
        return rc, out.getvalue(), err.getvalue()

    def test_ledger_5a_is_registered(self):
        self.assertIn("5a", pc.GENERATED_LEDGERS)

    def test_header_only_ledger_fails_until_targets_hold_tests(self):
        (self.repo / VERSION_TARGET).write_text("//! Ports of ADK-Go `server/adkrest/controllers/version_test.go`.\n")
        rc, out, err = self.run_cli("--ledger", "5a")
        self.assertEqual((rc, out), (1, ""))
        self.assertIn(
            "server/adkrest/controllers/version_test.go TestVersionHandler: missing (no fn test_version_handler"
            " and no `// parity: skipped TestVersionHandler:` in " + VERSION_TARGET + ")", err)

    def test_targets_generate_and_check(self):
        self.assertEqual(self.run_cli("--ledger", "5a", "--count"), (0, "4\n", ""))
        self.assertEqual(self.run_cli("--ledger", "5a", "--generate")[0], 0)
        self.assertEqual(self.run_cli("--ledger", "5a"), (0, "", ""))
        rows = self.ledger.read_text().splitlines()[-4:]
        self.assertEqual(rows, [
            "| internal/compactionvalidate/validate_test.go | TestAgainstAgentsRefusesAConfigNoAgentCanServe"
            " | ported | " + VALIDATE_TARGET + " | test_against_agents_refuses_a_config_no_agent_can_serve |",
            "| server/adkrest/controllers/version_test.go | TestVersionHandler | ported | " + VERSION_TARGET
            + " | test_version_handler |",
            "| server/adkrest/controllers/version_test.go | TestVersionIsPublic | skipped | " + VERSION_TARGET
            + " | covered by the router |",
            "| internal/telemetry/functionaltest/telemetry_schema_test.go | TestTelemetrySchema | ported | "
            + SCHEMA_DIR + "/main.rs | test_telemetry_schema |",
        ])


LEDGER_5B = (
    "# Parity ledger — sub-project 5b\n\n"
    "<!-- in-scope:\nserver/adka2a/v2/parts_test.go\nagent/remoteagent/v2/auth_test.go\n"
    "server/adka2a/v2/example_test.go\n-->\n\n"
    "<!-- deferred:\nagent/remoteagent/a2a_agent_compat_test.go: out of scope, not ported: v1\n-->\n\n"
    + pc.TABLE_HEADER + "\n" + pc.TABLE_SEP + "\n"
)

PARTS_TARGET = "crates/gadk-a2a/tests/server_adka2a_v2_parts.rs"
AUTH_DIR = "crates/gadk-a2a/src/parity_tests/agent_remoteagent_v2_auth"


class FiveBTest(unittest.TestCase):
    """5b: one crate; a `tests/` target, an in-crate module directory split between two tasks'
    files (`auth_test.go`), an `Example`-only Go file without rows, and a v1 Go file kept out of
    scope through the deferred block."""

    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        d = pathlib.Path(self._tmp.name)
        self.go, self.repo = d / "go", d / "repo"
        for f, body in (
            ("server/adka2a/v2/parts_test.go",
             "func TestPartsTwoWayConversion(t *testing.T) {}\nfunc TestPartsSkip(t *testing.T) {}\n"),
            ("agent/remoteagent/v2/auth_test.go",
             "func TestMintGroupDedupes(t *testing.T) {}\nfunc TestAuthCoversTheCardFetchOnce(t *testing.T) {}\n"),
            ("server/adka2a/v2/example_test.go", "func Example() {}\n"),
            ("agent/remoteagent/a2a_agent_compat_test.go", "func TestCompat(t *testing.T) {}\n"),
        ):
            (self.go / f).parent.mkdir(parents=True, exist_ok=True)
            (self.go / f).write_text("package x\n" + body)
        (self.repo / PARTS_TARGET).parent.mkdir(parents=True, exist_ok=True)
        (self.repo / AUTH_DIR).mkdir(parents=True)
        (self.repo / PARTS_TARGET).write_text(
            "#[test]\nfn test_parts_two_way_conversion() {}\n"
            "// parity: skipped TestPartsSkip: no such API\n")
        (self.repo / AUTH_DIR / "mod.rs").write_text("mod agent;\nmod transport;\n")
        (self.repo / AUTH_DIR / "transport.rs").write_text("#[tokio::test]\nasync fn test_mint_group_dedupes() {}\n")
        (self.repo / AUTH_DIR / "agent.rs").write_text(
            "// parity: adapted: the card is fetched by gadk-a2a\n#[tokio::test]\n"
            "async fn test_auth_covers_the_card_fetch_once() {}\n")
        (self.repo / "docs/parity").mkdir(parents=True)
        self.ledger = self.repo / "docs/parity/5b.md"
        self.ledger.write_text(LEDGER_5B)

    def tearDown(self):
        self._tmp.cleanup()

    def run_cli(self, *args):
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            rc = pc.main(["--adk-go", str(self.go), "--repo", str(self.repo), *args])
        return rc, out.getvalue(), err.getvalue()

    def test_ledger_5b_is_registered(self):
        self.assertIn("5b", pc.GENERATED_LEDGERS)
        self.assertEqual(self.run_cli("--ledger", "5b", "--count"), (0, "4\n", ""))

    def test_stub_targets_report_missing_tests(self):
        (self.repo / AUTH_DIR / "agent.rs").write_text("//! Filled by a later task.\n")
        rc, out, err = self.run_cli("--ledger", "5b")
        self.assertEqual((rc, out), (1, ""))
        self.assertIn(
            "agent/remoteagent/v2/auth_test.go TestAuthCoversTheCardFetchOnce: missing (no fn"
            " test_auth_covers_the_card_fetch_once and no `// parity: skipped TestAuthCoversTheCardFetchOnce:`"
            " in " + AUTH_DIR + "/)", err)

    def test_the_out_of_scope_file_cannot_be_in_scope(self):
        self.ledger.write_text(LEDGER_5B.replace(
            "server/adka2a/v2/example_test.go\n", "server/adka2a/v2/example_test.go\n"
            "agent/remoteagent/a2a_agent_compat_test.go\n"))
        rc, _, err = self.run_cli("--ledger", "5b")
        self.assertEqual(rc, 1)
        self.assertIn("file is both in-scope and deferred: agent/remoteagent/a2a_agent_compat_test.go", err)

    def test_targets_generate_and_check(self):
        self.assertEqual(self.run_cli("--ledger", "5b", "--generate")[0], 0)
        self.assertEqual(self.run_cli("--ledger", "5b"), (0, "", ""))
        rows = self.ledger.read_text().splitlines()[-4:]
        self.assertEqual(rows, [
            "| server/adka2a/v2/parts_test.go | TestPartsTwoWayConversion | ported | " + PARTS_TARGET
            + " | test_parts_two_way_conversion |",
            "| server/adka2a/v2/parts_test.go | TestPartsSkip | skipped | " + PARTS_TARGET + " | no such API |",
            "| agent/remoteagent/v2/auth_test.go | TestMintGroupDedupes | ported | " + AUTH_DIR
            + "/transport.rs | test_mint_group_dedupes |",
            "| agent/remoteagent/v2/auth_test.go | TestAuthCoversTheCardFetchOnce | ported (adapted: the card"
            " is fetched by gadk-a2a) | " + AUTH_DIR + "/agent.rs | test_auth_covers_the_card_fetch_once |",
        ])


LEDGER_6 = (
    "# Parity ledger — sub-project 6\n\n"
    "<!-- in-scope:\ncmd/launcher/launcher_validate_test.go\ninternal/cli/util/text_helpers_test.go\n-->\n\n"
    "<!-- deferred:\ncmd/launcher/web/agentengine/agentengine_test.go: sub-project 5c\n-->\n\n"
    + pc.TABLE_HEADER + "\n" + pc.TABLE_SEP + "\n"
)

VALIDATE6_TARGET = "crates/gadk-launcher/tests/cmd_launcher_launcher_validate.rs"
TEXT_HELPERS_TARGET = "crates/gadk-launcher/src/parity_tests/internal_cli_util_text_helpers.rs"


class SixTest(unittest.TestCase):
    """6: one crate; a `tests/` target and an all-skipped `src/parity_tests/` target (6b)."""

    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        d = pathlib.Path(self._tmp.name)
        self.go, self.repo = d / "go", d / "repo"
        for f, body in (
            ("cmd/launcher/launcher_validate_test.go",
             "func TestConfigValidateRejectsUnusableCompaction(t *testing.T) {}\n"),
            ("internal/cli/util/text_helpers_test.go", "func TestValidateShellArgSafe(t *testing.T) {}\n"),
            ("cmd/launcher/web/agentengine/agentengine_test.go", "func TestAgentEngine(t *testing.T) {}\n"),
        ):
            (self.go / f).parent.mkdir(parents=True, exist_ok=True)
            (self.go / f).write_text("package x\n" + body)
        for target in (VALIDATE6_TARGET, TEXT_HELPERS_TARGET):
            (self.repo / target).parent.mkdir(parents=True, exist_ok=True)
        (self.repo / VALIDATE6_TARGET).write_text(
            "#[test]\nfn test_config_validate_rejects_unusable_compaction() {}\n")
        (self.repo / TEXT_HELPERS_TARGET).write_text(
            "// parity: skipped TestValidateShellArgSafe: cmd/adkgo helper (sub-project 6b)\n")
        (self.repo / "docs/parity").mkdir(parents=True)
        self.ledger = self.repo / "docs/parity/6.md"
        self.ledger.write_text(LEDGER_6)

    def tearDown(self):
        self._tmp.cleanup()

    def run_cli(self, *args):
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            rc = pc.main(["--adk-go", str(self.go), "--repo", str(self.repo), *args])
        return rc, out.getvalue(), err.getvalue()

    def test_ledger_6_is_registered(self):
        self.assertIn("6", pc.GENERATED_LEDGERS)
        self.assertEqual(self.run_cli("--ledger", "6", "--count"), (0, "2\n", ""))

    def test_targets_generate_and_check(self):
        self.assertEqual(self.run_cli("--ledger", "6", "--generate")[0], 0)
        self.assertEqual(self.run_cli("--ledger", "6"), (0, "", ""))
        rows = self.ledger.read_text().splitlines()[-2:]
        self.assertEqual(rows, [
            "| cmd/launcher/launcher_validate_test.go | TestConfigValidateRejectsUnusableCompaction | ported | "
            + VALIDATE6_TARGET + " | test_config_validate_rejects_unusable_compaction |",
            "| internal/cli/util/text_helpers_test.go | TestValidateShellArgSafe | skipped | " + TEXT_HELPERS_TARGET
            + " | cmd/adkgo helper (sub-project 6b) |",
        ])


LEDGER_5C = (
    "# Parity ledger — sub-project 5c\n\n"
    "<!-- in-scope:\nagentregistry/resolve_test.go\nserver/agentengine/handler_test.go\n-->\n\n"
    + pc.TABLE_HEADER + "\n" + pc.TABLE_SEP + "\n"
)

RESOLVE_TARGET = "crates/gadk-agentregistry/src/parity_tests/agentregistry_resolve.rs"
HANDLER_TARGET = "crates/gadk-server/src/parity_tests/server_agentengine_handler.rs"


class FiveCTest(unittest.TestCase):
    """5c: two crates' `src/parity_tests/` targets (the registry crate and gadk-server)."""

    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        d = pathlib.Path(self._tmp.name)
        self.go, self.repo = d / "go", d / "repo"
        for f, body in (
            ("agentregistry/resolve_test.go", "func TestCleanName(t *testing.T) {}\n"),
            ("server/agentengine/handler_test.go", "func TestQuerySSEWriteTimeout(t *testing.T) {}\n"),
        ):
            (self.go / f).parent.mkdir(parents=True, exist_ok=True)
            (self.go / f).write_text("package x\n" + body)
        for target in (RESOLVE_TARGET, HANDLER_TARGET):
            (self.repo / target).parent.mkdir(parents=True, exist_ok=True)
        (self.repo / RESOLVE_TARGET).write_text("#[test]\nfn test_clean_name() {}\n")
        (self.repo / HANDLER_TARGET).write_text(
            "// parity: adapted: a recording WriteDeadline\n#[tokio::test]\nasync fn test_query_sse_write_timeout() {}\n")
        (self.repo / "docs/parity").mkdir(parents=True)
        self.ledger = self.repo / "docs/parity/5c.md"
        self.ledger.write_text(LEDGER_5C)

    def tearDown(self):
        self._tmp.cleanup()

    def run_cli(self, *args):
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            rc = pc.main(["--adk-go", str(self.go), "--repo", str(self.repo), *args])
        return rc, out.getvalue(), err.getvalue()

    def test_ledger_5c_is_registered(self):
        self.assertIn("5c", pc.GENERATED_LEDGERS)
        self.assertEqual(self.run_cli("--ledger", "5c", "--count"), (0, "2\n", ""))

    def test_targets_generate_and_check(self):
        self.assertEqual(self.run_cli("--ledger", "5c", "--generate")[0], 0)
        self.assertEqual(self.run_cli("--ledger", "5c"), (0, "", ""))
        rows = self.ledger.read_text().splitlines()[-2:]
        self.assertEqual(rows, [
            "| agentregistry/resolve_test.go | TestCleanName | ported | " + RESOLVE_TARGET + " | test_clean_name |",
            "| server/agentengine/handler_test.go | TestQuerySSEWriteTimeout | ported (adapted: a recording"
            " WriteDeadline) | " + HANDLER_TARGET + " | test_query_sse_write_timeout |",
        ])


class FourATest(unittest.TestCase):
    """4a: a Go package whose test files land in two crates' `src/parity_tests/`."""

    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        d = pathlib.Path(self._tmp.name)
        self.go, self.repo = d / "go", d / "repo"
        (self.go / "session/database").mkdir(parents=True)
        (self.go / "session/database/compaction_test.go").write_text(
            "package database\nfunc TestACompactionCorrectionSortsAfterTheRecordItCorrects(t *testing.T) {}\n")
        (self.go / "session/database/session_test.go").write_text(
            "package database\nfunc TestLocalSessionEventsSnapshot(t *testing.T) {}\n")
        for target in (ENGINE_TARGET, SESSION_TARGET):
            (self.repo / target).parent.mkdir(parents=True, exist_ok=True)
        (self.repo / ENGINE_TARGET).write_text(
            "#[tokio::test]\nasync fn test_a_compaction_correction_sorts_after_the_record_it_corrects() {}\n")
        (self.repo / SESSION_TARGET).write_text("#[test]\nfn test_local_session_events_snapshot() {}\n")
        (self.repo / "docs/parity").mkdir(parents=True)
        self.ledger = self.repo / "docs/parity/4a.md"
        self.ledger.write_text(LEDGER_4A)

    def tearDown(self):
        self._tmp.cleanup()

    def run_cli(self, *args):
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            rc = pc.main(["--adk-go", str(self.go), "--repo", str(self.repo), *args])
        return rc, out.getvalue(), err.getvalue()

    def test_ledger_4a_is_registered(self):
        self.assertIn("4a", pc.GENERATED_LEDGERS)

    def test_targets_in_two_crates_generate_and_check(self):
        self.assertEqual(self.run_cli("--ledger", "4a", "--count"), (0, "2\n", ""))
        self.assertEqual(self.run_cli("--ledger", "4a", "--generate")[0], 0)
        self.assertEqual(self.run_cli("--ledger", "4a"), (0, "", ""))
        rows = self.ledger.read_text().splitlines()[-2:]
        self.assertEqual(rows, [
            "| session/database/compaction_test.go | TestACompactionCorrectionSortsAfterTheRecordItCorrects"
            " | ported | " + ENGINE_TARGET + " | test_a_compaction_correction_sorts_after_the_record_it_corrects |",
            "| session/database/session_test.go | TestLocalSessionEventsSnapshot | ported | "
            + SESSION_TARGET + " | test_local_session_events_snapshot |",
        ])


ADK_GO = pathlib.Path(os.environ.get("ADK_GO_DIR", HERE.parent.parent / "adk-go"))


@unittest.skipUnless((ADK_GO / "agent/agent_test.go").is_file(), "ADK-Go checkout not found")
class RealTreeTest(unittest.TestCase):
    def test_1b_count_is_739(self):
        in_scope, deferred = pc.parse_header(HERE.parent / "docs/parity/1b.md")
        self.assertEqual((len(in_scope), len(deferred)), (137, 0))
        self.assertEqual(pc.count(ADK_GO, in_scope), 739)
        self.assertEqual(pc.check_header(ADK_GO, in_scope, deferred, "1b"), [])
        self.assertEqual(deferred, {})

    def test_1b_live_count_is_31(self):
        in_scope, deferred = pc.parse_header(HERE.parent / "docs/parity/1b-live.md")
        self.assertEqual(in_scope, [
            "agent/workflowagents/sequentialagent/live_injection_test.go",
            "internal/llminternal/audio_cache_manager_test.go",
            "internal/llminternal/base_flow_live_test.go",
            "internal/llminternal/live_injection_test.go",
            "internal/llminternal/live_reconnect_test.go",
            "runner/live_runner_test.go",
        ])
        self.assertEqual(deferred, {})
        self.assertEqual(pc.count(ADK_GO, in_scope), 31)
        self.assertEqual(pc.check_header(ADK_GO, in_scope, deferred, "1b-live"), [])

    def test_1c_count_is_190(self):
        in_scope, deferred = pc.parse_header(HERE.parent / "docs/parity/1c.md")
        self.assertEqual(in_scope, [
            "agent/llmagent/llmagent_compaction_test.go",
            "agent/llmagent/llmagent_tail_retention_test.go",
            "internal/agent/compactionctx/compactionctx_test.go",
            "internal/compactioninternal/apply_test.go",
            "internal/compactioninternal/compactor_test.go",
            "internal/compactioninternal/summary_event_test.go",
            "internal/compactioninternal/tail_retention_test.go",
            "internal/compactioninternal/telemetry_test.go",
            "internal/compactioninternal/window_test.go",
            "internal/llminternal/compaction_concurrent_test.go",
            "internal/llminternal/compaction_mode_test.go",
            "internal/llminternal/compaction_processor_test.go",
            "internal/llminternal/contents_processor_compaction_test.go",
            "runner/compaction_test.go",
            "session/compaction/llm_summarizer_test.go",
        ])
        self.assertEqual(deferred, {
            # 5a: validate_test.go and compaction_integration_test.go moved to the 5a ledger.
            "cmd/launcher/console/compaction_test.go": "launcher (sub-project 6)",
            "session/vertexai/compaction_persistence_test.go": "Vertex AI session service (sub-project 4)",
        })
        self.assertEqual(pc.count(ADK_GO, in_scope), 190)
        self.assertEqual(pc.check_header(ADK_GO, in_scope, deferred, "1c"), [])

    def test_2a_count_is_148(self):
        in_scope, deferred = pc.parse_header(HERE.parent / "docs/parity/2a.md")
        self.assertEqual(in_scope, [
            "examples/toolsearch/main_test.go",
            "internal/ranksearch/bm25_test.go",
            "internal/ranksearch/ranksearch_test.go",
            "plugin/functioncallmodifier/integration_test.go",
            "plugin/functioncallmodifier/plugin_test.go",
            "plugin/retryandreflect/plugin_test.go",
            "tool/exampletool/tool_test.go",
            "tool/geminitool/enterprise_web_search_test.go",
            "tool/geminitool/tool_test.go",
            "tool/loadartifactstool/load_artifacts_tool_test.go",
            "tool/loadmemorytool/tool_test.go",
            "tool/preloadmemorytool/tool_test.go",
            "tool/skilltoolset/instruction_test.go",
            "tool/skilltoolset/internal/skilltool/tools_test.go",
            "tool/skilltoolset/skill/complete_preload_test.go",
            "tool/skilltoolset/skill/filesystem_source_test.go",
            "tool/skilltoolset/skill/frontmatter_preload_test.go",
            "tool/skilltoolset/skill/frontmatter_test.go",
            "tool/skilltoolset/skill/merged_source_test.go",
            "tool/skilltoolset/toolset_test.go",
            "tool/toolsearch/toolsearch_test.go",
        ])
        self.assertEqual(deferred, {})
        self.assertEqual(pc.count(ADK_GO, in_scope), 148)
        self.assertEqual(pc.check_header(ADK_GO, in_scope, deferred, "2a"), [])
        # tool/toolutils stays a 1b file.
        in_1b, _ = pc.parse_header(HERE.parent / "docs/parity/1b.md")
        self.assertIn("tool/toolutils/toolutils_test.go", in_1b)
        self.assertNotIn("tool/toolutils/toolutils_test.go", in_scope)

    def test_2b_count_is_203(self):
        in_scope, deferred = pc.parse_header(HERE.parent / "docs/parity/2b.md")
        self.assertEqual(in_scope, [
            "auth/credential_test.go",
            "auth/gcp/client_test.go",
            "auth/gcp/provider_internal_test.go",
            "auth/gcp/provider_test.go",
            "auth/gcp/redact_fuzz_test.go",
            "auth/gcp/redact_reference_test.go",
            "auth/gcp/unescape_test.go",
            "auth/providers_internal_test.go",
            "auth/providers_test.go",
            "auth/store_internal_test.go",
            "auth/store_test.go",
            "auth/transport_test.go",
            "tool/toolauth/auth_credential_test.go",
            "tool/toolauth/auth_schemes_test.go",
            "tool/toolauth/auth_tool_test.go",
            "tool/toolauth/redact_test.go",
        ])
        self.assertEqual(deferred, {})
        self.assertEqual(pc.count(ADK_GO, in_scope), 203)
        self.assertEqual(pc.check_header(ADK_GO, in_scope, deferred, "2b"), [])
        self.assertIn("FuzzRedact", pc.go_tests(ADK_GO, "auth/gcp/redact_fuzz_test.go"))

    def test_2c_count_is_47(self):
        in_scope, deferred = pc.parse_header(HERE.parent / "docs/parity/2c.md")
        self.assertEqual(in_scope, [
            "tool/mcptoolset/auth_session_test.go",
            "tool/mcptoolset/client_cancellation_test.go",
            "tool/mcptoolset/client_test.go",
            "tool/mcptoolset/set_test.go",
            "tool/mcptoolset/tool_test.go",
            "tool/mcptoolset/tracking_test.go",
            "tool/mcptoolset/transport_config_test.go",
        ])
        self.assertEqual(deferred, {})
        self.assertEqual(pc.count(ADK_GO, in_scope), 47)
        self.assertEqual(pc.check_header(ADK_GO, in_scope, deferred, "2c"), [])
        for f in in_scope:
            self.assertEqual([t for t in pc.go_tests(ADK_GO, f) if t.startswith("Fuzz")], [], f)

    def test_3a_count_is_28(self):
        in_scope, deferred = pc.parse_header(HERE.parent / "docs/parity/3a.md")
        self.assertEqual(in_scope, [
            "model/anthropicmodel/anthropicmodel_test.go",
            "model/apigee/apigee_test.go",
        ])
        self.assertEqual(deferred, {})
        self.assertEqual(pc.count(ADK_GO, in_scope), 28)
        self.assertEqual(pc.check_header(ADK_GO, in_scope, deferred, "3a"), [])
        self.assertEqual(len(pc.go_tests(ADK_GO, "model/anthropicmodel/anthropicmodel_test.go")), 21)

    def test_3b_count_is_217(self):
        in_scope, deferred = pc.parse_header(HERE.parent / "docs/parity/3b.md")
        self.assertEqual(len(in_scope), 19)
        self.assertEqual(deferred, {})
        self.assertEqual(pc.count(ADK_GO, in_scope), 217)
        self.assertEqual(pc.check_header(ADK_GO, in_scope, deferred, "3b"), [])
        fuzz = [t for f in in_scope for t in pc.go_tests(ADK_GO, f) if t.startswith("Fuzz")]
        self.assertEqual(fuzz, ["FuzzFailedResponseError", "FuzzClipServerText"])

    def test_earlier_ledgers_have_no_fuzz_tests(self):
        # Widening GO_TEST to `Fuzz*` must not change any 1a-2a ledger.
        for name in ("1a", "1b", "1b-live", "1c", "2a"):
            in_scope, _ = pc.parse_header(HERE.parent / f"docs/parity/{name}.md")
            for f in in_scope:
                fuzz = [t for t in pc.go_tests(ADK_GO, f) if t.startswith("Fuzz")]
                self.assertEqual(fuzz, [], f"{name}: {f}")

    def test_1a_header_has_13_files(self):
        in_scope, deferred = pc.parse_header(HERE.parent / "docs/parity/1a.md")
        self.assertEqual((len(in_scope), deferred), (13, {}))

    def test_4a_count_is_15(self):
        in_scope, deferred = pc.parse_header(HERE.parent / "docs/parity/4a.md")
        self.assertEqual(in_scope, [
            "session/database/compaction_test.go",
            "session/database/service_test.go",
            "session/database/session_test.go",
        ])
        self.assertEqual(deferred, {})
        self.assertEqual(pc.count(ADK_GO, in_scope), 15)
        self.assertEqual(pc.check_header(ADK_GO, in_scope, deferred, "4a"), [])

    def test_4b_count_is_16(self):
        in_scope, deferred = pc.parse_header(HERE.parent / "docs/parity/4b.md")
        self.assertEqual(in_scope, [
            "artifact/gcsartifact/gcs_client_test.go",
            "artifact/gcsartifact/gcs_test.go",
        ])
        self.assertEqual(deferred, {})
        self.assertEqual(pc.count(ADK_GO, in_scope), 16)
        self.assertEqual(pc.check_header(ADK_GO, in_scope, deferred, "4b"), [])

    def test_4c_count_is_35(self):
        in_scope, deferred = pc.parse_header(HERE.parent / "docs/parity/4c.md")
        self.assertEqual(in_scope, [
            "session/vertexai/append_event_temp_state_test.go",
            "session/vertexai/compaction_persistence_test.go",
            "session/vertexai/delete_ownership_test.go",
            "session/vertexai/filter_test.go",
            "session/vertexai/notfound_test.go",
            "session/vertexai/raw_event_crossruntime_test.go",
            "session/vertexai/service_test.go",
            "session/vertexai/session_test.go",
            "session/vertexai/vertexai_test.go",
        ])
        self.assertEqual(deferred, {})
        self.assertEqual(pc.count(ADK_GO, in_scope), 35)
        self.assertEqual(pc.check_header(ADK_GO, in_scope, deferred, "4c"), [])

    def test_5a_count_is_246(self):
        in_scope, deferred = pc.parse_header(HERE.parent / "docs/parity/5a.md")
        self.assertEqual(len(in_scope), 42)
        self.assertNotIn("agent/loader_test.go", in_scope)
        self.assertEqual(deferred, {})
        self.assertEqual(pc.count(ADK_GO, in_scope), 246)
        self.assertEqual(pc.check_header(ADK_GO, in_scope, deferred, "5a"), [])

    def test_5b_count_is_145(self):
        in_scope, deferred = pc.parse_header(HERE.parent / "docs/parity/5b.md")
        self.assertEqual(len(in_scope), 16)
        self.assertEqual(sorted({f.split("/")[0] for f in in_scope}), ["agent", "internal", "server"])
        self.assertEqual(list(deferred), ["agent/remoteagent/a2a_agent_compat_test.go"])
        self.assertEqual(pc.count(ADK_GO, in_scope), 145)
        self.assertEqual(pc.check_header(ADK_GO, in_scope, deferred, "5b"), [])

    def test_5c_count_is_62(self):
        in_scope, deferred = pc.parse_header(HERE.parent / "docs/parity/5c.md")
        self.assertEqual(len(in_scope), 11)
        self.assertEqual(sorted({f.split("/")[0] for f in in_scope}), ["agentregistry", "server"])
        self.assertEqual(deferred, {})
        self.assertEqual(pc.count(ADK_GO, in_scope), 62)
        self.assertEqual(pc.check_header(ADK_GO, in_scope, deferred, "5c"), [])

    def test_6_count_is_94(self):
        in_scope, deferred = pc.parse_header(HERE.parent / "docs/parity/6.md")
        self.assertEqual(len(in_scope), 14)
        self.assertEqual(sorted({f.split("/")[0] for f in in_scope}), ["cmd", "internal"])
        self.assertEqual(pc.count(ADK_GO, in_scope), 94)
        self.assertEqual(pc.check_header(ADK_GO, in_scope, deferred, "6"), [])


if __name__ == "__main__":
    unittest.main()
