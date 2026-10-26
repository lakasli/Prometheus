#!/usr/bin/env bash
set -euo pipefail
root=/home/campanus/apps/dsh
exec 9>"$root/.deployment.lock"
flock -n 9 || { echo 'Another DSH deployment or rollback is in progress' >&2; exit 2; }
sha=${1:?usage: rollback.sh PREVIOUS_FULL_SHA}
[[ $sha =~ ^[0-9a-f]{40}$ ]] || exit 2
release=$root/releases/$sha
[[ -x $release/bin/dshd && -f $release/web/release.json ]] || { echo 'Complete release not found' >&2; exit 2; }
[[ $(git -C "$release/source" rev-parse HEAD) == "$sha" ]] || exit 2
gate=$root/current/source/deploy/check-compatibility.py
[[ -f $gate ]] || gate=$release/source/deploy/check-compatibility.py
[[ -f $gate ]] || { echo 'No compatibility gate is available; preserve data and use a compatible deployment tool' >&2; exit 2; }
# A live receiver could commit its first durable feature after an online check.
# Keep the current pointer until the stopped database passes the current gate.
was_active=false
if systemctl --user is-active --quiet dsh.service; then was_active=true; fi
systemctl --user stop dsh.service
if ! python3 "$gate" "$root/shared/data" "$release/source"; then
  if [[ $was_active == true ]]; then
    if ! systemctl --user start dsh.service; then
      echo 'Rollback rejected; current release pointer is unchanged, but its service could not be restarted. Preserve data and inspect the service.' >&2
      exit 1
    fi
  fi
  echo 'Rollback rejected; current release pointer and database are unchanged.' >&2
  exit 2
fi
ln -sfn "$release" "$root/current.next"
mv -Tf "$root/current.next" "$root/current"
systemctl --user restart dsh.service
curl --retry 10 --retry-delay 1 --max-time 2 -fsS http://127.0.0.1:8419/health/ready | python3 -c 'import json,sys; assert json.load(sys.stdin)["commit"]==sys.argv[1]' "$sha"
echo 'Application release restored. Database and external effects were not reverted.'
