#!/bin/sh
# Stop the services before the binaries go. The data directory is left alone:
# removing a package should not destroy the burrow's database, accounts and
# identity key.
set -e
if command -v systemctl >/dev/null 2>&1; then
    systemctl stop burrow.service >/dev/null 2>&1 || true
    systemctl stop looking-glass.service >/dev/null 2>&1 || true
fi
