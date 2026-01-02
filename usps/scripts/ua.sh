#!/usr/bin/env bash

set -euo pipefail

# Fetch Chrome stable version from Google's API
VERSION_JSON=$(curl -s "https://versionhistory.googleapis.com/v1/chrome/platforms/win/channels/stable/versions")
FULL_VERSION=$(echo "$VERSION_JSON" | jq -r '.versions[0].version')
MAJOR_VERSION=$(echo "$FULL_VERSION" | cut -d'.' -f1)
REDUCED_VERSION="${MAJOR_VERSION}.0.0.0"

# Default to mac if no argument provided
PLATFORM="${1:-mac}"

case "$PLATFORM" in
    mac|macos)
        echo "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/${REDUCED_VERSION} Safari/537.36"
        ;;
    windows|win)
        echo "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/${REDUCED_VERSION} Safari/537.36"
        ;;
    linux)
        echo "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/${REDUCED_VERSION} Safari/537.36"
        ;;
    *)
        echo "Unknown platform: $PLATFORM" >&2
        echo "Usage: $0 [mac|windows|linux]" >&2
        exit 1
        ;;
esac
