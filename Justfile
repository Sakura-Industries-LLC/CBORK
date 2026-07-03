set shell := ["bash", "-cu"]

set dotenv-load

DEVTOOLS_IMAGE := env("DEVTOOLS_IMAGE", "sakura-dev-tools:latest")
# This project officially supports Podman as its container runtime.
CONTAINER_RUNTIME := env("CONTAINER_RUNTIME", "podman")

mod local 'just/local.just'

_default:
    @just --list

# Setup moon for local use/development
moon-setup:
    moon sync config-schemas

# Make sure cache-dirs exist
setup-cache-dir:
    mkdir -p .moon/cache
    mkdir -p .moon/container-cache

# Fix what can be fixed
fix: setup-cache-dir (_container-mount "moon run :fix")

# Run a CI run inside the build container.
ci: setup-cache-dir (_container-mount "moon run :ci")

# Run Fix + CI run inside the build container.
fix-ci: setup-cache-dir (_container-mount "moon run :fix && moon run :ci")

# Execute a shell directly inside the build container.
container-shell: (_container-mount "bash")

# Run the cbork command
cbork *cmd: setup-cache-dir (_container-mount ("cargo run -p cbork -- " + cmd))

# Common mounted container
_container-mount *cmd:
    {{ CONTAINER_RUNTIME }} run --rm \
        -it \
        -v .:/repo:rw \
        -v .moon/container-cache:/repo/.moon/cache:rw \
        -w /repo \
        {{ DEVTOOLS_IMAGE }} \
        bash -c '{{ cmd }}'

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
