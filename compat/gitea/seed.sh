#!/usr/bin/env bash
# Starts the LFS leg's Gitea (compose.yaml next to this script), creates or
# resets the user the leg pushes as, and prints the variables the leg reads:
#
#   eval "$(compat/gitea/seed.sh)"
#
# The password is OPENXET_COMPAT_GITEA_PASSWORD when set, else a new random
# one. It protects a throwaway container that listens on 127.0.0.1 only.
# `docker compose -f compat/gitea/compose.yaml down -v` removes it all.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
compose=(docker compose -f "$here/compose.yaml")
port=${OPENXET_COMPAT_GITEA_PORT:-3000}
user=compat
password=${OPENXET_COMPAT_GITEA_PASSWORD:-$(openssl rand -hex 16)}

"${compose[@]}" up -d --wait >&2
gitea_admin=("${compose[@]}" exec -T -u git gitea gitea admin user)
if "${gitea_admin[@]}" list | awk 'NR > 1 { print $2 }' | grep -qx "$user"; then
  "${gitea_admin[@]}" change-password --username "$user" --password "$password" \
    --must-change-password=false >&2
else
  "${gitea_admin[@]}" create --username "$user" --password "$password" \
    --email "$user@example.com" --must-change-password=false >&2
fi

printf 'export OPENXET_COMPAT_GITEA_URL=%s\n' "http://127.0.0.1:$port"
printf 'export OPENXET_COMPAT_GITEA_USER=%s\n' "$user"
printf 'export OPENXET_COMPAT_GITEA_PASSWORD=%s\n' "$password"
