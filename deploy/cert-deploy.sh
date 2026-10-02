#!/bin/sh
set -eu
: "${RENEWED_LINEAGE:?Certbot must set RENEWED_LINEAGE}"
install -o root -g vorp -m 0640 "$RENEWED_LINEAGE/fullchain.pem" /etc/vorp/fullchain.pem
install -o root -g vorp -m 0640 "$RENEWED_LINEAGE/privkey.pem" /etc/vorp/privkey.pem
