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
//! The crates whose types cross rust-common's public API are one package each in this workspace's
//! graph, and the ones rust-common re-exports are taken through the re-export, not declared here:
//! bollard as `docker_client::bollard`, tracing-subscriber as `tracing_setup::tracing_subscriber`.
//! Two copies compile apart and fail at the boundary with E0277 or E0308. The check is
//! `repo_meta::boundary`'s, on `cargo metadata --locked --all-features` (CI runs
//! `cargo fetch --locked` first); the negative controls plant a split and a declaration into the
//! real metadata.

use repo_meta::boundary::{TABLE, check, check_metadata, metadata};
use repo_meta::git_pins::RUST_COMMON_URL;
use repo_meta::yaml::Value;
use repo_meta::{report, strings};

const DAEMON: &str = "swarm-rootless-docker";

fn real_metadata() -> Value {
    metadata(meta::repo().root()).unwrap_or_else(|problem| panic!("{problem}"))
}

fn array<'value>(value: &'value mut Value, key: &str) -> &'value mut Vec<Value> {
    match value.get_mut(key) {
        Some(Value::Array(items)) => items,
        _ => panic!("cargo metadata has no {key} array"),
    }
}

/// The one package named `name`, cloned.
fn package(metadata: &Value, name: &str) -> Value {
    let found: Vec<&Value> = metadata["packages"]
        .as_array()
        .unwrap_or_else(|| panic!("cargo metadata has no packages array"))
        .iter()
        .filter(|package| package["name"] == name)
        .collect();
    assert_eq!(found.len(), 1, "want one {name} package");
    found[0].clone()
}

fn node_mut<'value>(metadata: &'value mut Value, id: &Value) -> &'value mut Value {
    array(array_parent(metadata), "nodes")
        .iter_mut()
        .find(|node| node["id"] == *id)
        .unwrap_or_else(|| panic!("no resolve node {id}"))
}

fn array_parent(metadata: &mut Value) -> &mut Value {
    metadata
        .get_mut("resolve")
        .unwrap_or_else(|| panic!("cargo metadata has no resolve graph"))
}

#[test]
fn every_boundary_crate_is_one_package_and_none_is_declared_beside_its_reexport() {
    let problems = check(meta::repo().root(), RUST_COMMON_URL, &[]);
    assert!(problems.is_empty(), "{}", report(&problems));
}

/// The launcher's own tracing-subscriber 0.3.19 beside tracing-setup's: named, with both copies
/// and the re-export to take it through.
#[test]
fn a_tracing_subscriber_split_is_named() {
    let mut metadata = real_metadata();
    let ours = package(&metadata, "tracing-subscriber");
    let mut theirs = ours.clone();
    let id = Value::String(
        "registry+https://github.com/rust-lang/crates.io-index#tracing-subscriber@0.3.19".into(),
    );
    theirs["id"] = id.clone();
    theirs["version"] = Value::String("0.3.19".into());
    array(&mut metadata, "packages").push(theirs);
    let mut node = node_mut(&mut metadata, &ours["id"]).clone();
    node["id"] = id.clone();
    array(array_parent(&mut metadata), "nodes").push(node);
    let daemon = package(&metadata, DAEMON);
    let daemon_node = node_mut(&mut metadata, &daemon["id"]);
    let mut edge = array(daemon_node, "deps")[0].clone();
    edge["name"] = Value::String("tracing_subscriber".into());
    edge["pkg"] = id;
    array(daemon_node, "deps").push(edge);

    let problems = strings(&check_metadata(&metadata, TABLE, RUST_COMMON_URL, &[]));
    assert_eq!(problems.len(), 1, "{problems:#?}");
    let problem = &problems[0];
    assert!(
        problem.starts_with(&format!(
            "2 copies of tracing-subscriber: 0.3.19 (crates.io) via {DAEMON} -> tracing-subscriber; \
             {} (crates.io), rust-common's, via {DAEMON} -> tracing-setup -> tracing-subscriber;",
            ours["version"].as_str().expect("version")
        )),
        "{problem}"
    );
    assert!(
        problem.ends_with("(E0277/E0308); take it through tracing_setup::tracing_subscriber"),
        "{problem}"
    );
}

/// bollard declared by the launcher, at the very package rust-common uses: refused all the same,
/// as a direct declaration of a re-exported crate.
#[test]
fn a_direct_bollard_declaration_is_refused() {
    let mut metadata = real_metadata();
    let packages = array(&mut metadata, "packages");
    let daemon = packages
        .iter_mut()
        .find(|package| package["name"] == DAEMON)
        .expect("the daemon package");
    let declared = array(daemon, "dependencies");
    let mut dependency = declared[0].clone();
    dependency["name"] = Value::String("bollard".into());
    dependency["rename"] = Value::Null;
    dependency["kind"] = Value::Null;
    dependency["target"] = Value::Null;
    declared.push(dependency);

    assert_eq!(
        strings(&check_metadata(&metadata, TABLE, RUST_COMMON_URL, &[])),
        [format!(
            "{DAEMON} declares bollard directly (normal dependency); take it through \
             docker_client::bollard, or pass an Exception with the reason"
        )]
    );
}
