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

//! Integration tests: the built image against the machine's own Docker, the real launcher, probe
//! and rootless daemon.
//!
//! Off unless `SRD_IT=1`. They need the image (`make image-build`; `SRD_IT_IMAGE` names another),
//! the `docker` CLI on the host socket, and unprivileged user namespaces outside AppArmor's
//! restriction (`kernel.apparmor_restrict_unprivileged_userns=0`). CI also sets
//! `SRD_IT_REQUIRE=1`, so tests that did not run fail instead of passing.
//!
//! The host paths live under `SRD_IT_ROOT` (default `/srv/srd-it`), whose parent must be
//! root-owned and not writable by others. Root-owned directories there are made through a helper
//! container on the host Docker, so the tests need no sudo. One test runs at a time: they share
//! the node, its lock and its volume list.
//!
//! The Swarm test deploys a service into the node's swarm, so it is off unless `SRD_IT_SWARM=1`
//! as well (CI sets it on a throwaway single-node swarm). The ACL test installs `acl` into an
//! Alpine container, so it needs the network.

mod harness;
mod life;
mod limits;
mod node;
mod probe;
mod swarm;
