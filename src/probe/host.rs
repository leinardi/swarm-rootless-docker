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

//! [`Host`] on the real filesystem: `lstat` and `lgetxattr` through rustix, which never follow
//! a symlink at the last component.

use std::io;

use rustix::io::Errno;

use super::{Host, Meta, split_mode};

/// The largest xattr value the probe reads; an NFSv4 ACL of [`super::nfs4`]'s maximum entries
/// fits.
const XATTR_MAX: usize = 64 * 1024;

/// The real host.
#[derive(Debug, Clone, Copy, Default)]
pub struct RealHost;

impl Host for RealHost {
    fn lstat(&self, path: &str) -> io::Result<Meta> {
        let stat = rustix::fs::lstat(path)?;
        let (kind, mode) = split_mode(stat.st_mode);
        Ok(Meta {
            kind,
            uid: stat.st_uid,
            mode,
            device: stat.st_dev,
            inode: stat.st_ino,
        })
    }

    fn xattr(&self, path: &str, name: &str) -> io::Result<Option<Vec<u8>>> {
        let mut value = vec![0_u8; XATTR_MAX];
        match rustix::fs::lgetxattr(path, name, value.as_mut_slice()) {
            Ok(len) => {
                value.truncate(len);
                Ok(Some(value))
            }
            Err(Errno::NODATA | Errno::NOTSUP) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    fn readlink(&self, path: &str) -> io::Result<String> {
        std::fs::read_link(path)?
            .into_os_string()
            .into_string()
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "symlink target not UTF-8"))
    }

    fn read(&self, path: &str) -> io::Result<Option<String>> {
        match std::fs::read_to_string(path) {
            Ok(text) => Ok(Some(text)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }
}
