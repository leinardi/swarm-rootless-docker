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

use std::collections::HashMap;
use std::os::unix::fs::MetadataExt as _;
use std::path::PathBuf;
use std::process::Command;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;

use super::*;
use crate::config::{
    AbsolutePath, ExtraMount, IdRange, InstanceName, LaunchConfig, LaunchValues, MemoryBytes,
    NetworkName, PidsLimit,
};
use crate::mounts::{Device, Placement};
use crate::spec::{ImageIdentity, LABEL_IMAGE_GID, LABEL_IMAGE_UID};

const ROOT: &str = "/host";
const DATA: &str = "/mnt/pool/docker/herdquay-docker/data";
const SOCKET: &str = "/mnt/pool/docker/herdquay-docker/socket";
const WORKSPACE: &str = "/mnt/pool/docker/herdquay/workspace";
const DOCKER_ROOT: &str = "/mnt/.ix-apps/docker";

/// The probe's mount table: its own root, the host's `/` (8:1) at `/host`, the pool (0:52), and
/// the host's Docker data root (0:60) with a container's overlay below it.
const MOUNTINFO_TABLE: &str = "\
500 400 0:70 / / ro,relatime - overlay overlay rw
600 500 8:1 / /host ro,relatime master:1 - ext4 /dev/sda1 rw
601 600 0:52 / /host/mnt/pool ro,relatime master:2 - zfs pool rw
602 600 0:60 / /host/mnt/.ix-apps ro,relatime master:3 - zfs pool/ix-apps rw
603 602 0:71 / /host/mnt/.ix-apps/docker/overlay2/abc/merged ro - overlay overlay rw
";

/// A host as a map, every path as the probe sees it (under [`ROOT`]).
#[derive(Debug, Clone, Default)]
struct FakeHost {
    files: HashMap<String, Meta>,
    xattrs: HashMap<(String, String), Vec<u8>>,
    contents: HashMap<String, String>,
    links: HashMap<String, String>,
    lstat_errors: HashMap<String, io::ErrorKind>,
    xattr_errors: HashMap<String, io::ErrorKind>,
}

impl FakeHost {
    fn add(&mut self, path: &str, kind: Kind, uid: u32, mode: u32) {
        let inode = u64::try_from(self.files.len()).unwrap() + 1000;
        self.files.insert(
            on_host(ROOT, path),
            Meta {
                kind,
                uid,
                mode,
                device: 52,
                inode,
            },
        );
    }

    fn change(&mut self, path: &str, change: impl FnOnce(&mut Meta)) {
        change(self.files.get_mut(&on_host(ROOT, path)).unwrap());
    }

    fn symlink(&mut self, path: &str, target: &str) {
        self.add(path, Kind::Symlink, 0, 0o777);
        self.links.insert(on_host(ROOT, path), target.to_owned());
    }

    fn mount(&mut self, line: &str) {
        let table = self.contents.get_mut(MOUNTINFO).unwrap();
        table.push_str(line);
        table.push('\n');
    }

    fn set_xattr(&mut self, path: &str, name: &str, value: Vec<u8>) {
        self.xattrs
            .insert((on_host(ROOT, path), name.to_owned()), value);
    }
}

impl Host for FakeHost {
    fn lstat(&self, path: &str) -> io::Result<Meta> {
        if let Some(kind) = self.lstat_errors.get(path) {
            return Err((*kind).into());
        }
        self.files
            .get(path)
            .copied()
            .ok_or_else(|| io::ErrorKind::NotFound.into())
    }

    fn xattr(&self, path: &str, name: &str) -> io::Result<Option<Vec<u8>>> {
        if let Some(kind) = self.xattr_errors.get(path) {
            return Err((*kind).into());
        }
        Ok(self
            .xattrs
            .get(&(path.to_owned(), name.to_owned()))
            .cloned())
    }

    fn readlink(&self, path: &str) -> io::Result<String> {
        self.links
            .get(path)
            .cloned()
            .ok_or_else(|| io::ErrorKind::InvalidInput.into())
    }

    fn read(&self, path: &str) -> io::Result<Option<String>> {
        Ok(self.contents.get(path).cloned())
    }
}

fn fixture(name: &str) -> Vec<u8> {
    let path = format!("{}/testdata/nfs4/{name}.b64", env!("CARGO_MANIFEST_DIR"));
    STANDARD
        .decode(std::fs::read_to_string(path).unwrap().trim())
        .unwrap()
}

/// The accepted layout: root-owned 0755 parents with trivial ACLs, consumer-owned leaves.
fn host() -> FakeHost {
    let mut host = FakeHost::default();
    for dir in [
        "/",
        "/mnt",
        "/mnt/pool",
        "/mnt/pool/docker",
        "/mnt/pool/docker/herdquay-docker",
        "/mnt/pool/docker/herdquay",
        "/mnt/.ix-apps",
        "/mnt/.ix-apps/docker",
    ] {
        host.add(dir, Kind::Directory, 0, 0o755);
    }
    host.add(DATA, Kind::Directory, 568, 0o750);
    host.add(SOCKET, Kind::Directory, 568, 0o750);
    host.add(WORKSPACE, Kind::Directory, 568, 0o770);
    host.set_xattr(
        "/mnt/pool/docker",
        nfs4::XATTR,
        fixture("trivial-root-0755"),
    );
    host.set_xattr(DATA, nfs4::XATTR, fixture("trivial-consumer-0750"));
    host.contents
        .insert(MOUNTINFO.to_owned(), MOUNTINFO_TABLE.to_owned());
    for (file, text) in [
        (
            "/etc/passwd",
            "root:x:0:0:root:/root:/bin/sh\napps:x:568:568::/:/bin/false\n",
        ),
        ("/etc/group", "root:x:0:\napps:x:568:\n"),
        ("/etc/subuid", "root:2147000001:1000000000\n"),
        ("/etc/subgid", "root:2147000001:1000000000\n"),
    ] {
        host.contents.insert(on_host(ROOT, file), text.to_owned());
    }
    host
}

fn config(mounts: &[&str]) -> LaunchConfig {
    LaunchConfig::new(LaunchValues {
        instance: InstanceName::parse("herdquay").unwrap(),
        subuid: IdRange::parse("100000:65536").unwrap(),
        subgid: None,
        data_root: HostPath::parse(DATA).unwrap(),
        socket_dir: HostPath::parse(SOCKET).unwrap(),
        mounts: mounts
            .iter()
            .map(|mount| ExtraMount::parse(mount).unwrap())
            .collect(),
        network: NetworkName::parse("docker").unwrap(),
        aliases: Vec::new(),
        memory: MemoryBytes::parse("1g").unwrap(),
        pids_limit: PidsLimit::parse("1024").unwrap(),
        cpus: None,
    })
    .unwrap()
}

fn request(config: &LaunchConfig) -> String {
    let labels = HashMap::from([
        (LABEL_IMAGE_UID.to_owned(), "568".to_owned()),
        (LABEL_IMAGE_GID.to_owned(), "568".to_owned()),
    ]);
    let image = ImageIdentity::new(&format!("sha256:{}", "a".repeat(64)), &labels).unwrap();
    let denied = [AbsolutePath::parse(DOCKER_ROOT).unwrap()];
    serde_json::to_string(&ProbeRequest::new(config, &image, &denied)).unwrap()
}

fn workspace_mount() -> String {
    format!("{WORKSPACE}:/workspace")
}

fn refusal(host: &FakeHost, config: &LaunchConfig) -> String {
    match run(host, ROOT, &request(config)) {
        ProbeReport::Refused { reason } => reason,
        accepted @ ProbeReport::Accepted { .. } => panic!("accepted: {accepted:?}"),
    }
}

/// The accepted layout gives each path's location, in the request's order, and the report
/// round-trips as the launcher will read it.
#[test]
fn accepts_the_trusted_layout() {
    let host = host();
    let report = run(&host, ROOT, &request(&config(&[&workspace_mount()])));
    let location = |path: &str, fs_path: &str| HostLocation {
        placement: Placement::new(
            Device {
                major: 0,
                minor: 52,
            },
            fs_path,
        )
        .unwrap(),
        file: FileId {
            device: 52,
            inode: host.files[&on_host(ROOT, path)].inode,
        },
    };
    let want = ProbeReport::Accepted {
        data_root: location(DATA, "/docker/herdquay-docker/data"),
        socket_dir: location(SOCKET, "/docker/herdquay-docker/socket"),
        mounts: vec![location(WORKSPACE, "/docker/herdquay/workspace")],
    };
    assert_eq!(report, want);
    let line = serde_json::to_string(&report).unwrap();
    assert_eq!(serde_json::from_str::<ProbeReport>(&line).unwrap(), want);
}

type Break = fn(&mut FakeHost);

/// Each break, alone on the accepted layout, refuses with a reason that names it.
fn assert_refused(cases: Vec<(&str, Break, &str)>) {
    let config = config(&[&workspace_mount()]);
    for (name, broken, want) in cases {
        let mut host = host();
        broken(&mut host);
        let reason = refusal(&host, &config);
        assert!(reason.contains(want), "{name}: {reason:?} lacks {want:?}");
    }
}

/// Every parent: no symlink, a directory, root's, not group- or other-writable.
#[test]
fn refuses_broken_parents() {
    assert_refused(vec![
        (
            "symlink parent",
            |host| host.change("/mnt/pool/docker", |meta| meta.kind = Kind::Symlink),
            "/mnt/pool/docker is a symlink",
        ),
        (
            "symlink leaf",
            |host| host.change(DATA, |meta| meta.kind = Kind::Symlink),
            "data is a symlink",
        ),
        (
            "parent not root's",
            |host| host.change("/mnt/pool/docker", |meta| meta.uid = 568),
            "belongs to UID 568; every parent must belong to root",
        ),
        (
            "group-writable parent",
            |host| host.change("/mnt/pool", |meta| meta.mode = 0o775),
            "/mnt/pool is group- or other-writable",
        ),
        (
            "other-writable root",
            |host| host.change("/", |meta| meta.mode = 0o1777),
            "/ is group- or other-writable",
        ),
        (
            "parent not a directory",
            |host| host.change("/mnt/pool/docker", |meta| meta.kind = Kind::File),
            "/mnt/pool/docker is not a directory",
        ),
        (
            "lstat fails",
            |host| {
                host.lstat_errors
                    .insert(on_host(ROOT, "/mnt/pool"), io::ErrorKind::PermissionDenied);
            },
            "lstat /mnt/pool",
        ),
    ]);
}

/// ACLs on any component, the path itself, and the mount table.
#[test]
fn refuses_broken_acls_and_leaves() {
    assert_refused(vec![
        (
            "POSIX ACL on a parent",
            |host| host.set_xattr("/mnt", "system.posix_acl_access", vec![2, 0, 0, 0]),
            "/mnt: carries a POSIX ACL",
        ),
        (
            "default POSIX ACL on the leaf",
            |host| host.set_xattr(DATA, "system.posix_acl_default", vec![2, 0, 0, 0]),
            "carries a POSIX ACL (system.posix_acl_default)",
        ),
        (
            "named-user NFSv4 ACL on a parent",
            |host| host.set_xattr("/mnt/pool/docker", nfs4::XATTR, fixture("named-user")),
            "/mnt/pool/docker: the NFSv4 ACL is not trivial",
        ),
        (
            "inheriting NFSv4 ACL on the leaf",
            |host| host.set_xattr(WORKSPACE, nfs4::XATTR, fixture("inherit-flags")),
            "workspace: the NFSv4 ACL is not trivial",
        ),
        (
            "truncated NFSv4 ACL",
            |host| host.set_xattr("/mnt", nfs4::XATTR, fixture("truncated")),
            "not a whole number of entries",
        ),
        (
            "xattr read fails",
            |host| {
                host.xattr_errors
                    .insert(on_host(ROOT, "/mnt"), io::ErrorKind::Other);
            },
            "/mnt: read system.posix_acl_access",
        ),
        (
            "data root not the daemon's",
            |host| host.change(DATA, |meta| meta.uid = 1000),
            "belongs to UID 1000, not the daemon's UID 568",
        ),
        (
            "group-writable socket dir",
            |host| host.change(SOCKET, |meta| meta.mode = 0o770),
            "--socket-dir /mnt/pool/docker/herdquay-docker/socket: is group- or other-writable",
        ),
        (
            "data root a file",
            |host| host.change(DATA, |meta| meta.kind = Kind::File),
            "--data-root /mnt/pool/docker/herdquay-docker/data: is not a directory",
        ),
        (
            "mount source missing",
            |host| {
                host.files.remove(&on_host(ROOT, WORKSPACE));
            },
            "workspace does not exist",
        ),
        (
            "mount source a device",
            |host| host.change(WORKSPACE, |meta| meta.kind = Kind::Other),
            "is neither a directory nor a regular file",
        ),
        (
            "a mount below the data root",
            |host| {
                let table = format!(
                    "{MOUNTINFO_TABLE}604 601 0:72 / /host{DATA}/inner ro - zfs pool/inner rw\n"
                );
                host.contents.insert(MOUNTINFO.to_owned(), table);
            },
            "is mounted inside it",
        ),
        (
            "no mount table",
            |host| {
                host.contents.remove(MOUNTINFO);
            },
            "/proc/self/mountinfo does not exist",
        ),
    ]);
}

/// The ranges against the host's accounts, groups and subordinate ID files.
#[test]
fn refuses_broken_ids() {
    assert_refused(vec![
        (
            "host account inside the range",
            |host| append(host, "/etc/passwd", "ci:x:100500:568::/:/bin/sh\n"),
            "--subuid 100000:65536 contains UID 100500 of host account ci",
        ),
        (
            "host primary group inside the range",
            |host| append(host, "/etc/passwd", "ci:x:3000:100007::/:/bin/sh\n"),
            "contains GID 100007, primary group of host account ci",
        ),
        (
            "host group inside the range",
            |host| append(host, "/etc/group", "builders:x:165535:\n"),
            "--subgid 100000:65536 contains GID 165535 of host group builders",
        ),
        (
            "host subuid range overlapping",
            |host| append(host, "/etc/subuid", "someone:150000:65536\n"),
            "--subuid 100000:65536 overlaps someone:150000:65536 (/etc/subuid line 2)",
        ),
        (
            "host subgid range overlapping",
            |host| append(host, "/etc/subgid", "someone:40000:60001\n"),
            "--subgid 100000:65536 overlaps someone:40000:60001",
        ),
        (
            "unparsable passwd line",
            |host| append(host, "/etc/passwd", "+::::::\n"),
            "/etc/passwd line 3 does not parse",
        ),
        (
            "no passwd",
            |host| {
                host.contents.remove(&on_host(ROOT, "/etc/passwd"));
            },
            "/etc/passwd does not exist",
        ),
    ]);
}

fn append(host: &mut FakeHost, file: &str, line: &str) {
    host.contents
        .get_mut(&on_host(ROOT, file))
        .unwrap()
        .push_str(line);
}

/// Missing subordinate ID files hold no range: nothing to overlap.
#[test]
fn missing_subordinate_files_are_empty() {
    let mut host = host();
    host.contents.remove(&on_host(ROOT, "/etc/subuid"));
    host.contents.remove(&on_host(ROOT, "/etc/subgid"));
    assert!(matches!(
        run(&host, ROOT, &request(&config(&[]))),
        ProbeReport::Accepted { .. }
    ));
}

/// A mount source inside the host's Docker data root, or a bind of the data root elsewhere,
/// refuses: no daemon gets the host's Docker storage.
#[test]
fn refuses_the_host_docker_root() {
    let mut host = host();
    host.add("/mnt/.ix-apps/docker/volumes", Kind::Directory, 0, 0o755);
    let config_in = config(&["/mnt/.ix-apps/docker/volumes:/volumes"]);
    let reason = refusal(&host, &config_in);
    assert!(
        reason.contains("reaches the storage of /mnt/.ix-apps/docker, which no daemon may get"),
        "{reason}"
    );

    // /mnt/elsewhere is a bind of the Docker root's volumes: no lexical overlap, same storage.
    let table = format!(
        "{MOUNTINFO_TABLE}605 600 0:60 /docker/volumes /host/mnt/elsewhere ro - zfs pool/ix-apps rw\n"
    );
    host.contents.insert(MOUNTINFO.to_owned(), table);
    host.add("/mnt/elsewhere", Kind::Directory, 0, 0o755);
    let reason = refusal(&host, &config(&["/mnt/elsewhere:/volumes"]));
    assert!(reason.contains("which no daemon may get"), "{reason}");
}

/// A bind of a host system directory elsewhere passes every lexical rule and every ancestry
/// check, but reaches the system directory's storage: refused.
#[test]
fn refuses_system_paths_bound_elsewhere() {
    for (name, mountinfo, want) in [
        (
            "/etc bound at /mnt/alias",
            "610 600 8:1 /etc /host/mnt/alias ro - ext4 /dev/sda1 rw",
            "reaches the storage of /etc",
        ),
        (
            "the host's / bound at /mnt/alias",
            "610 600 8:1 / /host/mnt/alias ro - ext4 /dev/sda1 rw",
            "reaches the storage of /bin",
        ),
        (
            "a filesystem mounted under /run, bound at /mnt/alias",
            "611 600 0:25 / /host/run ro - tmpfs tmpfs rw\n612 611 0:90 / /host/run/secrets ro - tmpfs tmpfs rw\n613 600 0:90 / /host/mnt/alias ro - tmpfs tmpfs rw",
            "reaches the storage of /run",
        ),
    ] {
        let mut host = host();
        host.mount(mountinfo);
        host.add("/mnt/alias", Kind::Directory, 0, 0o755);
        let reason = refusal(&host, &config(&["/mnt/alias:/alias"]));
        assert!(reason.contains(want), "{name}: {reason}");
    }
}

/// A filesystem mounted below the host's Docker data root has its own device; bound elsewhere,
/// it is still Docker's storage: refused.
#[test]
fn refuses_child_filesystems_of_denied_paths() {
    let mut host = host();
    host.mount("614 602 0:80 / /host/mnt/.ix-apps/docker/volumes/v1 ro - zfs pool/v1 rw");
    host.mount("615 600 0:80 /data /host/mnt/alias ro - zfs pool/v1 rw");
    host.add("/mnt/alias", Kind::Directory, 0, 0o755);
    let reason = refusal(&host, &config(&["/mnt/alias:/alias"]));
    assert!(
        reason.contains("reaches the storage of /mnt/.ix-apps/docker"),
        "{reason}"
    );
}

/// A system path that is a symlink denies where it points too: here the host's `/root` is an
/// absolute symlink into the pool.
#[test]
fn refuses_what_symlinked_system_paths_point_to() {
    let mut host = host();
    host.symlink("/root", "/mnt/pool/admin-home");
    host.add("/mnt/pool/admin-home", Kind::Directory, 0, 0o755);
    host.add("/mnt/pool/admin-home/notes", Kind::Directory, 0, 0o755);
    let reason = refusal(&host, &config(&["/mnt/pool/admin-home/notes:/notes"]));
    assert!(reason.contains("reaches the storage of /root"), "{reason}");

    // A relative target through `..`, and a loop, resolve or refuse.
    let mut host = host_with_loop();
    let reason = refusal(&host, &config(&[]));
    assert!(reason.contains("more than 40 symlinks"), "{reason}");
    host.links.clear();
    host.files.remove(&on_host(ROOT, "/usr"));
    host.symlink("/usr", "mnt/../mnt/pool/usr-elsewhere");
    host.add("/mnt/pool/usr-elsewhere", Kind::Directory, 0, 0o755);
    let reason = refusal(&host, &config(&["/mnt/pool/usr-elsewhere:/x"]));
    assert!(reason.contains("reaches the storage of /usr"), "{reason}");
}

fn host_with_loop() -> FakeHost {
    let mut host = host();
    host.symlink("/usr", "/usr");
    host
}

/// A mount source that is the data root again, through a bind mount, refuses: the consumer would
/// see the daemon's storage.
#[test]
fn refuses_own_paths_sharing_storage() {
    let mut host = host();
    let table = format!(
        "{MOUNTINFO_TABLE}606 600 0:52 /docker/herdquay-docker/data /host/mnt/alias ro - zfs pool rw\n"
    );
    host.contents.insert(MOUNTINFO.to_owned(), table);
    host.add("/mnt/alias", Kind::Directory, 568, 0o750);
    let reason = refusal(&host, &config(&["/mnt/alias:/alias"]));
    assert!(
        reason.contains("--mount source /mnt/alias and --data-root"),
        "{reason}"
    );
}

/// The request is checked again in the probe: it trusts no field.
#[test]
fn refuses_bad_requests() {
    let host = host();
    let good: serde_json::Value = serde_json::from_str(&request(&config(&[]))).unwrap();
    let with = |key: &str, value: serde_json::Value| {
        let mut request = good.clone();
        request[key] = value;
        request.to_string()
    };
    for (name, request, want) in [
        ("not JSON", "{".to_owned(), "does not parse"),
        ("unknown field", with("extra", 1.into()), "does not parse"),
        ("root owner", with("owner_uid", 0.into()), "owner_uid 0"),
        (
            "system path",
            with("data_root", "/etc/docker".into()),
            "data_root \"/etc/docker\"",
        ),
        (
            "relative denied path",
            with("denied", serde_json::json!(["docker"])),
            "denied",
        ),
        (
            "low range",
            with("subuid", serde_json::json!({"start": 1000, "count": 65536})),
            "subuid",
        ),
    ] {
        match run(&host, ROOT, &request) {
            ProbeReport::Refused { reason } => {
                assert!(reason.contains(want), "{name}: {reason:?} lacks {want:?}");
            }
            accepted @ ProbeReport::Accepted { .. } => panic!("{name}: {accepted:?}"),
        }
    }
}

/// A scratch directory of this test, removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("srd-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn path(&self, name: &str) -> String {
        self.0.join(name).to_str().unwrap().to_owned()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// [`RealHost`] on real files: types, owner and inode as `std` sees them, never following a
/// symlink, absent xattrs and files as `None`. With `setfacl` installed, a POSIX ACL shows up. On
/// ZFS with `acltype=nfsv4` (TrueNAS) every new directory carries an NFSv4 ACL, so that one is
/// only checked to decode and be trivial when present.
#[test]
fn real_host_reads_what_the_kernel_says() {
    let scratch = Scratch::new("real-host");
    let dir = scratch.path("dir");
    let file = scratch.path("file");
    let link = scratch.path("link");
    std::fs::create_dir(&dir).unwrap();
    std::fs::write(&file, "x").unwrap();
    std::os::unix::fs::symlink(&dir, &link).unwrap();

    let host = RealHost;
    let std_meta = std::fs::symlink_metadata(&dir).unwrap();
    let meta = host.lstat(&dir).unwrap();
    assert_eq!(
        (meta.kind, meta.uid, meta.mode, meta.device, meta.inode),
        (
            Kind::Directory,
            std_meta.uid(),
            std_meta.mode() & 0o7777,
            std_meta.dev(),
            std_meta.ino()
        )
    );
    assert_eq!(host.lstat(&file).unwrap().kind, Kind::File);
    assert_eq!(host.lstat(&link).unwrap().kind, Kind::Symlink);
    assert_eq!(
        host.lstat(&scratch.path("missing")).unwrap_err().kind(),
        io::ErrorKind::NotFound
    );
    for name in [
        POSIX_ACL_XATTRS[0],
        POSIX_ACL_XATTRS[1],
        "user.swarm-rootless-docker.absent",
    ] {
        assert_eq!(host.xattr(&dir, name).unwrap(), None, "{name}");
    }
    // On ZFS with nfs4acl (TrueNAS) every new directory carries the ACL: then it must be one the
    // probe accepts.
    if let Some(bytes) = host.xattr(&dir, nfs4::XATTR).unwrap() {
        let acl = nfs4::decode(&bytes).unwrap();
        nfs4::check_trivial(&acl, meta.mode).unwrap();
    }
    assert_eq!(host.read(&scratch.path("missing")).unwrap(), None);
    assert_eq!(host.read(&file).unwrap().as_deref(), Some("x"));

    let setfacl = Command::new("setfacl")
        .args(["-m", "u:12345:rwx", &dir])
        .status();
    match setfacl {
        Ok(status) if status.success() => {
            assert!(host.xattr(&dir, POSIX_ACL_XATTRS[0]).unwrap().is_some());
            // Never the symlink's target: the link itself has no ACL.
            assert_eq!(host.xattr(&link, POSIX_ACL_XATTRS[0]).unwrap(), None);
        }
        Ok(_) | Err(_) => {
            assert!(
                std::env::var_os("SWARM_ROOTLESS_DOCKER_REQUIRE_TOOLS").is_none(),
                "setfacl is required"
            );
            let mut stderr = io::stderr();
            let _ = std::io::Write::write_all(
                &mut stderr,
                b"note: setfacl unavailable, POSIX ACL check skipped\n",
            );
        }
    }
}
