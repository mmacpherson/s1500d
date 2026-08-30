#!/bin/sh
set -eu

if [ "$#" -ne 1 ]; then
    echo "usage: $0 VERSION" >&2
    exit 2
fi

VERSION=$1
DIST_DIR=${DIST_DIR:-dist}
STAGE=$(mktemp -d)
trap 'rm -rf "$STAGE"' EXIT HUP INT TERM

case "$VERSION" in
    ''|*[!0-9.]*)
        echo "version must contain only digits and dots: $VERSION" >&2
        exit 2
        ;;
esac

command -v curl >/dev/null 2>&1 || {
    echo "curl is required" >&2
    exit 1
}
command -v docker >/dev/null 2>&1 || {
    echo "docker is required" >&2
    exit 1
}

cp PKGBUILD s1500d.install "$STAGE/"
curl --fail --location --retry 5 \
    --output "$STAGE/s1500d-$VERSION.tar.gz" \
    "https://github.com/mmacpherson/s1500d/archive/v$VERSION.tar.gz"
SOURCE_SHA=$(sha256sum "$STAGE/s1500d-$VERSION.tar.gz" | cut -d ' ' -f 1)

sed -i \
    -e "s/^pkgver=.*/pkgver=$VERSION/" \
    -e "s/^pkgrel=.*/pkgrel=1/" \
    -e "s/^sha256sums=.*/sha256sums=('$SOURCE_SHA')/" \
    "$STAGE/PKGBUILD"

docker run --rm --volume "$STAGE:/build" archlinux:base-devel sh -euxc '
    pacman -Syu --noconfirm --needed rust libusb
    useradd --create-home builder
    chown -R builder:builder /build
    runuser -u builder -- sh -c "cd /build && makepkg --cleanbuild --noconfirm"
'

mkdir -p "$DIST_DIR"
cp "$STAGE/s1500d-$VERSION-"*.pkg.tar.zst "$DIST_DIR/"
