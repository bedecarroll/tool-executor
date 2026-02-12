#!/usr/bin/env bash
set -euo pipefail

lcov_path="${1:-coverage/lcov.info}"
workspace="$(pwd -P)"
source_prefix="${workspace}/src/"

if [[ ! -f "${lcov_path}" ]]; then
  echo "error: lcov file not found: ${lcov_path}" >&2
  exit 1
fi

read -r total missed < <(
  awk -v prefix="${source_prefix}" '
    BEGIN { file = ""; total = 0; missed = 0 }
    /^SF:/ {
      file = substr($0, 4);
      next;
    }
    /^DA:/ {
      split(substr($0, 4), fields, ",");
      if (index(file, prefix) == 1) {
        total += 1;
        if (fields[2] == 0) {
          missed += 1;
        }
      }
    }
    END { printf "%d %d\n", total, missed }
  ' "${lcov_path}"
)

if [[ "${total}" -eq 0 ]]; then
  echo "error: no line data found under ${source_prefix}" >&2
  exit 1
fi

covered=$((total - missed))
percent="$(awk -v c="${covered}" -v t="${total}" 'BEGIN { printf "%.2f", (100 * c) / t }')"

echo "lcov source line coverage: ${covered}/${total} (${percent}%)"
if [[ "${missed}" -ne 0 ]]; then
  echo "error: ${missed} uncovered source lines remain" >&2
  exit 1
fi
