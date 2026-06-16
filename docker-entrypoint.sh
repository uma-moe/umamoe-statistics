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
child_pid=""

stop_child_and_exit() {
  exit_code="$1"
  if [ -n "${child_pid:-}" ]; then
    kill "$child_pid" 2>/dev/null || true
    wait "$child_pid" 2>/dev/null || true
  fi
  exit "$exit_code"
}

trap 'stop_child_and_exit 130' INT
trap 'stop_child_and_exit 143' TERM

is_true() {
  case "$1" in
    1|true|TRUE|yes|YES|on|ON)
      return 0
      ;;
    *)
      return 1
      ;;
  esac
}

strip_leading_zeroes() {
  value="$1"
  while [ "${value#0}" != "$value" ]; do
    value="${value#0}"
  done
  if [ -z "$value" ]; then
    value=0
  fi
  printf '%s\n' "$value"
}

require_uint_range() {
  name="$1"
  value="$2"
  min="$3"
  max="$4"

  case "$value" in
    ''|*[!0-9]*)
      echo "$name must be an integer between $min and $max, got: $value" >&2
      exit 1
      ;;
  esac

  value="$(strip_leading_zeroes "$value")"
  if [ "$value" -lt "$min" ] || [ "$value" -gt "$max" ]; then
    echo "$name must be an integer between $min and $max, got: $value" >&2
    exit 1
  fi
}

run_generator() {
  run_dataset_version="$1"
  run_limit="$2"

  set -- \
    --repo-root "$repo_root" \
    --progress-every "$progress_every"

  if [ -n "$run_dataset_version" ]; then
    set -- "$@" --dataset-version "$run_dataset_version"
  fi

  if [ -n "$run_limit" ]; then
    set -- "$@" --limit "$run_limit"
  fi

  if [ -n "${UMAMOE_WORKER_THREADS:-}" ]; then
    set -- "$@" --worker-threads "$UMAMOE_WORKER_THREADS"
  fi

  if [ -n "${UMAMOE_BATCH_ROWS:-}" ]; then
    set -- "$@" --batch-rows "$UMAMOE_BATCH_ROWS"
  fi

  if is_true "${UMAMOE_RESOURCE_USAGE:-false}"; then
    set -- "$@" --resource-usage
  fi

  if [ -n "${UMAMOE_TARGET_ROOTS:-}" ]; then
    for target_root in $UMAMOE_TARGET_ROOTS; do
      set -- "$@" --publish-dir "${target_root%/}/${statistics_relative_dir#/}"
    done
  else
    set -- "$@" --output-dir "$output_dir"
  fi

  echo "Starting statistics export at $(date -u +%Y-%m-%dT%H:%M:%SZ)."
  umamoe-statistics-generator "$@" &
  child_pid="$!"

  set +e
  wait "$child_pid"
  status="$?"
  set -e
  child_pid=""

  if [ "$status" -eq 0 ]; then
    echo "Finished statistics export at $(date -u +%Y-%m-%dT%H:%M:%SZ)."
  else
    echo "Statistics export failed with exit code $status at $(date -u +%Y-%m-%dT%H:%M:%SZ)." >&2
  fi

  return "$status"
}

format_epoch() {
  TZ="$schedule_timezone" date -d "@$1" +"%Y-%m-%d %H:%M:%S %Z"
}

next_run_epoch() {
  now_epoch="$(TZ="$schedule_timezone" date +%s)"
  today="$(TZ="$schedule_timezone" date +%u)"
  current_hour="$(strip_leading_zeroes "$(TZ="$schedule_timezone" date +%H)")"
  current_minute="$(strip_leading_zeroes "$(TZ="$schedule_timezone" date +%M)")"
  current_second="$(strip_leading_zeroes "$(TZ="$schedule_timezone" date +%S)")"
  current_seconds=$((current_hour * 3600 + current_minute * 60 + current_second))
  target_seconds=$((schedule_hour * 3600 + schedule_minute * 60))

  days_ahead=$((schedule_day - today))
  if [ "$days_ahead" -lt 0 ]; then
    days_ahead=$((days_ahead + 7))
  fi
  if [ "$days_ahead" -eq 0 ] && [ "$current_seconds" -ge "$target_seconds" ]; then
    days_ahead=7
  fi

  local_date="$(TZ="$schedule_timezone" date +%F)"
  target_time="$(printf '%02d:%02d:00' "$schedule_hour" "$schedule_minute")"
  base_epoch="$(TZ="$schedule_timezone" date -d "$local_date $target_time" +%s)"

  printf '%s\n' "$((base_epoch + days_ahead * 86400))"
}

sleep_for() {
  seconds="$1"
  sleep "$seconds" &
  child_pid="$!"

  set +e
  wait "$child_pid"
  status="$?"
  set -e
  child_pid=""

  return "$status"
}

run_scheduler() {
  schedule_timezone="${UMAMOE_SCHEDULE_TZ:-Asia/Tokyo}"
  schedule_day="${UMAMOE_SCHEDULE_DAY:-6}"
  schedule_hour="${UMAMOE_SCHEDULE_HOUR:-0}"
  schedule_minute="${UMAMOE_SCHEDULE_MINUTE:-0}"

  require_uint_range UMAMOE_SCHEDULE_DAY "$schedule_day" 1 7
  require_uint_range UMAMOE_SCHEDULE_HOUR "$schedule_hour" 0 23
  require_uint_range UMAMOE_SCHEDULE_MINUTE "$schedule_minute" 0 59

  schedule_day="$(strip_leading_zeroes "$schedule_day")"
  schedule_hour="$(strip_leading_zeroes "$schedule_hour")"
  schedule_minute="$(strip_leading_zeroes "$schedule_minute")"

  echo "Statistics scheduler enabled: weekday $schedule_day at $(printf '%02d:%02d' "$schedule_hour" "$schedule_minute") in $schedule_timezone."

  if is_true "${UMAMOE_RUN_ON_START:-false}"; then
    if ! run_generator "${UMAMOE_RUN_ON_START_DATASET_VERSION:-}" "${UMAMOE_RUN_ON_START_LIMIT:-}"; then
      echo "Startup statistics export failed; scheduler will keep waiting for future runs." >&2
    fi
  fi

  while :; do
    next_epoch="$(next_run_epoch)"
    now_epoch="$(TZ="$schedule_timezone" date +%s)"
    sleep_seconds=$((next_epoch - now_epoch))
    if [ "$sleep_seconds" -lt 0 ]; then
      sleep_seconds=0
    fi

    echo "Next scheduled statistics export: $(format_epoch "$next_epoch") ($sleep_seconds seconds from now)."
    sleep_for "$sleep_seconds"

    if ! run_generator "" ""; then
      echo "Scheduled statistics export failed; scheduler will keep waiting for future runs." >&2
    fi
  done
}

case "${UMAMOE_RUN_MODE:-once}" in
  once|oneshot|one-shot)
    run_generator "${UMAMOE_DATASET_VERSION:-}" "${UMAMOE_LIMIT:-}"
    ;;
  schedule|scheduled|scheduler)
    run_scheduler
    ;;
  *)
    echo "Unsupported UMAMOE_RUN_MODE: ${UMAMOE_RUN_MODE:-once}" >&2
    exit 1
    ;;
esac
