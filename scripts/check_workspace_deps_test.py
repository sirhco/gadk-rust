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
        self.assertTrue(any("only gadk, gadk-tools, gadk-server, gadk-a2a, gadk-launcher, gadk-agentregistry, gadk-telemetry, gadk-configurable, gadk-agentanalytics, gadk-prompts, gadk-eval and gadk-bigquery may depend on gadk-engine" in e for e in errs), errs)

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
        # 2c: gadk-mcp applies credentials to MCP requests; 5b: gadk-a2a applies them to A2A calls;
        # 5c: gadk-agentregistry authenticates registry calls with ADC.
        self.assertEqual([c for c, deps in cwd.ALLOWED.items() if "gadk-auth" in deps],
                         ["gadk-mcp", "gadk-a2a", "gadk-agentregistry", "gadk"])

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
        # 5c: the registry's `mcp` feature builds MCP toolsets; 6c-2: the `McpToolset` factory.
        self.assertEqual([c for c, deps in cwd.ALLOWED.items() if "gadk-mcp" in deps],
                         ["gadk-agentregistry", "gadk-configurable", "gadk"])
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

    def test_vertexai_edges_are_exactly_session_memory_genai_model_platform_tools_agent(self):
        # 8b: the Example Store provider implements gadk_tools::example::Provider.
        self.assertEqual(cwd.ALLOWED["gadk-vertexai"],
                         {"gadk-session", "gadk-memory", "gadk-genai", "gadk-model", "gadk-platform",
                          "gadk-tools", "gadk-agent"})
        # 6b: the deploy CLI uses the Reasoning Engine client.
        self.assertEqual([c for c, deps in cwd.ALLOWED.items() if "gadk-vertexai" in deps],
                         ["gadk-cli", "gadk"])

    def test_server_edges_are_exactly_the_engine_stack(self):
        self.assertEqual(cwd.ALLOWED["gadk-server"],
                         {"gadk-agent", "gadk-engine", "gadk-session", "gadk-memory", "gadk-artifact",
                          "gadk-genai", "gadk-model", "gadk-platform"})
        # 5b: gadk-a2a's optional `gadk-server` feature reads the authn `Caller`; 6: the launcher's
        # `web` feature serves the REST API.
        # 6b: the deploy CLI lists the Agent Engine class methods (feature `agentengine`).
        self.assertEqual([c for c, deps in cwd.ALLOWED.items() if "gadk-server" in deps],
                         ["gadk-a2a", "gadk-launcher", "gadk-cli", "gadk"])
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
        # 6: the launcher's `a2a` feature serves A2A; 5c: the registry's `a2a` feature builds remote agents.
        self.assertEqual([c for c, deps in cwd.ALLOWED.items() if "gadk-a2a" in deps],
                         ["gadk-launcher", "gadk-agentregistry", "gadk"])
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
        self.edit("gadk", 'gadk-a2a = { workspace = true, optional = true, features = ["server", "client"] }\n', "")
        errs = cwd.check(self.repo)
        self.assertTrue(any("gadk -> gadk-a2a is missing" in e for e in errs), errs)

    def test_launcher_edges_are_the_engine_stack_plus_optional_server_and_a2a(self):
        self.assertEqual(cwd.ALLOWED["gadk-launcher"],
                         {"gadk-agent", "gadk-engine", "gadk-session", "gadk-artifact", "gadk-memory",
                          "gadk-genai", "gadk-model", "gadk-platform", "gadk-server", "gadk-a2a",
                      "gadk-telemetry"})
        # 6c-2: `adkcli` runs the full launcher.
        self.assertEqual([c for c, deps in cwd.ALLOWED.items() if "gadk-launcher" in deps],
                         ["gadk-configurable", "gadk"])
        self.assertIn("gadk-launcher", cwd.ENGINE_DEPENDENTS)
        self.assertIn(("gadk-launcher", "gadk-server"), cwd.OPTIONAL)
        self.assertIn(("gadk-launcher", "gadk-a2a"), cwd.OPTIONAL)

    def test_launcher_server_edge_must_be_optional(self):
        self.edit("gadk-launcher", "gadk-server = { workspace = true, optional = true }",
                  "gadk-server = { workspace = true }")
        errs = cwd.check(self.repo)
        self.assertTrue(any("gadk-launcher -> gadk-server must be optional = true" in e for e in errs), errs)

    def test_launcher_a2a_edge_must_be_optional(self):
        self.edit("gadk-launcher", 'gadk-a2a = { workspace = true, optional = true, features = ["server", "client"] }',
                  'gadk-a2a = { workspace = true, features = ["server"] }')
        errs = cwd.check(self.repo)
        self.assertTrue(any("gadk-launcher -> gadk-a2a must be optional = true" in e for e in errs), errs)

    def test_launcher_dev_dependency_on_engine_is_allowed(self):
        text = (self.repo / "crates/gadk-launcher/Cargo.toml").read_text()
        dev = text.split("[dev-dependencies]\n", 1)[1]
        self.assertIn('gadk-engine = { workspace = true, features = ["testutil"] }', dev)
        self.assertEqual(cwd.check(self.repo), [])

    def test_agentregistry_edges_are_agent_auth_platform_plus_optional_a2a_and_mcp(self):
        self.assertEqual(cwd.ALLOWED["gadk-agentregistry"],
                         {"gadk-agent", "gadk-auth", "gadk-platform", "gadk-a2a", "gadk-mcp"})
        # Only the façade depends on it (feature `agentregistry`).
        self.assertEqual([c for c, deps in cwd.ALLOWED.items() if "gadk-agentregistry" in deps],
                         ["gadk"])
        self.assertIn("gadk-agentregistry", cwd.ENGINE_DEPENDENTS)
        self.assertIn(("gadk-agentregistry", "gadk-a2a"), cwd.OPTIONAL)
        self.assertIn(("gadk-agentregistry", "gadk-mcp"), cwd.OPTIONAL)

    def test_agentregistry_a2a_edge_must_be_optional(self):
        self.edit("gadk-agentregistry",
                  'gadk-a2a = { workspace = true, optional = true, features = ["client"] }',
                  'gadk-a2a = { workspace = true, features = ["client"] }')
        errs = cwd.check(self.repo)
        self.assertTrue(any("gadk-agentregistry -> gadk-a2a must be optional = true" in e for e in errs), errs)

    def test_agentregistry_normal_dependency_on_engine_is_rejected(self):
        self.edit("gadk-agentregistry", "[dependencies]\n", "[dependencies]\ngadk-engine = { workspace = true }\n")
        errs = cwd.check(self.repo)
        self.assertTrue(any("gadk-agentregistry -> gadk-engine is not allowed" in e for e in errs), errs)

    def test_agentregistry_dev_dependency_on_engine_is_allowed(self):
        text = (self.repo / "crates/gadk-agentregistry/Cargo.toml").read_text()
        dev = text.split("[dev-dependencies]\n", 1)[1]
        self.assertIn('gadk-engine = { workspace = true, features = ["testutil"] }', dev)
        self.assertEqual(cwd.check(self.repo), [])

    def test_telemetry_edges_are_exactly_engine_model_platform(self):
        self.assertEqual(cwd.ALLOWED["gadk-telemetry"], {"gadk-engine", "gadk-model", "gadk-platform"})
        self.assertIn("gadk-telemetry", cwd.ENGINE_DEPENDENTS)

    def test_telemetry_normal_dependency_on_server_is_rejected(self):
        self.edit("gadk-telemetry", "[dependencies]\n", "[dependencies]\ngadk-server = { workspace = true }\n")
        errs = cwd.check(self.repo)
        self.assertTrue(any("gadk-telemetry -> gadk-server is not allowed" in e for e in errs), errs)

    def test_telemetry_missing_engine_edge_is_rejected(self):
        self.edit("gadk-telemetry", "[dependencies]\ngadk-engine = { workspace = true }\n", "[dependencies]\n")
        errs = cwd.check(self.repo)
        self.assertTrue(any("gadk-telemetry -> gadk-engine is missing" in e for e in errs), errs)

    def test_telemetry_dev_dependency_on_server_is_allowed(self):
        text = (self.repo / "crates/gadk-telemetry/Cargo.toml").read_text()
        dev = text.split("[dev-dependencies]\n", 1)[1]
        self.assertIn("gadk-server = { workspace = true }", dev)
        self.assertEqual(cwd.check(self.repo), [])

    def test_launcher_depends_on_telemetry_required(self):
        self.assertIn("gadk-telemetry", cwd.ALLOWED["gadk-launcher"])
        self.assertNotIn(("gadk-launcher", "gadk-telemetry"), cwd.OPTIONAL)
        self.edit("gadk-launcher", "gadk-telemetry = { workspace = true }\n", "")
        errs = cwd.check(self.repo)
        self.assertTrue(any("gadk-launcher -> gadk-telemetry is missing" in e for e in errs), errs)

    def test_facade_telemetry_edge_must_be_optional(self):
        self.assertIn("gadk-telemetry", cwd.ALLOWED["gadk"])
        self.edit("gadk", "gadk-telemetry = { workspace = true, optional = true }\n",
                  "gadk-telemetry = { workspace = true }\n")
        errs = cwd.check(self.repo)
        self.assertTrue(any("gadk -> gadk-telemetry must be optional = true" in e for e in errs), errs)

    def test_launcher_normal_dependency_on_tools_is_rejected(self):
        self.edit("gadk-launcher", "[dependencies]\n", "[dependencies]\ngadk-tools = { workspace = true }\n")
        errs = cwd.check(self.repo)
        self.assertTrue(any("gadk-launcher -> gadk-tools is not allowed" in e for e in errs), errs)

    def test_facade_missing_launcher_edge_is_rejected(self):
        self.edit("gadk", "gadk-launcher = { workspace = true, optional = true }\n", "")
        errs = cwd.check(self.repo)
        self.assertTrue(any("gadk -> gadk-launcher is missing" in e for e in errs), errs)

    def test_cli_edges_are_exactly_platform_vertexai_server_model(self):
        self.assertEqual(cwd.ALLOWED["gadk-cli"],
                         {"gadk-platform", "gadk-vertexai", "gadk-server", "gadk-model"})
        # Nothing depends on the deploy binary's crate, and it has no direct engine edge.
        self.assertEqual([c for c, deps in cwd.ALLOWED.items() if "gadk-cli" in deps], [])
        self.assertNotIn("gadk-cli", cwd.ENGINE_DEPENDENTS)

    def test_cli_normal_dependency_on_engine_is_rejected(self):
        self.edit("gadk-cli", "[dependencies]\n", "[dependencies]\ngadk-engine = { workspace = true }\n")
        errs = cwd.check(self.repo)
        self.assertTrue(any("gadk-cli -> gadk-engine is not allowed" in e for e in errs), errs)

    def test_cli_missing_server_edge_is_rejected(self):
        self.edit("gadk-cli", 'gadk-server = { workspace = true, features = ["agentengine"] }\n', "")
        errs = cwd.check(self.repo)
        self.assertTrue(any("gadk-cli -> gadk-server is missing" in e for e in errs), errs)

    def test_goyaml_edges_are_exactly_platform(self):
        self.assertEqual(cwd.ALLOWED["gadk-goyaml"], {"gadk-platform"})
        # 6c-1: the façade never depends on the yaml.v3 port; 6c-2: gadk-configurable does.
        self.assertEqual([c for c, deps in cwd.ALLOWED.items() if "gadk-goyaml" in deps],
                         ["gadk-configurable"])
        self.assertNotIn("gadk-goyaml", cwd.ENGINE_DEPENDENTS)

    def test_goyaml_normal_dependency_on_genai_is_rejected(self):
        self.edit("gadk-goyaml", "[dependencies]\n", "[dependencies]\ngadk-genai = { workspace = true }\n")
        errs = cwd.check(self.repo)
        self.assertTrue(any("gadk-goyaml -> gadk-genai is not allowed" in e for e in errs), errs)

    def test_goyaml_missing_platform_edge_is_rejected(self):
        self.edit("gadk-goyaml", "gadk-platform = { workspace = true }\n", "")
        errs = cwd.check(self.repo)
        self.assertTrue(any("gadk-goyaml -> gadk-platform is missing" in e for e in errs), errs)

    def test_goyaml_license_must_be_apache_and_mit(self):
        self.assertEqual(cwd.LICENSE_OVERRIDES["gadk-goyaml"], "Apache-2.0 AND MIT")
        self.edit("gadk-goyaml", 'license = "Apache-2.0 AND MIT"', "license.workspace = true")
        errs = cwd.check(self.repo)
        self.assertTrue(any("package.license must be 'Apache-2.0 AND MIT'" in e for e in errs), errs)

    def test_license_override_is_only_for_goyaml(self):
        self.edit("gadk-cli", "license.workspace = true", 'license = "Apache-2.0 AND MIT"')
        errs = cwd.check(self.repo)
        self.assertTrue(any("gadk-cli/Cargo.toml: package.license must be inherited" in e for e in errs), errs)

    def test_configurable_edges_are_exactly_its_port_dependencies(self):
        self.assertEqual(cwd.ALLOWED["gadk-configurable"],
                         {"gadk-agent", "gadk-engine", "gadk-genai", "gadk-model", "gadk-gemini",
                          "gadk-goyaml", "gadk-launcher", "gadk-mcp", "gadk-platform",
                          "gadk-session", "gadk-tools"})
        # 6c-2: nothing depends on it, the façade included; its tests use the engine's testutil.
        self.assertEqual([c for c, deps in cwd.ALLOWED.items() if "gadk-configurable" in deps], [])
        self.assertIn("gadk-configurable", cwd.ENGINE_DEPENDENTS)
        self.assertNotIn("gadk-configurable", cwd.LICENSE_OVERRIDES)

    def test_configurable_dev_dependency_on_engine_is_allowed(self):
        text = (self.repo / "crates/gadk-configurable/Cargo.toml").read_text()
        dev = text.split("[dev-dependencies]\n", 1)[1]
        self.assertIn('gadk-engine = { workspace = true, features = ["testutil"] }', dev)
        self.assertEqual(cwd.check(self.repo), [])

    def test_configurable_missing_goyaml_edge_is_rejected(self):
        self.edit("gadk-configurable", "gadk-goyaml = { workspace = true }\n", "")
        errs = cwd.check(self.repo)
        self.assertTrue(any("gadk-configurable -> gadk-goyaml is missing" in e for e in errs), errs)

    def test_configurable_normal_dependency_on_server_is_rejected(self):
        self.edit("gadk-configurable", "[dependencies]\n", "[dependencies]\ngadk-server = { workspace = true }\n")
        errs = cwd.check(self.repo)
        self.assertTrue(any("gadk-configurable -> gadk-server is not allowed" in e for e in errs), errs)

    def test_facade_dependency_on_configurable_is_rejected(self):
        self.edit("gadk", "[dependencies]\n", "[dependencies]\ngadk-configurable = { workspace = true }\n")
        errs = cwd.check(self.repo)
        self.assertTrue(any("gadk -> gadk-configurable is not allowed" in e for e in errs), errs)

    def test_agentanalytics_edges_are_exactly_agent_engine_genai_model_platform_session(self):
        self.assertEqual(cwd.ALLOWED["gadk-agentanalytics"],
                         {"gadk-agent", "gadk-engine", "gadk-genai", "gadk-model", "gadk-platform",
                          "gadk-session"})
        # 7b-1: only the façade depends on it, optionally (feature `agentanalytics`).
        self.assertEqual([c for c, deps in cwd.ALLOWED.items() if "gadk-agentanalytics" in deps], ["gadk"])
        self.assertIn(("gadk", "gadk-agentanalytics"), cwd.OPTIONAL)
        # 7b-3a: the plugin reads the engine's LLM agent config and ADK span IDs.
        self.assertIn("gadk-agentanalytics", cwd.ENGINE_DEPENDENTS)
        self.assertEqual(cwd.LICENSE_OVERRIDES["gadk-agentanalytics"], "Apache-2.0 AND BSD-3-Clause")

    def test_agentanalytics_missing_engine_edge_is_rejected(self):
        self.edit("gadk-agentanalytics", "gadk-engine = { workspace = true }\n", "")
        errs = cwd.check(self.repo)
        self.assertTrue(any("gadk-agentanalytics -> gadk-engine is missing" in e for e in errs), errs)

    def test_agentanalytics_missing_agent_edge_is_rejected(self):
        self.edit("gadk-agentanalytics", "gadk-agent = { workspace = true }\n", "")
        errs = cwd.check(self.repo)
        self.assertTrue(any("gadk-agentanalytics -> gadk-agent is missing" in e for e in errs), errs)

    def test_agentanalytics_dev_dependency_on_engine_is_allowed(self):
        text = (self.repo / "crates/gadk-agentanalytics/Cargo.toml").read_text()
        dev = text.split("[dev-dependencies]\n", 1)[1]
        self.assertIn('gadk-engine = { workspace = true, features = ["testutil"] }', dev)
        self.assertEqual(cwd.check(self.repo), [])

    def test_agentanalytics_normal_dependency_on_tools_is_rejected(self):
        self.edit("gadk-agentanalytics", "[dependencies]\n", "[dependencies]\ngadk-tools = { workspace = true }\n")
        errs = cwd.check(self.repo)
        self.assertTrue(any("gadk-agentanalytics -> gadk-tools is not allowed" in e for e in errs), errs)

    def test_facade_agentanalytics_edge_must_be_optional(self):
        self.edit("gadk", "gadk-agentanalytics = { workspace = true, optional = true }\n",
                  "gadk-agentanalytics = { workspace = true }\n")
        errs = cwd.check(self.repo)
        self.assertTrue(any("gadk -> gadk-agentanalytics must be optional = true" in e for e in errs), errs)

    def test_facade_missing_agentanalytics_edge_is_rejected(self):
        self.edit("gadk", "gadk-agentanalytics = { workspace = true, optional = true }\n", "")
        errs = cwd.check(self.repo)
        self.assertTrue(any("gadk -> gadk-agentanalytics is missing" in e for e in errs), errs)

    def test_agentanalytics_missing_session_edge_is_rejected(self):
        self.edit("gadk-agentanalytics", "gadk-session = { workspace = true }\n", "")
        errs = cwd.check(self.repo)
        self.assertTrue(any("gadk-agentanalytics -> gadk-session is missing" in e for e in errs), errs)

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

    def test_eval_monitor_edges_are_exactly_agent_cloudtrace_eval_gemini_model_platform(self):
        self.assertEqual(cwd.ALLOWED["gadk-eval-monitor"],
                         {"gadk-agent", "gadk-cloudtrace", "gadk-eval", "gadk-gemini",
                          "gadk-model", "gadk-platform"})
        # Only the console (its Evals pages) depends on it; the façade does not.
        self.assertEqual([c for c, deps in cwd.ALLOWED.items() if "gadk-eval-monitor" in deps],
                         ["gadk-console"])
        self.assertNotIn("gadk-eval-monitor", cwd.ENGINE_DEPENDENTS)

    def test_console_may_depend_on_cloudtrace_eval_eval_monitor_and_bigquery(self):
        self.assertEqual(cwd.ALLOWED["gadk-console"],
                         {"gadk-cloudtrace", "gadk-model", "gadk-prompts", "gadk-prompts-cli",
                          "gadk-eval", "gadk-eval-monitor", "gadk-bigquery"})
        self.assertEqual(cwd.check(self.repo), [])

    def test_cloudtrace_depends_only_on_model(self):
        self.assertEqual(cwd.ALLOWED["gadk-cloudtrace"], {"gadk-model"})
        # The console (trace reads) and the eval monitor (Cloud Trace source) depend on it.
        self.assertEqual(sorted(c for c, deps in cwd.ALLOWED.items() if "gadk-cloudtrace" in deps),
                         ["gadk-console", "gadk-eval-monitor"])
        self.assertNotIn("gadk-cloudtrace", cwd.ENGINE_DEPENDENTS)

    def test_cloudtrace_dependency_on_engine_is_rejected(self):
        self.edit("gadk-cloudtrace", "[dependencies]\n", "[dependencies]\ngadk-engine = { workspace = true }\n")
        errs = cwd.check(self.repo)
        self.assertTrue(any("gadk-cloudtrace -> gadk-engine is not allowed" in e for e in errs), errs)

    def test_cloudtrace_missing_model_edge_is_rejected(self):
        self.edit("gadk-cloudtrace", 'gadk-model = { workspace = true, features = ["gcp-auth"] }\n', "")
        errs = cwd.check(self.repo)
        self.assertTrue(any("gadk-cloudtrace -> gadk-model is missing" in e for e in errs), errs)

    def test_bigquery_edges_are_exactly_agent_engine_model_session(self):
        self.assertEqual(cwd.ALLOWED["gadk-bigquery"],
                         {"gadk-agent", "gadk-engine", "gadk-model", "gadk-session"})
        self.assertIn("gadk-bigquery", cwd.ENGINE_DEPENDENTS)

    def test_bigquery_dependency_on_tools_is_rejected(self):
        self.edit("gadk-bigquery", "[dependencies]\n", "[dependencies]\ngadk-tools = { workspace = true }\n")
        errs = cwd.check(self.repo)
        self.assertTrue(any("gadk-bigquery -> gadk-tools is not allowed" in e for e in errs), errs)

    def test_bigquery_missing_session_edge_is_rejected(self):
        self.edit("gadk-bigquery", "gadk-session = { workspace = true }\n", "")
        errs = cwd.check(self.repo)
        self.assertTrue(any("gadk-bigquery -> gadk-session is missing" in e for e in errs), errs)

    def test_facade_bigquery_edge_is_optional(self):
        self.assertEqual(sorted(c for c, deps in cwd.ALLOWED.items() if "gadk-bigquery" in deps),
                         ["gadk", "gadk-console"])
        self.assertNotIn(("gadk-console", "gadk-bigquery"), cwd.OPTIONAL)
        self.assertIn(("gadk", "gadk-bigquery"), cwd.OPTIONAL)
        self.edit("gadk", "gadk-bigquery = { workspace = true, optional = true }",
                  "gadk-bigquery = { workspace = true }")
        errs = cwd.check(self.repo)
        self.assertTrue(any("gadk -> gadk-bigquery must be optional = true" in e for e in errs), errs)

    def test_facade_missing_bigquery_edge_is_rejected(self):
        self.edit("gadk", "gadk-bigquery = { workspace = true, optional = true }\n", "")
        errs = cwd.check(self.repo)
        self.assertTrue(any("gadk -> gadk-bigquery is missing" in e for e in errs), errs)

    def test_eval_monitor_missing_cloudtrace_edge_is_rejected(self):
        self.edit("gadk-eval-monitor", "gadk-cloudtrace = { workspace = true }\n", "")
        errs = cwd.check(self.repo)
        self.assertTrue(any("gadk-eval-monitor -> gadk-cloudtrace is missing" in e for e in errs), errs)

    def test_eval_monitor_normal_dependency_on_engine_is_rejected(self):
        self.edit("gadk-eval-monitor", "[dependencies]\n", "[dependencies]\ngadk-engine = { workspace = true }\n")
        errs = cwd.check(self.repo)
        self.assertTrue(any("gadk-eval-monitor -> gadk-engine is not allowed" in e for e in errs), errs)

    def test_eval_monitor_missing_agent_edge_is_rejected(self):
        self.edit("gadk-eval-monitor", "gadk-agent = { workspace = true }\n", "")
        errs = cwd.check(self.repo)
        self.assertTrue(any("gadk-eval-monitor -> gadk-agent is missing" in e for e in errs), errs)

    def test_eval_monitor_missing_eval_edge_is_rejected(self):
        self.edit("gadk-eval-monitor", "gadk-eval = { workspace = true }\n", "")
        errs = cwd.check(self.repo)
        self.assertTrue(any("gadk-eval-monitor -> gadk-eval is missing" in e for e in errs), errs)


if __name__ == "__main__":
    unittest.main()
