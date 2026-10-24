#!/usr/bin/env python3
"""Bounded, synthetic Linux/systemd sandbox probe; never enables Harness.

Only this process's mkdtemp directories and uniquely named transient units are
modified. Reports contain comparisons, not canary bytes, namespace IDs, PIDs,
private environment values, temporary paths or raw journal/command output.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import shutil
import socket
import stat
import subprocess
import sys
import tempfile
import threading
import uuid
from datetime import datetime, timezone


NAMESPACES = ("user", "mnt", "net", "pid")
LIMIT_FILES = ("memory.max", "memory.swap.max", "memory.peak", "memory.events",
               "pids.max", "pids.current", "pids.events")
BASE_PROPERTIES = {
    "PrivateUsers": "yes", "ProtectHome": "yes", "ProtectSystem": "strict",
    "PrivateNetwork": "yes", "NoNewPrivileges": "yes", "MemoryAccounting": "yes",
    "MemoryMax": "96M", "MemorySwapMax": "0", "TasksAccounting": "yes",
    "TasksMax": "32", "RuntimeMaxSec": "25s", "TimeoutStopSec": "5s",
    "KillMode": "control-group",
}


def run(command, timeout=8):
    return subprocess.run(command, text=True, capture_output=True, timeout=timeout)


def namespaces():
    return {name: os.readlink("/proc/self/ns/" + name) for name in NAMESPACES}


def read_check(path, expected_digest=None):
    try:
        content = Path(path).read_bytes()
        result = {"readable": True}
        if expected_digest is not None:
            result["matches_canary"] = hashlib.sha256(content).hexdigest() == expected_digest
        return result
    except OSError as error:
        return {"readable": False, "errno": error.errno}


def write_check(path):
    try:
        with open(path, "xb") as output:
            output.write(b"synthetic probe\n")
        Path(path).unlink()
        return {"writable": True}
    except OSError as error:
        return {"writable": False, "errno": error.errno}


def connect_check(family, address):
    with socket.socket(family, socket.SOCK_STREAM) as connection:
        connection.settimeout(1)
        try:
            code = connection.connect_ex(address)
            return {"reachable": code == 0, "errno": code}
        except OSError as error:
            return {"reachable": False, "errno": error.errno}


def cgroup():
    try:
        relative = next(line.split(":", 2)[2] for line in Path("/proc/self/cgroup").read_text().splitlines() if line.startswith("0::"))
        location = Path("/sys/fs/cgroup") / relative.lstrip("/")
        result = {}
        for name in LIMIT_FILES:
            try:
                result[name] = (location / name).read_text().strip()
            except OSError as error:
                result[name] = {"errno": error.errno}
        return result
    except (OSError, StopIteration):
        return {"available": False}


def pid_attempts():
    result = []
    for flags in (("--pid", "--fork", "--mount-proc"),
                  ("--mount", "--pid", "--fork", "--mount-proc"),
                  ("--user", "--map-root-user", "--pid", "--fork", "--mount-proc")):
        process = run(["/usr/bin/unshare", *flags, "/usr/bin/python3", "-c",
                       "import json,os; print(json.dumps({'pid_is_one':os.getpid()==1}))"], timeout=5)
        result.append({"flags": list(flags), "exit_code": process.returncode,
                       "operation_not_permitted": "Operation not permitted" in process.stderr,
                       "pid_is_one": '"pid_is_one": true' in process.stdout})
    return result


def child(mode, manifest_file, resource_tests):
    if mode == "memory" and not resource_tests:
        raise RuntimeError("memory pressure requires --resource-tests")
    if mode == "final":
        print(json.dumps({"kind": "unit_result", "service_result": os.getenv("SERVICE_RESULT"),
                          "exit_code": os.getenv("EXIT_CODE"), "exit_status": os.getenv("EXIT_STATUS"),
                          "cgroup": cgroup()}), flush=True)
        return 0
    if mode == "memory":
        print(json.dumps({"kind": "before_pressure", "cgroup": cgroup()}), flush=True)
        chunks = []
        for _ in range(128):
            block = bytearray(1024 * 1024)
            for position in range(0, len(block), 4096):
                block[position] = 1
            chunks.append(block)
        print(json.dumps({"kind": "pressure_not_enforced", "allocated_bytes": len(chunks) * 1024 * 1024}), flush=True)
        return 42
    manifest = json.loads(Path(manifest_file).read_text())
    root = Path(manifest["temporary_root"])
    expected = manifest["canary_digest"]
    state = {
        "kind": "isolation", "namespace_changed": {name: value != manifest["host_namespaces"][name] for name, value in namespaces().items()},
        "home_canary": read_check(manifest["home_canary"], expected),
        "unbound_sibling": read_check(manifest["unbound_sibling"]),
        "fixed_bind_canary": read_check(root / "allowed", expected),
        "root_readonly": bool(os.statvfs("/").f_flag & os.ST_RDONLY),
        "etc_readonly": bool(os.statvfs("/etc").f_flag & os.ST_RDONLY),
        "owned_var_tmp_write": write_check(root / "must-be-readonly"),
        "workspace_write": write_check(root / "workspace" / "allowed-write"),
        "same_uid_host_environ": read_check("/proc/" + str(manifest["host_pid"]) + "/environ"),
        "same_uid_host_fd": read_check("/proc/" + str(manifest["host_pid"]) + "/fd/" + str(manifest["host_fd"]), expected),
        "host_loopback": connect_check(socket.AF_INET, ("127.0.0.1", manifest["host_port"])),
        "host_unix_path_socket": connect_check(socket.AF_UNIX, manifest["host_unix_socket"]),
        "host_abstract_unix_socket": connect_check(socket.AF_UNIX, "\0" + manifest["host_abstract_socket"]),
        "only_loopback_interface": sorted(path.name for path in Path("/sys/class/net").iterdir()) == ["lo"],
        "cgroup": cgroup(), "pid_namespace_attempts": pid_attempts(),
        "tasks_enforcement": {"status": "not_run", "reason": "requires --resource-tests"},
    }
    if resource_tests:
        stop = threading.Event()
        threads = []
        try:
            for _ in range(64):
                worker = threading.Thread(target=stop.wait, daemon=True)
                try:
                    worker.start()
                except RuntimeError:
                    break
                threads.append(worker)
            state["tasks_enforcement"] = {"threads_started": len(threads), "cgroup_at_limit": cgroup()}
        finally:
            stop.set()
            for worker in threads:
                worker.join(timeout=2)
    print(json.dumps(state), flush=True)
    return 0


def checked_remove(path, expected_parent):
    """Never delete a caller path: only an owned mkdtemp child recorded by us."""
    path = Path(path)
    info = path.lstat()
    if (not stat.S_ISDIR(info.st_mode) or info.st_uid != os.getuid()
            or path.parent != expected_parent or path.resolve() != path
            or not path.name.startswith("dsh-m3-probe-")
            or not shutil.rmtree.avoids_symlink_attacks):
        raise RuntimeError("refusing unsafe scratch cleanup")
    shutil.rmtree(path)


def probe(resource_tests):
    if sys.platform != "linux" or os.geteuid() == 0:
        raise RuntimeError("run as the intended unprivileged Linux service user, never with sudo")
    for binary in ("/usr/bin/python3", "/usr/bin/unshare", "/usr/bin/sleep"):
        if not Path(binary).is_file():
            raise RuntimeError("required installed probe executable unavailable")
    for binary in ("systemd-run", "systemctl", "journalctl"):
        if shutil.which(binary) is None:
            raise RuntimeError("systemd user-manager tools unavailable")
    cache_parent = Path.home() / ".cache"
    if cache_parent.is_symlink() or cache_parent.stat().st_uid != os.getuid():
        raise RuntimeError("home cache must exist and be owned without a symlink")
    cache_parent = cache_parent.resolve()
    temporary_parent = Path("/var/tmp").resolve()
    temporary = Path(tempfile.mkdtemp(prefix="dsh-m3-probe-", dir=temporary_parent))
    try:
        home = Path(tempfile.mkdtemp(prefix="dsh-m3-probe-", dir=cache_parent))
    except BaseException:
        checked_remove(temporary, temporary_parent)
        raise
    units = []
    sockets = []
    host_process = None
    canary_fd = None
    cleanup = {"all_owned_units_stopped": False, "scratch_removed": False, "synthetic_process_stopped": False}
    report = {"schema": "dsh.harness-isolation-probe.v1", "started_at": datetime.now(timezone.utc).isoformat(),
              "m3_enabled": False, "resource_tests_opted_in": resource_tests,
              "platform": {"arch": platform.machine(), "kernel": platform.release(),
                           "os": platform.freedesktop_os_release().get("PRETTY_NAME"),
                           "systemd": run(["systemctl", "--version"]).stdout.splitlines()[0]},
              "model_or_platform_requests": False, "production_files_modified": False,
              "memory_pressure": {"status": "not_run", "reason": "requires --resource-tests"}}
    token = uuid.uuid4().hex
    try:
        canary = os.urandom(32)
        (home / "private").write_bytes(canary)
        (home / "sibling").write_bytes(os.urandom(32))
        (home / "workspace").mkdir(mode=0o700)
        (temporary / "workspace").mkdir(mode=0o700)
        (temporary / "allowed").touch(mode=0o600)
        (temporary / "node").touch(mode=0o600)
        (temporary / "harness").mkdir(mode=0o700)
        source = temporary / "probe.py"
        shutil.copyfile(Path(__file__).resolve(), source)
        canary_fd = os.open(home / "private", os.O_RDONLY)
        host_process = subprocess.Popen(["/usr/bin/sleep", "120"], env={"PATH": "/usr/bin:/bin", "DSH_SYNTHETIC_PROBE": "1"}, pass_fds=[canary_fd])
        tcp = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        sockets.append(tcp)
        tcp.bind(("127.0.0.1", 0))
        tcp.listen(4)
        unix = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        sockets.append(unix)
        unix.bind(str(temporary / "host.sock"))
        unix.listen(4)
        abstract = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        sockets.append(abstract)
        abstract.bind("\0dsh-m3-probe-" + token)
        abstract.listen(4)
        manifest = {"temporary_root": str(temporary), "home_canary": str(home / "private"),
                    "unbound_sibling": str(home / "sibling"), "canary_digest": hashlib.sha256(canary).hexdigest(),
                    "host_namespaces": namespaces(), "host_pid": host_process.pid, "host_fd": canary_fd,
                    "host_port": tcp.getsockname()[1], "host_unix_socket": str(temporary / "host.sock"),
                    "host_abstract_socket": "dsh-m3-probe-" + token}
        manifest_file = temporary / "manifest.json"
        manifest_file.write_text(json.dumps(manifest))
        report["host_control"] = {"home_canary": read_check(home / "private", manifest["canary_digest"]),
                                  "owned_var_tmp_write": write_check(temporary / "baseline-write"),
                                  "same_uid_host_environ": read_check("/proc/" + str(host_process.pid) + "/environ"),
                                  "same_uid_host_fd": read_check("/proc/" + str(host_process.pid) + "/fd/" + str(canary_fd), manifest["canary_digest"])}

        def launch(label, properties, mode="isolation", allow_pressure=False):
            unit = "dsh-m3-probe-" + token + "-" + label
            units.append(unit + ".service")
            command = ["systemd-run", "--user", "--unit=" + unit, "--quiet", "--wait", "--pipe", "--collect"]
            for name, value in properties.items():
                command.extend(["-p", name + "=" + value])
            command.extend(["/usr/bin/python3", str(source), "--_child", mode, "--_manifest", str(manifest_file)])
            if resource_tests and (mode == "isolation" or allow_pressure):
                command.append("--resource-tests")
            try:
                process = run(command, timeout=40)
                result = {"runner_exit_code": process.returncode, "events": []}
                for line in process.stdout.splitlines():
                    try:
                        result["events"].append(json.loads(line))
                    except json.JSONDecodeError:
                        result["unexpected_output_omitted"] = True
            except subprocess.TimeoutExpired:
                result = {"runner_exit_code": None, "timed_out": True, "events": []}
            journal = run(["journalctl", "--user", "-u", unit + ".service", "--no-pager", "--output=cat", "-n", "16"]).stdout
            result["journal_signals"] = {
                "network_isolation_skipped": "proceeding without" in journal and "PrivateNetwork" in journal,
                "mount_namespace_unavailable": "Failed to set up mount namespacing" in journal,
                "namespace_exit_226": "226/NAMESPACE" in journal,
            }
            return result

        report["baseline_properties"] = dict(BASE_PROPERTIES)
        report["baseline"] = launch("baseline", BASE_PROPERTIES)
        bindings = str(home / "private") + ":" + str(temporary / "allowed")
        node = next((candidate for candidate in (Path.home() / ".local/node/bin/node", Path("/usr/bin/node"), Path("/usr/local/bin/node")) if candidate.is_file()), None)
        if node:
            version = run([str(node), "--version"]).stdout.strip()
            report["node"] = {"path": str(node), "version": version if version.startswith("v") and len(version) < 32 else "unavailable"}
            bindings += " " + str(node) + ":" + str(temporary / "node")
        harness = Path.home() / ".local/share/dsh-harness/node_modules"
        report["harness_runtime_found"] = (harness / "@deepseek-ai/dsh/package.json").is_file()
        if report["harness_runtime_found"]:
            bindings += " " + str(harness) + ":" + str(temporary / "harness")
        fixed = dict(BASE_PROPERTIES)
        fixed.update({"ProtectProc": "ptraceable", "ProcSubset": "pid", "BindReadOnlyPaths": bindings,
                      "BindPaths": str(home / "workspace") + ":" + str(temporary / "workspace"),
                      "ReadWritePaths": str(temporary / "workspace")})
        report["fixed_bind"] = launch("fixed-bind", fixed)
        # Isolate resource enforcement from unavailable namespace features.
        if resource_tests:
            memory = {"MemoryAccounting": "yes", "MemoryMax": "64M", "MemorySwapMax": "0",
                      "TasksAccounting": "yes", "TasksMax": "16", "OOMPolicy": "stop",
                      "RuntimeMaxSec": "20s", "TimeoutStopSec": "5s", "KillMode": "control-group",
                      "NoNewPrivileges": "yes", "ExecStopPost": "/usr/bin/python3 " + str(source) + " --_child final"}
            report["memory_pressure"] = launch("memory", memory, mode="memory", allow_pressure=True)
        # Repeat PID attempts without NoNewPrivileges, so its presence is not
        # mistaken for the reason namespace creation failed.
        pid = {"PrivateUsers": "yes", "MemoryMax": "96M", "TasksMax": "16", "RuntimeMaxSec": "20s"}
        report["independent_pid"] = launch("pid", pid, mode="pid")
        report["isolation_verdict"] = "No automatic enablement; inspect all boundaries. Namespace flags and runner exit 0 alone never certify M3."
    finally:
        for connection in sockets:
            connection.close()
        if host_process is not None:
            host_process.terminate()
            try:
                host_process.wait(timeout=3)
            except subprocess.TimeoutExpired:
                host_process.kill()
                host_process.wait(timeout=3)
            cleanup["synthetic_process_stopped"] = host_process.returncode is not None
        if canary_fd is not None:
            os.close(canary_fd)
        stopped = True
        for unit in units:
            run(["systemctl", "--user", "stop", unit], timeout=8)
            active = run(["systemctl", "--user", "is-active", unit]).stdout.strip()
            stopped = stopped and active not in ("active", "activating", "deactivating", "reloading")
            run(["systemctl", "--user", "reset-failed", unit])
        cleanup["all_owned_units_stopped"] = stopped
        if stopped:
            checked_remove(temporary, temporary_parent)
            checked_remove(home, cache_parent)
            cleanup["scratch_removed"] = True
        report["cleanup"] = cleanup
    report["completed_at"] = datetime.now(timezone.utc).isoformat()
    return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--resource-tests", action="store_true", help="opt in to up to 64 threads and a bounded 64 MiB cgroup OOM test (128 MiB allocation ceiling)")
    parser.add_argument("--_child", choices=("isolation", "memory", "final", "pid"), help=argparse.SUPPRESS)
    parser.add_argument("--_manifest", help=argparse.SUPPRESS)
    arguments = parser.parse_args()
    if arguments._child == "pid":
        print(json.dumps({"kind": "pid_attempts", "attempts": pid_attempts()}), flush=True)
        return 0
    if arguments._child:
        return child(arguments._child, arguments._manifest, arguments.resource_tests)
    try:
        report = probe(arguments.resource_tests)
    except (OSError, RuntimeError, subprocess.SubprocessError) as error:
        # Do not include raw errors, which may contain private scratch paths.
        print(json.dumps({"schema": "dsh.harness-isolation-probe.v1", "m3_enabled": False,
                          "probe_error": type(error).__name__}), flush=True)
        return 1
    print(json.dumps(report, indent=2, sort_keys=True), flush=True)
    return 0 if all(report["cleanup"].values()) else 1


if __name__ == "__main__":
    sys.exit(main())
