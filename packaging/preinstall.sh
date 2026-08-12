#!/bin/sh
# Create the unprivileged account the burrow runs as, before any file lands
# owned by it. Idempotent: an upgrade must not fail because the user exists.
set -e
if ! getent passwd burrow >/dev/null 2>&1; then
    if command -v useradd >/dev/null 2>&1; then
        # Debian/RPM
        useradd --system --user-group --home-dir /var/lib/burrow \
                --shell /usr/sbin/nologin --comment "RabbitHole burrow" burrow
    elif command -v adduser >/dev/null 2>&1; then
        # Alpine/busybox
        addgroup -S burrow 2>/dev/null || true
        adduser -S -G burrow -h /var/lib/burrow -s /sbin/nologin -D burrow
    fi
fi
