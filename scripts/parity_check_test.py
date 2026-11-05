import importlib.machinery, importlib.util, pathlib, tempfile, unittest, os

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


if __name__ == "__main__":
    unittest.main()
