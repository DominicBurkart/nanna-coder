import json
import os
import queue
import shutil
import signal
import subprocess
import sys
import tempfile
import textwrap
import threading
import time
import unittest

REPO = os.path.abspath(os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", ".."))
BRIDGE = os.path.join(REPO, "scripts", "lumo-bridge.py")
PREFIX = "nanna-bridge/"

FAKE_PODMAN = """#!/usr/bin/env bash
printf '%s\\n' "$*" >> "$FAKE_PODMAN_LOG"
[[ "${1:-}" == exec ]] || exit 0
exec python3 -I "$FAKE_DAEMON_SCRIPT"
"""

FAKE_LAUNCHER = """#!/usr/bin/env bash
printf '%s\\n' "$*" >> "$FAKE_LAUNCHER_LOG"
if [[ -f "$FAKE_DAEMON_DIR/launcher_delay" ]]; then sleep "$(cat "$FAKE_DAEMON_DIR/launcher_delay")"; fi
exit 0
"""

FAKE_DAEMON = """
import json, os, sys, time

d = os.environ["FAKE_DAEMON_DIR"]
counter = os.path.join(d, "counter")
try:
    n = int(open(counter).read()) + 1
except OSError:
    n = 1
open(counter, "w").write(str(n))
open(os.path.join(d, "pid.%d" % n), "w").write(str(os.getpid()))
plan = json.load(open(os.path.join(d, "config.json"))).get("connections", [])
mode = plan[min(n - 1, len(plan) - 1)] if plan else {}
logf = open(os.path.join(d, "daemon.log"), "a")


def log(entry):
    entry["conn"] = n
    logf.write(json.dumps(entry) + "\\n")
    logf.flush()


def reply(obj):
    sys.stdout.write(json.dumps(obj) + "\\n")
    sys.stdout.flush()


calls = 0
silent = False
if mode.get("noise"):
    sys.stdout.write("this is not json\\n")
    sys.stdout.flush()
for raw in sys.stdin.buffer:
    line = raw.decode().strip()
    if not line:
        continue
    msg = json.loads(line)
    log({"method": msg.get("method"), "id": msg.get("id"), "params": msg.get("params")})
    if silent:
        continue
    method = msg.get("method")
    if "id" not in msg:
        continue
    if method == "initialize":
        reply({"jsonrpc": "2.0", "id": msg["id"], "result": {"protocolVersion": "2025-11-25", "capabilities": {}, "serverInfo": {"name": "fake", "version": "0", "conn": n}}})
        if mode.get("silent_after_handshake"):
            silent = True
    elif method == "ping":
        reply({"jsonrpc": "2.0", "id": msg["id"], "result": {}})
    elif method == "tools/list":
        reply({"jsonrpc": "2.0", "id": msg["id"], "result": {"tools": [{"name": "echo"}]}})
    elif method == "tools/call":
        calls += 1
        if mode.get("announce_closed"):
            sys.stderr.write("nanna mcp-attach: the nanna daemon closed the connection while the client was still connected\\n")
            sys.stderr.flush()
            silent = True
            continue
        if mode.get("die_after_requests") and calls >= mode["die_after_requests"]:
            os._exit(3)
        if mode.get("defer_calls"):
            continue
        reply({"jsonrpc": "2.0", "id": msg["id"], "result": {"echo": msg.get("params"), "conn": n}})
    else:
        reply({"jsonrpc": "2.0", "id": msg["id"], "error": {"code": -32601, "message": "no such method"}})
if mode.get("ignore_eof"):
    time.sleep(1000)
"""


class Bridge:
    def __init__(self, test, plan, env=None, launcher_delay=None):
        self.dir = tempfile.mkdtemp(prefix="lumo-bridge-test-")
        test.addCleanup(shutil.rmtree, self.dir, True)
        test.addCleanup(self.close)
        bindir = os.path.join(self.dir, "bin")
        os.makedirs(bindir)
        self.write("bin/podman", FAKE_PODMAN, 0o755)
        self.write("launcher.sh", FAKE_LAUNCHER, 0o755)
        self.write("daemon.py", textwrap.dedent(FAKE_DAEMON))
        self.write("config.json", json.dumps({"connections": plan}))
        if launcher_delay is not None:
            self.write("launcher_delay", str(launcher_delay))
        self.env = {
            "PATH": bindir + ":/usr/bin:/bin",
            "HOME": self.dir,
            "FAKE_DAEMON_DIR": self.dir,
            "FAKE_DAEMON_SCRIPT": os.path.join(self.dir, "daemon.py"),
            "FAKE_PODMAN_LOG": os.path.join(self.dir, "podman.log"),
            "FAKE_LAUNCHER_LOG": os.path.join(self.dir, "launcher.log"),
            "NANNA_BRIDGE_PING_INTERVAL_SECS": "1000",
            "NANNA_BRIDGE_PING_TIMEOUT_SECS": "0.5",
            "NANNA_BRIDGE_QUEUE_SECS": "20",
            "NANNA_BRIDGE_BACKOFF_MIN_SECS": "0.05",
            "NANNA_BRIDGE_BACKOFF_MAX_SECS": "0.2",
            "NANNA_BRIDGE_TICK_SECS": "0.05",
            "NANNA_BRIDGE_CLOSE_GRACE_SECS": "0.5",
        }
        for key, value in (env or {}).items():
            self.env[key] = value.replace("@DIR@", self.dir)
        self.stderr_path = os.path.join(self.dir, "bridge.stderr")
        self.proc = subprocess.Popen(
            [sys.executable, "-I", BRIDGE, "--daemon", "nanna-fake", "--client", "lumo",
             "--launcher", os.path.join(self.dir, "launcher.sh")],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE,
            stderr=open(self.stderr_path, "wb"), env=self.env,
        )
        self.raw = []
        self.lines = queue.Queue()
        threading.Thread(target=self.pump, daemon=True).start()

    def write(self, name, content, mode=0o644):
        path = os.path.join(self.dir, name)
        with open(path, "w") as handle:
            handle.write(content)
        os.chmod(path, mode)

    def pump(self):
        for raw in self.proc.stdout:
            self.raw.append(raw)
            self.lines.put(raw)

    def send(self, obj):
        data = obj if isinstance(obj, str) else json.dumps(obj)
        self.proc.stdin.write(data.encode() + b"\n")
        self.proc.stdin.flush()

    def recv(self, timeout=5.0):
        try:
            return json.loads(self.lines.get(timeout=timeout))
        except queue.Empty:
            return None

    def request(self, rid, method, params=None, timeout=5.0):
        msg = {"jsonrpc": "2.0", "id": rid, "method": method}
        if params is not None:
            msg["params"] = params
        self.send(msg)
        return self.recv(timeout)

    def handshake(self):
        reply = self.request(1, "initialize", INIT_PARAMS)
        self.send({"jsonrpc": "2.0", "method": "notifications/initialized"})
        return reply

    def daemon_log(self):
        path = os.path.join(self.dir, "daemon.log")
        if not os.path.exists(path):
            return []
        with open(path) as handle:
            return [json.loads(line) for line in handle if line.strip()]

    def wait_log(self, predicate, timeout=5.0):
        end = time.monotonic() + timeout
        while time.monotonic() < end:
            found = [e for e in self.daemon_log() if predicate(e)]
            if found:
                return found
            time.sleep(0.05)
        return []

    def close(self):
        try:
            self.proc.stdin.close()
        except OSError:
            pass
        try:
            self.proc.wait(timeout=3)
        except subprocess.TimeoutExpired:
            self.proc.kill()
            self.proc.wait()
        for name in os.listdir(self.dir):
            if name.startswith("pid."):
                try:
                    os.kill(int(open(os.path.join(self.dir, name)).read()), signal.SIGKILL)
                except (OSError, ValueError):
                    pass


INIT_PARAMS = {"protocolVersion": "2025-11-25", "capabilities": {"x": 1}, "clientInfo": {"name": "lumo-test"}}
CALL = {"name": "echo"}


def call(rid):
    return {"jsonrpc": "2.0", "id": rid, "method": "tools/call", "params": CALL}


def initialize_calls(bridge, conn):
    return [e for e in bridge.daemon_log() if e["method"] == "initialize" and e["conn"] == conn]


class ForwardingTests(unittest.TestCase):
    def test_requests_and_responses_are_relayed_and_stdout_is_pure(self):
        bridge = Bridge(self, [{"noise": True}])
        init = bridge.handshake()
        self.assertEqual(init["id"], 1)
        self.assertEqual(init["result"]["protocolVersion"], "2025-11-25")
        listed = bridge.request(2, "tools/list")
        self.assertEqual(listed["result"]["tools"], [{"name": "echo"}])
        called = bridge.request("abc", "tools/call", {"name": "echo", "arguments": {"v": 1}})
        self.assertEqual(called["id"], "abc")
        self.assertEqual(called["result"]["echo"], {"name": "echo", "arguments": {"v": 1}})
        bridge.proc.stdin.close()
        self.assertEqual(bridge.proc.wait(timeout=5), 0)
        self.assertEqual(len(bridge.raw), 3)
        for raw in bridge.raw:
            self.assertIsInstance(json.loads(raw), dict, raw)

    def test_podman_invocation_targets_the_daemon_with_the_client_tag(self):
        bridge = Bridge(self, [{}])
        bridge.handshake()
        with open(bridge.env["FAKE_PODMAN_LOG"]) as handle:
            self.assertIn("exec -i nanna-fake /bin/nanna mcp-attach --client lumo", handle.read().splitlines())


class ReconnectTests(unittest.TestCase):
    def test_child_death_errors_pending_ids_immediately_then_reconnects_transparently(self):
        bridge = Bridge(self, [{"die_after_requests": 2, "defer_calls": True}, {}])
        bridge.handshake()
        started = time.monotonic()
        bridge.send(call(5))
        bridge.send(call("six"))
        errors = {}
        for _ in range(2):
            reply = bridge.recv(2.0)
            self.assertIsNotNone(reply, "pending ids must be answered promptly")
            errors[json.dumps(reply["id"])] = reply
        self.assertLess(time.monotonic() - started, 2.0)
        self.assertEqual(set(errors), {"5", '"six"'})
        for reply in errors.values():
            self.assertEqual(reply["error"]["code"], -32000)
            self.assertIn("retry", reply["error"]["message"])
        after = bridge.request(7, "tools/call", CALL, timeout=8.0)
        self.assertEqual(after["id"], 7)
        self.assertEqual(after["result"]["conn"], 2)
        second = initialize_calls(bridge, 2)
        self.assertEqual(len(second), 1)
        self.assertEqual(second[0]["params"], INIT_PARAMS)
        self.assertTrue(bridge.wait_log(lambda e: e["conn"] == 2 and e["method"] == "notifications/initialized"))
        self.assertEqual(len(bridge.raw), 4)
        for raw in bridge.raw:
            self.assertNotIn("serverInfo", json.loads(raw).get("result", {}) if json.loads(raw)["id"] != 1 else {})
        with open(bridge.env["FAKE_LAUNCHER_LOG"]) as handle:
            self.assertIn("--start", handle.read())

    def test_hung_child_is_detected_by_the_ping_timeout(self):
        bridge = Bridge(self, [{"silent_after_handshake": True}, {}],
                        env={"NANNA_BRIDGE_PING_INTERVAL_SECS": "0.4", "NANNA_BRIDGE_PING_TIMEOUT_SECS": "0.4"})
        bridge.handshake()
        self.assertTrue(bridge.wait_log(lambda e: e["conn"] == 2 and e["method"] == "initialize", timeout=6.0))
        reply = bridge.request(9, "tools/call", CALL, timeout=5.0)
        self.assertEqual(reply["id"], 9)
        self.assertEqual(reply["result"]["conn"], 2)
        self.assertEqual(len(bridge.raw), 2)

    def test_pending_request_on_a_hung_child_gets_the_error_when_declared_dead(self):
        bridge = Bridge(self, [{"silent_after_handshake": True}, {}],
                        env={"NANNA_BRIDGE_PING_INTERVAL_SECS": "0.4", "NANNA_BRIDGE_PING_TIMEOUT_SECS": "0.4"})
        bridge.handshake()
        bridge.send(call(11))
        reply = bridge.recv(4.0)
        self.assertIsNotNone(reply)
        self.assertEqual(reply["id"], 11)
        self.assertEqual(reply["error"]["code"], -32000)

    def test_attach_reporting_a_closed_daemon_connection_reconnects_without_waiting_for_a_ping(self):
        bridge = Bridge(self, [{"announce_closed": True}, {}])
        bridge.handshake()
        bridge.send(call(12))
        reply = bridge.recv(2.0)
        self.assertIsNotNone(reply)
        self.assertEqual(reply["id"], 12)
        self.assertEqual(reply["error"]["code"], -32000)
        self.assertTrue(bridge.wait_log(lambda e: e["conn"] == 2 and e["method"] == "initialize", timeout=4.0))
        with open(bridge.stderr_path) as handle:
            self.assertIn("closed the connection", handle.read())

    def test_wall_clock_jump_triggers_an_immediate_probe(self):
        bridge = Bridge(self, [{}], env={"NANNA_BRIDGE_JUMP_SECS": "5", "NANNA_BRIDGE_TEST_WALL_OFFSET_FILE": "@DIR@/wall_offset"})
        bridge.handshake()
        time.sleep(0.5)
        self.assertFalse([e for e in bridge.daemon_log() if e["method"] == "ping"])
        bridge.write("wall_offset", "100")
        probes = bridge.wait_log(lambda e: e["method"] == "ping", timeout=2.0)
        self.assertTrue(probes, "a clock jump must probe immediately")
        self.assertTrue(str(probes[0]["id"]).startswith(PREFIX))
        time.sleep(0.3)
        self.assertEqual(len(bridge.raw), 1)

    def test_failed_probe_after_a_jump_reconnects(self):
        bridge = Bridge(self, [{"silent_after_handshake": True}, {}],
                        env={"NANNA_BRIDGE_JUMP_SECS": "5", "NANNA_BRIDGE_TEST_WALL_OFFSET_FILE": "@DIR@/wall_offset"})
        bridge.handshake()
        time.sleep(0.3)
        bridge.write("wall_offset", "100")
        self.assertTrue(bridge.wait_log(lambda e: e["conn"] == 2 and e["method"] == "initialize", timeout=4.0))
        reply = bridge.request(3, "tools/list", timeout=5.0)
        self.assertEqual(reply["id"], 3)
        self.assertIn("result", reply)


class QueueTests(unittest.TestCase):
    def test_requests_during_reconnect_are_answered_after_it(self):
        bridge = Bridge(self, [{"die_after_requests": 1}, {}], launcher_delay=1.0)
        bridge.handshake()
        bridge.send(call(20))
        first = bridge.recv(2.0)
        self.assertEqual(first["id"], 20)
        self.assertEqual(first["error"]["code"], -32000)
        bridge.send({"jsonrpc": "2.0", "id": 21, "method": "tools/list"})
        bridge.send({"jsonrpc": "2.0", "id": 22, "method": "tools/list"})
        got = [bridge.recv(8.0), bridge.recv(8.0)]
        self.assertEqual([g["id"] for g in got], [21, 22])
        for reply in got:
            self.assertEqual(reply["result"]["tools"], [{"name": "echo"}])

    def test_requests_older_than_the_queue_window_get_the_error(self):
        bridge = Bridge(self, [{"die_after_requests": 1}, {}], launcher_delay=3.0, env={"NANNA_BRIDGE_QUEUE_SECS": "1"})
        bridge.handshake()
        bridge.send(call(30))
        self.assertEqual(bridge.recv(2.0)["error"]["code"], -32000)
        queued = time.monotonic()
        bridge.send({"jsonrpc": "2.0", "id": 31, "method": "tools/list"})
        reply = bridge.recv(5.0)
        self.assertEqual(reply["id"], 31)
        self.assertEqual(reply["error"]["code"], -32000)
        self.assertGreaterEqual(time.monotonic() - queued, 0.8)
        self.assertLess(time.monotonic() - queued, 2.5)
        bridge.write("launcher_delay", "0")
        self.assertTrue(bridge.wait_log(lambda e: e["conn"] == 2 and e["method"] == "initialize", timeout=8.0))
        later =bridge.request(32, "tools/list", timeout=10.0)
        self.assertEqual(later["id"], 32)
        self.assertIn("result", later)


class ShutdownTests(unittest.TestCase):
    def test_stdin_close_terminates_the_child_and_exits_zero(self):
        bridge = Bridge(self, [{"ignore_eof": True}])
        bridge.handshake()
        pid = int(open(os.path.join(bridge.dir, "pid.1")).read())
        os.kill(pid, 0)
        bridge.proc.stdin.close()
        self.assertEqual(bridge.proc.wait(timeout=5), 0)
        time.sleep(0.2)
        with self.assertRaises(ProcessLookupError):
            os.kill(pid, 0)

    def test_a_request_followed_at_once_by_stdin_close_is_still_answered(self):
        bridge = Bridge(self, [{}])
        bridge.send({"jsonrpc": "2.0", "id": 40, "method": "tools/list"})
        bridge.proc.stdin.close()
        reply = bridge.recv(5.0)
        self.assertEqual(reply["id"], 40)
        self.assertEqual(bridge.proc.wait(timeout=5), 0)

    def test_stdin_close_while_idle_exits_promptly(self):
        bridge = Bridge(self, [{}])
        bridge.handshake()
        began = time.monotonic()
        bridge.proc.stdin.close()
        self.assertEqual(bridge.proc.wait(timeout=5), 0)
        self.assertLess(time.monotonic() - began, 3.0)


class IdNamespaceTests(unittest.TestCase):
    def test_client_ids_never_collide_with_private_ping_ids(self):
        bridge = Bridge(self, [{}], env={"NANNA_BRIDGE_PING_INTERVAL_SECS": "0.1", "NANNA_BRIDGE_PING_TIMEOUT_SECS": "2"})
        bridge.handshake()
        ids = [0, 2, 3, "ping-1", "1", "nanna-bridge", "x/nanna-bridge/1", 4.5]
        for rid in ids:
            reply = bridge.request(rid, "tools/call", CALL)
            self.assertEqual(reply["id"], rid)
            self.assertIn("result", reply)
            time.sleep(0.15)
        self.assertTrue(bridge.wait_log(lambda e: e["method"] == "ping"))
        for raw in bridge.raw:
            self.assertFalse(str(json.loads(raw)["id"]).startswith(PREFIX))
        for entry in [e for e in bridge.daemon_log() if e["method"] == "ping"]:
            self.assertTrue(entry["id"].startswith(PREFIX))
            self.assertNotIn(entry["id"], ids)

    def test_a_client_id_in_the_private_namespace_is_rejected_not_forwarded(self):
        bridge = Bridge(self, [{}])
        bridge.handshake()
        reply = bridge.request(PREFIX + "ping-1", "tools/list")
        self.assertEqual(reply["id"], PREFIX + "ping-1")
        self.assertEqual(reply["error"]["code"], -32600)
        time.sleep(0.2)
        self.assertFalse([e for e in bridge.daemon_log() if e["method"] == "tools/list"])


if __name__ == "__main__":
    unittest.main()
