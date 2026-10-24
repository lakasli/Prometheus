#!/usr/bin/env bash
set -euo pipefail
root=/home/campanus/apps/dsh
sha=${1:?usage: rollback.sh PREVIOUS_FULL_SHA}
[[ $sha =~ ^[0-9a-f]{40}$ ]] || exit 2
release=$root/releases/$sha
[[ -x $release/bin/dshd && -f $release/web/release.json ]] || { echo 'Complete release not found' >&2; exit 2; }
[[ $(git -C "$release/source" rev-parse HEAD) == "$sha" ]] || exit 2
ln -sfn "$release" "$root/current.next"
mv -Tf "$root/current.next" "$root/current"
systemctl --user restart dsh.service
curl --retry 10 --retry-delay 1 --max-time 2 -fsS http://127.0.0.1:8419/health/ready
echo 'Application release restored. Database and external effects were not reverted.'
