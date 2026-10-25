#!/usr/bin/env bash
# Emit a file as gzip+base64 notice annotations (60k chars each), which the
# public check-runs API serves without auth.
set -euo pipefail
data=$(gzip -9c "$1" | base64 -w0)
n=$(( (${#data} + 59999) / 60000 ))
for ((i = 0; i < n; i++)); do
  echo "::notice title=native-sample $((i + 1))/$n::${data:i*60000:60000}"
done
