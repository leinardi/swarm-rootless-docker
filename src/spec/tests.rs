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

use std::collections::{HashMap, HashSet};
use std::io::Read as _;

use serde_json::{Value, json};

use super::*;
use crate::config::{
    ExtraMount, HostPath, InstanceName, LaunchValues, MemoryBytes, NanoCpus, NetworkName, PidsLimit,
};
use crate::mounts::{Device, Placement};

const IMAGE_ID: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const LAUNCHER_ID: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

fn values() -> LaunchValues {
    LaunchValues {
        instance: InstanceName::parse("herdquay").unwrap(),
        subuid: IdRange::parse("100000:65536").unwrap(),
        subgid: Some(IdRange::parse("200000:65536").unwrap()),
        data_root: HostPath::parse("/mnt/pool/docker/herdquay-docker/data").unwrap(),
        socket_dir: HostPath::parse("/mnt/pool/docker/herdquay-docker/socket").unwrap(),
        mounts: vec![
            ExtraMount::parse("/mnt/pool/docker/herdquay/workspace:/home/herdquay/workspace")
                .unwrap(),
            ExtraMount::parse("/mnt/pool/docker/herdquay/cache:/cache:ro").unwrap(),
        ],
        network: NetworkName::parse("herdquay_docker").unwrap(),
        aliases: vec![
            NetworkAlias::parse("docker").unwrap(),
            NetworkAlias::parse("dind").unwrap(),
        ],
        memory: MemoryBytes::parse("8g").unwrap(),
        pids_limit: PidsLimit::parse("8192").unwrap(),
        cpus: Some(NanoCpus::parse("4").unwrap()),
    }
}

fn config() -> LaunchConfig {
    LaunchConfig::new(values()).unwrap()
}

fn minimal_config() -> LaunchConfig {
    LaunchConfig::new(LaunchValues {
        subgid: None,
        mounts: Vec::new(),
        aliases: Vec::new(),
        cpus: None,
        ..values()
    })
    .unwrap()
}

fn image_labels(uid: &str, gid: &str) -> HashMap<String, String> {
    HashMap::from([
        (LABEL_IMAGE_UID.to_owned(), uid.to_owned()),
        (LABEL_IMAGE_GID.to_owned(), gid.to_owned()),
    ])
}

fn image() -> ImageIdentity {
    ImageIdentity::new(IMAGE_ID, &image_labels("568", "568")).unwrap()
}

fn launcher() -> ContainerId {
    ContainerId::parse(LAUNCHER_ID).unwrap()
}

fn file_id(inode: u64) -> FileId {
    FileId { device: 42, inode }
}

/// `path` at its own position on filesystem 0:52, as file `inode`.
fn location(path: &str, inode: u64) -> HostLocation {
    HostLocation {
        placement: Placement::new(
            Device {
                major: 0,
                minor: 52,
            },
            path,
        )
        .unwrap(),
        file: file_id(inode),
    }
}

/// The JSON [`daemon_labels`] must write for [`location`], spelled out.
fn entry_json(path: &str, inode: u64) -> String {
    format!(
        r#"{{"path":"{path}","device":{{"major":0,"minor":52}},"fs_path":"{path}","file":{{"device":42,"inode":{inode}}}}}"#
    )
}

fn storage(config: &LaunchConfig) -> StorageIds {
    let mounts = config
        .mounts()
        .iter()
        .enumerate()
        .map(|(index, mount)| {
            location(
                mount.source().path().as_str(),
                300 + u64::try_from(index).unwrap(),
            )
        })
        .collect();
    StorageIds::new(
        config,
        location(config.data_root().path().as_str(), 100),
        location(config.socket_dir().path().as_str(), 200),
        mounts,
    )
    .unwrap()
}

fn wire(spec: &ContainerSpec) -> Value {
    serde_json::to_value(&spec.body).unwrap()
}

/// The profile every daemon gets, whatever the flags, plus the per-instance network, mounts and
/// limits.
fn daemon_host_config(mounts: &Value, limits: &[(&str, Value)]) -> Value {
    let mut host_config = json!({
        "Privileged": false,
        "CapDrop": ["ALL"],
        "CapAdd": ["SETUID", "SETGID"],
        "SecurityOpt": [
            "seccomp=unconfined",
            "apparmor=unconfined"
        ],
        "MaskedPaths": [],
        "ReadonlyPaths": [],
        "Devices": [{
            "PathOnHost": "/dev/net/tun",
            "PathInContainer": "/dev/net/tun",
            "CgroupPermissions": "rwm"
        }],
        "IpcMode": "private",
        "CgroupnsMode": "private",
        "NetworkMode": "herdquay_docker",
        "ReadonlyRootfs": false,
        "Init": false,
        "RestartPolicy": {"Name": "no"},
        "AutoRemove": false,
        "Mounts": mounts,
        "LogConfig": {"Type": "local", "Config": {"max-size": "10m", "max-file": "3"}}
    });
    for (key, value) in limits {
        host_config[*key] = value.clone();
    }
    host_config
}

fn bind_json(source: &str, target: &str, read_only: bool) -> Value {
    json!({
        "Source": source,
        "Target": target,
        "Type": "bind",
        "ReadOnly": read_only,
        "BindOptions": {"Propagation": "rprivate"}
    })
}

fn fixed_tmpfs_json() -> [Value; 2] {
    [
        json!({
            "Target": "/var/lib/docker",
            "Type": "tmpfs",
            "TmpfsOptions": {"SizeBytes": 16_777_216, "Mode": 0o755}
        }),
        json!({
            "Target": "/run/user",
            "Type": "tmpfs",
            "TmpfsOptions": {"SizeBytes": 67_108_864, "Mode": 0o1777}
        }),
    ]
}

fn daemon_body(labels: &Value, host_config: &Value, networking: Option<Value>) -> Value {
    let mut body = json!({
        "Hostname": "swarm-rootless-docker-daemon-herdquay",
        "Image": IMAGE_ID,
        "Entrypoint": ["/usr/local/bin/swarm-rootless-docker"],
        "Cmd": ["daemon"],
        "User": "568:568",
        "Labels": labels,
        "Healthcheck": {
            "Test": ["CMD", "/usr/local/bin/swarm-rootless-docker", "daemon-health"],
            "Interval": 10_000_000_000_i64,
            "Timeout": 5_000_000_000_i64,
            "StartPeriod": 60_000_000_000_i64,
            "Retries": 3
        },
        "StopSignal": "SIGTERM",
        "StopTimeout": 15,
        "HostConfig": host_config
    });
    if let Some(networking) = networking {
        body["NetworkingConfig"] = networking;
    }
    body
}

/// The whole daemon create request, field by field as it goes on the wire: anything added,
/// dropped or changed fails here. Port of swarm-device-access `TestDaemonSpec`.
#[test]
fn daemon_spec_is_pinned() {
    let config = config();
    let spec = daemon_spec(&config, &image(), &launcher(), &storage(&config)).unwrap();
    assert_eq!(spec.name, "swarm-rootless-docker-daemon-herdquay");

    let [var_lib_docker, run_user] = fixed_tmpfs_json();
    let mounts = json!([
        bind_json(
            "/mnt/pool/docker/herdquay-docker/data",
            "/home/rootless/.local/share/docker",
            false
        ),
        bind_json(
            "/mnt/pool/docker/herdquay-docker/socket",
            "/home/rootless/socket",
            false
        ),
        bind_json(
            "/mnt/pool/docker/herdquay/workspace",
            "/home/herdquay/workspace",
            false
        ),
        bind_json("/mnt/pool/docker/herdquay/cache", "/cache", true),
        var_lib_docker,
        run_user,
    ]);
    let labels = json!({
        "io.github.leinardi.swarm-rootless-docker.role": "daemon",
        "io.github.leinardi.swarm-rootless-docker.launcher": LAUNCHER_ID,
        "io.github.leinardi.swarm-rootless-docker.instance": "herdquay",
        "io.github.leinardi.swarm-rootless-docker.subuid": "100000:65536",
        "io.github.leinardi.swarm-rootless-docker.subgid": "200000:65536",
        "io.github.leinardi.swarm-rootless-docker.storage": format!(
            "[{},{},{},{}]",
            entry_json("/mnt/pool/docker/herdquay-docker/data", 100),
            entry_json("/mnt/pool/docker/herdquay-docker/socket", 200),
            entry_json("/mnt/pool/docker/herdquay/workspace", 300),
            entry_json("/mnt/pool/docker/herdquay/cache", 301),
        ),
        "io.github.leinardi.swarm-rootless-docker.network": "herdquay_docker",
        "io.github.leinardi.swarm-rootless-docker.aliases": "docker,dind",
    });
    let host_config = daemon_host_config(
        &mounts,
        &[
            ("Memory", json!(8_589_934_592_i64)),
            ("MemorySwap", json!(8_589_934_592_i64)),
            ("PidsLimit", json!(8192)),
            ("NanoCpus", json!(4_000_000_000_i64)),
        ],
    );
    let networking = json!({
        "EndpointsConfig": {"herdquay_docker": {"Aliases": ["docker", "dind"]}}
    });
    assert_eq!(
        wire(&spec),
        daemon_body(&labels, &host_config, Some(networking))
    );
}

/// Without mounts, aliases or `--cpus`: only the fixed mounts, no `NetworkingConfig`, no CPU
/// limit, and `--subgid` defaults to `--subuid`.
#[test]
fn daemon_spec_without_optional_flags_is_pinned() {
    let config = minimal_config();
    let spec = daemon_spec(&config, &image(), &launcher(), &storage(&config)).unwrap();

    let [var_lib_docker, run_user] = fixed_tmpfs_json();
    let mounts = json!([
        bind_json(
            "/mnt/pool/docker/herdquay-docker/data",
            "/home/rootless/.local/share/docker",
            false
        ),
        bind_json(
            "/mnt/pool/docker/herdquay-docker/socket",
            "/home/rootless/socket",
            false
        ),
        var_lib_docker,
        run_user,
    ]);
    let labels = json!({
        "io.github.leinardi.swarm-rootless-docker.role": "daemon",
        "io.github.leinardi.swarm-rootless-docker.launcher": LAUNCHER_ID,
        "io.github.leinardi.swarm-rootless-docker.instance": "herdquay",
        "io.github.leinardi.swarm-rootless-docker.subuid": "100000:65536",
        "io.github.leinardi.swarm-rootless-docker.subgid": "100000:65536",
        "io.github.leinardi.swarm-rootless-docker.storage": format!(
            "[{},{}]",
            entry_json("/mnt/pool/docker/herdquay-docker/data", 100),
            entry_json("/mnt/pool/docker/herdquay-docker/socket", 200),
        ),
        "io.github.leinardi.swarm-rootless-docker.network": "herdquay_docker",
        "io.github.leinardi.swarm-rootless-docker.aliases": "",
    });
    let host_config = daemon_host_config(
        &mounts,
        &[
            ("Memory", json!(8_589_934_592_i64)),
            ("MemorySwap", json!(8_589_934_592_i64)),
            ("PidsLimit", json!(8192)),
        ],
    );
    assert_eq!(wire(&spec), daemon_body(&labels, &host_config, None));
}

/// A path is a JSON string in the storage label, so a quote or backslash in it cannot break the
/// label apart.
#[test]
fn storage_label_escapes_paths() {
    let config = LaunchConfig::new(LaunchValues {
        data_root: HostPath::parse(r#"/mnt/a"b\c"#).unwrap(),
        mounts: Vec::new(),
        ..values()
    })
    .unwrap();
    let labels = daemon_labels(&config, &launcher(), &storage(&config)).unwrap();
    let entries: Value = serde_json::from_str(&labels[LABEL_STORAGE]).unwrap();
    assert_eq!(entries[0]["path"], r#"/mnt/a"b\c"#);
}

/// The probe create request, whole. Its one argument is the request, as JSON.
#[test]
fn probe_spec_is_pinned() {
    let config = config();
    let image = image();
    let denied = [AbsolutePath::parse("/mnt/.ix-apps/docker").unwrap()];
    let request = ProbeRequest::new(&config, &image, &denied);
    let spec = probe_spec(&config, &image, &launcher(), &request).unwrap();
    assert_eq!(spec.name, "swarm-rootless-docker-probe-herdquay");

    let argument = concat!(
        r#"{"data_root":"/mnt/pool/docker/herdquay-docker/data","#,
        r#""socket_dir":"/mnt/pool/docker/herdquay-docker/socket","#,
        r#""mount_sources":["/mnt/pool/docker/herdquay/workspace","/mnt/pool/docker/herdquay/cache"],"#,
        r#""denied":["/mnt/.ix-apps/docker"],"#,
        r#""owner_uid":568,"#,
        r#""subuid":{"start":100000,"count":65536},"#,
        r#""subgid":{"start":200000,"count":65536}}"#
    );
    let expected = json!({
        "Image": IMAGE_ID,
        "Entrypoint": ["/usr/local/bin/swarm-rootless-docker"],
        "Cmd": ["probe", argument],
        "User": "0:0",
        "Labels": {
            "io.github.leinardi.swarm-rootless-docker.role": "probe",
            "io.github.leinardi.swarm-rootless-docker.launcher": LAUNCHER_ID
        },
        "Healthcheck": {"Test": ["NONE"]},
        "NetworkDisabled": true,
        "StopSignal": "SIGTERM",
        "StopTimeout": 1,
        "HostConfig": {
            "Privileged": false,
            "CapDrop": ["ALL"],
            "CapAdd": ["DAC_READ_SEARCH"],
            "SecurityOpt": ["no-new-privileges"],
            "IpcMode": "private",
            "CgroupnsMode": "private",
            "NetworkMode": "none",
            "ReadonlyRootfs": true,
            "Init": false,
            "RestartPolicy": {"Name": "no"},
            "AutoRemove": false,
            "Mounts": [{
                "Source": "/",
                "Target": "/host",
                "Type": "bind",
                "ReadOnly": true,
                "BindOptions": {"ReadOnlyForceRecursive": true}
            }],
            "Memory": 67_108_864,
            "MemorySwap": 67_108_864,
            "PidsLimit": 64,
            "LogConfig": {"Type": "none"}
        }
    });
    assert_eq!(wire(&spec), expected);
}

/// The lock create request, whole: never started, and harmless if it were.
#[test]
fn lock_spec_is_pinned() {
    let spec = lock_spec(&image(), &launcher());
    assert_eq!(spec.name, "swarm-rootless-docker-lock");
    let expected = json!({
        "Image": IMAGE_ID,
        "Entrypoint": ["/usr/local/bin/swarm-rootless-docker"],
        "Cmd": [],
        "User": "568:568",
        "Labels": {
            "io.github.leinardi.swarm-rootless-docker.role": "lock",
            "io.github.leinardi.swarm-rootless-docker.launcher": LAUNCHER_ID
        },
        "Healthcheck": {"Test": ["NONE"]},
        "NetworkDisabled": true,
        "HostConfig": {
            "Privileged": false,
            "CapDrop": ["ALL"],
            "SecurityOpt": ["no-new-privileges"],
            "NetworkMode": "none",
            "ReadonlyRootfs": true,
            "RestartPolicy": {"Name": "no"},
            "AutoRemove": false,
            "LogConfig": {"Type": "none"}
        }
    });
    assert_eq!(wire(&spec), expected);
}

/// No instance name gives a daemon or probe the lock's name, or another instance's or role's
/// name, instance names that look like the infixes included.
#[test]
fn container_names_are_disjoint() {
    let mut names = HashSet::from([LOCK_NAME.to_owned()]);
    for instance in [
        "lock",
        "x",
        "probe-x",
        "daemon-x",
        "daemon-probe-x",
        "probe-daemon-x",
    ] {
        let config = LaunchConfig::new(LaunchValues {
            instance: InstanceName::parse(instance).unwrap(),
            ..values()
        })
        .unwrap();
        for name in [daemon_name(&config), probe_name(&config)] {
            assert!(names.insert(name.clone()), "{name} produced twice");
        }
    }
}

#[test]
fn image_identity() {
    let image = image();
    assert_eq!((image.id(), image.uid(), image.gid()), (IMAGE_ID, 568, 568));
    let short = &IMAGE_ID[..IMAGE_ID.len() - 1];
    let upper = IMAGE_ID.to_uppercase().replace("SHA256", "sha256");
    let unprefixed = IMAGE_ID.trim_start_matches("sha256:");
    for bad in ["", short, &upper, unprefixed, "sha512:aa"] {
        assert!(
            matches!(
                ImageIdentity::new(bad, &image_labels("568", "568")),
                Err(SpecError::ImageId(_))
            ),
            "{bad:?} accepted"
        );
    }
    for bad in [
        "",
        "0",
        "4294967295",
        "4294967296",
        "-1",
        "+5",
        " 5",
        "5a",
        "0x10",
    ] {
        for labels in [image_labels(bad, "568"), image_labels("568", bad)] {
            assert!(
                matches!(
                    ImageIdentity::new(IMAGE_ID, &labels),
                    Err(SpecError::LabelInvalid { .. })
                ),
                "{bad:?} accepted"
            );
        }
    }
    for label in [LABEL_IMAGE_UID, LABEL_IMAGE_GID] {
        let mut labels = image_labels("568", "568");
        labels.remove(label);
        assert!(matches!(
            ImageIdentity::new(IMAGE_ID, &labels),
            Err(SpecError::LabelMissing(missing)) if missing == label
        ));
    }
}

/// A range that contains the daemon's own UID or GID is refused, naming the flag.
#[test]
fn ranges_exclude_the_image_ids() {
    let config = config();
    image().check_ranges(&config).unwrap();
    let owner_in_subuid = ImageIdentity::new(IMAGE_ID, &image_labels("100000", "568")).unwrap();
    let error = owner_in_subuid.check_ranges(&config).unwrap_err();
    assert!(matches!(
        error,
        SpecError::RangeContainsImageId {
            flag: "subuid",
            id: 100_000,
            ..
        }
    ));
    let group_in_subgid = ImageIdentity::new(IMAGE_ID, &image_labels("568", "265535")).unwrap();
    let error = group_in_subgid.check_ranges(&config).unwrap_err();
    assert!(matches!(
        error,
        SpecError::RangeContainsImageId {
            flag: "subgid",
            id: 265_535,
            ..
        }
    ));
    // Just outside both ranges.
    ImageIdentity::new(IMAGE_ID, &image_labels("165536", "199999"))
        .unwrap()
        .check_ranges(&config)
        .unwrap();
}

#[test]
fn container_ids() {
    assert_eq!(launcher().as_str(), LAUNCHER_ID);
    let long = format!("{LAUNCHER_ID}b");
    let upper = LAUNCHER_ID.to_uppercase();
    for bad in ["", &LAUNCHER_ID[..12], &upper, &long] {
        assert!(ContainerId::parse(bad).is_err(), "{bad:?} accepted");
    }
}

#[test]
fn storage_ids_need_one_location_per_mount() {
    let config = config();
    let error = StorageIds::new(
        &config,
        location("/a", 1),
        location("/b", 2),
        vec![location("/c", 3)],
    )
    .unwrap_err();
    assert!(matches!(
        error,
        SpecError::StorageCount {
            expected: 2,
            actual: 1
        }
    ));
}

/// Locations overlap by filesystem position (see `mounts`) or as the same file: a hard link
/// elsewhere on the filesystem is the same file.
#[test]
fn host_locations_overlap() {
    let tree = location("/srv/work/tree", 12);
    assert!(tree.overlaps(&location("/srv/work/tree/subdir", 13)));
    assert!(tree.overlaps(&location("/srv/work", 11)));
    assert!(tree.overlaps(&location("/data/hard-link", 12)));
    assert!(!tree.overlaps(&location("/srv/work/other", 14)));
    let elsewhere = HostLocation {
        placement: Placement::new(
            Device {
                major: 0,
                minor: 53,
            },
            "/srv/work/tree",
        )
        .unwrap(),
        file: FileId {
            device: 53,
            inode: 12,
        },
    };
    assert!(!tree.overlaps(&elsewhere));
}

/// The uploaded files: exactly `subuid` and `subgid`, regular, root-owned, 0644, one line each.
#[test]
fn subid_archive_contents() {
    let archive = subid_archive(&config()).unwrap();
    let mut archive = tar::Archive::new(archive.as_slice());
    let mut files = Vec::new();
    for entry in archive.entries().unwrap() {
        let mut entry = entry.unwrap();
        let header = entry.header();
        assert_eq!(header.entry_type(), tar::EntryType::Regular);
        assert_eq!(
            (
                header.uid().unwrap(),
                header.gid().unwrap(),
                header.mode().unwrap()
            ),
            (0, 0, 0o644)
        );
        let path = entry.path().unwrap().to_string_lossy().into_owned();
        let mut content = String::new();
        entry.read_to_string(&mut content).unwrap();
        files.push((path, content));
    }
    assert_eq!(
        files,
        [
            ("subuid".to_owned(), "rootless:100000:65536\n".to_owned()),
            ("subgid".to_owned(), "rootless:200000:65536\n".to_owned()),
        ]
    );
}
