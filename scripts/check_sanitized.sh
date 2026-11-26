#!/usr/bin/env bash
# Fail if any tracked file matches a pattern from .github/sanitize-patterns.txt
# (generic secret/private-path shapes only). Run from the repo root.
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"
patterns="${SANITIZE_PATTERNS_FILE:-.github/sanitize-patterns.txt}"
if [[ ! -f "$patterns" ]]; then
  echo "sanitization guard: pattern file not found: $patterns" >&2
  exit 2
fi
tmp=$(mktemp)
matches=$(mktemp)
trap 'rm -f "$tmp" "$matches"' EXIT
grep -vE '^[[:space:]]*(#|$)' "$patterns" > "$tmp" || true
if [[ ! -s "$tmp" ]]; then
  echo "sanitization guard: pattern file is empty: $patterns" >&2
  exit 2
fi

set +e
git grep -nEIz -f "$tmp" -- . ":(exclude)$patterns" ":(exclude)scripts/check_sanitized.sh" > "$matches"
rc=$?
set -e

if [[ $rc -eq 0 ]]; then
  # Emit ONLY file:line locations — never echo matched secret lines into CI logs.
  while IFS= read -r -d '' file && IFS= read -r -d '' lineno && IFS= read -r _content; do
    printf '%s:%s\n' "$file" "$lineno" >&2
  done < "$matches"
  echo "sanitization guard: forbidden pattern found (line content redacted)" >&2
  exit 1
elif [[ $rc -eq 1 ]]; then
  echo "sanitization guard: clean"
  exit 0
else
  echo "sanitization guard: scanner error (git grep exited with status $rc)" >&2
  exit 2
fi
