#!/usr/bin/env bash
# Update Formula/coord.rb with the SHA256s of a published GitHub release.
#
# Usage:
#   ./scripts/release-formula.sh v0.3.0
#
# Pulls SHA256SUMS from the release and rewrites the placeholders in
# Formula/coord.rb in place. Commit the result.

set -euo pipefail

if [[ $# -ne 1 ]]; then
    echo "usage: $0 <vX.Y.Z>" >&2
    exit 1
fi

VERSION="${1#v}"
TAG="v${VERSION}"
REPO="DmarshalTU/coord"
FORMULA="Formula/coord.rb"

echo "==> Fetching SHA256SUMS for ${TAG} from ${REPO}"
SUMS="$(curl -fsSL "https://github.com/${REPO}/releases/download/${TAG}/SHA256SUMS")"

extract() {
    local target="$1"
    echo "${SUMS}" | awk -v t="coord-${target}" '$2==t {print $1}'
}

AARCH64_DARWIN="$(extract aarch64-apple-darwin)"
X86_64_LINUX="$(extract x86_64-unknown-linux-gnu)"

for v in "$AARCH64_DARWIN" "$X86_64_LINUX"; do
    if [[ -z "$v" ]]; then
        echo "FATAL: missing one or more checksums in SHA256SUMS" >&2
        echo "${SUMS}"
        exit 1
    fi
done

echo "==> Rewriting ${FORMULA}"
sed -i.bak \
    -e "s|^  version \".*\"|  version \"${VERSION}\"|" \
    -e "s|REPLACE_WITH_SHA256_AARCH64_DARWIN|${AARCH64_DARWIN}|" \
    -e "s|REPLACE_WITH_SHA256_X86_64_LINUX|${X86_64_LINUX}|" \
    "${FORMULA}"
rm -f "${FORMULA}.bak"

echo "==> Done. Review with:  git diff ${FORMULA}"
