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


ifndef MK_LOCAL_IMAGE_INCLUDED
MK_LOCAL_IMAGE_INCLUDED := 1

# Local snippet (NOT part of make-common): the image. One variant per consumer UID and GID, built
# from the floating upstream tag as is; only the release pins the base digest (BASE_DIGEST). The
# version strings are rust.mk's, so the labels and the binary say the same.
#
# Two ways to build it. image-build runs the whole Dockerfile, its build stage included (dhi.io/rust
# and cargo-zigbuild; it needs a dhi.io login): the default for local development. image-binary and
# image-build-prebuilt are the release's packaging path, which CI tests on every pull request and
# every push to main: the static binary is built here, and a buildx named context replaces the
# Dockerfile's build stage with it, so dhi.io is never contacted and the image's binary is that
# file byte for byte. release.yaml runs image-binary with its own stamps and builds the image the
# same way.

DOCKERFILE   ?= deployments/docker/Dockerfile
IMAGE_REPO   ?= ghcr.io/leinardi/$(BIN_NAME)
ROOTLESS_UID ?= 568
ROOTLESS_GID ?= $(ROOTLESS_UID)
IMAGE_TAG    ?= dev-uid$(ROOTLESS_UID)
IMAGE        ?= $(IMAGE_REPO):$(IMAGE_TAG)

IMAGE_BUILD_ARGS = \
  --build-arg VERSION=$(GIT_VERSION) \
  --build-arg COMMIT=$(GIT_COMMIT) \
  --build-arg DATE=$(BUILD_DATE) \
  --build-arg ROOTLESS_UID=$(ROOTLESS_UID) \
  --build-arg ROOTLESS_GID=$(ROOTLESS_GID)

.PHONY: image-build
image-build: ## Build the image for ROOTLESS_UID/ROOTLESS_GID (default 568; needs a dhi.io login)
	docker buildx build --load -f "$(DOCKERFILE)" -t "$(IMAGE)" $(IMAGE_BUILD_ARGS) .

IMAGE_CONTEXT_DIR ?= target/image-context
MUSL_TARGET       ?= x86_64-unknown-linux-musl
# The binary the prebuilt image COPYs: the Dockerfile's runtime stage takes /out/<binary> from the
# build stage, which the named context replaces with IMAGE_CONTEXT_DIR.
IMAGE_BINARY      := $(IMAGE_CONTEXT_DIR)/out/$(BIN_NAME)

.PHONY: image-binary
image-binary: ## Build the static launcher binary the prebuilt image COPYs, with the version stamps
	$(RUST_VERSION_ENV) $(CARGO) build --release --locked --target $(MUSL_TARGET) --bin $(BIN_NAME)
	mkdir -p "$(dir $(IMAGE_BINARY))"
	cp "$(RUST_TARGET_DIR)/$(MUSL_TARGET)/release/$(BIN_NAME)" "$(IMAGE_BINARY)"

.PHONY: image-build-prebuilt
image-build-prebuilt: image-binary ## Build the image from the binary image-binary built (no dhi.io login needed)
	docker buildx build --load -f "$(DOCKERFILE)" -t "$(IMAGE)" $(IMAGE_BUILD_ARGS) \
	  --build-context build="$(IMAGE_CONTEXT_DIR)" .

# IMAGE_TEST_BINARY: the binary the image's launcher must be byte for byte; CI passes IMAGE_BINARY
# after image-build-prebuilt. Empty skips that check (image-build compiles its own).
IMAGE_TEST_BINARY ?=

.PHONY: image-test
image-test: ## Check the built image (user, files, labels, entrypoint, healthcheck; IMAGE_TEST_BINARY)
	scripts/image-test.sh "$(IMAGE)" "$(ROOTLESS_UID)" "$(ROOTLESS_GID)" $(if $(IMAGE_TEST_BINARY),"$(IMAGE_TEST_BINARY)")

# image-build and image-test in order. Goals on one command line are not ordered under make -j, so
# `make image-build image-test` could check a missing or previous image before the build has loaded
# the new one; each step here is its own make run, the second started only after the first
# succeeded. Both stay on their own: image-test also checks the prebuilt image, as CI does.
.PHONY: image-build-test
image-build-test: ## Build the image (image-build), then check it (image-test; ordered, safe under make -j)
	$(MAKE) --no-print-directory image-build
	$(MAKE) --no-print-directory image-test

# One test at a time: they share the node, its lock and its volume list. On hosts that restrict
# unprivileged user namespaces through AppArmor (Ubuntu 23.10 and later), rootlesskit needs
# kernel.apparmor_restrict_unprivileged_userns=0 for the run; set it back afterwards.
.PHONY: integration-test
integration-test: ## Run the integration tests against the built image (IMAGE; see tests/integration.rs)
	SRD_IT=1 SRD_IT_IMAGE="$(IMAGE)" $(CARGO) test --locked --test integration -- --test-threads=1

endif  # MK_LOCAL_IMAGE_INCLUDED
