#!/usr/bin/env bash
# Build the Washburn container images with docker buildx, optionally pushing them.
#
# Images:
#   <registry>/washburn-judge:<tag>    from docker/Dockerfile.rust --target judge
#   <registry>/washburn-sweeper:<tag>  from docker/Dockerfile.rust --target sweeper
#   <registry>/washburn-web:<tag>      from web/Dockerfile (skipped if absent), context web/
# Each image also gets :<git short sha> when the repo has at least one commit.
#
# Registry and tag come from [images] in washburn.toml (WASHBURN_TOML overrides the path).
#
# Usage: scripts/build-images.sh [--push] [--dry-run] [--platform PLATFORM]
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

readonly DEFAULT_REGISTRY="ghcr.io/OWNER"
readonly DEFAULT_TAG="latest"

PUSH=0
DRY_RUN=0
PLATFORM="linux/amd64"
CONFIG="${WASHBURN_TOML:-washburn.toml}"

log() { printf 'build-images: %s\n' "$*" >&2; }
die() { printf 'build-images: error: %s\n' "$*" >&2; exit 1; }

usage() {
  cat <<USAGE
Usage: scripts/build-images.sh [--push] [--dry-run] [--platform PLATFORM]

  --push               push the images to the registry (default: load into the local daemon)
  --dry-run            print the docker buildx commands without running them
  --platform PLATFORM  target platform (default: linux/amd64; the VMs are amd64)

Environment:
  WASHBURN_TOML        path to washburn.toml (default: ./washburn.toml)
USAGE
}

while (($#)); do
  case "$1" in
    --push) PUSH=1 ;;
    --dry-run) DRY_RUN=1 ;;
    --platform)
      (($# >= 2)) || die "--platform needs a value"
      PLATFORM="$2"
      shift
      ;;
    --platform=*) PLATFORM="${1#--platform=}" ;;
    -h|--help) usage; exit 0 ;;
    *) usage >&2; die "unknown argument: $1" ;;
  esac
  shift
done

# Read [images].registry and [images].tag. Missing file or keys fall back to the
# placeholder defaults with a warning.
REGISTRY="$DEFAULT_REGISTRY"
TAG="$DEFAULT_TAG"
if [[ -f "$CONFIG" ]]; then
  if ! cfg_out="$(python3 -I -c '
import sys, tomllib
with open(sys.argv[1], "rb") as fh:
    images = tomllib.load(fh).get("images", {})
print(images.get("registry", ""))
print(images.get("tag", ""))
' "$CONFIG")"; then
    die "could not parse $CONFIG"
  fi
  mapfile -t cfg <<<"$cfg_out"
  if [[ -n "${cfg[0]:-}" ]]; then
    REGISTRY="${cfg[0]}"
  else
    log "warning: [images].registry missing in $CONFIG; using $DEFAULT_REGISTRY"
  fi
  if [[ -n "${cfg[1]:-}" ]]; then
    TAG="${cfg[1]}"
  else
    log "warning: [images].tag missing in $CONFIG; using $DEFAULT_TAG"
  fi
else
  log "warning: $CONFIG not found; using $DEFAULT_REGISTRY and tag $DEFAULT_TAG"
fi
REGISTRY="${REGISTRY%/}"

if ((PUSH)) && [[ "$REGISTRY" == *OWNER* ]]; then
  die "registry is still the placeholder '$REGISTRY'; set [images].registry in $CONFIG before --push"
fi

# Git short sha, when the repo has commits. Empty otherwise (no error).
SHA="$(git rev-parse --short HEAD 2>/dev/null || true)"

# Source URL for the OCI label: origin, normalised to https when it is scp-style SSH.
SOURCE_URL=""
if origin_url="$(git remote get-url origin 2>/dev/null)"; then
  SOURCE_URL="$(printf '%s' "$origin_url" | sed -E 's#^git@([^:]+):#https://\1/#; s#\.git$##')"
fi

if ((DRY_RUN == 0)); then
  command -v docker >/dev/null 2>&1 || die "docker not found in PATH"
fi

# build_image NAME DOCKERFILE CONTEXT [TARGET]
build_image() {
  local name="$1" dockerfile="$2" context="$3" target="${4:-}"
  local image="${REGISTRY}/washburn-${name}"
  local -a cmd=(docker buildx build --platform "$PLATFORM" -f "$dockerfile")
  local -a tags=("${image}:${TAG}")
  [[ -n "$SHA" ]] && tags+=("${image}:${SHA}")
  if [[ -n "$target" ]]; then
    cmd+=(--target "$target")
  fi
  if [[ -n "$SOURCE_URL" && "$dockerfile" == docker/Dockerfile.rust ]]; then
    cmd+=(--build-arg "SOURCE_URL=${SOURCE_URL}")
  fi
  local tag
  for tag in "${tags[@]}"; do
    cmd+=(-t "$tag")
  done
  if ((PUSH)); then
    cmd+=(--push)
  else
    cmd+=(--load)
  fi
  cmd+=("$context")

  if ((DRY_RUN)); then
    printf '%q ' "${cmd[@]}"
    printf '\n'
  else
    log "building ${name} (${tags[*]})"
    "${cmd[@]}"
  fi
}

build_image judge docker/Dockerfile.rust . judge
build_image sweeper docker/Dockerfile.rust . sweeper

if [[ -f web/Dockerfile ]]; then
  build_image web web/Dockerfile web
else
  log "notice: web/Dockerfile not present; skipping washburn-web"
fi

if ((DRY_RUN)); then
  log "dry run: no images built"
elif ((PUSH)); then
  log "pushed images to ${REGISTRY}"
else
  log "loaded images into the local daemon"
fi
