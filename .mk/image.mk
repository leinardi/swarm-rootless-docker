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

DOCKERFILE   ?= deployments/docker/Dockerfile
IMAGE_REPO   ?= ghcr.io/leinardi/$(BIN_NAME)
ROOTLESS_UID ?= 568
ROOTLESS_GID ?= $(ROOTLESS_UID)
IMAGE_TAG    ?= dev-uid$(ROOTLESS_UID)
IMAGE        ?= $(IMAGE_REPO):$(IMAGE_TAG)

.PHONY: image-build
image-build: ## Build the image for ROOTLESS_UID/ROOTLESS_GID (default 568; needs a dhi.io login)
	docker buildx build --load \
	  -f "$(DOCKERFILE)" \
	  -t "$(IMAGE)" \
	  --build-arg VERSION=$(GIT_VERSION) \
	  --build-arg COMMIT=$(GIT_COMMIT) \
	  --build-arg DATE=$(BUILD_DATE) \
	  --build-arg ROOTLESS_UID=$(ROOTLESS_UID) \
	  --build-arg ROOTLESS_GID=$(ROOTLESS_GID) \
	  .

.PHONY: image-test
image-test: ## Check the built image (user, files, labels, entrypoint, healthcheck)
	scripts/image-test.sh "$(IMAGE)" "$(ROOTLESS_UID)" "$(ROOTLESS_GID)"

endif  # MK_LOCAL_IMAGE_INCLUDED
