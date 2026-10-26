"""Execute deployment stop/gate branches with synthetic paths and shell functions.

These checks do not run either full deployment script. Every external command
in the extracted branches is replaced, and an empty PATH rejects new unstubbed
commands. No service, release pointer, database or network is accessed.
"""
import os
import pathlib
import shlex
import shutil
import subprocess
import tempfile
import unittest


REPOSITORY = pathlib.Path(__file__).resolve().parents[2]


def find_bash():
    if os.name != "nt":
        return shutil.which("bash")
    # Do not select Windows' bash.exe launcher: that would start WSL instead of
    # the local Git Bash used for these isolated shell checks.
    roots = []
    for variable in ("ProgramFiles", "ProgramFiles(x86)"):
        if os.environ.get(variable):
            roots.append(pathlib.Path(os.environ[variable]) / "Git")
    if os.environ.get("LOCALAPPDATA"):
        roots.append(pathlib.Path(os.environ["LOCALAPPDATA"]) / "Programs/Git")
    git = shutil.which("git")
    if git:
        roots.append(pathlib.Path(git).resolve().parent.parent)
    for root in roots:
        for relative in ("bin/bash.exe", "usr/bin/bash.exe"):
            candidate = root / relative
            if candidate.is_file():
                return str(candidate)
    return None


BASH = find_bash()
STUBS = r"""
set -euo pipefail
PATH=''
trace() { printf 'TRACE:%s\n' "$*" >&2; }
stopped=false
systemctl() {
  [[ "$1" == --user ]] || return 97
  case "$2" in
    is-active) [[ $SIM_ACTIVE == 1 ]];;
    stop) stopped=true; trace STOP;;
    start) trace START; [[ $SIM_START_FAIL == 0 ]];;
    restart) trace RESTART;;
    *) return 97;;
  esac
}
python3() {
  if [[ "$1" == -c ]]; then
    while IFS= read -r line; do :; done
    return 0
  fi
  [[ $stopped == true ]] || { trace GATE_BEFORE_STOP; return 96; }
  if [[ "$1" == "$previous/source/deploy/check-compatibility.py" ]]; then
    trace GATE_CURRENT
    return "$SIM_CURRENT_RC"
  fi
  trace GATE_TARGET
  return "$SIM_TARGET_RC"
}
ln() { trace LINK; }
mv() { trace MOVE; }
curl() { trace HEALTH; printf '{"commit":"%s"}\n' "$sha"; }
journalctl() { trace JOURNAL; }
"""


def section(source, start, end=None):
    if source.count(start) != 1 or (end is not None and source.count(end) != 1):
        raise AssertionError("Deployment branch markers changed; review test extraction")
    first = source.index(start)
    last = len(source) if end is None else source.index(end, first)
    return source[first:last]


@unittest.skipUnless(BASH, "requires Bash; Windows requires a local Git Bash installation")
class DeployStopGateTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        rollback = (REPOSITORY / "deploy/rollback.sh").read_text(encoding="utf-8")
        deploy = (REPOSITORY / "deploy/deploy.sh").read_text(encoding="utf-8")
        cls.manual = section(rollback, "was_active=false")
        cls.pre_switch = section(deploy, "was_active=false", 'mkdir -p "$root/shared/backups"')
        cls.automatic = section(
            deploy,
            "if [[ $healthy != true ]]; then",
            'python3 - "$root" "$sha" "$previous"',
        )

    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="dsh-stop-gate-")
        self.addCleanup(self.temporary.cleanup)
        self.root = pathlib.Path(self.temporary.name)
        self.previous = self.root / "releases/previous"
        self.release = self.root / "releases/target"
        for release in (self.previous, self.release):
            gate = release / "source/deploy/check-compatibility.py"
            gate.parent.mkdir(parents=True)
            gate.write_text("# Synthetic path marker; never executed.\n", encoding="utf-8")

    def run_branch(self, branch, code, events, *, active=1, target_rc=0,
                   current_rc=0, start_fail=0, has_previous=True):
        assignments = {
            "root": self.root.as_posix(),
            "previous": self.previous.as_posix() if has_previous else "",
            "release": self.release.as_posix(),
            "gate": (self.release / "source/deploy/check-compatibility.py").as_posix(),
            "sha": "a" * 40,
            "healthy": "false",
            "SIM_ACTIVE": str(active),
            "SIM_TARGET_RC": str(target_rc),
            "SIM_CURRENT_RC": str(current_rc),
            "SIM_START_FAIL": str(start_fail),
        }
        script = STUBS + "\n".join(
            name + "=" + shlex.quote(value) for name, value in assignments.items()
        ) + "\n" + branch
        environment = dict(os.environ, BASH_ENV="", ENV="")
        result = subprocess.run(
            [BASH, "-c", script], cwd=self.root, env=environment,
            stdin=subprocess.DEVNULL, capture_output=True, text=True, timeout=10,
        )
        actual = [
            line[len("TRACE:"):] for line in result.stderr.splitlines()
            if line.startswith("TRACE:")
        ]
        self.assertEqual(result.returncode, code, result.stderr)
        self.assertEqual(actual, events, result.stderr)

    def test_manual_rejection_stops_before_gate_and_restores_original_service(self):
        self.run_branch(self.manual, 2, ["STOP", "GATE_TARGET", "START"], target_rc=2)

    def test_manual_rejection_preserves_originally_stopped_service(self):
        self.run_branch(self.manual, 2, ["STOP", "GATE_TARGET"], active=0, target_rc=2)

    def test_manual_rejection_reports_restart_failure_without_switching(self):
        self.run_branch(
            self.manual, 1, ["STOP", "GATE_TARGET", "START"], target_rc=2, start_fail=1,
        )

    def test_manual_compatible_target_switches_only_after_stopped_gate(self):
        self.run_branch(
            self.manual, 0, ["STOP", "GATE_TARGET", "LINK", "MOVE", "RESTART", "HEALTH"],
        )

    def test_deployment_target_gate_rejection_restores_current_service(self):
        self.run_branch(
            self.pre_switch, 2, ["STOP", "GATE_TARGET", "GATE_CURRENT", "START"],
            target_rc=2,
        )

    def test_deployment_current_gate_catches_features_unknown_to_target(self):
        self.run_branch(
            self.pre_switch, 2, ["STOP", "GATE_TARGET", "GATE_CURRENT", "START"],
            current_rc=2,
        )

    def test_deployment_both_gates_accept_stopped_database(self):
        self.run_branch(self.pre_switch, 0, ["STOP", "GATE_TARGET", "GATE_CURRENT"])

    def test_automatic_rejected_rollback_stays_stopped_without_switching(self):
        self.run_branch(
            self.automatic, 1, ["STOP", "JOURNAL", "GATE_TARGET"], target_rc=2,
        )

    def test_automatic_compatible_rollback_switches_after_stopped_gate(self):
        self.run_branch(
            self.automatic, 1,
            ["STOP", "JOURNAL", "GATE_TARGET", "LINK", "MOVE", "RESTART", "HEALTH"],
        )

    def test_automatic_failure_without_previous_release_stays_stopped(self):
        self.run_branch(self.automatic, 1, ["STOP", "JOURNAL"], has_previous=False)


if __name__ == "__main__":
    unittest.main()
