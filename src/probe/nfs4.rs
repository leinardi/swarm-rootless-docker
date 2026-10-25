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

//! TrueNAS's NFSv4 ACL xattr (`system.nfs4_acl_xdr`), and whether an ACL is trivial: no more
//! than the mode bits say.
//!
//! The layout, from captures on a TrueNAS dataset: big-endian 32-bit words, the ACL flags and the
//! entry count, then five words per entry: type, flags, `special` (1 for `owner@`, `group@`,
//! `everyone@`), access mask and who (1, 2, 3 for those, or a raw UID or GID).
//!
//! A trivial ACL, as ZFS writes it for a plain mode, has the trivial flag and exactly three allow
//! entries, `owner@`, `group@` (flagged as a group) and `everyone@`, with no inheritance. Anything
//! else, a named user or group, a deny entry, inheritance, or a right that `chmod` cannot grant,
//! is refused: the probe does not evaluate arbitrary ACLs, it requires the ones whose meaning is
//! the mode.

#[cfg(test)]
mod tests;

/// The xattr name.
pub const XATTR: &str = "system.nfs4_acl_xdr";

const ACL_IS_TRIVIAL: u32 = 0x0001_0000;
const ACE_ALLOW: u32 = 0;
const ACE_IDENTIFIER_GROUP: u32 = 0x40;
const WHO_OWNER: u32 = 1;
const WHO_GROUP: u32 = 2;
const WHO_EVERYONE: u32 = 3;
const HEADER_WORDS: usize = 2;
const ENTRY_WORDS: usize = 5;
const MAX_ENTRIES: usize = 1024;

/// Rights that let the holder change a directory's entries: what a mode's `w` bit grants.
const WRITE_CONTENT: u32 = WRITE_DATA | APPEND_DATA | DELETE_CHILD;
/// Rights no mode grants beyond the owner: deleting the file itself, changing its ACL, owner,
/// attributes or named attributes.
const OWNER_ONLY: u32 = WRITE_NAMED_ATTRS | WRITE_ATTRIBUTES | DELETE | WRITE_ACL | WRITE_OWNER;
const WRITE_DATA: u32 = 0x0000_0002;
const APPEND_DATA: u32 = 0x0000_0004;
const WRITE_NAMED_ATTRS: u32 = 0x0000_0010;
const DELETE_CHILD: u32 = 0x0000_0040;
const WRITE_ATTRIBUTES: u32 = 0x0000_0100;
const DELETE: u32 = 0x0001_0000;
const WRITE_ACL: u32 = 0x0004_0000;
const WRITE_OWNER: u32 = 0x0008_0000;

/// Why an ACL is refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AclError {
    #[error("the NFSv4 ACL is {len} bytes, not a whole number of entries")]
    Malformed { len: usize },
    #[error("the NFSv4 ACL is not trivial: {0}")]
    NotTrivial(String),
}

/// One ACL entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ace {
    pub kind: u32,
    pub flags: u32,
    pub special: bool,
    pub mask: u32,
    pub who: u32,
}

/// A decoded ACL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Acl {
    pub flags: u32,
    pub entries: Vec<Ace>,
}

/// Decodes the xattr's bytes.
///
/// # Errors
///
/// [`AclError::Malformed`] unless the bytes are exactly the header and the entries it counts.
pub fn decode(bytes: &[u8]) -> Result<Acl, AclError> {
    let malformed = || AclError::Malformed { len: bytes.len() };
    let words: Vec<u32> = bytes
        .chunks(4)
        .map(|chunk| <[u8; 4]>::try_from(chunk).map(u32::from_be_bytes))
        .collect::<Result<_, _>>()
        .map_err(|_| malformed())?;
    let [flags, count, entries @ ..] = words.as_slice() else {
        return Err(malformed());
    };
    let count = usize::try_from(*count).map_err(|_| malformed())?;
    if count > MAX_ENTRIES || entries.len() != count * ENTRY_WORDS {
        return Err(malformed());
    }
    debug_assert_eq!(words.len(), HEADER_WORDS + count * ENTRY_WORDS);
    let entries = entries
        .as_chunks::<ENTRY_WORDS>()
        .0
        .iter()
        .map(|&[kind, flags, special, mask, who]| Ace {
            kind,
            flags,
            special: special == 1,
            mask,
            who,
        })
        .collect();
    Ok(Acl {
        flags: *flags,
        entries,
    })
}

/// Checks that `acl` says no more than `mode`: the trivial shape, and `group@` and `everyone@`
/// holding a write right only when the mode's group or other `w` bit grants it, and never a right
/// no mode grants them.
///
/// # Errors
///
/// [`AclError::NotTrivial`] naming the first thing that is not.
pub fn check_trivial(acl: &Acl, mode: u32) -> Result<(), AclError> {
    let refuse = |why: String| Err(AclError::NotTrivial(why));
    if acl.flags & ACL_IS_TRIVIAL == 0 {
        return refuse(format!("flags {:#x} lack the trivial flag", acl.flags));
    }
    let [owner, group, everyone] = acl.entries.as_slice() else {
        return refuse(format!("{} entries, not 3", acl.entries.len()));
    };
    for (ace, who, flags, name) in [
        (owner, WHO_OWNER, 0, "owner@"),
        (group, WHO_GROUP, ACE_IDENTIFIER_GROUP, "group@"),
        (everyone, WHO_EVERYONE, 0, "everyone@"),
    ] {
        if !ace.special || ace.who != who {
            return refuse(format!("{name} expected, found {ace:?}"));
        }
        if ace.kind != ACE_ALLOW {
            return refuse(format!("{name} is not an allow entry"));
        }
        if ace.flags != flags {
            return refuse(format!("{name} has flags {:#x}", ace.flags));
        }
    }
    for (ace, write_bit, name) in [(group, 0o020, "group@"), (everyone, 0o002, "everyone@")] {
        if ace.mask & OWNER_ONLY != 0 {
            return refuse(format!(
                "{name} holds {:#x}, which no mode grants",
                ace.mask & OWNER_ONLY
            ));
        }
        if ace.mask & WRITE_CONTENT != 0 && mode & write_bit == 0 {
            return refuse(format!(
                "{name} may write ({:#x}) though mode {mode:o} does not say so",
                ace.mask & WRITE_CONTENT
            ));
        }
    }
    Ok(())
}
