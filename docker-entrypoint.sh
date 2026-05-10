#!/bin/sh
set -eu

if [ -z "${DATABASE_URL:-}" ]; then
  echo "DATABASE_URL is required." >&2
  exit 1
fi

repo_root="${UMAMOE_REPO_ROOT:-/app}"
statistics_relative_dir="${UMAMOE_STATISTICS_RELATIVE_DIR:-assets/statistics}"
output_dir="${UMAMOE_OUTPUT_DIR:-/output/statistics}"
progress_every="${UMAMOE_PROGRESS_EVERY:-250000}"

set -- \
  --repo-root "$repo_root" \
  --progress-every "$progress_every"

if [ -n "${UMAMOE_DATASET_VERSION:-}" ]; then
  set -- "$@" --dataset-version "$UMAMOE_DATASET_VERSION"
fi

if [ -n "${UMAMOE_LIMIT:-}" ]; then
  set -- "$@" --limit "$UMAMOE_LIMIT"
fi

case "${UMAMOE_RESOURCE_USAGE:-}" in
  1|true|TRUE|yes|YES)
    set -- "$@" --resource-usage
    ;;
esac

if [ -n "${UMAMOE_TARGET_ROOTS:-}" ]; then
  for target_root in $UMAMOE_TARGET_ROOTS; do
    set -- "$@" --publish-dir "${target_root%/}/${statistics_relative_dir#/}"
  done
else
  set -- "$@" --output-dir "$output_dir"
fi

exec umamoe-statistics-generator "$@"
