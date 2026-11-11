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
        self.assertTrue(any("only gadk, gadk-tools, gadk-server, gadk-a2a and gadk-launcher may depend on gadk-engine" in e for e in errs), errs)

    def test_tools_dev_dependency_on_engine_is_allowed(self):
        text = (self.repo / "crates/gadk-tools/Cargo.toml").read_text()
        dev = text.split("[dev-dependencies]\n", 1)[1]
        self.assertIn('gadk-engine = { workspace = true, features = ["testutil"] }', dev)
        self.assertEqual(cwd.check(self.repo), [])

    def test_engine_dev_dependency_on_tools_is_rejected(self):
        self.edit("gadk-engine", "[dev-dependencies]\n", "[dev-dependencies]\ngadk-tools = { workspace = true }\n")
        errs = cwd.check(self.repo)
        self.assertTrue(any("only gadk may depend on gadk-tools" in e for e in errs), errs)

    def test_agent_normal_dependency_on_tools_is_rejected(self):
        self.edit("gadk-agent", "[dependencies]\n", "[dependencies]\ngadk-tools = { workspace = true }\n")
        errs = cwd.check(self.repo)
        self.assertTrue(any("gadk-agent -> gadk-tools is not allowed" in e for e in errs), errs)

    def test_tools_missing_required_edge_is_rejected(self):
        self.edit("gadk-tools", "gadk-platform = { workspace = true }\n", "")
        errs = cwd.check(self.repo)
        self.assertTrue(any("gadk-tools -> gadk-platform is missing" in e for e in errs), errs)

    def test_auth_edges_are_exactly_agent_and_platform(self):
        self.assertEqual(cwd.ALLOWED["gadk-auth"], {"gadk-agent", "gadk-platform"})
        self.assertIn("gadk-auth", cwd.ALLOWED["gadk"])
        # 2c: gadk-mcp applies credentials to MCP requests; 5b: gadk-a2a applies them to A2A calls.
        self.assertEqual([c for c, deps in cwd.ALLOWED.items() if "gadk-auth" in deps],
                         ["gadk-mcp", "gadk-a2a", "gadk"])

    def test_auth_normal_dependency_on_engine_is_rejected(self):
        self.edit("gadk-auth", "[dependencies]\n", "[dependencies]\ngadk-engine = { workspace = true }\n")
        errs = cwd.check(self.repo)
        self.assertTrue(any("gadk-auth -> gadk-engine is not allowed" in e for e in errs), errs)

    def test_tools_normal_dependency_on_auth_is_rejected(self):
        self.edit("gadk-tools", "[dependencies]\n", "[dependencies]\ngadk-auth = { workspace = true }\n")
        errs = cwd.check(self.repo)
        self.assertTrue(any("gadk-tools -> gadk-auth is not allowed" in e for e in errs), errs)

    def test_auth_missing_required_edge_is_rejected(self):
        self.edit("gadk-auth", "gadk-platform = { workspace = true }\n", "")
        errs = cwd.check(self.repo)
        self.assertTrue(any("gadk-auth -> gadk-platform is missing" in e for e in errs), errs)

    def test_facade_missing_auth_edge_is_rejected(self):
        self.edit("gadk", "gadk-auth = { workspace = true }\n", "")
        errs = cwd.check(self.repo)
        self.assertTrue(any("gadk -> gadk-auth is missing" in e for e in errs), errs)

    def test_mcp_edges_are_exactly_agent_auth_genai_platform(self):
        self.assertEqual(cwd.ALLOWED["gadk-mcp"], {"gadk-agent", "gadk-auth", "gadk-genai", "gadk-platform"})
        self.assertEqual([c for c, deps in cwd.ALLOWED.items() if "gadk-mcp" in deps], ["gadk"])
        self.assertIn("gadk-platform", cwd.ALLOWED["gadk-gemini"])

    def test_mcp_normal_dependency_on_engine_is_rejected(self):
        self.edit("gadk-mcp", "[dependencies]\n", "[dependencies]\ngadk-engine = { workspace = true }\n")
        errs = cwd.check(self.repo)
        self.assertTrue(any("gadk-mcp -> gadk-engine is not allowed" in e for e in errs), errs)

    def test_mcp_dev_dependency_on_tools_is_rejected(self):
        self.edit("gadk-mcp", "[dev-dependencies]\n", "[dev-dependencies]\ngadk-tools = { workspace = true }\n")
        errs = cwd.check(self.repo)
        self.assertTrue(any("only gadk may depend on gadk-tools" in e for e in errs), errs)

    def test_facade_missing_mcp_edge_is_rejected(self):
        self.edit("gadk", "gadk-mcp = { workspace = true, optional = true }\n", "")
        errs = cwd.check(self.repo)
        self.assertTrue(any("gadk -> gadk-mcp is missing" in e for e in errs), errs)

    def test_anthropic_edges_are_exactly_genai_model_platform(self):
        self.assertEqual(cwd.ALLOWED["gadk-anthropic"], {"gadk-genai", "gadk-model", "gadk-platform"})
        self.assertEqual([c for c, deps in cwd.ALLOWED.items() if "gadk-anthropic" in deps], ["gadk"])

    def test_anthropic_normal_dependency_on_gemini_is_rejected(self):
        self.edit("gadk-anthropic", "[dependencies]\n", "[dependencies]\ngadk-gemini = { workspace = true }\n")
        errs = cwd.check(self.repo)
        self.assertTrue(any("gadk-anthropic -> gadk-gemini is not allowed" in e for e in errs), errs)

    def test_facade_missing_anthropic_edge_is_rejected(self):
        self.edit("gadk", "gadk-anthropic = { workspace = true, optional = true }\n", "")
        errs = cwd.check(self.repo)
        self.assertTrue(any("gadk -> gadk-anthropic is missing" in e for e in errs), errs)

    def test_openai_edges_are_exactly_genai_model_platform(self):
        self.assertEqual(cwd.ALLOWED["gadk-openai"], {"gadk-genai", "gadk-model", "gadk-platform"})
        self.assertEqual([c for c, deps in cwd.ALLOWED.items() if "gadk-openai" in deps], ["gadk"])

    def test_openai_normal_dependency_on_anthropic_is_rejected(self):
        self.edit("gadk-openai", "[dependencies]\n", "[dependencies]\ngadk-anthropic = { workspace = true }\n")
        errs = cwd.check(self.repo)
        self.assertTrue(any("gadk-openai -> gadk-anthropic is not allowed" in e for e in errs), errs)

    def test_facade_missing_openai_edge_is_rejected(self):
        self.edit("gadk", "gadk-openai = { workspace = true, optional = true }\n", "")
        errs = cwd.check(self.repo)
        self.assertTrue(any("gadk -> gadk-openai is missing" in e for e in errs), errs)

    def test_dev_dependency_on_openai_outside_the_facade_is_rejected(self):
        self.edit("gadk-anthropic", "[dev-dependencies]\n", "[dev-dependencies]\ngadk-openai = { workspace = true }\n")
        errs = cwd.check(self.repo)
        self.assertTrue(any("only gadk may depend on gadk-openai (dev-dependency)" in e for e in errs), errs)

    def test_session_database_edges_are_exactly_session_model_platform(self):
        self.assertEqual(cwd.ALLOWED["gadk-session-database"], {"gadk-session", "gadk-model", "gadk-platform"})
        self.assertEqual([c for c, deps in cwd.ALLOWED.items() if "gadk-session-database" in deps], ["gadk"])

    def test_engine_dev_dependency_on_session_database_is_allowed(self):
        self.assertFalse(cwd.reaches("gadk-session-database", "gadk-engine"))

    def test_artifact_gcs_edges_are_exactly_artifact_genai_model_platform(self):
        self.assertEqual(cwd.ALLOWED["gadk-artifact-gcs"],
                         {"gadk-artifact", "gadk-genai", "gadk-model", "gadk-platform"})
        self.assertEqual([c for c, deps in cwd.ALLOWED.items() if "gadk-artifact-gcs" in deps], ["gadk"])

    def test_vertexai_edges_are_exactly_session_memory_genai_model_platform(self):
        self.assertEqual(cwd.ALLOWED["gadk-vertexai"],
                         {"gadk-session", "gadk-memory", "gadk-genai", "gadk-model", "gadk-platform"})
        self.assertEqual([c for c, deps in cwd.ALLOWED.items() if "gadk-vertexai" in deps], ["gadk"])

    def test_server_edges_are_exactly_the_engine_stack(self):
        self.assertEqual(cwd.ALLOWED["gadk-server"],
                         {"gadk-agent", "gadk-engine", "gadk-session", "gadk-memory", "gadk-artifact",
                          "gadk-genai", "gadk-model", "gadk-platform"})
        # 5b: gadk-a2a's optional `gadk-server` feature reads the authn `Caller`; 6: the launcher's
        # `web` feature serves the REST API.
        self.assertEqual([c for c, deps in cwd.ALLOWED.items() if "gadk-server" in deps],
                         ["gadk-a2a", "gadk-launcher", "gadk"])
        self.assertIn("gadk-server", cwd.ENGINE_DEPENDENTS)

    def test_server_dev_dependency_on_engine_is_allowed(self):
        text = (self.repo / "crates/gadk-server/Cargo.toml").read_text()
        dev = text.split("[dev-dependencies]\n", 1)[1]
        self.assertIn('gadk-engine = { workspace = true, features = ["testutil"] }', dev)
        self.assertEqual(cwd.check(self.repo), [])

    def test_server_normal_dependency_on_tools_is_rejected(self):
        self.edit("gadk-server", "[dependencies]\n", "[dependencies]\ngadk-tools = { workspace = true }\n")
        errs = cwd.check(self.repo)
        self.assertTrue(any("gadk-server -> gadk-tools is not allowed" in e for e in errs), errs)

    def test_facade_missing_server_edge_is_rejected(self):
        self.edit("gadk", "gadk-server = { workspace = true, optional = true }\n", "")
        errs = cwd.check(self.repo)
        self.assertTrue(any("gadk -> gadk-server is missing" in e for e in errs), errs)

    def test_a2a_edges_are_exactly_the_engine_auth_stack_plus_optional_server(self):
        self.assertEqual(cwd.ALLOWED["gadk-a2a"],
                         {"gadk-agent", "gadk-engine", "gadk-session", "gadk-genai", "gadk-model",
                          "gadk-auth", "gadk-platform", "gadk-server"})
        # 6: the launcher's `a2a` feature serves A2A.
        self.assertEqual([c for c, deps in cwd.ALLOWED.items() if "gadk-a2a" in deps], ["gadk-launcher", "gadk"])
        self.assertIn("gadk-a2a", cwd.ENGINE_DEPENDENTS)
        self.assertIn(("gadk-a2a", "gadk-server"), cwd.OPTIONAL)

    def test_a2a_dev_dependency_on_engine_is_allowed(self):
        text = (self.repo / "crates/gadk-a2a/Cargo.toml").read_text()
        dev = text.split("[dev-dependencies]\n", 1)[1]
        self.assertIn('gadk-engine = { workspace = true, features = ["testutil"] }', dev)
        self.assertEqual(cwd.check(self.repo), [])

    def test_a2a_server_edge_must_be_optional(self):
        self.edit("gadk-a2a", "gadk-server = { workspace = true, optional = true }", "gadk-server = { workspace = true }")
        errs = cwd.check(self.repo)
        self.assertTrue(any("gadk-a2a -> gadk-server must be optional = true" in e for e in errs), errs)

    def test_a2a_normal_dependency_on_tools_is_rejected(self):
        self.edit("gadk-a2a", "[dependencies]\n", "[dependencies]\ngadk-tools = { workspace = true }\n")
        errs = cwd.check(self.repo)
        self.assertTrue(any("gadk-a2a -> gadk-tools is not allowed" in e for e in errs), errs)

    def test_facade_missing_a2a_edge_is_rejected(self):
        self.edit("gadk", "gadk-a2a = { workspace = true, optional = true }\n", "")
        errs = cwd.check(self.repo)
        self.assertTrue(any("gadk -> gadk-a2a is missing" in e for e in errs), errs)

    def test_launcher_edges_are_the_engine_stack_plus_optional_server_and_a2a(self):
        self.assertEqual(cwd.ALLOWED["gadk-launcher"],
                         {"gadk-agent", "gadk-engine", "gadk-session", "gadk-artifact", "gadk-memory",
                          "gadk-genai", "gadk-platform", "gadk-server", "gadk-a2a"})
        self.assertEqual([c for c, deps in cwd.ALLOWED.items() if "gadk-launcher" in deps], ["gadk"])
        self.assertIn("gadk-launcher", cwd.ENGINE_DEPENDENTS)
        self.assertIn(("gadk-launcher", "gadk-server"), cwd.OPTIONAL)
        self.assertIn(("gadk-launcher", "gadk-a2a"), cwd.OPTIONAL)

    def test_launcher_server_edge_must_be_optional(self):
        self.edit("gadk-launcher", "gadk-server = { workspace = true, optional = true }",
                  "gadk-server = { workspace = true }")
        errs = cwd.check(self.repo)
        self.assertTrue(any("gadk-launcher -> gadk-server must be optional = true" in e for e in errs), errs)

    def test_launcher_a2a_edge_must_be_optional(self):
        self.edit("gadk-launcher", 'gadk-a2a = { workspace = true, optional = true, features = ["server"] }',
                  'gadk-a2a = { workspace = true, features = ["server"] }')
        errs = cwd.check(self.repo)
        self.assertTrue(any("gadk-launcher -> gadk-a2a must be optional = true" in e for e in errs), errs)

    def test_launcher_dev_dependency_on_engine_is_allowed(self):
        text = (self.repo / "crates/gadk-launcher/Cargo.toml").read_text()
        dev = text.split("[dev-dependencies]\n", 1)[1]
        self.assertIn('gadk-engine = { workspace = true, features = ["testutil"] }', dev)
        self.assertEqual(cwd.check(self.repo), [])

    def test_launcher_normal_dependency_on_tools_is_rejected(self):
        self.edit("gadk-launcher", "[dependencies]\n", "[dependencies]\ngadk-tools = { workspace = true }\n")
        errs = cwd.check(self.repo)
        self.assertTrue(any("gadk-launcher -> gadk-tools is not allowed" in e for e in errs), errs)

    def test_facade_missing_launcher_edge_is_rejected(self):
        self.edit("gadk", "gadk-launcher = { workspace = true, optional = true }\n", "")
        errs = cwd.check(self.repo)
        self.assertTrue(any("gadk -> gadk-launcher is missing" in e for e in errs), errs)

    def test_model_edges_are_unchanged_by_gcp_auth(self):
        self.assertEqual(cwd.ALLOWED["gadk-model"], {"gadk-genai"})

    def test_genai_edge_is_exactly_platform(self):
        self.assertEqual(cwd.ALLOWED["gadk-genai"], {"gadk-platform"})
        self.assertEqual(cwd.ALLOWED["gadk-platform"], set())

    def test_genai_missing_platform_edge_is_rejected(self):
        self.edit("gadk-genai", "gadk-platform = { workspace = true }\n", "")
        errs = cwd.check(self.repo)
        self.assertTrue(any("gadk-genai -> gadk-platform is missing" in e for e in errs), errs)

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
