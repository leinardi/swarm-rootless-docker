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

/// The mount table as the probe sees it: its own root, the host's `/` at `/host` and the host's
/// mounts below it. `/mnt/alias` is a bind of `/srv/work/tree/subdir`; `/mnt/pool/docker` is a
/// child dataset; `/mnt/stack` has one mount on top of another; `/mnt/twins` has two siblings on
/// one point; `/mnt/gone` shows a deleted root. `/a/child` (filesystem 0:61) is hidden by 0:62
/// mounted over `/a` afterwards, and `/b` is a bind of 0:62's `/child`.
pub const TABLE: &str = "\
500 400 0:60 / / ro,relatime - overlay overlay rw,lowerdir=/x
600 500 8:1 / /host ro,relatime master:1 - ext4 /dev/sda1 rw
601 600 0:52 / /host/mnt/pool ro,relatime master:5 - zfs pool rw
602 601 0:53 / /host/mnt/pool/docker ro,relatime master:6 - zfs pool/docker rw
603 600 8:1 /srv/work/tree/subdir /host/mnt/alias ro,relatime master:1 - ext4 /dev/sda1 rw
604 600 0:54 / /host/mnt/space\\040dir ro,relatime master:7 - zfs pool/space rw
605 600 0:55 / /host/mnt/stack ro,relatime master:8 - zfs pool/lower rw
606 605 0:56 / /host/mnt/stack ro,relatime master:9 - zfs pool/upper rw
607 600 0:57 / /host/mnt/twins ro,relatime master:10 - zfs pool/a rw
608 600 0:58 / /host/mnt/twins ro,relatime master:11 - zfs pool/b rw
609 600 8:1 /srv/old//deleted /host/mnt/gone ro,relatime master:1 - ext4 /dev/sda1 rw
610 600 0:61 / /host/a/child ro,relatime master:12 - zfs pool/hidden rw
611 600 0:62 / /host/a ro,relatime master:13 - zfs pool/cover rw
612 600 0:62 /child /host/b ro,relatime master:13 - zfs pool/cover rw
";

fn table() -> Vec<Mount> {
    parse(TABLE).unwrap()
}

fn placed(path: &str) -> Placement {
    place(&table(), path).unwrap_or_else(|error| panic!("{path}: {error}"))
}

fn at(major: u32, minor: u32, fs_path: &str) -> Placement {
    Placement::new(Device { major, minor }, fs_path).unwrap()
}

#[test]
fn places_paths_on_their_filesystem() {
    let cases = [
        ("/host/srv/work/tree", at(8, 1, "/srv/work/tree")),
        ("/host/mnt/pool/data", at(0, 52, "/data")),
        (
            "/host/mnt/pool/docker/herdquay/data",
            at(0, 53, "/herdquay/data"),
        ),
        ("/host/mnt/alias", at(8, 1, "/srv/work/tree/subdir")),
        (
            "/host/mnt/alias/deeper",
            at(8, 1, "/srv/work/tree/subdir/deeper"),
        ),
        ("/host/mnt/space dir/data", at(0, 54, "/data")),
        ("/host/mnt/stack/data", at(0, 56, "/data")),
    ];
    for (path, want) in cases {
        assert_eq!(placed(path), want, "{path}");
    }
}

/// A bind of a reserved tree's descendant, or of an ancestor, overlaps that tree, though no
/// walk of either path names the other. A path of another filesystem does not, whatever its
/// position.
#[test]
fn bind_mounts_overlap_what_they_show() {
    let tree = placed("/host/srv/work/tree");
    let alias = placed("/host/mnt/alias");
    assert!(tree.overlaps(&alias) && alias.overlaps(&tree));
    assert!(placed("/host/mnt/alias/deeper").overlaps(&tree));
    assert!(placed("/host/srv/work").overlaps(&alias));
    assert!(!placed("/host/srv/work/other").overlaps(&alias));
    let pool_data = placed("/host/mnt/pool/data");
    let docker_data = placed("/host/mnt/pool/docker/data");
    assert_eq!(pool_data.fs_path, docker_data.fs_path);
    assert!(!pool_data.overlaps(&docker_data));
}

/// A mount hidden by another mounted over its parent directory is not what a path shows, however
/// deep its mount point: `/a/child/data` lies on the covering filesystem, and so overlaps a bind
/// of that filesystem's `/child`.
#[test]
fn hidden_mounts_are_not_chosen() {
    let through_cover = placed("/host/a/child/data");
    assert_eq!(through_cover, at(0, 62, "/child/data"));
    let through_bind = placed("/host/b/data/subdir");
    assert_eq!(through_bind, at(0, 62, "/child/data/subdir"));
    assert!(through_cover.overlaps(&through_bind) && through_bind.overlaps(&through_cover));
}

#[test]
fn refuses_what_it_cannot_place() {
    let table = table();
    let refused = |path: &str| place(&table, path).unwrap_err();
    assert!(matches!(
        refused("/host/mnt/pool"),
        MountError::MountBelow { mount_point, .. } if mount_point == "/host/mnt/pool/docker"
    ));
    assert!(matches!(
        refused("/host/mnt"),
        MountError::MountBelow { .. }
    ));
    assert!(matches!(
        refused("/host/mnt/twins/data"),
        MountError::Ambiguous { count: 2, .. }
    ));
    assert!(matches!(
        refused("/host/mnt/gone/data"),
        MountError::Deleted { .. }
    ));
    // The hidden mount still lies below /a: refused, the safe side.
    assert!(matches!(
        refused("/host/a"),
        MountError::MountBelow { mount_point, .. } if mount_point == "/host/a/child"
    ));
    let no_root = parse("601 600 0:52 / /host/mnt/pool ro - zfs pool rw").unwrap();
    assert!(matches!(
        place(&no_root, "/host/srv").unwrap_err(),
        MountError::NoMount { .. }
    ));
}

/// Two root mounts cannot be resolved. Mounts attached to nothing the walk reaches are not
/// chosen, whatever their mount point.
#[test]
fn resolves_from_the_root_mount_only() {
    let two_roots = parse("1 0 0:1 / / ro - ext4 a rw\n2 0 0:2 / / ro - ext4 b rw").unwrap();
    assert!(matches!(
        place(&two_roots, "/x").unwrap_err(),
        MountError::Ambiguous { count: 2, .. }
    ));
    let detached = parse(
        "1 0 0:1 / / ro - ext4 a rw\n2 3 0:2 / /m ro - ext4 b rw\n3 2 0:3 / /m ro - ext4 c rw",
    )
    .unwrap();
    assert_eq!(place(&detached, "/m/x").unwrap(), at(0, 1, "/m/x"));
}

#[test]
fn refuses_malformed_tables() {
    for (line, reason) in [
        ("x 600 0:52 / /m ro - zfs p rw", "mount ID not a number"),
        ("1 600 052 / /m ro - zfs p rw", "device not major:minor"),
        (
            "1 600 0:52 / /m ro shared:1 zfs p rw",
            "no separator before the filesystem type",
        ),
        ("1 600 0:52 / /m\\04 ro - zfs p rw", "short escape"),
        (
            "1 600 0:52 / /m/../n ro - zfs p rw",
            "mount point not a clean absolute path",
        ),
        ("1 600 0:52 /", "no mount point"),
    ] {
        let error = parse(&format!("500 400 0:60 / / ro - overlay o rw\n{line}")).unwrap_err();
        assert_eq!(error, MountError::Malformed { line: 2, reason }, "{line}");
    }
}

#[test]
fn placements_from_labels_are_checked() {
    let device = Device { major: 0, minor: 1 };
    for good in ["/", "/a", "/a/b c"] {
        assert!(Placement::new(device, good).is_ok(), "{good:?}");
    }
    for bad in ["", "a", "/a/", "//a", "/a/./b", "/a/../b"] {
        assert!(Placement::new(device, bad).is_err(), "{bad:?}");
    }
}
