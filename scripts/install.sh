#!/bin/sh
# POSIX installer for Context Radar (context-radar-mcp, Antigravity plugin)
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/eatingDream-antigravity-plugins/context-radar/main/scripts/install.sh | sh
#   curl -fsSL https://raw.githubusercontent.com/eatingDream-antigravity-plugins/context-radar/main/scripts/install.sh | sh -s -- --uninstall
# Optional env overrides:
#   CONTEXT_RADAR_VERSION=v1.0.0
#   ANTIGRAVITY_PLUGINS_DIR=$HOME/.gemini/config/plugins

set -eu

REPO="eatingDream-antigravity-plugins/context-radar"
PLUGIN_NAME="context-radar-mcp"
PLUGINS_DIR="${ANTIGRAVITY_PLUGINS_DIR:-$HOME/.gemini/config/plugins}"
INSTALL_DIR="${PLUGINS_DIR}/${PLUGIN_NAME}"
UNINSTALL=0

while [ $# -gt 0 ]; do
  case "$1" in
    --uninstall)
      UNINSTALL=1
      shift
      ;;
    -h|--help)
      printf 'Usage: %s [--uninstall]\n' "$0"
      printf 'Env:   CONTEXT_RADAR_VERSION, ANTIGRAVITY_PLUGINS_DIR\n'
      exit 0
      ;;
    *)
      printf 'error: unknown argument: %s\n' "$1" >&2
      exit 2
      ;;
  esac
done

die() {
  printf 'error: %s\n' "$1" >&2
  exit 1
}

need_cmd() {
  command -v "$1" >/dev/null 2>&1 || die "required command not found: $1"
}

detect_target() {
  os=$(uname -s)
  arch=$(uname -m)

  case "$os" in
    Darwin)
      case "$arch" in
        arm64|aarch64) printf 'aarch64-apple-darwin\n' ;;
        x86_64|amd64)  printf 'x86_64-apple-darwin\n' ;;
        *) die "unsupported macOS architecture: $arch (build from source with cargo)" ;;
      esac
      ;;
    Linux)
      case "$arch" in
        x86_64|amd64)  printf 'x86_64-unknown-linux-gnu\n' ;;
        *) die "unsupported Linux architecture: $arch (build from source with cargo)" ;;
      esac
      ;;
    *)
      die "unsupported operating system: $os (build from source with cargo)"
      ;;
  esac
}

resolve_tag() {
  if [ -n "${CONTEXT_RADAR_VERSION:-}" ]; then
    case "$CONTEXT_RADAR_VERSION" in
      v*) printf '%s\n' "$CONTEXT_RADAR_VERSION" ;;
      *)  printf 'v%s\n' "$CONTEXT_RADAR_VERSION" ;;
    esac
    return
  fi

  effective_url=$(curl -fsSLI -o /dev/null -w '%{url_effective}' "https://github.com/${REPO}/releases/latest") \
    || die "failed to query latest release from https://github.com/${REPO}/releases/latest"
  tag=$(printf '%s' "$effective_url" | sed 's#.*/tag/##')
  case "$tag" in
    v*) printf '%s\n' "$tag" ;;
    *)  die "could not resolve release tag from URL: $effective_url (set CONTEXT_RADAR_VERSION explicitly)" ;;
  esac
}

verify_sha256() {
  archive_path="$1"
  checksum_path="$2"
  expected=$(awk '{print $1; exit}' "$checksum_path")
  [ -n "$expected" ] || die "empty checksum file: $checksum_path"

  if command -v sha256sum >/dev/null 2>&1; then
    actual=$(sha256sum "$archive_path" | awk '{print $1}')
  elif command -v shasum >/dev/null 2>&1; then
    actual=$(shasum -a 256 "$archive_path" | awk '{print $1}')
  else
    die "neither sha256sum nor shasum is available to verify checksum"
  fi

  if [ "$actual" != "$expected" ]; then
    die "SHA-256 mismatch! expected=${expected} actual=${actual}"
  fi
}

uninstall_plugin() {
  case "$INSTALL_DIR" in
    ""|"/"|"$HOME"|"$HOME/")
      die "refusing to remove unsafe install path: ${INSTALL_DIR}"
      ;;
  esac
  if [ -d "$INSTALL_DIR" ]; then
    rm -rf "$INSTALL_DIR"
    printf 'Uninstalled %s from %s\n' "$PLUGIN_NAME" "$INSTALL_DIR"
  else
    printf '%s is not installed at %s\n' "$PLUGIN_NAME" "$INSTALL_DIR"
  fi
}

main() {
  if [ "$UNINSTALL" -eq 1 ]; then
    uninstall_plugin
    return 0
  fi

  need_cmd curl
  need_cmd tar
  need_cmd mktemp

  target=$(detect_target)
  tag=$(resolve_tag)
  archive="${PLUGIN_NAME}-${tag}-${target}.tar.gz"
  base_url="https://github.com/${REPO}/releases/download/${tag}"

  tmp_dir=$(mktemp -d)
  trap 'rm -rf "$tmp_dir"' EXIT INT TERM

  printf 'Installing %s %s (%s)...\n' "$PLUGIN_NAME" "$tag" "$target"
  curl -fsSL -o "${tmp_dir}/${archive}" "${base_url}/${archive}" \
    || die "failed to download ${base_url}/${archive}"
  curl -fsSL -o "${tmp_dir}/${archive}.sha256" "${base_url}/${archive}.sha256" \
    || die "failed to download ${base_url}/${archive}.sha256"

  verify_sha256 "${tmp_dir}/${archive}" "${tmp_dir}/${archive}.sha256"
  printf 'SHA-256 verified.\n'

  stage_dir="${tmp_dir}/stage"
  mkdir -p "$stage_dir"
  tar -xzf "${tmp_dir}/${archive}" -C "$stage_dir"

  [ -f "${stage_dir}/bin/${PLUGIN_NAME}" ] || die "archive missing bin/${PLUGIN_NAME}"
  [ -f "${stage_dir}/plugin.json" ] || die "archive missing plugin.json"

  mkdir -p "${INSTALL_DIR}/bin" "${INSTALL_DIR}/panel/dist" "${INSTALL_DIR}/rules" "${INSTALL_DIR}/skills/context-radar" "${INSTALL_DIR}/assets" "${INSTALL_DIR}/docs"

  # Atomic binary swap so an active daemon binary is never overwritten in place
  cp "${stage_dir}/bin/${PLUGIN_NAME}" "${INSTALL_DIR}/bin/${PLUGIN_NAME}.new"
  chmod 0755 "${INSTALL_DIR}/bin/${PLUGIN_NAME}.new"
  mv -f "${INSTALL_DIR}/bin/${PLUGIN_NAME}.new" "${INSTALL_DIR}/bin/${PLUGIN_NAME}"

  cp "${stage_dir}/panel/dist/panel.html" "${INSTALL_DIR}/panel/dist/panel.html"
  cp "${stage_dir}/rules/AGENTS.md" "${INSTALL_DIR}/rules/AGENTS.md"
  cp "${stage_dir}/skills/context-radar/SKILL.md" "${INSTALL_DIR}/skills/context-radar/SKILL.md"
  cp "${stage_dir}/assets/logo.svg" "${INSTALL_DIR}/assets/logo.svg"
  if [ -f "${stage_dir}/docs/ai-guide.md" ]; then
    cp "${stage_dir}/docs/ai-guide.md" "${INSTALL_DIR}/docs/ai-guide.md"
  fi
  for f in plugin.json mcp_config.json hooks.json LICENSE README.md llms.txt settings.example.json settings.schema.json; do
    if [ -f "${stage_dir}/${f}" ]; then
      cp "${stage_dir}/${f}" "${INSTALL_DIR}/${f}"
    fi
  done

  printf 'Installed %s %s to %s\n' "$PLUGIN_NAME" "$tag" "$INSTALL_DIR"
  "${INSTALL_DIR}/bin/${PLUGIN_NAME}" --version || true
}

main "$@"
