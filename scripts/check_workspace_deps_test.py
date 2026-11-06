import importlib.machinery, importlib.util, pathlib, shutil, tempfile, unittest

HERE = pathlib.Path(__file__).resolve().parent
loader = importlib.machinery.SourceFileLoader("check_workspace_deps", str(HERE / "check-workspace-deps.py"))
spec = importlib.util.spec_from_loader("check_workspace_deps", loader)
cwd = importlib.util.module_from_spec(spec)
loader.exec_module(cwd)


class EdgeTest(unittest.TestCase):
    """Runs the check on a temp copy of every crates/*/Cargo.toml."""

    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.repo = pathlib.Path(self._tmp.name)
        for m in sorted((HERE.parent / "crates").glob("*/Cargo.toml")):
            dst = self.repo / m.relative_to(HERE.parent)
            dst.parent.mkdir(parents=True)
            shutil.copyfile(m, dst)

    def tearDown(self):
        self._tmp.cleanup()

    def edit(self, crate, old, new):
        p = self.repo / "crates" / crate / "Cargo.toml"
        text = p.read_text()
        self.assertIn(old, text)
        p.write_text(text.replace(old, new, 1))

    def test_real_manifests_pass(self):
        self.assertEqual(cwd.check(self.repo), [])

    def test_agent_dev_dependency_on_engine_is_rejected(self):
        self.edit("gadk-agent", "[dev-dependencies]\n", "[dev-dependencies]\ngadk-engine = { workspace = true }\n")
        errs = cwd.check(self.repo)
        self.assertTrue(any("only gadk may depend on gadk-engine" in e for e in errs), errs)

    def test_disallowed_normal_edge_is_rejected(self):
        self.edit("gadk-artifact", "[dependencies]\n", "[dependencies]\ngadk-session = { workspace = true }\n")
        errs = cwd.check(self.repo)
        self.assertTrue(any("gadk-artifact -> gadk-session is not allowed" in e for e in errs), errs)

    def test_missing_required_edge_is_rejected(self):
        self.edit("gadk-memory", "gadk-session = { workspace = true }\n", "")
        errs = cwd.check(self.repo)
        self.assertTrue(any("gadk-memory -> gadk-session is missing" in e for e in errs), errs)

    def test_engine_gemini_must_be_optional(self):
        self.edit("gadk-engine", "gadk-gemini = { workspace = true, optional = true }", "gadk-gemini = { workspace = true }")
        errs = cwd.check(self.repo)
        self.assertTrue(any("gadk-engine -> gadk-gemini must be optional = true" in e for e in errs), errs)

    def test_dev_dependency_cycle_is_rejected(self):
        self.edit("gadk-session", "[dev-dependencies]\n", "[dev-dependencies]\ngadk-memory = { workspace = true }\n")
        errs = cwd.check(self.repo)
        self.assertTrue(any("gadk-session -> gadk-memory creates a cycle" in e for e in errs), errs)


if __name__ == "__main__":
    unittest.main()
