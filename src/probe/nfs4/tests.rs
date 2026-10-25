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

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;

use super::*;

/// A capture from castor (TrueNAS, 2026-10-25), base64 in `testdata/nfs4/`.
fn fixture(name: &str) -> Vec<u8> {
    let path = format!("{}/testdata/nfs4/{name}.b64", env!("CARGO_MANIFEST_DIR"));
    let text = std::fs::read_to_string(&path).unwrap();
    STANDARD.decode(text.trim()).unwrap()
}

fn encode(flags: u32, entries: &[[u32; 5]]) -> Vec<u8> {
    let count = u32::try_from(entries.len()).unwrap();
    [flags, count]
        .into_iter()
        .chain(entries.iter().flatten().copied())
        .flat_map(u32::to_be_bytes)
        .collect()
}

const OWNER: [u32; 5] = [0, 0, 1, 0x1e_01ff, 1];
const GROUP_RX: [u32; 5] = [0, 0x40, 1, 0x12_00a9, 2];
const EVERYONE_RX: [u32; 5] = [0, 0, 1, 0x12_00a9, 3];

/// The trivial captures pass for their modes: a root 0755 parent and a consumer 0750 leaf.
#[test]
fn trivial_captures_pass() {
    let root = decode(&fixture("trivial-root-0755")).unwrap();
    assert_eq!(root.entries.len(), 3);
    check_trivial(&root, 0o755).unwrap();
    check_trivial(&decode(&fixture("trivial-consumer-0750")).unwrap(), 0o750).unwrap();
}

/// A named-user entry (the one that let 568 rename a leaf under a root 0755 parent on castor) and
/// inheritance flags are refused; a truncated capture does not decode.
#[test]
fn non_trivial_captures_are_refused() {
    for name in ["named-user", "inherit-flags"] {
        let acl = decode(&fixture(name)).unwrap();
        assert!(
            matches!(check_trivial(&acl, 0o755), Err(AclError::NotTrivial(_))),
            "{name}"
        );
    }
    assert!(matches!(
        decode(&fixture("truncated")),
        Err(AclError::Malformed { len: 44 })
    ));
}

#[test]
fn malformed_bytes_do_not_decode() {
    let good = encode(0x3_0000, &[OWNER, GROUP_RX, EVERYONE_RX]);
    for bad in [
        Vec::new(),
        good[..4].to_vec(),
        good[..good.len() - 1].to_vec(),
        [good.clone(), vec![0; 4]].concat(),
        // The header counts three entries; two follow.
        good[..8 + 40].to_vec(),
    ] {
        assert!(decode(&bad).is_err(), "{bad:?}");
    }
    // A count far beyond the bytes.
    let mut huge = good.clone();
    huge[4..8].copy_from_slice(&u32::MAX.to_be_bytes());
    assert!(decode(&huge).is_err());
}

/// Each departure from the trivial shape refuses, and write rights follow the mode's bits.
#[test]
fn trivial_rule() {
    let check = |flags, entries: &[[u32; 5]], mode| {
        check_trivial(&decode(&encode(flags, entries)).unwrap(), mode)
    };
    check(0x3_0000, &[OWNER, GROUP_RX, EVERYONE_RX], 0o755).unwrap();

    let group_rwx = [0, 0x40, 1, 0x12_00ef, 2];
    check(0x3_0000, &[OWNER, group_rwx, EVERYONE_RX], 0o775).unwrap();
    let refused = [
        (
            "no trivial flag",
            0x2_0000,
            vec![OWNER, GROUP_RX, EVERYONE_RX],
            0o755,
        ),
        (
            "group write beyond the mode",
            0x3_0000,
            vec![OWNER, group_rwx, EVERYONE_RX],
            0o755,
        ),
        (
            "everyone delete-child",
            0x3_0000,
            vec![OWNER, GROUP_RX, [0, 0, 1, 0x12_00e9, 3]],
            0o755,
        ),
        (
            "everyone write-acl, with mode 0777",
            0x3_0000,
            vec![OWNER, GROUP_RX, [0, 0, 1, 0x16_00ef, 3]],
            0o777,
        ),
        (
            "group delete",
            0x3_0000,
            vec![OWNER, [0, 0x40, 1, 0x13_00a9, 2], EVERYONE_RX],
            0o775,
        ),
        (
            "deny entry",
            0x3_0000,
            vec![OWNER, [1, 0x40, 1, 0x12_00a9, 2], EVERYONE_RX],
            0o755,
        ),
        (
            "named group",
            0x3_0000,
            vec![OWNER, [0, 0x40, 0, 0x12_00a9, 568], EVERYONE_RX],
            0o755,
        ),
        (
            "inheritance",
            0x3_0000,
            vec![[0, 0x3, 1, 0x1e_01ff, 1], GROUP_RX, EVERYONE_RX],
            0o755,
        ),
        (
            "out of order",
            0x3_0000,
            vec![GROUP_RX, OWNER, EVERYONE_RX],
            0o755,
        ),
        (
            "four entries",
            0x3_0000,
            vec![OWNER, GROUP_RX, EVERYONE_RX, EVERYONE_RX],
            0o755,
        ),
        ("two entries", 0x3_0000, vec![OWNER, GROUP_RX], 0o755),
    ];
    for (name, flags, entries, mode) in refused {
        assert!(
            matches!(check(flags, &entries, mode), Err(AclError::NotTrivial(_))),
            "{name} passed"
        );
    }
}
