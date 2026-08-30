#!/bin/sh
set -u

if command -v systemd-sysusers >/dev/null 2>&1; then
    systemd-sysusers s1500d.conf >/dev/null 2>&1 || true
fi

if command -v udevadm >/dev/null 2>&1; then
    udevadm control --reload-rules >/dev/null 2>&1 || true
fi

if command -v systemctl >/dev/null 2>&1 &&
    systemctl --quiet is-system-running >/dev/null 2>&1; then
    systemctl daemon-reload
fi

printf '%s\n' \
    's1500d was installed but not enabled or started.' \
    'Verify the scanner first with: s1500d --doctor' \
    'Then edit /etc/s1500d/config.toml and enable s1500d.service.'
