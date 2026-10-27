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
for tool in git cargo rustc cc python3 systemctl flock; do command -v "$tool" >/dev/null; done
[[ $(uname -m) == aarch64 ]] || { echo 'This deployment target must be aarch64' >&2; exit 2; }

mkdir -p "$root/shared/data" "$root/shared/logs" "$root/shared/config" "$root/releases" "$root/cache"
exec 9>"$root/.deployment.lock"
flock -n 9 || { echo 'Another DSH deployment or rollback is in progress' >&2; exit 2; }
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
build_jobs=${DSH_BUILD_JOBS:-2}
[[ $build_jobs == 1 || $build_jobs == 2 ]] || { echo 'Build jobs must be 1 or 2 for the 8GB target' >&2; exit 2; }
export CARGO_BUILD_JOBS=$build_jobs
cargo_cmd=(cargo)
if [[ ${DSH_CARGO_MIRROR:-} == rsproxy ]]; then
  cargo_cmd+=(--config 'source.crates-io.replace-with="rsproxy-sparse"' --config 'source.rsproxy-sparse.registry="sparse+https://rsproxy.cn/index/"')
elif [[ -n ${DSH_CARGO_MIRROR:-} ]]; then
  echo 'Unsupported cargo mirror; use empty or rsproxy' >&2; exit 2
fi
cd "$release/source"
"${cargo_cmd[@]}" test --workspace --locked -j "$build_jobs" >"$root/shared/logs/test-$sha.log" 2>&1 || { tail -n 60 "$root/shared/logs/test-$sha.log"; exit 1; }
"${cargo_cmd[@]}" build --release --locked -p dshd -j "$build_jobs" >"$root/shared/logs/build-$sha.log" 2>&1 || { tail -n 60 "$root/shared/logs/build-$sha.log"; exit 1; }
install -m 0755 "$CARGO_TARGET_DIR/release/dshd" "$release/bin/dshd"

# Deployment-only repairs can use a separately pinned, pushed gate revision
# while the binary, frontend and release source retain their verified SHA.
compat_source=$release/source
compat_sha=$sha
if [[ -n ${DSH_COMPATIBILITY_SOURCE_SHA:-} ]]; then
  compat_sha=$DSH_COMPATIBILITY_SOURCE_SHA
  [[ $compat_sha =~ ^[0-9a-f]{40}$ ]] || { echo 'A complete compatibility tooling SHA is required' >&2; exit 2; }
  git -C "$repo" cat-file -e "$compat_sha^{commit}"
  git -C "$repo" merge-base --is-ancestor "$compat_sha" origin/main
  compat_source=$root/cache/deploy-tools/$compat_sha
  mkdir -p "$root/cache/deploy-tools"
  if [[ ! -d $compat_source ]]; then git -C "$repo" worktree add --detach "$compat_source" "$compat_sha"; fi
  [[ $(git -C "$compat_source" rev-parse HEAD) == "$compat_sha" && -z $(git -C "$compat_source" status --porcelain) ]] || exit 2
fi
# Build before stopping the live service. Check durable features before switching.
python3 "$compat_source/deploy/check-compatibility.py" "$root/shared/data" "$release/source"

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
systemctl --user daemon-reload
systemctl --user enable dsh.service
was_active=false
if systemctl --user is-active --quiet dsh.service; then was_active=true; fi
systemctl --user stop dsh.service
# Recheck after stopping: acknowledged events can introduce a durable feature
# between the online preflight and stop. The current gate also knows features
# that an older target release's gate cannot recognize.
compatible=true
if ! python3 "$compat_source/deploy/check-compatibility.py" "$root/shared/data" "$release/source"; then compatible=false; fi
if [[ -n $previous && -f $previous/source/deploy/check-compatibility.py ]]; then
  if [[ $(<"$compat_source/deploy/check-compatibility.py") == *'def check_previous('* ]]; then
    if ! python3 "$compat_source/deploy/check-compatibility.py" "$root/shared/data" "$release/source" --previous-gate "$previous/source/deploy/check-compatibility.py"; then compatible=false; fi
  elif ! python3 "$previous/source/deploy/check-compatibility.py" "$root/shared/data" "$release/source"; then
    compatible=false
  fi
fi
if [[ $compatible != true ]]; then
  if [[ $was_active == true ]]; then
    if ! systemctl --user start dsh.service; then
      echo 'Deployment rejected; current release pointer is unchanged, but its service could not be restarted. Preserve data and inspect the service.' >&2
      exit 1
    fi
  fi
  echo 'Deployment rejected by stopped-database compatibility check; current release pointer and database are unchanged.' >&2
  exit 2
fi
mkdir -p "$root/shared/backups"
if ! python3 - "$root/shared/data/dsh.db" "$root/shared/backups/pre-$sha-$(date -u +%Y%m%dT%H%M%S%N).sqlite" <<'PY'
import os,pathlib,sqlite3,sys
source,destination=sys.argv[1:]
if pathlib.Path(source).is_file():
    with sqlite3.connect(pathlib.Path(source).resolve().as_uri()+'?mode=ro',uri=True) as src, sqlite3.connect(destination) as dst:
        src.backup(dst)
    os.chmod(destination,0o600)
PY
then
  if [[ -n $previous ]]; then systemctl --user start dsh.service; fi
  echo 'Pre-upgrade backup failed; release was not switched' >&2
  exit 1
fi
ln -sfn "$release" "$root/current.next"
mv -Tf "$root/current.next" "$root/current"
if ! systemctl --user restart dsh.service; then
  echo 'Restart failed; checking health before restoring the prior release' >&2
fi
healthy=false
for attempt in $(seq 1 30); do
  if curl --max-time 2 -fsS http://127.0.0.1:8419/health/ready | python3 -c 'import json,sys; assert json.load(sys.stdin)["commit"]==sys.argv[1]' "$sha" 2>/dev/null; then healthy=true; break; fi
  sleep 1
done
if [[ $healthy != true ]]; then
  echo 'New release did not become healthy' >&2
  # Stop new receipts before testing whether the previous binary is still safe.
  systemctl --user stop dsh.service
  journalctl --user -u dsh.service -n 30 --no-pager || true
  if [[ -n $previous ]]; then
    if ! python3 "$compat_source/deploy/check-compatibility.py" "$root/shared/data" "$previous/source"; then
      echo 'Previous release is incompatible with durable features; service stopped. Preserve data and inspect the private pre-upgrade backup.' >&2
      exit 1
    fi
    ln -sfn "$previous" "$root/current.next"
    mv -Tf "$root/current.next" "$root/current"
    systemctl --user restart dsh.service
    curl --retry 5 --retry-delay 1 --max-time 2 -fsS http://127.0.0.1:8419/health/ready >/dev/null
    echo 'Previous release restored and health checked' >&2
  else
    echo 'No previous application release exists; failed service stopped' >&2
  fi
  exit 1
fi
python3 - "$root" "$sha" "$previous" "$compat_sha" <<'PY'
import datetime,json,pathlib,sys
root,sha,previous,compat_sha=sys.argv[1:]
value={'commit':sha,'previous_release':previous,'compatibility_tools_commit':compat_sha,'deployed_at':datetime.datetime.now(datetime.timezone.utc).isoformat(),'os':pathlib.Path('/etc/os-release').read_text(),'web_manifest':json.loads((pathlib.Path(root)/'current/web/release.json').read_text())}
(pathlib.Path(root)/'shared/deployment.json').write_text(json.dumps(value,ensure_ascii=False,indent=2))
print(json.dumps({'deployed_commit':sha,'health':'ready','service':'systemctl --user status dsh.service'}))
PY
if [[ $(loginctl show-user campanus -p Linger --value) != yes ]]; then
  echo 'Enable user lingering for unattended operation: loginctl enable-linger campanus' >&2
fi
