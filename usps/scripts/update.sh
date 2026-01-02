#!/usr/bin/env bash

set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."
USER_AGENT="$(scripts/ua.sh)"

cd newsroom

arg="${1:-$(date +%Y)}"

if [[ "$arg" == "all" ]]; then
    years=({2009..2026})
else
    years=("$arg")
fi

for year in "${years[@]}"; do
    curl \
        -A "$USER_AGENT" \
        "https://about.usps.com/newsroom/national-releases/${year}/data.json" \
        | jq . > "${year}.json"
    sleep 1
done
