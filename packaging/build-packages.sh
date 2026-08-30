#!/bin/sh
set -eu

if [ "$#" -ne 1 ]; then
    echo "usage: $0 VERSION" >&2
    exit 2
fi

VERSION=$1
ARCH=${NFPM_ARCH:-amd64}
DIST_DIR=${DIST_DIR:-dist}
STAGE=$(mktemp -d)
trap 'rm -rf "$STAGE"' EXIT HUP INT TERM

case "$VERSION" in
    ''|*[!0-9.]*)
        echo "version must contain only digits and dots: $VERSION" >&2
        exit 2
        ;;
esac

command -v nfpm >/dev/null 2>&1 || {
    echo "nfpm is required: https://nfpm.goreleaser.com/" >&2
    exit 1
}

test -x target/release/s1500d || {
    echo "target/release/s1500d is missing; run cargo build --release --locked" >&2
    exit 1
}

mkdir -p "$DIST_DIR"
NFPM_VERSION=$VERSION NFPM_ARCH=$ARCH \
    nfpm package --config packaging/nfpm.yaml --packager deb --target "$DIST_DIR/"
NFPM_VERSION=$VERSION NFPM_ARCH=$ARCH \
    nfpm package --config packaging/nfpm.yaml --packager rpm --target "$DIST_DIR/"

make install DESTDIR="$STAGE"
tar -C "$STAGE" -czf "$DIST_DIR/s1500d-$VERSION-linux-$ARCH.tar.gz" .

(
    cd "$DIST_DIR"
    sha256sum ./*.deb ./*.rpm ./*.tar.gz > SHA256SUMS
)
