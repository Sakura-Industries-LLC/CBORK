set shell := ["bash", "-cu"]

set dotenv-load

DEVTOOLS_IMAGE := env("DEVTOOLS_IMAGE", "sakura-dev-tools:latest")
# This project officially supports Podman as its container runtime.
CONTAINER_RUNTIME := env("CONTAINER_RUNTIME", "podman")
# Set to 1 when running inside the Sakura dev container, where the build tools
# already run on the host and podman is neither available nor needed.
SAKURA_DEV_WORKSPACE := env("SAKURA_DEV_WORKSPACE", "0")

mod local 'just/local.just'

_default:
    @just --list

# Setup moon for local use/development
moon-setup:
    moon sync config-schemas

# Make sure cache-dirs exist
setup-cache-dir:
    #!/usr/bin/env sh
    if [ "{{ SAKURA_DEV_WORKSPACE }}" = "1" ]; then
        exit 0
    else
        mkdir -p .moon/cache
        mkdir -p .moon/container-cache
    fi

# Fix what can be fixed
fix: setup-cache-dir (_container-mount "moon run :fix")

# Run a CI run inside the build container.
ci: setup-cache-dir (_container-mount "moon run :ci")

# Run Fix + CI run inside the build container.
fix-ci: setup-cache-dir (_container-mount "moon run :fix && moon run :ci")

# Execute a shell directly inside the build container.
container-shell: (_container-mount "bash")

# Run the cbork command
cbork *cmd: setup-cache-dir (_container-mount ("cargo run --release --frozen -p cbork -- " + cmd))

# Render, lint, re-render, and compare every repository CDDL schema.
render-roundtrip: setup-cache-dir (_container-mount "moon run :render-roundtrip-all")

# Common mounted container
_container-mount *cmd:
    @just _container-run "" '{{ cmd }}'

# Sync a set of CDDL files from another repo for testing.
_sync-cddl SRC DEST="test":
    mkdir -p {{ DEST }}
    rsync -av --prune-empty-dirs \
      --include='*/' \
      --include='*.cddl' \
      --include='*.cbor' \
      --exclude='*' \
      --delete \
      {{ SRC }}/ \
      {{ DEST }}/

# Run tests on the dntls-libs CDDL definitions.
test-dntls-libs: setup-cache-dir (_sync-cddl "../DNTLS/dntls-libs/libs") (_container-mount "./test-dntls-libs.sh")

# Run a command inside the build container, or directly on the host when the
# workspace already runs inside the container (SAKURA_DEV_WORKSPACE=1).
_container-run extra-flags *cmd:
    #!/usr/bin/env sh
    if [ "{{ SAKURA_DEV_WORKSPACE }}" = "1" ]; then
        bash -c '{{ cmd }}'
    else
        {{ CONTAINER_RUNTIME }} run --rm \
            -it \
            {{ extra-flags }} \
            -v .:/repo:rw \
            -v .moon/container-cache:/repo/.moon/cache:rw \
            -w /repo \
            {{ DEVTOOLS_IMAGE }} \
            bash -c '{{ cmd }}'
    fi
