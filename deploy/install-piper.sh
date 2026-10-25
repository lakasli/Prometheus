#!/usr/bin/env bash
# Run only from a committed/pushed DSH release. Installs private, pinned runtime
# dependencies; never edits source, live application configuration or user data.
set -euo pipefail
umask 077
[[ $(uname -s) == Linux && $(uname -m) == aarch64 ]] || { echo 'Requires Linux aarch64' >&2; exit 2; }
[[ $(python3 -c 'import sys; print(f"{sys.version_info.major}.{sys.version_info.minor}")') == 3.12 ]] || { echo 'Requires Python 3.12' >&2; exit 2; }
script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
root=${1:-/home/campanus/apps/dsh/shared/speech}
[[ $root == /home/campanus/apps/dsh/shared/speech ]] || { echo 'Unexpected installation root' >&2; exit 2; }
[[ -f $script_dir/piper-aarch64-py312.lock ]] || { echo 'Pinned dependency lock missing' >&2; exit 2; }
mkdir -p "$root"
[[ $(stat -c %U "$root") == "$(id -un)" && ! -L $root ]] || { echo 'Unexpected speech directory owner/type' >&2; exit 2; }
runtime=$root/piper-1.8.0
mkdir -p "$runtime/models"
if [[ ! -x $runtime/venv/bin/python ]]; then
  python3 -m venv --copies "$runtime/venv"
fi
"$runtime/venv/bin/python" -m pip install --disable-pip-version-check --require-hashes --only-binary=:all: -r "$script_dir/piper-aarch64-py312.lock"
"$runtime/venv/bin/python" -m pip check
python3 - "$runtime/models" <<'PY'
import hashlib, os, pathlib, sys, urllib.request
root = pathlib.Path(sys.argv[1])
base = 'https://huggingface.co/rhasspy/piper-voices/resolve/5041a7a49ae5b3a0f30e6837769eb03fdbb231bd/zh/zh_CN/huayan/medium/'
files = {
    'zh_CN-huayan-medium.onnx': ('9929917bf8cabb26fd528ea44d3a6699c11e87317a14765312420be230be0f3d', 63201294),
    'zh_CN-huayan-medium.onnx.json': ('d521dc45504a8ccc99e325822b35946dd701840bfb07e3dbb31a40929ed6a82b', 4822),
    'MODEL_CARD': ('25d7d8f7a03e9382e629c5c4f074176e7ca84e08be12a873022e91fcac98c2c7', 276),
}
for name, (expected, expected_size) in files.items():
    target = root / name
    if target.is_symlink():
        raise SystemExit('Refusing symlink model target')
    if target.is_file() and target.stat().st_size == expected_size and hashlib.sha256(target.read_bytes()).hexdigest() == expected:
        print(f'{name}: verified')
        continue
    temporary = target.with_suffix(target.suffix + '.download')
    try:
        with urllib.request.urlopen(base + name, timeout=120) as response, temporary.open('xb') as output:
            count = 0
            while block := response.read(1024 * 1024):
                count += len(block)
                if count > expected_size:
                    raise ValueError('Model download exceeds pinned size')
                output.write(block)
        if temporary.stat().st_size != expected_size or hashlib.sha256(temporary.read_bytes()).hexdigest() != expected:
            raise ValueError('Model download digest mismatch')
        os.replace(temporary, target)
        print(f'{name}: installed and verified')
    finally:
        if temporary.is_file() and not temporary.is_symlink():
            temporary.unlink()
PY
"$runtime/venv/bin/python" -I -c 'import importlib.metadata as m; import piper, onnxruntime; print("Piper",m.version("piper-tts"),"ONNX Runtime",onnxruntime.__version__)'
echo 'Piper installed. Local synthesis uses the shared speech directory; enable speech through the versioned DSH configuration.'
echo 'Engine: GPL-3.0-or-later. Chinese huayan model card: dataset license Unknown; personal evaluation only, no commercial redistribution claim.'
