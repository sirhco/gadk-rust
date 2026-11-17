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

//! The sub-launcher that adds the Eventarc trigger
//! endpoint to the web server.

use std::io::Write;
use std::sync::{Arc, Mutex};

use gadk_server::mux::{Request, handler};
use gadk_server::rest::triggers::EventarcController;

use super::triggerauth::Trigger;
use crate::config::Config;
use crate::error::Error;
use crate::web::{WebRouter, WebSublauncher};

/// The `eventarc` web sub-launcher.
pub struct EventarcLauncher {
    pub(crate) trigger: Trigger,
}

/// An `eventarc` launcher with its flags.
pub fn new() -> EventarcLauncher {
    EventarcLauncher {
        trigger: Trigger::new("eventarc", "Eventarc"),
    }
}

impl EventarcLauncher {
    /// Replaces where the flag set writes its errors and usage (default
    /// stderr), for tests and the CLI replay.
    pub fn with_flag_output(mut self, out: Arc<Mutex<dyn Write + Send>>) -> EventarcLauncher {
        self.trigger.set_flag_output(out);
        self
    }
}

impl WebSublauncher for EventarcLauncher {
    fn keyword(&self) -> String {
        "eventarc".to_owned()
    }

    fn parse(&mut self, args: Vec<String>) -> Result<Vec<String>, Error> {
        self.trigger.parse(args)
    }

    fn command_line_syntax(&self) -> String {
        self.trigger.command_line_syntax()
    }

    fn simple_description(&self) -> String {
        "starts ADK Eventarc trigger endpoint server".to_owned()
    }

    fn setup_subrouters(&self, router: &mut WebRouter, config: &mut Config) -> Result<(), Error> {
        self.trigger.setup_subrouters(router, config, |cfg| {
            let controller = Arc::new(EventarcController::new(cfg)?);
            Ok(handler(move |req: Request| {
                let controller = Arc::clone(&controller);
                async move { controller.eventarc_trigger_handler(req).await }
            }))
        })
    }

    fn user_message(&self, web_url: &str, printer: &mut dyn FnMut(&str)) {
        // One space after the colon here, two in the other sub-launchers' lines, as ADK
        // launchers print them.
        printer(&format!(
            "       eventarc: Eventarc trigger endpoint is available at {web_url}{}/apps/{{app_name}}/trigger/eventarc",
            self.trigger.path_prefix()
        ));
    }
}
