#!/usr/bin/env python3
import argparse
import json
import os
import queue
import signal
import subprocess
import sys
import threading
import time

PRIVATE = "nanna-bridge/"
LOST_CODE = -32000
LOST_MESSAGE = "nanna daemon connection was lost; the daemon is being reconnected, retry the call"
DEAD_MARKERS = (
    b"the nanna daemon closed the connection",
    b"cannot reach the nanna daemon",
)
STABLE_SECS = 10.0
START_TIMEOUT_SECS = 180.0


def env_float(name, default):
    raw = os.environ.get(name)
    if not raw:
        return default
    try:
        return float(raw)
    except ValueError:
        return default


def log(message):
    sys.stderr.write("%s nanna-bridge: %s\n" % (time.strftime("%Y-%m-%dT%H:%M:%S"), message))
    sys.stderr.flush()


def error_response(rid, code, message):
    return {"jsonrpc": "2.0", "id": rid, "error": {"code": code, "message": message}}


def key_of(rid):
    return json.dumps(rid, sort_keys=True)


class Child:
    def __init__(self, proc):
        self.proc = proc


class Bridge:
    def __init__(self, daemon, client, launcher):
        self.daemon = daemon
        self.client = client
        self.launcher = launcher
        self.ping_interval = env_float("NANNA_BRIDGE_PING_INTERVAL_SECS", 20.0)
        self.ping_timeout = env_float("NANNA_BRIDGE_PING_TIMEOUT_SECS", 10.0)
        self.queue_secs = env_float("NANNA_BRIDGE_QUEUE_SECS", 20.0)
        self.backoff_min = env_float("NANNA_BRIDGE_BACKOFF_MIN_SECS", 0.5)
        self.backoff_max = env_float("NANNA_BRIDGE_BACKOFF_MAX_SECS", 5.0)
        self.tick = env_float("NANNA_BRIDGE_TICK_SECS", 1.0)
        self.jump_secs = env_float("NANNA_BRIDGE_JUMP_SECS", 15.0)
        self.close_grace = env_float("NANNA_BRIDGE_CLOSE_GRACE_SECS", 3.0)
        self.wall_offset_file = os.environ.get("NANNA_BRIDGE_TEST_WALL_OFFSET_FILE")
        self.events = queue.Queue()
        self.state = "connecting"
        self.child = None
        self.token = 0
        self.attempt = 0
        self.pending = {}
        self.queued = []
        self.init_params = None
        self.initialized_seen = False
        self.last_rx = time.monotonic()
        self.connected_at = None
        self.ping = None
        self.ping_seq = 0
        self.handshake_id = None
        self.handshake_deadline = None
        self.close_deadline = None
        self.stdout = sys.stdout.buffer
        self.exit_code = None
        self.close_after_connect = False

    def wall(self):
        now = time.time()
        if self.wall_offset_file:
            try:
                with open(self.wall_offset_file) as handle:
                    now += float(handle.read().strip() or 0)
            except (OSError, ValueError):
                pass
        return now

    def emit(self, obj):
        data = obj if isinstance(obj, bytes) else json.dumps(obj, separators=(",", ":")).encode()
        try:
            self.stdout.write(data + b"\n")
            self.stdout.flush()
        except (BrokenPipeError, OSError):
            self.begin_close(0.0)

    def child_argv(self):
        return ["podman", "exec", "-i", self.daemon, "/bin/nanna", "mcp-attach", "--client", self.client]

    def ensure_daemon(self):
        if not self.launcher:
            return True
        try:
            result = subprocess.run(
                ["bash", self.launcher, "--start"],
                stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, timeout=START_TIMEOUT_SECS,
            )
        except (OSError, subprocess.SubprocessError) as error:
            log("could not run the launcher: %s" % error)
            return False
        if result.returncode != 0:
            log("launcher --start exited %d" % result.returncode)
            return False
        return True

    def connect_worker(self, token, delay, run_start):
        if delay:
            time.sleep(delay)
        if run_start and not self.ensure_daemon():
            self.events.put(("spawned", token, None, None))
            return
        try:
            proc = subprocess.Popen(
                self.child_argv(), stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            )
        except FileNotFoundError as error:
            self.events.put(("spawned", token, None, error))
            return
        except OSError as error:
            log("could not start the daemon connection: %s" % error)
            self.events.put(("spawned", token, None, None))
            return
        child = Child(proc)
        threading.Thread(target=self.read_child, args=(child,), daemon=True).start()
        threading.Thread(target=self.read_child_stderr, args=(child,), daemon=True).start()
        self.events.put(("spawned", token, child, None))

    def read_child(self, child):
        for line in child.proc.stdout:
            self.events.put(("child_line", child, line))
        self.events.put(("child_eof", child))

    def read_child_stderr(self, child):
        for line in child.proc.stderr:
            sys.stderr.buffer.write(line)
            sys.stderr.buffer.flush()
            if any(marker in line for marker in DEAD_MARKERS):
                self.events.put(("child_dead", child, line.strip().decode(errors="replace")))

    def read_stdin(self):
        stream = sys.stdin.buffer
        while True:
            line = stream.readline()
            if not line:
                self.events.put(("stdin", None))
                return
            self.events.put(("stdin", line))

    def start_connecting(self, run_start):
        self.state = "connecting"
        self.token += 1
        delay = 0.0
        if run_start:
            delay = min(self.backoff_max, self.backoff_min * (2 ** self.attempt))
            self.attempt += 1
        threading.Thread(target=self.connect_worker, args=(self.token, delay, run_start), daemon=True).start()

    def send_line(self, data):
        try:
            self.child.proc.stdin.write(data.rstrip(b"\r\n") + b"\n")
            self.child.proc.stdin.flush()
            return True
        except (OSError, ValueError):
            return False

    def reap(self, child):
        proc = child.proc
        try:
            proc.kill()
        except OSError:
            pass
        threading.Thread(target=proc.wait, daemon=True).start()

    def lose(self, child, reason):
        if child is not self.child or self.state in ("connecting", "closing"):
            return
        log("connection lost: %s" % reason)
        self.reap(child)
        self.child = None
        self.ping = None
        self.handshake_id = None
        if self.connected_at is not None and time.monotonic() - self.connected_at >= STABLE_SECS:
            self.attempt = 0
        self.connected_at = None
        pending, self.pending = self.pending, {}
        for rid in pending.values():
            self.emit(error_response(rid, LOST_CODE, LOST_MESSAGE))
        self.start_connecting(True)

    def on_spawned(self, token, child, fatal):
        if token != self.token or self.state != "connecting":
            if child is not None:
                self.reap(child)
            return
        if fatal is not None:
            log("cannot run podman: %s" % fatal)
            self.exit_code = 127
            return
        if child is None:
            log("reconnect attempt failed; backing off")
            self.start_connecting(True)
            return
        self.child = child
        self.last_rx = time.monotonic()
        if self.init_params is None:
            self.on_connected()
            return
        self.state = "handshaking"
        self.handshake_id = PRIVATE + "init-%d" % self.token
        self.handshake_deadline = time.monotonic() + self.ping_timeout
        request = {"jsonrpc": "2.0", "id": self.handshake_id, "method": "initialize", "params": self.init_params}
        if not self.send_line(json.dumps(request).encode()):
            self.lose_handshake("write failed")

    def lose_handshake(self, reason):
        child = self.child
        self.state = "connected"
        self.lose(child, reason)

    def on_connected(self):
        self.state = "connected"
        self.connected_at = time.monotonic()
        self.handshake_id = None
        log("connected to %s" % self.daemon)
        queued, self.queued = self.queued, []
        now = time.monotonic()
        for stamp, line, msg in queued:
            if now - stamp > self.queue_secs:
                self.expire(msg)
                continue
            self.forward(line, msg)
        if self.close_after_connect:
            self.begin_close(self.close_grace)

    def expire(self, msg):
        if isinstance(msg, dict) and "id" in msg and "method" in msg:
            self.emit(error_response(msg["id"], LOST_CODE, LOST_MESSAGE))

    def forward(self, line, msg):
        if self.state != "connected":
            self.queued.append((time.monotonic(), line, msg))
            return
        if isinstance(msg, dict) and "id" in msg and "method" in msg:
            self.pending[key_of(msg["id"])] = msg["id"]
            if msg["method"] == "initialize":
                self.init_params = msg.get("params", {})
        elif isinstance(msg, dict) and msg.get("method") == "notifications/initialized":
            self.initialized_seen = True
        if not self.send_line(line):
            self.lose(self.child, "write to the daemon connection failed")

    def on_stdin(self, line):
        if line is None:
            self.begin_close(self.close_grace)
            return
        if self.state == "closing":
            return
        stripped = line.strip()
        if not stripped:
            return
        try:
            msg = json.loads(stripped)
        except ValueError:
            msg = None
        if isinstance(msg, dict) and "method" in msg and "id" in msg:
            rid = msg["id"]
            if isinstance(rid, str) and rid.startswith(PRIVATE):
                self.emit(error_response(rid, -32600, "ids starting with %s are reserved by the bridge" % PRIVATE))
                return
        if self.state == "handshaking":
            self.queued.append((time.monotonic(), stripped, msg))
            return
        self.forward(stripped, msg)

    def on_child_line(self, child, line):
        if child is not self.child:
            return
        self.last_rx = time.monotonic()
        text = line.strip()
        if not text:
            return
        try:
            msg = json.loads(text)
        except ValueError:
            log("dropped a non-JSON line from the daemon connection")
            return
        if not isinstance(msg, dict):
            if self.state in ("connected", "closing"):
                self.emit(text)
            return
        rid = msg.get("id")
        is_response = "method" not in msg and "id" in msg
        if is_response and isinstance(rid, str) and rid.startswith(PRIVATE):
            if self.state == "handshaking" and rid == self.handshake_id:
                if "error" in msg:
                    self.lose_handshake("daemon rejected the re-handshake")
                    return
                if self.initialized_seen:
                    self.send_line(json.dumps({"jsonrpc": "2.0", "method": "notifications/initialized"}).encode())
                self.on_connected()
            elif self.ping is not None and rid == self.ping[0]:
                self.ping = None
            return
        if self.state not in ("connected", "closing"):
            return
        if is_response:
            self.pending.pop(key_of(rid), None)
        self.emit(text.encode() if isinstance(text, str) else text)

    def on_child_eof(self, child):
        if self.state == "closing" and child is self.child:
            self.exit_code = 0
            return
        self.lose_handshake_or_connected(child, "the daemon connection closed")

    def lose_handshake_or_connected(self, child, reason):
        if self.state == "handshaking":
            self.lose_handshake(reason)
        else:
            self.lose(child, reason)

    def send_ping(self):
        if self.ping is not None or self.state != "connected":
            return
        self.ping_seq += 1
        rid = PRIVATE + "ping-%d" % self.ping_seq
        self.ping = (rid, time.monotonic())
        request = {"jsonrpc": "2.0", "id": rid, "method": "ping"}
        if not self.send_line(json.dumps(request).encode()):
            self.lose(self.child, "write to the daemon connection failed")

    def begin_close(self, grace):
        if grace > 0 and self.state in ("connecting", "handshaking") and self.queued:
            self.close_after_connect = True
            return
        if self.state == "closing":
            if grace == 0.0:
                self.close_deadline = time.monotonic()
            return
        self.state = "closing"
        self.close_deadline = time.monotonic() + grace
        if self.child is None:
            self.exit_code = 0
            return
        try:
            self.child.proc.stdin.close()
        except (OSError, ValueError):
            pass

    def on_tick(self, prev):
        now = time.monotonic()
        wall = self.wall()
        jump = (wall - prev["wall"]) - (now - prev["mono"])
        prev["wall"], prev["mono"] = wall, now
        if self.state == "closing":
            if now >= self.close_deadline:
                self.exit_code = 0
            return
        if self.queued:
            keep = []
            for entry in self.queued:
                if now - entry[0] > self.queue_secs:
                    self.expire(entry[2])
                else:
                    keep.append(entry)
            self.queued = keep
            if self.close_after_connect and not keep:
                self.exit_code = 0
                return
        if self.state == "handshaking" and now >= self.handshake_deadline:
            self.lose_handshake("re-handshake timed out")
            return
        if self.state != "connected":
            return
        if jump > self.jump_secs:
            log("clock jumped %.0fs (suspend/resume); probing the daemon connection" % jump)
            self.send_ping()
        elif self.ping is None and now - self.last_rx >= self.ping_interval:
            self.send_ping()
        if self.ping is not None and now - self.ping[1] >= self.ping_timeout:
            self.lose(self.child, "no reply to a ping within %.1fs" % self.ping_timeout)

    def terminate_child(self):
        child = self.child
        if child is None or child.proc.poll() is not None:
            return
        child.proc.terminate()
        try:
            child.proc.wait(timeout=1.0)
        except subprocess.TimeoutExpired:
            child.proc.kill()
            child.proc.wait()

    def run(self):
        def on_signal(_signum, _frame):
            self.events.put(("signal",))

        for signum in (signal.SIGTERM, signal.SIGHUP, signal.SIGINT):
            signal.signal(signum, on_signal)
        threading.Thread(target=self.read_stdin, daemon=True).start()
        self.start_connecting(False)
        prev = {"wall": self.wall(), "mono": time.monotonic()}
        next_tick = time.monotonic() + self.tick
        try:
            while self.exit_code is None:
                try:
                    event = self.events.get(timeout=max(0.0, next_tick - time.monotonic()))
                except queue.Empty:
                    event = None
                if event is not None:
                    kind = event[0]
                    if kind == "stdin":
                        self.on_stdin(event[1])
                    elif kind == "child_line":
                        self.on_child_line(event[1], event[2])
                    elif kind == "child_dead":
                        self.lose_handshake_or_connected(event[1], event[2])
                    elif kind == "child_eof":
                        self.on_child_eof(event[1])
                    elif kind == "spawned":
                        self.on_spawned(event[1], event[2], event[3])
                    elif kind == "signal":
                        self.begin_close(0.0)
                if time.monotonic() >= next_tick:
                    self.on_tick(prev)
                    next_tick = time.monotonic() + self.tick
        finally:
            self.terminate_child()
        return self.exit_code


def main():
    here = os.path.dirname(os.path.abspath(__file__))
    parser = argparse.ArgumentParser(description="Keep Lumo's connection to the nanna daemon alive across restarts and host sleep.")
    parser.add_argument("--daemon", default=os.environ.get("NANNA_DAEMON_NAME", "nanna-daemon"))
    parser.add_argument("--client", default="lumo")
    parser.add_argument("--launcher", default=os.path.join(here, "lumo-mcp.sh"))
    args = parser.parse_args()
    return bridge_exit(Bridge(args.daemon, args.client, args.launcher))


def bridge_exit(bridge):
    try:
        return bridge.run()
    except Exception as error:
        log("unrecoverable error: %r" % error)
        return 1


if __name__ == "__main__":
    sys.exit(main())
