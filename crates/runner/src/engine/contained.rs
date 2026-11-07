use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use bosun_types::ir::{self, Shell, Template};
use serde_json::{Map, Value};
use tokio::time::{Instant, sleep_until};

use super::{Execution, Program, cancelled, text};
use crate::container::launch::{self, Words};
use crate::container::{self, Container, ContainerJob, Layout, Mounts, Request};
use crate::safe;
use crate::scope;
use crate::shell::Invocation;

pub(super) enum Stop {
    Cancelled(Vec<String>),
    TimedOut(Vec<String>),
    Failed(Vec<String>),
}

enum Started {
    Done(Result<Vec<PathBuf>, String>),
    Cancelled,
    TimedOut,
}

impl Execution {
    pub(super) async fn contain(
        &mut self,
        spec: ContainerJob,
        id: &str,
        template: Option<&ir::Container>,
    ) -> Result<(), Stop> {
        let failed = |line: String| Stop::Failed(vec![line]);
        let configuration = self.configure(template).map_err(failed)?;
        let mounts = canonical(&spec.mounts).map_err(failed)?;
        safe::directory(&self.temp.join("_github_home")).map_err(|error| {
            failed(format!("the home of the job could not be created: {error}"))
        })?;
        let layout = Layout::new(&mounts, &self.temp);
        let mut container = Container::new(&spec, id, layout);
        let request = Request {
            workspace: self.workspace.clone(),
            options: configuration.options,
            volumes: configuration.volumes,
            ports: configuration.ports,
            environment: configuration.environment,
            nodes: !self.actions.is_empty(),
        };
        let deadline = self.deadline;
        let started = {
            let start = container.start(&spec, request);
            tokio::pin!(start);
            tokio::select! {
                started = &mut start => Started::Done(started),
                () = cancelled(&mut self.cancel) => Started::Cancelled,
                () = sleep_until(deadline.unwrap_or_else(Instant::now)), if deadline.is_some() => {
                    Started::TimedOut
                }
            }
        };
        let nodes = match started {
            Started::Cancelled => return Err(Stop::Cancelled(container.teardown().await)),
            Started::TimedOut => {
                let mut lines = vec![
                    "the job ran beyond its `timeout-minutes` while its container was being \
                     prepared"
                        .to_owned(),
                ];
                lines.extend(container.teardown().await);
                return Err(Stop::TimedOut(lines));
            }
            Started::Done(Ok(nodes)) => nodes,
            Started::Done(Err(reason)) => {
                let mut lines = vec![reason];
                lines.extend(container.teardown().await);
                return Err(Stop::Failed(lines));
            }
        };
        match self.enter(&container, nodes) {
            Ok(()) => {
                self.container = Some(container);
                Ok(())
            }
            Err(reason) => {
                let mut lines = vec![reason];
                lines.extend(container.teardown().await);
                Err(Stop::Failed(lines))
            }
        }
    }

    fn configure(&self, template: Option<&ir::Container>) -> Result<Configuration, String> {
        let Some(template) = template else {
            return Ok(Configuration::default());
        };
        let env = self.env_context();
        let scope = self.scope(&self.github, &env);
        let text = |what: &str, template: &Template| scope::text(what, template, &scope);
        let mut configuration = Configuration::default();
        for (position, volume) in template.volumes.iter().enumerate() {
            configuration.volumes.push(text(
                &format!("`volumes[{position}]` of `container`"),
                volume,
            )?);
        }
        for (position, port) in template.ports.iter().enumerate() {
            configuration
                .ports
                .push(text(&format!("`ports[{position}]` of `container`"), port)?);
        }
        if let Some(options) = &template.options {
            configuration.options = Some(text("`options` of `container`", options)?);
        }
        for (name, value) in &template.env {
            configuration.environment.insert(
                name.clone(),
                text(&format!("`env.{name}` of `container`"), value)?,
            );
        }
        Ok(configuration)
    }

    fn enter(&mut self, container: &Container, nodes: Vec<PathBuf>) -> Result<(), String> {
        let to = |path: &Path| {
            container.translate(path).ok_or_else(|| {
                format!(
                    "{} is not a directory that the job container mounts",
                    path.display()
                )
            })
        };
        if !nodes.is_empty() {
            if nodes.len() != self.actions.len() {
                return Err(format!(
                    "the agent provided {} Node runtimes for {} actions",
                    nodes.len(),
                    self.actions.len()
                ));
            }
            for (action, node) in self.actions.iter_mut().zip(nodes) {
                action.node = node;
            }
        }
        let workspace = to(&self.workspace)?;
        self.runner.temp = to(&self.temp)?;
        self.runner.workspace = match self.workspace.parent() {
            Some(parent) => to(parent)?,
            None => String::new(),
        };
        if let Some(tools) = container
            .layout()
            .pairs()
            .iter()
            .find(|(_, mounted, _)| *mounted == container::TOOLS)
        {
            self.runner.tool_cache = to(&tools.0)?;
        }
        self.github
            .insert("workspace".to_owned(), Value::String(workspace));
        self.fixed.set_runner(&self.runner);
        Ok(())
    }

    pub(super) fn host_files(&self, github: &Map<String, Value>) -> Map<String, Value> {
        let mut github = github.clone();
        let Some(container) = &self.container else {
            return github;
        };
        for property in ["env", "output", "path", "state", "step_summary"] {
            let host = github
                .get(property)
                .and_then(Value::as_str)
                .and_then(|path| container.layout().to_host(path));
            if let Some(host) = host {
                github.insert(property.to_owned(), Value::String(text(&host)));
            }
        }
        github
    }

    pub(super) fn contained(
        &self,
        index: u32,
        program: Program,
        directory: &Path,
        env: &BTreeMap<OsString, OsString>,
    ) -> Result<(OsString, Vec<OsString>, PathBuf), String> {
        let Some(container) = &self.container else {
            return Err("the step has no job container".to_owned());
        };
        let to = |path: &Path| {
            container.translate(path).ok_or_else(|| {
                format!(
                    "{} is not a directory that the job container mounts",
                    path.display()
                )
            })
        };
        let words = match program {
            Program::Script { script, shell } => {
                let (first, second) = match shell {
                    Some(shell) => (Invocation::resolve(Some(&shell), None)?, None),
                    None => (
                        Invocation::resolve(Some(&Shell::Custom("bash -e {0}".to_owned())), None)?,
                        Some(Invocation::resolve(
                            Some(&Shell::Custom("sh -e {0}".to_owned())),
                            None,
                        )?),
                    ),
                };
                let file = self.temp.join(format!("step-{index}{}", first.extension()));
                safe::create(&file, 0o666, script.as_bytes()).map_err(|error| {
                    format!(
                        "the script could not be written to {}: {error}",
                        file.display()
                    )
                })?;
                let inside = to(&file)?;
                let inside = Path::new(&inside);
                let words = |invocation: &Invocation| {
                    let mut words = vec![invocation.program(inside)];
                    words.extend(invocation.arguments(inside));
                    words
                };
                match second {
                    Some(second) => Words::Choice {
                        first: words(&first),
                        second: words(&second),
                    },
                    None => Words::Fixed(words(&first)),
                }
            }
            Program::Node { node, entry } => Words::Fixed(vec![to(&node)?, to(&entry)?]),
        };
        let mut environment = BTreeMap::new();
        environment.insert("HOME".to_owned(), container::HOME.to_owned());
        environment.extend(container.environment().clone());
        for (name, value) in env {
            environment.insert(
                name.to_string_lossy().into_owned(),
                value.to_string_lossy().into_owned(),
            );
        }
        let launcher = self.temp.join(format!("step-{index}.launch"));
        let inside = to(&launcher)?;
        let path: Vec<String> = self.paths.iter().rev().cloned().collect();
        let text = launch::script(&launch::Launch {
            remove: &inside,
            directory: &to(directory)?,
            path: &path,
            environment: &environment,
            words: &words,
        })?;
        safe::create(&launcher, 0o600, text.as_bytes()).map_err(|error| {
            format!(
                "the launcher of the step could not be written to {}: {error}",
                launcher.display()
            )
        })?;
        let (command, arguments) = container.exec(&inside);
        Ok((command, arguments, launcher))
    }
}

#[derive(Default)]
struct Configuration {
    options: Option<String>,
    volumes: Vec<String>,
    ports: Vec<String>,
    environment: BTreeMap<String, String>,
}

fn canonical(mounts: &Mounts) -> Result<Mounts, String> {
    let make = |path: &Path| {
        fs::create_dir_all(path)
            .and_then(|()| fs::canonicalize(path))
            .map_err(|error| {
                format!(
                    "the directory {} for the job container could not be prepared: {error}",
                    path.display()
                )
            })
    };
    Ok(Mounts {
        work: make(&mounts.work)?,
        externals: make(&mounts.externals)?,
        tools: make(&mounts.tools)?,
    })
}
