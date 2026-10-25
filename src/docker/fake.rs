/*
 * MIT License
 *
 * Copyright (c) 2026 Roberto Leinardi
 *
 * Permission is hereby granted, free of charge, to any person obtaining a copy
 * of this software and associated documentation files (the "Software"), to deal
 * in the Software without restriction, including without limitation the rights
 * to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
 * copies of the Software, and to permit persons to whom the Software is
 * furnished to do so, subject to the following conditions:
 *
 * The above copyright notice and this permission notice shall be included in all
 * copies or substantial portions of the Software.
 *
 * THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
 * IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
 * FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
 * AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
 * LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
 * OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
 * SOFTWARE.
 */

//! A recording, in-memory [`DockerApi`]: one shared engine state, every call logged, and failures
//! injected per call. Port of swarm-device-access `fake_test.go`.
//!
//! It enforces what the launcher promises: a remove names a full container ID, never a name, and
//! a list without `all` hides every container that is not running.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

use super::{ContainerInfo, ContainerSummary, DockerApi, DockerError};
use crate::spec::ContainerSpec;

/// A container's state in the fake engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Created,
    Running,
    Exited,
}

/// A container in the fake engine.
#[derive(Debug, Clone, PartialEq)]
pub struct Container {
    pub id: String,
    pub name: String,
    pub labels: HashMap<String, String>,
    pub status: Status,
    /// When set, inspect reports no state at all.
    pub no_state: bool,
    /// The create request, for containers the code under test created.
    pub spec: Option<ContainerSpec>,
}

/// One call, as the code under test made it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Call {
    List { label: String, all: bool },
    Inspect(String),
    Create(String),
    Remove(String),
}

/// Which call an injected failure applies to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    List,
    Inspect,
    Create,
    Remove,
}

#[derive(Debug, Default)]
struct State {
    containers: Vec<Container>,
    calls: Vec<Call>,
    /// `(op, target, error)`: the next call of `op` on `target` (an ID, a name, or `""` for any)
    /// fails with `error`, once.
    failures: Vec<(Op, String, DockerError)>,
    next_id: u64,
}

/// The fake engine. Clones share one state, as two launchers share one node.
#[derive(Debug, Clone, Default)]
pub struct FakeDocker(Arc<Mutex<State>>);

impl FakeDocker {
    /// Adds a container and returns its ID.
    pub fn add<Key: Into<String>, Value: Into<String>>(
        &self,
        name: &str,
        labels: impl IntoIterator<Item = (Key, Value)>,
        status: Status,
    ) -> String {
        let mut state = self.state();
        let id = next_id(&mut state);
        state.containers.push(Container {
            id: id.clone(),
            name: name.to_owned(),
            labels: labels
                .into_iter()
                .map(|(key, value)| (key.into(), value.into()))
                .collect(),
            status,
            no_state: false,
            spec: None,
        });
        id
    }

    /// Makes inspect of `id` report no state.
    pub fn drop_state(&self, id: &str) {
        self.with_container(id, |container| container.no_state = true);
    }

    /// Sets the status of `id`.
    pub fn set_status(&self, id: &str, status: Status) {
        self.with_container(id, |container| container.status = status);
    }

    /// Fails the next `op` on `target` (`""` for any target) with `error`.
    pub fn fail(&self, op: Op, target: &str, error: DockerError) {
        self.state().failures.push((op, target.to_owned(), error));
    }

    /// The containers now in the engine.
    #[must_use]
    pub fn containers(&self) -> Vec<Container> {
        self.state().containers.clone()
    }

    /// The container named `name`, if any.
    #[must_use]
    pub fn by_name(&self, name: &str) -> Option<Container> {
        self.state()
            .containers
            .iter()
            .find(|container| container.name == name)
            .cloned()
    }

    /// Every call so far.
    #[must_use]
    pub fn calls(&self) -> Vec<Call> {
        self.state().calls.clone()
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn with_container(&self, id: &str, change: impl FnOnce(&mut Container)) {
        let mut state = self.state();
        let container = state
            .containers
            .iter_mut()
            .find(|container| container.id == id)
            .unwrap();
        change(container);
    }

    /// Records `call` and returns the failure injected for it, if any.
    fn enter(
        &self,
        call: Call,
        op: Op,
        target: &str,
    ) -> Result<MutexGuard<'_, State>, DockerError> {
        let mut state = self.state();
        state.calls.push(call);
        let injected = state
            .failures
            .iter()
            .position(|(failing, on, _)| *failing == op && (on.is_empty() || on == target));
        if let Some(index) = injected {
            return Err(state.failures.remove(index).2);
        }
        Ok(state)
    }
}

impl DockerApi for FakeDocker {
    async fn list_containers(
        &self,
        label: &str,
        all: bool,
    ) -> Result<Vec<ContainerSummary>, DockerError> {
        // Every call yields first, as a real round trip would, so concurrent launchers
        // interleave between calls.
        tokio::task::yield_now().await;
        let call = Call::List {
            label: label.to_owned(),
            all,
        };
        let state = self.enter(call, Op::List, "")?;
        let (key, value) = label.split_once('=').unwrap();
        Ok(state
            .containers
            .iter()
            .filter(|container| all || container.status == Status::Running)
            .filter(|container| container.labels.get(key).map(String::as_str) == Some(value))
            .map(|container| ContainerSummary {
                id: container.id.clone(),
                labels: container.labels.clone(),
            })
            .collect())
    }

    async fn inspect_container(&self, id_or_name: &str) -> Result<ContainerInfo, DockerError> {
        tokio::task::yield_now().await;
        let state = self.enter(
            Call::Inspect(id_or_name.to_owned()),
            Op::Inspect,
            id_or_name,
        )?;
        let container = state
            .containers
            .iter()
            .find(|container| container.id == id_or_name || container.name == id_or_name)
            .ok_or(DockerError::NotFound)?;
        Ok(ContainerInfo {
            id: container.id.clone(),
            name: container.name.clone(),
            labels: container.labels.clone(),
            running: (!container.no_state).then_some(container.status == Status::Running),
        })
    }

    async fn create_container(&self, spec: &ContainerSpec) -> Result<String, DockerError> {
        tokio::task::yield_now().await;
        let mut state = self.enter(Call::Create(spec.name.clone()), Op::Create, &spec.name)?;
        if state
            .containers
            .iter()
            .any(|container| container.name == spec.name)
        {
            return Err(DockerError::Conflict(format!(
                "the container name \"/{}\" is already in use",
                spec.name
            )));
        }
        let id = next_id(&mut state);
        let labels = spec.body.labels.clone().unwrap_or_default();
        state.containers.push(Container {
            id: id.clone(),
            name: spec.name.clone(),
            labels,
            status: Status::Created,
            no_state: false,
            spec: Some(spec.clone()),
        });
        Ok(id)
    }

    async fn remove_container(&self, id: &str) -> Result<(), DockerError> {
        tokio::task::yield_now().await;
        assert!(
            id.len() == 64 && id.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "remove by {id:?}: the launcher removes by full ID only"
        );
        let mut state = self.enter(Call::Remove(id.to_owned()), Op::Remove, id)?;
        let before = state.containers.len();
        state.containers.retain(|container| container.id != id);
        if state.containers.len() == before {
            return Err(DockerError::NotFound);
        }
        Ok(())
    }
}

fn next_id(state: &mut State) -> String {
    state.next_id += 1;
    format!("{:064x}", state.next_id)
}
