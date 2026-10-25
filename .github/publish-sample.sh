#!/usr/bin/env bash
# Emit a file as gzip+base64 notice annotations, which the public check-runs
# API serves without auth. GitHub caps a message at 4096 chars and a step at
# 10 notices, so: publish-sample.sh FILE BATCH  emits chunks BATCH*10..+9.
set -euo pipefail
python3 -c 'import json,sys; print(json.dumps(json.load(open(sys.argv[1])), separators=(",", ":")))' "$1" > /tmp/compact.json
data=$(gzip -9c /tmp/compact.json | base64 -w0)
size=4000
n=$(( (${#data} + size - 1) / size ))
for ((i = ${2:-0} * 10; i < n && i < (${2:-0} + 1) * 10; i++)); do
  echo "::notice title=native-sample $((i + 1))/$n::${data:i*size:size}"
done
