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
//! It enforces what the launcher promises: a call that changes a container names its full ID,
//! never a name, and a list without `all` hides every container that is not running. What a
//! started container does (print, exit, or wait at a gate before it starts) is scripted by name.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

use tokio::sync::{Notify, mpsc, oneshot};

use super::{
    ContainerInfo, ContainerSummary, DockerApi, DockerError, ExitWait, ImageInfo, MountInfo,
    NetworkInfo, Output, OutputStream,
};
use crate::spec::ContainerSpec;

/// The exit code of a container stopped by `SIGTERM`.
pub const SIGTERM_EXIT: i64 = 143;

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
    pub image: Option<String>,
    pub health: Option<String>,
    pub mounts: Vec<MountInfo>,
    pub exit_code: Option<i64>,
    /// `(dir, archive)` of every upload, in order.
    pub uploads: Vec<(String, Vec<u8>)>,
}

/// What a container named so does when started.
#[derive(Debug, Clone, Default)]
pub struct Script {
    /// Printed at start, to whoever is attached.
    pub output: Vec<Output>,
    /// Exits with this code right after printing; `None` keeps running.
    pub exit: Option<i64>,
    /// The start waits here first, as long as the test wants.
    pub gate: Option<Arc<Notify>>,
}

/// One call, as the code under test made it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Call {
    List { label: String, all: bool },
    Inspect(String),
    Create(String),
    Remove(String),
    InspectImage(String),
    InspectNetwork(String),
    DockerRoot,
    Upload(String),
    Attach(String),
    Wait(String),
    Start(String),
    Stop(String),
}

/// Which call an injected failure applies to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    List,
    Inspect,
    Create,
    Remove,
    InspectImage,
    InspectNetwork,
    DockerRoot,
    Upload,
    Attach,
    Wait,
    Start,
    Stop,
}

#[derive(Debug, Default)]
struct State {
    containers: Vec<Container>,
    images: HashMap<String, HashMap<String, String>>,
    /// `(name, network)`.
    networks: Vec<(String, NetworkInfo)>,
    docker_root: String,
    scripts: Vec<(String, Script)>,
    outputs: HashMap<String, mpsc::Sender<Result<Output, DockerError>>>,
    /// Output streams kept open past their container's exit.
    held: Vec<mpsc::Sender<Result<Output, DockerError>>>,
    waiters: HashMap<String, Vec<oneshot::Sender<Result<i64, DockerError>>>>,
    calls: Vec<Call>,
    /// Every create request that succeeded, in order.
    created: Vec<ContainerSpec>,
    /// `(op, target, error)`: the next call of `op` on `target` (an ID, a name, or `""` for any)
    /// fails with `error`, once.
    failures: Vec<(Op, String, DockerError)>,
    next_id: u64,
}

impl State {
    fn container(&mut self, id: &str) -> Result<&mut Container, DockerError> {
        self.containers
            .iter_mut()
            .find(|container| container.id == id)
            .ok_or(DockerError::NotFound)
    }

    /// The container exits with `code`: its waiters answer, its output ends.
    fn finish(&mut self, id: &str, code: i64) {
        if let Ok(container) = self.container(id) {
            container.status = Status::Exited;
            container.exit_code = Some(code);
        }
        self.outputs.remove(id);
        for waiter in self.waiters.remove(id).unwrap_or_default() {
            let _ = waiter.send(Ok(code));
        }
    }
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
            image: None,
            health: None,
            mounts: Vec::new(),
            exit_code: None,
            uploads: Vec::new(),
        });
        id
    }

    /// Adds an image.
    pub fn add_image(&self, id: &str, labels: &[(&str, &str)]) {
        self.state().images.insert(
            id.to_owned(),
            labels
                .iter()
                .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
                .collect(),
        );
    }

    /// Adds a network.
    pub fn add_network(&self, name: &str, id: &str, default_bridge: bool) {
        self.state().networks.push((
            name.to_owned(),
            NetworkInfo {
                id: id.to_owned(),
                default_bridge,
            },
        ));
    }

    /// Sets the engine's data root.
    pub fn set_docker_root(&self, root: &str) {
        root.clone_into(&mut self.state().docker_root);
    }

    /// Scripts every container whose name starts with `prefix`.
    pub fn script(&self, prefix: &str, script: Script) {
        self.state().scripts.push((prefix.to_owned(), script));
    }

    /// Changes a container.
    ///
    /// # Panics
    ///
    /// When there is no container `id`.
    pub fn change(&self, id: &str, change: impl FnOnce(&mut Container)) {
        change(self.state().container(id).unwrap());
    }

    /// Makes inspect of `id` report no state.
    pub fn drop_state(&self, id: &str) {
        self.change(id, |container| container.no_state = true);
    }

    /// Sets the status of `id`.
    pub fn set_status(&self, id: &str, status: Status) {
        self.change(id, |container| container.status = status);
    }

    /// Makes a running container exit with `code`, as if it ended by itself.
    pub fn exit(&self, id: &str, code: i64) {
        self.state().finish(id, code);
    }

    /// Ends a container's output stream while it keeps running.
    pub fn cut_output(&self, id: &str) {
        self.state().outputs.remove(id);
    }

    /// Keeps the output stream of `id` open, silent, after the container exits: an attach that
    /// stalls.
    pub fn hold_output(&self, id: &str) {
        let mut state = self.state();
        if let Some(sender) = state.outputs.get(id).cloned() {
            state.held.push(sender);
        }
    }

    /// Breaks the output stream of `id` with `error`.
    pub fn fail_output(&self, id: &str, error: DockerError) {
        if let Some(sender) = self.state().outputs.remove(id) {
            let _ = sender.try_send(Err(error));
        }
    }

    /// Fails every wait registered on `id` with `error`, the container still running.
    pub fn fail_wait(&self, id: &str, error: &DockerError) {
        for waiter in self.state().waiters.remove(id).unwrap_or_default() {
            let _ = waiter.send(Err(error.clone()));
        }
    }

    /// Sends output to whoever is attached to `id`.
    pub fn print(&self, id: &str, output: Output) {
        if let Some(sender) = self.state().outputs.get(id) {
            let _ = sender.try_send(Ok(output));
        }
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

    /// Every create request that succeeded so far, removed containers included.
    #[must_use]
    pub fn created(&self) -> Vec<ContainerSpec> {
        self.state().created.clone()
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
            image: container.image.clone(),
            health: container.health.clone(),
            mounts: container.mounts.clone(),
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
        state.created.push(spec.clone());
        state.containers.push(Container {
            id: id.clone(),
            name: spec.name.clone(),
            labels: spec.body.labels.clone().unwrap_or_default(),
            status: Status::Created,
            no_state: false,
            spec: Some(spec.clone()),
            image: spec.body.image.clone(),
            health: None,
            mounts: Vec::new(),
            exit_code: None,
            uploads: Vec::new(),
        });
        Ok(id)
    }

    async fn remove_container(&self, id: &str) -> Result<(), DockerError> {
        tokio::task::yield_now().await;
        assert_full_id(id, "remove");
        let mut state = self.enter(Call::Remove(id.to_owned()), Op::Remove, id)?;
        if state.container(id)?.status == Status::Running {
            state.finish(id, 137);
        }
        state.containers.retain(|container| container.id != id);
        Ok(())
    }

    async fn inspect_image(&self, id: &str) -> Result<ImageInfo, DockerError> {
        tokio::task::yield_now().await;
        let state = self.enter(Call::InspectImage(id.to_owned()), Op::InspectImage, id)?;
        let labels = state.images.get(id).ok_or(DockerError::NotFound)?;
        Ok(ImageInfo {
            id: id.to_owned(),
            labels: labels.clone(),
        })
    }

    /// Resolves as Docker does: the full ID, then the name, then a unique ID prefix.
    async fn inspect_network(&self, name_or_id: &str) -> Result<NetworkInfo, DockerError> {
        tokio::task::yield_now().await;
        let state = self.enter(
            Call::InspectNetwork(name_or_id.to_owned()),
            Op::InspectNetwork,
            name_or_id,
        )?;
        let find = |matches: &dyn Fn(&(String, NetworkInfo)) -> bool| {
            let found: Vec<&NetworkInfo> = state
                .networks
                .iter()
                .filter(|network| matches(network))
                .map(|(_, network)| network)
                .collect();
            match found.as_slice() {
                [network] => Some((*network).clone()),
                _ => None,
            }
        };
        find(&|(_, network)| network.id == name_or_id)
            .or_else(|| find(&|(name, _)| name == name_or_id))
            .or_else(|| find(&|(_, network)| network.id.starts_with(name_or_id)))
            .ok_or(DockerError::NotFound)
    }

    async fn docker_root(&self) -> Result<String, DockerError> {
        tokio::task::yield_now().await;
        let state = self.enter(Call::DockerRoot, Op::DockerRoot, "")?;
        Ok(state.docker_root.clone())
    }

    async fn upload(&self, id: &str, dir: &str, archive: Vec<u8>) -> Result<(), DockerError> {
        tokio::task::yield_now().await;
        assert_full_id(id, "upload");
        let mut state = self.enter(Call::Upload(id.to_owned()), Op::Upload, id)?;
        state.container(id)?.uploads.push((dir.to_owned(), archive));
        Ok(())
    }

    async fn attach(&self, id: &str) -> Result<OutputStream, DockerError> {
        tokio::task::yield_now().await;
        assert_full_id(id, "attach");
        let mut state = self.enter(Call::Attach(id.to_owned()), Op::Attach, id)?;
        let exited = state.container(id)?.status == Status::Exited;
        let (sender, receiver) = mpsc::channel(64);
        if !exited {
            state.outputs.insert(id.to_owned(), sender);
        }
        Ok(receiver)
    }

    async fn wait(&self, id: &str) -> Result<ExitWait, DockerError> {
        tokio::task::yield_now().await;
        assert_full_id(id, "wait");
        let mut state = self.enter(Call::Wait(id.to_owned()), Op::Wait, id)?;
        let container = state.container(id)?;
        let (sender, receiver) = oneshot::channel();
        if container.status == Status::Running {
            state.waiters.entry(id.to_owned()).or_default().push(sender);
        } else {
            let _ = sender.send(Ok(container.exit_code.unwrap_or(0)));
        }
        Ok(receiver)
    }

    async fn start_container(&self, id: &str) -> Result<(), DockerError> {
        tokio::task::yield_now().await;
        assert_full_id(id, "start");
        let script = {
            let mut state = self.enter(Call::Start(id.to_owned()), Op::Start, id)?;
            let name = state.container(id)?.name.clone();
            state
                .scripts
                .iter()
                .rev()
                .find(|(prefix, _)| name.starts_with(prefix.as_str()))
                .map(|(_, script)| script.clone())
                .unwrap_or_default()
        };
        if let Some(gate) = &script.gate {
            gate.notified().await;
        }
        let mut state = self.state();
        state.container(id)?.status = Status::Running;
        if let Some(sender) = state.outputs.get(id) {
            for output in script.output {
                let _ = sender.try_send(Ok(output));
            }
        }
        if let Some(code) = script.exit {
            state.finish(id, code);
        }
        Ok(())
    }

    async fn stop_container(&self, id: &str, _timeout_secs: i32) -> Result<(), DockerError> {
        tokio::task::yield_now().await;
        assert_full_id(id, "stop");
        let mut state = self.enter(Call::Stop(id.to_owned()), Op::Stop, id)?;
        if state.container(id)?.status == Status::Running {
            state.finish(id, SIGTERM_EXIT);
        }
        Ok(())
    }
}

fn assert_full_id(id: &str, call: &str) {
    assert!(
        id.len() == 64 && id.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "{call} by {id:?}: the launcher names containers by full ID only"
    );
}

fn next_id(state: &mut State) -> String {
    state.next_id += 1;
    format!("{:064x}", state.next_id)
}
