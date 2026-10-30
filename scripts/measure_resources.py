#!/usr/bin/env -S uv run
"""Measure release radio/MPV/FFmpeg separately against a local paced MP3 source."""

import argparse
import contextlib
import http.server
import json
import os
from pathlib import Path
import pty
import select
import shlex
import shutil
import signal
import sqlite3
import subprocess
import tempfile
import threading
import time
import uuid


def descendants(pid):
    result = []
    pending = [pid]
    while pending:
        current = pending.pop()
        result.append(current)
        for task in Path(f"/proc/{current}/task").glob("*/children"):
            with contextlib.suppress(OSError):
                pending.extend(int(p) for p in task.read_text().split())
    return set(result)


def stats(pid):
    try:
        text = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()
        ticks = int(text[11]) + int(text[12])
        status = Path(f"/proc/{pid}/status").read_text().splitlines()
        rss = next(int(line.split()[1]) for line in status if line.startswith("VmRSS:"))
        name = Path(f"/proc/{pid}/comm").read_text().strip()
        return name, ticks, rss
    except (OSError, StopIteration, ValueError):
        return None


def measure(pid, seconds):
    before = {p: stats(p) for p in descendants(pid)}
    maximum = {}
    started = time.monotonic()
    while time.monotonic() - started < seconds:
        for child in descendants(pid):
            value = stats(child)
            if value:
                name, _, rss = value
                maximum[name] = max(maximum.get(name, 0), rss)
        time.sleep(0.1)
    elapsed = time.monotonic() - started
    cpu = {}
    for child in descendants(pid):
        value, old = stats(child), before.get(child)
        if value and old:
            name, ticks, _ = value
            cpu[name] = cpu.get(name, 0) + (ticks - old[1]) / os.sysconf("SC_CLK_TCK") / elapsed * 100
    return {name: {"rss_mib": round(rss / 1024, 2), "cpu_percent": round(cpu.get(name, 0), 2)}
            for name, rss in maximum.items()}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=Path("target/release/radio"))
    parser.add_argument("--mpv", default=os.environ.get("RADIO_TEST_MPV", "mpv"))
    parser.add_argument("--seconds", type=float, default=5)
    args = parser.parse_args()
    if args.seconds <= 0:
        parser.error("--seconds must be positive")
    binary = args.binary.resolve()
    mpv = shutil.which(args.mpv)
    if not mpv:
        parser.error("mpv required; install it or pass --mpv /path/to/mpv")
    audio = subprocess.check_output([
        "ffmpeg", "-v", "error", "-f", "lavfi", "-i", "sine=frequency=440:duration=120",
        "-c:a", "libmp3lame", "-b:a", "128k", "-f", "mp3", "pipe:1"])

    class Stream(http.server.BaseHTTPRequestHandler):
        def do_GET(self):
            self.send_response(200)
            self.send_header("Content-Type", "audio/mpeg")
            self.send_header("Connection", "close")
            self.end_headers()
            try:
                for pos in range(0, len(audio), 4096):
                    chunk = audio[pos:pos + 4096]
                    self.wfile.write(chunk)
                    self.wfile.flush()
                    time.sleep(len(chunk) / 16000)
            except (BrokenPipeError, ConnectionResetError):
                pass

        def log_message(self, *_):
            pass

    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Stream)
    server.daemon_threads = True
    threading.Thread(target=server.serve_forever, daemon=True).start()
    try:
        with tempfile.TemporaryDirectory(prefix="radio-benchmark-") as temp:
            root = Path(temp)
            wrapper = root / "mpv-null"
            wrapper.write_text(f"#!/bin/sh\nexec {shlex.quote(mpv)} --ao=null \"$@\"\n")
            wrapper.chmod(0o700)
            config = root / "config.toml"
            config.write_text(f"mpv = {json.dumps(str(wrapper))}\n")
            base = [str(binary), "--data-dir", str(root), "--config", str(config)]
            station_id = subprocess.check_output(base + ["station", "add", "Benchmark",
                f"http://127.0.0.1:{server.server_port}/live"], text=True).strip()
            # Exercise the full directory size without any external requests.
            db = sqlite3.connect(root / "radio.db")
            template = list(db.execute("select * from stations").fetchone())
            def rows():
                for i in range(49_999):
                    row = template.copy()
                    row[0] = str(uuid.uuid4())
                    row[1] = f"上海 Jazz Station {i}"
                    row[2] = row[1].lower()
                    document = json.loads(row[10])
                    document.update(stationuuid=row[0], name=row[1])
                    row[10] = json.dumps(document, ensure_ascii=False)
                    yield row
            db.executemany(f"insert into stations values ({','.join('?' for _ in template)})", rows())
            db.commit()
            db.close()
            results = {}
            for mode in ("idle_tui", "playback", "playback_recording"):
                master, slave = pty.openpty()
                # Set a useful size so terminal rendering is included.
                import fcntl
                import struct
                import termios
                fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 100, 0, 0))
                proc = subprocess.Popen(base + ["tui"], stdin=slave, stdout=slave, stderr=slave,
                                        env={**os.environ, "TERM": "xterm-256color"})
                os.close(slave)
                draining = threading.Event()
                def drain():
                    while not draining.is_set():
                        try:
                            if select.select([master], [], [], 0.1)[0]:
                                if not os.read(master, 65536):
                                    return
                        except OSError:
                            return
                reader = threading.Thread(target=drain, daemon=True)
                reader.start()
                try:
                    time.sleep(1)
                    if mode != "idle_tui":
                        # Search the unique benchmark station and wait for debounce.
                        os.write(master, b"/Benchmark")
                        time.sleep(0.5)
                        os.write(master, b"\r\r")
                        time.sleep(3)
                        if not any(stats(p) and stats(p)[0] == "mpv" for p in descendants(proc.pid)):
                            raise RuntimeError("playback did not start")
                    if mode == "playback_recording":
                        os.write(master, b"r")
                        deadline = time.monotonic() + 15
                        while not any(stats(p) and stats(p)[0] == "ffmpeg" for p in descendants(proc.pid)):
                            if time.monotonic() > deadline:
                                raise RuntimeError("recording did not start")
                            time.sleep(0.1)
                        time.sleep(1)
                    if proc.poll() is not None:
                        raise RuntimeError("radio exited before sampling")
                    results[mode] = measure(proc.pid, args.seconds)
                    os.write(master, b"q")
                    proc.wait(timeout=20)
                    if proc.returncode:
                        raise RuntimeError(f"radio exited with {proc.returncode}")
                finally:
                    if proc.poll() is None:
                        proc.send_signal(signal.SIGTERM)
                        try:
                            proc.wait(timeout=20)
                        except subprocess.TimeoutExpired:
                            proc.kill()
                            proc.wait()
                    draining.set()
                    reader.join(timeout=1)
                    os.close(master)
            print(json.dumps({"stations": 50_000, "sample_seconds": args.seconds,
                              "modes": results}, indent=2))
    finally:
        server.shutdown()
        server.server_close()


if __name__ == "__main__":
    main()
