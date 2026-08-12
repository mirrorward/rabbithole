#!/bin/sh
# Make systemd aware of the new units. Deliberately does NOT enable or start
# the burrow: installing a package is not the same as agreeing to open a
# listener, and a server that starts itself on install with default config is
# a surprise, not a convenience.
set -e
chown -R burrow:burrow /var/lib/burrow 2>/dev/null || true
if command -v systemctl >/dev/null 2>&1; then
    systemctl daemon-reload >/dev/null 2>&1 || true
fi
cat <<'EOF'

RabbitHole installed.

  burrow         the server        systemctl enable --now burrow
  looking-glass  the tracker       systemctl enable --now looking-glass
  rabbit         command-line client
  rabbit-tui     terminal client

Nothing is running yet. Configure /var/lib/burrow/burrow.toml first — see
/usr/share/doc/rabbithole/deployment.md.
EOF
