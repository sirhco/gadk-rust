// Copyright 2026 Chris Olson
// Portions derived from ADK-Go (https://github.com/google/adk-go), Copyright 2026 Google LLC.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Whether a compaction config can serve the applications behind a serving surface (the REST
//! server, the trigger controllers), checked by a dry run of [`Runner::new`] per application
//! rather than by re-implementing its checks.
//!
//! A config can pass [`Config::validate`] and still be unservable: with no summarizer, the
//! default one needs the root agent's model, and a workflow agent has none. Without this check a
//! server starts, reports healthy, and fails every request instead.

use std::sync::Arc;

use gadk_agent::{Agent, AgentError, Loader, go_quote};
use gadk_platform::GoWrapped;
use gadk_session::compaction::Config;

use crate::runner::{Runner, RunnerConfig};

/// Whether `cfg` can serve every application `loader` lists, given the services on `base`.
///
/// `None` config is always fine and the loader is then not touched; a `None` loader checks only
/// the config's shape. An application the loader cannot load is skipped (it fails its own
/// requests). Applications are checked in the loader's order.
pub fn against_agents(
    cfg: Option<&Config>,
    loader: Option<&dyn Loader>,
    base: &RunnerConfig,
) -> Result<(), AgentError> {
    let Some(cfg) = cfg else {
        return Ok(());
    };
    let Some(loader) = loader else {
        return check(cfg, Vec::new(), base);
    };
    let agents = loader
        .list_agents()
        .into_iter()
        .filter_map(|name| loader.load_agent(&name).ok().map(|a| (name, a)))
        .collect();
    check(cfg, agents, base)
}

/// [`against_agents`] for a surface that serves only the loader's root agent.
pub fn against_root_agent(
    cfg: Option<&Config>,
    root: Option<Arc<dyn Agent>>,
    base: &RunnerConfig,
) -> Result<(), AgentError> {
    let Some(cfg) = cfg else {
        return Ok(());
    };
    let agents = root
        .map(|r| vec![(r.name().to_owned(), r)])
        .unwrap_or_default();
    check(cfg, agents, base)
}

/// Validates `cfg` and dry-runs [`Runner::new`] for each application. There is no "cannot check
/// without a session service" case: a [`RunnerConfig`] always carries a session service.
fn check(
    cfg: &Config,
    agents: Vec<(String, Arc<dyn Agent>)>,
    base: &RunnerConfig,
) -> Result<(), AgentError> {
    cfg.validate()
        .map_err(|e| AgentError::Other(Box::new(GoWrapped::new("invalid compaction config", e))))?;
    for (name, agent) in agents {
        // Two runs, so only a compaction problem is reported: anything else a runner needs may
        // legitimately be missing at construction time, and the app fails its own requests.
        let mut without = base.clone();
        without.app_name = name.clone();
        without.agent = agent;
        without.compaction = None;
        if Runner::new(without.clone()).is_err() {
            continue;
        }
        let mut with = without;
        with.compaction = Some(cfg.clone());
        if let Err(e) = Runner::new(with) {
            return Err(AgentError::Other(Box::new(GoWrapped::new(
                format!("the Compaction config cannot serve app {}", go_quote(&name)),
                e,
            ))));
        }
    }
    Ok(())
}
