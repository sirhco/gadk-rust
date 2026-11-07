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
        }
        for go, rust in cases.items():
            self.assertEqual(pc.go_test_to_rust(go), rust, go)

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
            "internal/compactionvalidate/validate_test.go": "launcher and server validation (sub-projects 5 and 6)",
            "cmd/launcher/console/compaction_test.go": "launcher (sub-project 6)",
            "server/adkrest/compaction_integration_test.go": "REST server (sub-project 5)",
            "session/database/compaction_test.go": "database session service (sub-project 4)",
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

    def test_1a_header_has_13_files(self):
        in_scope, deferred = pc.parse_header(HERE.parent / "docs/parity/1a.md")
        self.assertEqual((len(in_scope), deferred), (13, {}))


if __name__ == "__main__":
    unittest.main()
