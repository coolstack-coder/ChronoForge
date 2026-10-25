#!/usr/bin/env bash
# Turn the interesting bits of a failed test log into a GitHub annotation,
# so failures show up on the run summary without digging through logs.
log="${1:-test.log}"
[ -f "$log" ] || exit 0
excerpt=$(grep -E -A12 "panicked at|^error(\[|:)|^failures:" "$log" | head -80)
[ -n "$excerpt" ] || excerpt=$(tail -40 "$log")
excerpt=${excerpt//'%'/'%25'}
excerpt=${excerpt//$'\r'/}
excerpt=${excerpt//$'\n'/'%0A'}
echo "::error title=test failures::${excerpt}"
