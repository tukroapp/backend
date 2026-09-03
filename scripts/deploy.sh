#!/usr/bin/env bash
set -euo pipefail

usage() {
    echo "uso: $0 [host] [versao]" >&2
    echo "  host   usuario@ip ou entrada do ~/.ssh/config; usa TUKRO_DEPLOY_HOST se omitido" >&2
    echo "  versao rotulo da release; usa git describe se omitido" >&2
    exit 1
}

HOST="${1:-${TUKRO_DEPLOY_HOST:-}}"
test -n "$HOST" || usage

TARGET=aarch64-unknown-linux-gnu
VERSION="${2:-$(git describe --tags --always --dirty)}"
case "$VERSION" in *-dirty)
    echo "working tree sujo, commita antes"
    exit 1
    ;;
esac

SQLX_OFFLINE=true cargo zigbuild --release --bins --target "$TARGET.2.39"
ssh "$HOST" "mkdir -p /opt/tukro/releases/$VERSION"
rsync -az --chmod=755 "target/$TARGET/release/tukro" "$HOST:/opt/tukro/releases/$VERSION/"
ssh "$HOST" "sudo /opt/tukro/deploy.sh $VERSION"
