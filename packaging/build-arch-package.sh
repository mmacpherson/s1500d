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

docker run --rm \
    --env BUILD_UID="$(id -u)" \
    --env BUILD_GID="$(id -g)" \
    --volume "$STAGE:/build" \
    archlinux:base-devel sh -euxc '
    pacman -Syu --noconfirm --needed rust libusb
    if ! getent group "$BUILD_GID" >/dev/null; then
        groupadd --gid "$BUILD_GID" builder
    fi
    useradd --create-home --uid "$BUILD_UID" --gid "$BUILD_GID" builder
    chown -R "$BUILD_UID:$BUILD_GID" /build
    runuser -u builder -- sh -c "cd /build && makepkg --cleanbuild --noconfirm"
'

mkdir -p "$DIST_DIR"
cp "$STAGE/s1500d-$VERSION-"*.pkg.tar.zst "$DIST_DIR/"
