#!/bin/bash
# handler-scan-to-pdf.sh — scan to PDF using scanimage + img2pdf
#
# Required packages:
#   Arch:   pacman -S sane img2pdf
#   Debian: apt install sane-utils img2pdf
#   Fedora: dnf install sane-backends img2pdf
#
# Scans all pages in the ADF to a timestamped PDF.
# Auto-detects one S1500; set SCAN_DEVICE to an exact scanimage -L name to override.
# Profile name (from config.toml) is used as a filename prefix.
# Failed attempts with files remain in SCAN_DIR/.s1500d-* for manual recovery.

SCAN_DIR="${SCAN_DIR:-$HOME/Scans}"
EVENT="${1:-}"
PROFILE="${2:-scan}"

log() {
    printf '%s\n' "$*" >&2
    logger -t s1500d -- "$*" || :
}

fail() {
    log "ERROR: $*"
    exit 1
}

finish() {
    local status=$?
    if [ -n "${WORK_DIR:-}" ]; then
        log "Recovery files retained in $WORK_DIR (attempt may be incomplete)"
    fi
    return "$status"
}

case "$EVENT" in
    scan)
        # Newly created directories are group-readable; raw pages stay private.
        umask 027
        for tool in scanimage img2pdf mkdir mktemp date chmod ln rm rmdir; do
            command -v "$tool" >/dev/null || fail "Required command missing: $tool"
        done
        if [ "${SCAN_DEVICE+x}" != x ]; then
            if DEVICE_LIST=$(LC_ALL=C scanimage -L); then
                # SANE lists exact names between a backtick and an apostrophe.
                # Match the backend model exactly, excluding S1500M and others.
                device_pattern="^device \`(fujitsu:ScanSnap S1500:[^']+)' is "
                DEVICES=()
                while IFS= read -r line; do
                    if [[ "$line" =~ $device_pattern ]]; then
                        DEVICES+=("${BASH_REMATCH[1]}")
                    fi
                done <<< "$DEVICE_LIST"
                if [ "${#DEVICES[@]}" -ne 1 ]; then
                    log "scanimage -L returned: ${DEVICE_LIST:-(no devices listed)}"
                    fail "Found ${#DEVICES[@]} ScanSnap S1500 devices; set SCAN_DEVICE to an exact name from scanimage -L"
                fi
                SCAN_DEVICE="${DEVICES[0]}"
                log "Auto-detected scanner: $SCAN_DEVICE (set SCAN_DEVICE to this exact name to skip detection)"
            else
                discovery_status=$?
                log "scanimage -L returned: ${DEVICE_LIST:-(no devices listed)}"
                fail "Scanner discovery failed (exit $discovery_status); set SCAN_DEVICE to an exact name from scanimage -L"
            fi
        fi
        [ -n "$SCAN_DEVICE" ] || fail "SCAN_DEVICE is empty; unset it for auto-detection or set an exact scanimage -L name"
        case "$PROFILE" in
            *[!a-zA-Z0-9_-]*) fail "Profile must contain only letters, digits, underscores or hyphens" ;;
        esac
        mkdir -p -- "$SCAN_DIR" || fail "Cannot create scan directory: $SCAN_DIR"
        # Absolute paths make recovery messages usable from any working directory.
        SCAN_DIR=$(CDPATH='' cd -- "$SCAN_DIR" && pwd -P) || fail "Cannot access scan directory"
        TIMESTAMP=$(date +%Y%m%d-%H%M%S) || fail "Cannot determine timestamp"
        OUTFILE="$SCAN_DIR/${PROFILE}_${TIMESTAMP}.pdf"
        WORK_DIR=$(mktemp -d "$SCAN_DIR/.s1500d-XXXXXXXXXX") || fail "Cannot create recovery directory"
        trap finish EXIT
        trap 'exit 129' HUP
        trap 'exit 130' INT
        trap 'exit 143' TERM

        log "Scanning: profile=$PROFILE → $OUTFILE; recovery=$WORK_DIR"

        if scanimage \
            --device-name="$SCAN_DEVICE" \
            --source="ADF Duplex" \
            --mode=Color \
            --resolution=300 \
            --format=tiff \
            --batch="$WORK_DIR/page_%04d.tiff"; then
            scan_status=0
        else
            scan_status=$?
        fi

        shopt -s nullglob
        PAGES=("$WORK_DIR"/page_*.tiff)
        if [ ${#PAGES[@]} -eq 0 ]; then
            # Empty feeders may return nonzero. Remove only an empty directory;
            # preserve any unexpected files rather than deleting recoverable data.
            if rmdir -- "$WORK_DIR"; then
                WORK_DIR=""
            fi
            fail "No pages scanned (scanimage exit $scan_status; check feeder and diagnostics)"
        fi
        if [ "$scan_status" -ne 0 ]; then
            fail "Acquisition failed (scanimage exit $scan_status); keeping all acquired pages"
        fi

        STAGED="$WORK_DIR/scan.pdf"
        img2pdf "${PAGES[@]}" -o "$STAGED" || fail "PDF conversion failed"
        [ -s "$STAGED" ] || fail "Converter produced no PDF data"
        chmod 0640 "$STAGED" || fail "Cannot set PDF permissions"
        # Same filesystem: atomic publication, refusing any existing destination.
        if ! ln -T -- "$STAGED" "$OUTFILE"; then
            # Check after the atomic operation to classify concurrent collisions too.
            if [ -e "$OUTFILE" ] || [ -L "$OUTFILE" ]; then
                fail "Destination already exists: $OUTFILE"
            fi
            fail "Cannot publish PDF: $OUTFILE; check filesystem hard-link support, permissions and free space"
        fi
        if rm -r -- "$WORK_DIR"; then
            WORK_DIR=""
        else
            fail "PDF published at $OUTFILE but recovery-directory cleanup failed"
        fi
        log "Saved $OUTFILE (${#PAGES[@]} pages)"
        ;;
    device-arrived)
        log "Scanner ready"
        ;;
    device-left)
        log "Scanner closed"
        ;;
    *)
        log "Event: $EVENT"
        ;;
esac
