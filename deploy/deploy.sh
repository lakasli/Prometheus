#!/usr/bin/env bash
# Run from an already-pushed checkout. No source or live configuration is edited.
set -euo pipefail
umask 077

sha=${1:?usage: deploy.sh FULL_SHA WEB_TAR_GZ WEB_SHA256}
archive=${2:?provide the locally built frontend archive}
archive_hash=${3:?provide the frontend SHA256}
[[ $sha =~ ^[0-9a-f]{40}$ ]] || { echo 'A complete commit SHA is required' >&2; exit 2; }
[[ $archive_hash =~ ^[0-9a-f]{64}$ ]] || { echo 'A complete archive SHA256 is required' >&2; exit 2; }
[[ $(id -un) == campanus ]] || { echo 'Deploy as the configured campanus user' >&2; exit 2; }
root=/home/campanus/apps/dsh
[[ $(realpath -m "$root") == /home/campanus/apps/dsh ]] || exit 2
[[ -f $archive ]] || { echo 'Frontend archive not found' >&2; exit 2; }
printf '%s  %s\n' "$archive_hash" "$archive" | sha256sum -c -
export PATH="/home/campanus/.cargo/bin:$PATH"
export GIT_TERMINAL_PROMPT=0
for tool in git cargo rustc cc python3 systemctl; do command -v "$tool" >/dev/null; done
[[ $(uname -m) == aarch64 ]] || { echo 'This deployment target must be aarch64' >&2; exit 2; }

mkdir -p "$root/shared/data" "$root/shared/logs" "$root/shared/config" "$root/releases" "$root/cache"
repo=$root/repo
remote=http://8.136.201.30:8418/Campanus/DSH.git
if [[ ! -d $repo ]]; then git clone "$remote" "$repo"; fi
[[ $(git -C "$repo" remote get-url origin) == "$remote" ]] || { echo 'Unexpected origin' >&2; exit 2; }
[[ -z $(git -C "$repo" status --porcelain) ]] || { echo 'Remote source checkout has changes' >&2; exit 2; }
git -C "$repo" fetch origin
git -C "$repo" cat-file -e "$sha^{commit}"
git -C "$repo" merge-base --is-ancestor "$sha" origin/main

release=$root/releases/$sha
mkdir -p "$release/bin" "$release/web"
if [[ ! -d $release/source ]]; then git -C "$repo" worktree add --detach "$release/source" "$sha"; fi
[[ $(git -C "$release/source" rev-parse HEAD) == "$sha" ]] || exit 2
[[ -z $(git -C "$release/source" status --porcelain) ]] || { echo 'Release source changed' >&2; exit 2; }

# Extract only regular files within the target; artifact identity must match source.
python3 - "$archive" "$release/web" "$sha" <<'PY'
import json,pathlib,sys,tarfile
archive,destination,expected=sys.argv[1:]
root=pathlib.Path(destination).resolve()
with tarfile.open(archive,'r:gz') as tf:
    members=tf.getmembers()
    if sum(m.size for m in members)>32*1024*1024: raise SystemExit('Frontend archive too large')
    for m in members:
        target=(root/m.name).resolve()
        if not target.is_relative_to(root) or not (m.isfile() or m.isdir()): raise SystemExit('Unsafe artifact member')
    tf.extractall(root, members=members, filter='data')
manifest=json.loads((root/'release.json').read_text())
if manifest['commit']!=expected: raise SystemExit('Frontend/source revision mismatch')
if not (root/'index.html').is_file(): raise SystemExit('Missing frontend entry point')
PY

export DSH_BUILD_SHA=$sha
export CARGO_TARGET_DIR=$root/cache/target
export CARGO_BUILD_JOBS=2
cd "$release/source"
cargo test --workspace --locked -j 2 >"$root/shared/logs/test-$sha.log" 2>&1 || { tail -n 60 "$root/shared/logs/test-$sha.log"; exit 1; }
cargo build --release --locked -p dshd -j 2 >"$root/shared/logs/build-$sha.log" 2>&1 || { tail -n 60 "$root/shared/logs/build-$sha.log"; exit 1; }
install -m 0755 "$CARGO_TARGET_DIR/release/dshd" "$release/bin/dshd"

unit_dir=/home/campanus/.config/systemd/user
mkdir -p "$unit_dir"
if [[ -f $unit_dir/dsh.service ]] && ! grep -q '^# Managed by DSH ' "$unit_dir/dsh.service"; then
  echo 'An unmanaged dsh.service already exists; refusing to overwrite it' >&2; exit 2
fi
sed "s|@ROOT@|$root|g" deploy/dsh.service.in >"$unit_dir/dsh.service"
previous=''
if [[ -e $root/current || -L $root/current ]]; then
  [[ -L $root/current ]] || { echo 'current is not a managed symlink' >&2; exit 2; }
  previous=$(readlink -f "$root/current")
  [[ $previous == "$root/releases/"* ]] || { echo 'current points outside managed releases' >&2; exit 2; }
fi
ln -sfn "$release" "$root/current.next"
mv -Tf "$root/current.next" "$root/current"
systemctl --user daemon-reload
systemctl --user enable dsh.service
systemctl --user restart dsh.service
healthy=false
for attempt in $(seq 1 30); do
  if curl --max-time 2 -fsS http://127.0.0.1:8419/health/ready | python3 -c 'import json,sys; assert json.load(sys.stdin)["commit"]==sys.argv[1]' "$sha" 2>/dev/null; then healthy=true; break; fi
  sleep 1
done
if [[ $healthy != true ]]; then
  echo 'New release did not become healthy' >&2
  journalctl --user -u dsh.service -n 30 --no-pager
  if [[ -n $previous ]]; then
    ln -sfn "$previous" "$root/current.next"
    mv -Tf "$root/current.next" "$root/current"
    systemctl --user restart dsh.service
    curl --retry 5 --retry-delay 1 --max-time 2 -fsS http://127.0.0.1:8419/health/ready >/dev/null
    echo 'Previous release restored and health checked' >&2
  else
    systemctl --user stop dsh.service
    echo 'No previous application release exists; failed service stopped' >&2
  fi
  exit 1
fi
python3 - "$root" "$sha" "$previous" <<'PY'
import datetime,json,pathlib,sys
root,sha,previous=sys.argv[1:]
value={'commit':sha,'previous_release':previous,'deployed_at':datetime.datetime.now(datetime.timezone.utc).isoformat(),'os':pathlib.Path('/etc/os-release').read_text(),'web_manifest':json.loads((pathlib.Path(root)/'current/web/release.json').read_text())}
(pathlib.Path(root)/'shared/deployment.json').write_text(json.dumps(value,ensure_ascii=False,indent=2))
print(json.dumps({'deployed_commit':sha,'health':'ready','service':'systemctl --user status dsh.service'}))
PY
if [[ $(loginctl show-user campanus -p Linger --value) != yes ]]; then
  echo 'Enable user lingering for unattended operation: loginctl enable-linger campanus' >&2
fi
