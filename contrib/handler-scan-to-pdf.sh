#!/bin/bash
# handler-scan-to-pdf.sh — scan to PDF using scanimage + img2pdf
#
# Required packages:
#   Arch:   pacman -S sane img2pdf
#   Debian: apt install sane-utils img2pdf
#   Fedora: dnf install sane-backends img2pdf
#
# Scans all pages in the ADF to a timestamped PDF.
# Profile name (from config.toml) is used as a filename prefix.

SCAN_DIR="${SCAN_DIR:-$HOME/Scans}"
EVENT="$1"
PROFILE="${2:-scan}"

case "$EVENT" in
    scan)
        mkdir -p "$SCAN_DIR"
        TIMESTAMP=$(date +%Y%m%d-%H%M%S)
        OUTFILE="$SCAN_DIR/${PROFILE}_${TIMESTAMP}.pdf"
        TMPDIR=$(mktemp -d)
        trap 'rm -rf "$TMPDIR"' EXIT

        logger -t s1500d "Scanning: profile=$PROFILE → $OUTFILE"

        scanimage \
            --device-name="fujitsu:ScanSnap S1500:*" \
            --source="ADF Duplex" \
            --mode=Color \
            --resolution=300 \
            --format=tiff \
            --batch="$TMPDIR/page_%04d.tiff" \
            --batch-count=0 \
            2>/dev/null

        PAGES=("$TMPDIR"/page_*.tiff)
        if [ ${#PAGES[@]} -eq 0 ] || [ ! -f "${PAGES[0]}" ]; then
            logger -t s1500d "No pages scanned"
            exit 1
        fi

        img2pdf "${PAGES[@]}" -o "$OUTFILE"
        logger -t s1500d "Saved $OUTFILE (${#PAGES[@]} pages)"
        ;;
    device-arrived)
        logger -t s1500d "Scanner ready"
        ;;
    device-left)
        logger -t s1500d "Scanner closed"
        ;;
    *)
        logger -t s1500d "Event: $EVENT"
        ;;
esac
