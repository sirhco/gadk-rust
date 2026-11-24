// Copyright 2026 Chris Olson
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

//! Subcommand implementations.

use std::io::{Read, Write};
use std::process::Command as Process;

use clap::ArgMatches;
use gadk_prompts::{Label, NewVersion, PromptStore, PromptVersion, Rollout, Selector, StoreError};

fn io(e: std::io::Error) -> StoreError {
    StoreError::Io(e.to_string())
}

fn arg<'a>(m: &'a ArgMatches, k: &str) -> Option<&'a String> {
    m.get_one::<String>(k)
}

fn req<'a>(m: &'a ArgMatches, k: &str) -> &'a str {
    arg(m, k).map_or("", String::as_str)
}

fn parse_version(s: &str) -> Result<u32, StoreError> {
    s.strip_prefix('v')
        .unwrap_or(s)
        .parse()
        .map_err(|_| StoreError::Invalid(format!("bad version {s:?}")))
}

/// `v<n>` | `latest` | a label.
fn parse_selector(s: &str) -> Result<Selector, StoreError> {
    if s == "latest" {
        return Ok(Selector::Latest);
    }
    if let Some(n) = s
        .strip_prefix('v')
        .filter(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
    {
        return Ok(Selector::Version(parse_version(n)?));
    }
    Ok(Selector::Label(Label::new(s)?))
}

fn git_email() -> String {
    Process::new("git")
        .args(["config", "user.email"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .unwrap_or_default()
}

pub(crate) async fn dispatch(
    store: &dyn PromptStore,
    m: &ArgMatches,
    stdin: &mut dyn Read,
    out: &mut dyn Write,
) -> Result<(), StoreError> {
    match m.subcommand() {
        Some(("push", m)) => {
            let template = match arg(m, "template").map(String::as_str) {
                None | Some("-") => {
                    let mut s = String::new();
                    stdin.read_to_string(&mut s).map_err(io)?;
                    s
                }
                Some(path) => std::fs::read_to_string(path).map_err(io)?,
            };
            let config = match arg(m, "config") {
                Some(path) => Some(
                    serde_json::from_str(&std::fs::read_to_string(path).map_err(io)?)
                        .map_err(|e| StoreError::Invalid(format!("config {path}: {e}")))?,
                ),
                None => None,
            };
            let name = req(m, "name");
            let (v, created) = store
                .create_version_reporting(NewVersion {
                    name: name.into(),
                    template,
                    config,
                    author: arg(m, "author").cloned().unwrap_or_else(git_email),
                    message: arg(m, "message").cloned().unwrap_or_default(),
                })
                .await?;
            writeln!(
                out,
                "{} v{}{}",
                v.name,
                v.version,
                if created { "" } else { " (unchanged)" }
            )
            .map_err(io)
        }
        Some(("promote", m)) => {
            let name = req(m, "name");
            let version = parse_version(req(m, "version"))?;
            let label = Label::new(req(m, "label"))?;
            let expected = match arg(m, "expect").map(String::as_str) {
                None => None,
                Some("none") => Some(None),
                Some(v) => Some(Some(parse_version(v)?)),
            };
            store.set_label(name, &label, version, expected).await?;
            writeln!(out, "{name} {label} -> v{version}").map_err(io)
        }
        Some(("unlabel", m)) => {
            let name = req(m, "name");
            let label = Label::new(req(m, "label"))?;
            store.remove_label(name, &label).await?;
            writeln!(out, "{name} {label} removed").map_err(io)
        }
        Some(("rollout", m)) => {
            let name = req(m, "name");
            let label = Label::new(req(m, "label"))?;
            if m.get_flag("promote") {
                let (snap, rollouts) = store.load_prompt(name).await?;
                let to = rollouts
                    .get(name, &label)
                    .ok_or_else(|| StoreError::NotFound {
                        name: name.into(),
                        selector: format!("{label} rollout"),
                    })?
                    .version;
                let from = snap.resolve(name, &Selector::Label(label.clone()))?.version;
                store.set_label(name, &label, to, Some(Some(from))).await?;
                return writeln!(out, "{name} {label} -> v{to}").map_err(io);
            }
            if m.get_flag("abort") {
                store.set_rollout(name, &label, None).await?;
                return writeln!(out, "{name} {label} rollout aborted").map_err(io);
            }
            let version = parse_version(req(m, "version"))?;
            let percent = req(m, "percent");
            let percent = percent
                .parse()
                .map_err(|_| StoreError::Invalid(format!("bad percent {percent:?}")))?;
            let rollout = Rollout::new(version, percent)?;
            store.set_rollout(name, &label, Some(rollout)).await?;
            let at = store
                .load_prompt(name)
                .await?
                .0
                .resolve(name, &Selector::Label(label.clone()))?
                .version;
            writeln!(out, "{name} {label} -> v{at} (v{version} at {percent}%)").map_err(io)
        }
        Some(("list", m)) => {
            let prefix = arg(m, "prefix").map_or("", String::as_str);
            let (snap, rollouts) = store.load_all_with_rollouts().await?;
            for (name, e) in snap.prompts.iter().filter(|(n, _)| n.starts_with(prefix)) {
                let latest = e.versions.keys().next_back().copied().unwrap_or(0);
                let labels: Vec<String> = e
                    .labels
                    .iter()
                    .map(|(l, v)| match rollouts.get(name, l) {
                        Some(r) => format!("{l}=v{v}(v{}@{}%)", r.version, r.percent),
                        None => format!("{l}=v{v}"),
                    })
                    .collect();
                writeln!(out, "{name}\tv{latest}\t{}", labels.join(",")).map_err(io)?;
            }
            Ok(())
        }
        Some(("show", m)) => {
            let sel = match (arg(m, "label"), arg(m, "version")) {
                (Some(l), _) => Selector::Label(Label::new(l)?),
                (_, Some(v)) => Selector::Version(parse_version(v)?),
                _ => Selector::Latest,
            };
            let name = req(m, "name");
            let (snap, rollouts) = store.load_prompt(name).await?;
            let v = snap.resolve(name, &sel)?;
            if m.get_flag("json") {
                let s =
                    serde_json::to_string(&*v).map_err(|e| StoreError::Decode(e.to_string()))?;
                return writeln!(out, "{s}").map_err(io);
            }
            show(out, &v)?;
            if let Selector::Label(l) = &sel
                && let Some(r) = rollouts.get(name, l)
            {
                writeln!(out, "rollout: v{} at {}%", r.version, r.percent).map_err(io)?;
            }
            Ok(())
        }
        Some(("diff", m)) => {
            let name = req(m, "name");
            let snap = store.load_prompt(name).await?.0;
            let (from, to) = (req(m, "from"), req(m, "to"));
            let a = snap.resolve(name, &parse_selector(from)?)?;
            let b = snap.resolve(name, &parse_selector(to)?)?;
            let pretty = |v: &PromptVersion| {
                serde_json::to_string_pretty(&v.config.clone().unwrap_or(serde_json::Value::Null))
                    .unwrap_or_default()
                    + "\n"
            };
            let tdiff = similar::TextDiff::from_lines(&a.template, &b.template);
            if a.template != b.template {
                write!(
                    out,
                    "{}",
                    tdiff
                        .unified_diff()
                        .context_radius(3)
                        .header(&format!("{name}@{from}"), &format!("{name}@{to}"))
                )
                .map_err(io)?;
            }
            let (ca, cb) = (pretty(&a), pretty(&b));
            if ca != cb {
                let cdiff = similar::TextDiff::from_lines(&ca, &cb);
                write!(
                    out,
                    "{}",
                    cdiff.unified_diff().context_radius(3).header(
                        &format!("{name}@{from} config"),
                        &format!("{name}@{to} config")
                    )
                )
                .map_err(io)?;
            }
            Ok(())
        }
        _ => Err(StoreError::Invalid("unknown subcommand".into())),
    }
}

fn show(out: &mut dyn Write, v: &PromptVersion) -> Result<(), StoreError> {
    writeln!(
        out,
        "{} v{} {} {} {}",
        v.name,
        v.version,
        v.meta.hash,
        v.meta
            .created_at
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        v.meta.author
    )
    .map_err(io)?;
    writeln!(out).map_err(io)?;
    write!(out, "{}", v.template).map_err(io)?;
    if !v.template.ends_with('\n') {
        writeln!(out).map_err(io)?;
    }
    Ok(())
}
