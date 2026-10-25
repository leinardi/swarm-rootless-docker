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

use super::*;

/// One edit of the base values, for the table tests.
type Change = fn(&mut LaunchValues);

fn values() -> LaunchValues {
    LaunchValues {
        instance: InstanceName::parse("herdquay").unwrap(),
        subuid: IdRange::parse("100000:65536").unwrap(),
        subgid: None,
        data_root: HostPath::parse("/mnt/pool/docker/herdquay-docker/data").unwrap(),
        socket_dir: HostPath::parse("/mnt/pool/docker/herdquay-docker/socket").unwrap(),
        mounts: vec![
            ExtraMount::parse("/mnt/pool/docker/herdquay/workspace:/home/herdquay/workspace")
                .unwrap(),
        ],
        network: NetworkName::parse("herdquay_docker").unwrap(),
        memory: MemoryBytes::parse("8g").unwrap(),
        pids_limit: PidsLimit::parse("8192").unwrap(),
        cpus: Some(NanoCpus::parse("4").unwrap()),
    }
}

#[test]
fn instance_names() {
    for good in ["a", "herdquay", "ci-runner-1", "0", &"a".repeat(32)] {
        assert!(InstanceName::parse(good).is_ok(), "{good:?} refused");
    }
    for bad in [
        "",
        "-a",
        "a-",
        "Herdquay",
        "a_b",
        "a.b",
        "a b",
        "à",
        &"a".repeat(33),
    ] {
        assert_eq!(
            InstanceName::parse(bad),
            Err(ValueError::InstanceName),
            "{bad:?}"
        );
    }
}

#[test]
fn ranges() {
    let range = IdRange::parse("100000:65536").unwrap();
    assert_eq!(
        (range.start(), range.count(), range.last()),
        (100_000, 65_536, 165_535)
    );
    assert!(range.contains(100_000) && range.contains(165_535));
    assert!(!range.contains(99_999) && !range.contains(165_536));
    assert_eq!(range.to_string(), "100000:65536");

    let highest = IdRange::parse(&format!("{}:65536", MAX_RANGE_ID - 65_535)).unwrap();
    assert_eq!(highest.last(), MAX_RANGE_ID);

    for (bad, error) in [
        ("", ValueError::RangeSyntax),
        ("100000", ValueError::RangeSyntax),
        ("100000:", ValueError::RangeSyntax),
        (":65536", ValueError::RangeSyntax),
        ("100000:65536:1", ValueError::RangeSyntax),
        ("+100000:65536", ValueError::RangeSyntax),
        ("100000:-1", ValueError::RangeSyntax),
        (" 100000:65536", ValueError::RangeSyntax),
        ("0x186a0:65536", ValueError::RangeSyntax),
        ("100_000:65536", ValueError::RangeSyntax),
        ("4294967296:65536", ValueError::RangeSyntax),
        ("0:65536", ValueError::RangeStart),
        ("568:65536", ValueError::RangeStart),
        ("99999:65536", ValueError::RangeStart),
        ("100000:65535", ValueError::RangeCount),
        ("100000:0", ValueError::RangeCount),
        ("4294901760:65536", ValueError::RangeEnd),
        ("4294967295:65536", ValueError::RangeEnd),
    ] {
        assert_eq!(IdRange::parse(bad), Err(error), "{bad:?}");
    }
}

#[test]
fn range_overlap() {
    let first = IdRange::parse("100000:65536").unwrap();
    let adjacent = IdRange::parse("165536:65536").unwrap();
    let sharing_one = IdRange::parse("165535:65536").unwrap();
    assert!(!first.overlaps(adjacent) && !adjacent.overlaps(first));
    assert!(first.overlaps(sharing_one) && sharing_one.overlaps(first));
    assert!(first.overlaps(first));
}

#[test]
fn absolute_paths() {
    for good in [
        "/",
        "/mnt",
        "/mnt/pool/a b",
        "/mnt/pool/.hidden",
        "/mnt/..x",
    ] {
        assert!(AbsolutePath::parse(good).is_ok(), "{good:?} refused");
    }
    for (bad, error) in [
        ("", ValueError::NotAbsolute),
        ("mnt/pool", ValueError::NotAbsolute),
        ("./mnt", ValueError::NotAbsolute),
        ("/mnt/", ValueError::NotClean),
        ("//mnt", ValueError::NotClean),
        ("/mnt//pool", ValueError::NotClean),
        ("/mnt/./pool", ValueError::NotClean),
        ("/mnt/../etc", ValueError::NotClean),
        ("/mnt/..", ValueError::NotClean),
        ("/mnt/a:b", ValueError::BadCharacter),
        ("/mnt/a\nb", ValueError::BadCharacter),
        ("/mnt/a\0b", ValueError::BadCharacter),
    ] {
        assert_eq!(AbsolutePath::parse(bad), Err(error), "{bad:?}");
    }
    let long = format!("/{}", "a".repeat(MAX_PATH_LEN));
    assert_eq!(AbsolutePath::parse(&long), Err(ValueError::TooLong));
}

#[test]
fn path_relations_are_component_wise() {
    let path = AbsolutePath::parse("/mnt/pool/data").unwrap();
    assert!(path.is_within("/mnt/pool/data"));
    assert!(path.is_within("/mnt/pool"));
    assert!(path.is_within("/"));
    assert!(!path.is_within("/mnt/po"));
    assert!(!path.is_within("/mnt/pool/data2"));
    assert!(path.overlaps("/mnt/pool/data/inner"));
    assert!(!path.overlaps("/mnt/pool/database"));
}

#[test]
fn host_paths_refuse_system_paths() {
    assert!(HostPath::parse("/mnt/pool/docker/x").is_ok());
    assert!(HostPath::parse("/home/someone/ws").is_ok());
    assert!(HostPath::parse("/var/lib/other").is_ok());
    assert_eq!(HostPath::parse("/"), Err(ValueError::Root));
    for (bad, system) in [
        ("/etc", "/etc"),
        ("/etc/subuid", "/etc"),
        ("/var/run/docker.sock", "/var/run"),
        ("/run/docker.sock", "/run"),
        ("/var", "/var/lib/docker"),
        ("/var/lib", "/var/lib/docker"),
        ("/var/lib/docker/volumes", "/var/lib/docker"),
        ("/proc/1/root", "/proc"),
        ("/sys/fs/cgroup", "/sys"),
        ("/dev/net/tun", "/dev"),
        ("/usr/bin", "/usr"),
        ("/root/.ssh", "/root"),
        ("/boot", "/boot"),
        ("/lib64", "/lib64"),
    ] {
        assert_eq!(
            HostPath::parse(bad),
            Err(ValueError::HostSystemPath(system)),
            "{bad:?}"
        );
    }
}

#[test]
fn container_paths_refuse_reserved_paths() {
    assert!(ContainerPath::parse("/home/herdquay/workspace").is_ok());
    assert!(ContainerPath::parse("/srv/ci").is_ok());
    assert!(ContainerPath::parse("/tmp/build").is_ok());
    assert_eq!(ContainerPath::parse("/"), Err(ValueError::Root));
    for (bad, reserved) in [
        ("/home", "/home/rootless"),
        ("/home/rootless", "/home/rootless"),
        ("/home/rootless/.local/share/docker", "/home/rootless"),
        ("/home/rootless/socket", "/home/rootless"),
        ("/etc/subuid", "/etc"),
        ("/var", "/var/lib/docker"),
        ("/var/lib/docker", "/var/lib/docker"),
        ("/run/user", "/run"),
        ("/usr/local/bin", "/usr"),
        ("/proc", "/proc"),
        ("/dev", "/dev"),
    ] {
        assert_eq!(
            ContainerPath::parse(bad),
            Err(ValueError::ReservedContainerPath(reserved)),
            "{bad:?}"
        );
    }
}

#[test]
fn extra_mounts() {
    let mount = ExtraMount::parse("/mnt/ws:/home/herdquay/workspace").unwrap();
    assert_eq!(mount.source().to_string(), "/mnt/ws");
    assert_eq!(mount.target().to_string(), "/home/herdquay/workspace");
    assert!(!mount.read_only());
    assert!(ExtraMount::parse("/mnt/ws:/ws:ro").unwrap().read_only());
    assert!(!ExtraMount::parse("/mnt/ws:/ws:rw").unwrap().read_only());
    assert_eq!(
        ExtraMount::parse("/mnt/ws:/ws:ro").unwrap().to_string(),
        "/mnt/ws:/ws:ro"
    );

    for bad in [
        "",
        "/mnt/ws",
        "/mnt/ws:",
        "/mnt/ws:/ws:RO",
        "/mnt/ws:/ws:ro:x",
        "/mnt/ws:/ws:z",
    ] {
        assert!(ExtraMount::parse(bad).is_err(), "{bad:?} accepted");
    }
    assert_eq!(
        ExtraMount::parse("/var/run/docker.sock:/var/run/docker.sock"),
        Err(ValueError::MountSource(Box::new(
            ValueError::HostSystemPath("/var/run")
        )))
    );
    assert_eq!(
        ExtraMount::parse("/mnt/ws:/etc"),
        Err(ValueError::MountTarget(Box::new(
            ValueError::ReservedContainerPath("/etc")
        )))
    );
    assert_eq!(
        ExtraMount::parse("relative:/ws"),
        Err(ValueError::MountSource(Box::new(ValueError::NotAbsolute)))
    );
}

#[test]
fn network_names() {
    for good in ["bridge", "herdquay_docker", "swarm.internet-1", "A1"] {
        assert!(NetworkName::parse(good).is_ok(), "{good:?} refused");
    }
    for bad in [
        "",
        "-x",
        "_x",
        "a b",
        "container:abc",
        "a/b",
        &"a".repeat(65),
    ] {
        assert_eq!(
            NetworkName::parse(bad),
            Err(ValueError::NetworkName),
            "{bad:?}"
        );
    }
    for mode in ["host", "none", "HOST", "None"] {
        assert_eq!(
            NetworkName::parse(mode),
            Err(ValueError::NetworkMode),
            "{mode:?}"
        );
    }
}

#[test]
fn memory() {
    assert_eq!(
        MemoryBytes::parse("256m").unwrap().bytes(),
        256 * 1024 * 1024
    );
    assert_eq!(
        MemoryBytes::parse("2G").unwrap().bytes(),
        2 * 1024 * 1024 * 1024
    );
    assert_eq!(
        MemoryBytes::parse("268435456").unwrap().bytes(),
        268_435_456
    );
    assert_eq!(MemoryBytes::parse("262144k").unwrap().bytes(), 268_435_456);
    for (bad, error) in [
        ("", ValueError::MemorySyntax),
        ("g", ValueError::MemorySyntax),
        ("1.5g", ValueError::MemorySyntax),
        ("2gb", ValueError::MemorySyntax),
        ("2 g", ValueError::MemorySyntax),
        ("-1g", ValueError::MemorySyntax),
        ("255m", ValueError::MemoryTooSmall),
        ("0", ValueError::MemoryTooSmall),
        ("9223372036854775808", ValueError::MemoryTooLarge),
        ("18446744073709551615g", ValueError::MemoryTooLarge),
    ] {
        assert_eq!(MemoryBytes::parse(bad), Err(error), "{bad:?}");
    }
}

#[test]
fn pids() {
    assert_eq!(PidsLimit::parse("256").unwrap().get(), 256);
    assert_eq!(PidsLimit::parse("4194304").unwrap().get(), 4_194_304);
    for (bad, error) in [
        ("", ValueError::PidsSyntax),
        ("-1", ValueError::PidsSyntax),
        ("1e4", ValueError::PidsSyntax),
        ("255", ValueError::PidsRange),
        ("0", ValueError::PidsRange),
        ("4194305", ValueError::PidsRange),
    ] {
        assert_eq!(PidsLimit::parse(bad), Err(error), "{bad:?}");
    }
}

#[test]
fn cpus() {
    assert_eq!(NanoCpus::parse("2").unwrap().get(), 2_000_000_000);
    assert_eq!(NanoCpus::parse("1.5").unwrap().get(), 1_500_000_000);
    assert_eq!(NanoCpus::parse("0.25").unwrap().get(), 250_000_000);
    assert_eq!(NanoCpus::parse("0.000000001").unwrap().get(), 1);
    assert_eq!(NanoCpus::parse("1024").unwrap().get(), MAX_NANO_CPUS);
    for (bad, error) in [
        ("", ValueError::CpusSyntax),
        (".5", ValueError::CpusSyntax),
        ("1.", ValueError::CpusSyntax),
        ("1.0000000001", ValueError::CpusSyntax),
        ("1,5", ValueError::CpusSyntax),
        ("-1", ValueError::CpusSyntax),
        ("1e2", ValueError::CpusSyntax),
        ("0", ValueError::CpusRange),
        ("0.0", ValueError::CpusRange),
        ("1024.000000001", ValueError::CpusRange),
        ("18446744073709551615", ValueError::CpusRange),
        ("99999999999999999999", ValueError::CpusSyntax),
    ] {
        assert_eq!(NanoCpus::parse(bad), Err(error), "{bad:?}");
    }
}

#[test]
fn config_defaults_subgid_to_subuid() {
    let config = LaunchConfig::new(values()).unwrap();
    assert_eq!(config.subgid(), config.subuid());

    let mut explicit = values();
    explicit.subgid = Some(IdRange::parse("200000:65536").unwrap());
    assert_eq!(
        LaunchConfig::new(explicit).unwrap().subgid().to_string(),
        "200000:65536"
    );
}

#[test]
fn config_refuses_overlapping_host_paths() {
    let cases: [(&str, Change); 4] = [
        ("same data root and socket dir", |values| {
            values.socket_dir = values.data_root.clone();
        }),
        ("socket dir inside the data root", |values| {
            values.socket_dir =
                HostPath::parse("/mnt/pool/docker/herdquay-docker/data/socket").unwrap();
        }),
        ("mount source containing the data root", |values| {
            values.mounts =
                vec![ExtraMount::parse("/mnt/pool/docker/herdquay-docker:/srv/all").unwrap()];
        }),
        ("two mounts of nested sources", |values| {
            values.mounts = vec![
                ExtraMount::parse("/mnt/ws:/ws").unwrap(),
                ExtraMount::parse("/mnt/ws/inner:/inner").unwrap(),
            ];
        }),
    ];
    for (name, change) in cases {
        let mut changed = values();
        change(&mut changed);
        assert!(
            matches!(
                LaunchConfig::new(changed),
                Err(ConfigError::PathOverlap { .. })
            ),
            "{name}"
        );
    }
}

#[test]
fn config_refuses_overlapping_targets() {
    let mut nested = values();
    nested.mounts = vec![
        ExtraMount::parse("/mnt/a:/srv/ws").unwrap(),
        ExtraMount::parse("/mnt/b:/srv/ws/cache").unwrap(),
    ];
    assert_eq!(
        LaunchConfig::new(nested),
        Err(ConfigError::TargetOverlap {
            first: "/srv/ws".to_owned(),
            second: "/srv/ws/cache".to_owned(),
        })
    );
}

#[test]
fn config_caps_the_mount_count() {
    let mounts = |count: usize| -> Vec<ExtraMount> {
        (0..count)
            .map(|index| ExtraMount::parse(&format!("/mnt/m{index}:/srv/m{index}")).unwrap())
            .collect()
    };
    let mut many = values();
    many.mounts = mounts(MAX_EXTRA_MOUNTS + 1);
    assert_eq!(
        LaunchConfig::new(many),
        Err(ConfigError::TooManyMounts(MAX_EXTRA_MOUNTS + 1))
    );
    let mut most = values();
    most.mounts = mounts(MAX_EXTRA_MOUNTS);
    assert!(LaunchConfig::new(most).is_ok());
}
