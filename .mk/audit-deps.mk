#
# MIT License
#
# Copyright (c) 2026 Roberto Leinardi
#
# Permission is hereby granted, free of charge, to any person obtaining a copy
# of this software and associated documentation files (the "Software"), to deal
# in the Software without restriction, including without limitation the rights
# to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
# copies of the Software, and to permit persons to whom the Software is
# furnished to do so, subject to the following conditions:
#
# The above copyright notice and this permission notice shall be included in all
# copies or substantial portions of the Software.
#
# THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
# IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
# FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
# AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
# LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
# OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
# SOFTWARE.
#

ifndef MK_LOCAL_AUDIT_DEPS_INCLUDED
MK_LOCAL_AUDIT_DEPS_INCLUDED := 1

# Local snippet (NOT part of make-common): check the dependencies this repository ships against
# the RustSec advisory database, with cargo-deny, configured in deny.toml.
#
# cargo-deny is not reachability-aware: any advisory against a crate in the graph counts, the
# dev-dependencies included, which is why deny.toml's ignores each carry a reason. It needs the
# network to fetch the advisory database, so this is the target run deliberately whenever
# Cargo.toml or Cargo.lock change (the pre-commit hook and CI run it too).

.PHONY: audit-deps
audit-deps: ## Check every Rust dependency against the RustSec advisories (cargo-deny; network required)
	$(CARGO) deny check advisories

endif  # MK_LOCAL_AUDIT_DEPS_INCLUDED
