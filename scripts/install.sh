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
MCP_SERVER_ID="${PLUGIN_NAME}_${PLUGIN_NAME}"
PLUGINS_DIR="${ANTIGRAVITY_PLUGINS_DIR:-$HOME/.gemini/config/plugins}"
INSTALL_DIR="${PLUGINS_DIR}/${PLUGIN_NAME}"
UNINSTALL=0
FORCE_RELOAD=0
NO_RELOAD=0

while [ $# -gt 0 ]; do
  case "$1" in
    --uninstall)
      UNINSTALL=1
      shift
      ;;
    --reload)
      FORCE_RELOAD=1
      shift
      ;;
    --no-reload)
      NO_RELOAD=1
      shift
      ;;
    -h|--help)
      printf 'Usage: %s [--uninstall] [--reload] [--no-reload]\n' "$0"
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

is_ascii_single_line() {
  [ -n "$1" ] && [ "$(printf '%s' "$1" | LC_ALL=C tr -d '\041-\176' | wc -c | tr -d ' ')" -eq 0 ]
}

validate_semver_tag() {
  tag_val="$1"
  is_ascii_single_line "$tag_val" || die "invalid release tag: ${tag_val}"
  case "$tag_val" in
    *..*)
      die "invalid release tag: ${tag_val}"
      ;;
  esac
  if ! printf '%s\n' "$tag_val" | LC_ALL=C grep -Eq '^v[0-9]+\.[0-9]+\.[0-9]+([-.][0-9A-Za-z.]+)?$'; then
    die "invalid release tag (expected vX.Y.Z): ${tag_val}"
  fi
}

resolve_tag() {
  if [ -n "${CONTEXT_RADAR_VERSION:-}" ]; then
    case "$CONTEXT_RADAR_VERSION" in
      v*) tag="$CONTEXT_RADAR_VERSION" ;;
      *)  tag="v${CONTEXT_RADAR_VERSION}" ;;
    esac
    validate_semver_tag "$tag"
    printf '%s\n' "$tag"
    return
  fi

  effective_url=$(curl -fsSLI -o /dev/null -w '%{url_effective}' "https://github.com/${REPO}/releases/latest") \
    || die "failed to query latest release from https://github.com/${REPO}/releases/latest"
  tag=$(printf '%s' "$effective_url" | sed 's#.*/tag/##')
  validate_semver_tag "$tag"
  printf '%s\n' "$tag"
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

validate_ls_addr() {
  addr="$1"
  is_ascii_single_line "$addr" || return 1
  printf '%s\n' "$addr" | LC_ALL=C grep -Eq '^(localhost|127\.0\.0\.1|\[::1\]):[0-9]{1,5}$' || return 1
  port="${addr##*:}"
  [ "$port" -ge 1 ] && [ "$port" -le 65535 ] 2>/dev/null
}

install_file() {
  src="$1"
  dst="$2"
  mode="$3"
  [ -f "$src" ] && [ ! -L "$src" ] || die "staged asset missing or symlink: $src"
  [ ! -L "$dst" ] || die "refusing to overwrite symlink destination: $dst"
  tmp_dst="${dst}.new.$$"
  rm -f "$tmp_dst"
  cp "$src" "$tmp_dst"
  chmod "$mode" "$tmp_dst"
  mv -f "$tmp_dst" "$dst"
}

refresh_mcp_host() {
  if [ "$NO_RELOAD" -eq 1 ]; then
    return 0
  fi
  if [ -n "${ANTIGRAVITY_CSRF_TOKEN:-}" ]; then
    ls_addr="${ANTIGRAVITY_LS_ADDRESS:-localhost:5387}"
    if validate_ls_addr "$ls_addr"; then
      if [ "$(printf '%s' "$ANTIGRAVITY_CSRF_TOKEN" | LC_ALL=C tr -d '\040-\176' | wc -c | tr -d ' ')" -eq 0 ]; then
        esc_token=$(printf '%s' "$ANTIGRAVITY_CSRF_TOKEN" | sed 's/\\/\\\\/g; s/"/\\"/g')
        if printf 'header = "Content-Type: application/json"\nheader = "x-codeium-csrf-token: %s"\n' "$esc_token" | \
          curl -sfg --connect-timeout 2 --max-time 5 -K - \
            -d "{\"serverName\":\"${MCP_SERVER_ID}\"}" \
            "http://${ls_addr}/exa.language_server_pb.LanguageServerService/RefreshMcpServers" >/dev/null 2>&1; then
          printf 'Reloaded MCP server %s in Antigravity (reopen any open Context Radar panel tab to see UI updates).\n' "$MCP_SERVER_ID"
          return 0
        fi
      fi
    else
      printf 'warning: ignoring invalid ANTIGRAVITY_LS_ADDRESS (expected loopback host:port)\n' >&2
    fi
  fi
  if [ "$FORCE_RELOAD" -eq 1 ] && command -v agy >/dev/null 2>&1; then
    if agy plugin disable "$PLUGIN_NAME" >/dev/null 2>&1 && agy plugin enable "$PLUGIN_NAME" >/dev/null 2>&1; then
      printf 'Reloaded plugin %s via agy CLI.\n' "$PLUGIN_NAME"
      return 0
    fi
  fi
  printf 'Note: To load the updated binary/UI, restart the MCP server in Antigravity (Customizations > Installed > %s > MCP > Restart) or run with --reload outside Antigravity.\n' "$PLUGIN_NAME"
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

  [ -f "${stage_dir}/bin/${PLUGIN_NAME}" ] && [ ! -L "${stage_dir}/bin/${PLUGIN_NAME}" ] || die "archive missing bin/${PLUGIN_NAME}"
  [ -f "${stage_dir}/plugin.json" ] && [ ! -L "${stage_dir}/plugin.json" ] || die "archive missing plugin.json"

  [ ! -L "$PLUGINS_DIR" ] || die "refusing symlink plugins directory: ${PLUGINS_DIR}"
  [ ! -L "$INSTALL_DIR" ] || die "refusing symlink install directory: ${INSTALL_DIR}"

  mkdir -p "${INSTALL_DIR}/bin" "${INSTALL_DIR}/panel/dist" "${INSTALL_DIR}/rules" "${INSTALL_DIR}/skills/context-radar" "${INSTALL_DIR}/assets" "${INSTALL_DIR}/docs"
  for d in bin panel panel/dist rules skills skills/context-radar assets docs; do
    [ ! -L "${INSTALL_DIR}/${d}" ] || die "refusing symlink directory: ${INSTALL_DIR}/${d}"
  done

  # Atomic binary & asset swaps so active files or symlinks are never overwritten in place
  install_file "${stage_dir}/bin/${PLUGIN_NAME}" "${INSTALL_DIR}/bin/${PLUGIN_NAME}" 0755
  install_file "${stage_dir}/panel/dist/panel.html" "${INSTALL_DIR}/panel/dist/panel.html" 0644
  install_file "${stage_dir}/rules/AGENTS.md" "${INSTALL_DIR}/rules/AGENTS.md" 0644
  install_file "${stage_dir}/skills/context-radar/SKILL.md" "${INSTALL_DIR}/skills/context-radar/SKILL.md" 0644
  install_file "${stage_dir}/assets/logo.svg" "${INSTALL_DIR}/assets/logo.svg" 0644
  if [ -f "${stage_dir}/docs/ai-guide.md" ]; then
    install_file "${stage_dir}/docs/ai-guide.md" "${INSTALL_DIR}/docs/ai-guide.md" 0644
  fi
  for f in plugin.json mcp_config.json hooks.json LICENSE README.md llms.txt settings.example.json settings.schema.json; do
    if [ -f "${stage_dir}/${f}" ]; then
      install_file "${stage_dir}/${f}" "${INSTALL_DIR}/${f}" 0644
    fi
  done

  printf 'Installed %s %s to %s\n' "$PLUGIN_NAME" "$tag" "$INSTALL_DIR"
  "${INSTALL_DIR}/bin/${PLUGIN_NAME}" --version || true
  refresh_mcp_host
}

main "$@"
