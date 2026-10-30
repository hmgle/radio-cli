#!/usr/bin/env -S uv run
"""Measure idle TUI and Explore RSS/CPU on Linux using a local catalog fixture.

Create a 200k-station fixture first:
RADIO_MAP_BENCH_DB=/tmp/radio-map/radio.db cargo test --release --test \
    catalog_perf geographic_catalog_benchmark -- --ignored --nocapture
Then: uv run scripts/measure_map_resources.py --binary target/release/radio \
    --data-dir /tmp/radio-map
The data directory must be isolated: the TUI writes source preferences.
"""
import argparse
import fcntl
import json
import os
from pathlib import Path
import pty
import re
import select
import struct
import subprocess
import tempfile
import termios
import threading
import time


def stats(pid):
    fields = Path(f'/proc/{pid}/stat').read_text().rsplit(')', 1)[1].split()
    status = Path(f'/proc/{pid}/status').read_text().splitlines()
    rss = next(int(line.split()[1]) for line in status if line.startswith('VmRSS:'))
    return int(fields[11]) + int(fields[12]), rss


def sample(pid, seconds):
    before, rss = stats(pid)
    started = time.monotonic()
    peak = rss
    while time.monotonic() - started < seconds:
        time.sleep(0.1)
        ticks, rss = stats(pid)
        peak = max(peak, rss)
    elapsed = time.monotonic() - started
    return {'rss_mib': round(peak / 1024, 2), 'cpu_percent': round((ticks - before) / os.sysconf('SC_CLK_TCK') / elapsed * 100, 2)}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', required=True, type=Path)
    parser.add_argument('--data-dir', required=True, type=Path)
    parser.add_argument('--seconds', type=float, default=5)
    args = parser.parse_args()
    if args.seconds <= 0:
        parser.error('--seconds must be positive')
    if not Path('/proc/self/stat').exists():
        parser.error('resource sampling requires Linux /proc')
    if not (args.data_dir / 'radio.db').is_file():
        parser.error('create the local fixture first')
    with tempfile.TemporaryDirectory(prefix='radio-map-measure-') as temporary:
        config = Path(temporary) / 'config.toml'
        config.write_text('resume = false\nartwork = "off"\n')
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 48, 160, 0, 0))
        process = subprocess.Popen([str(args.binary.resolve()), '--lang', 'en', '--config', str(config), '--data-dir', str(args.data_dir.resolve()), 'tui'], stdin=slave, stdout=slave, stderr=slave, start_new_session=True, env={**os.environ, 'TERM': 'xterm-256color'})
        os.close(slave)
        stop = threading.Event()
        output = bytearray()

        def drain():
            while not stop.is_set():
                if select.select([master], [], [], 0.1)[0]:
                    try:
                        data = os.read(master, 65536)
                    except OSError:
                        return
                    if not data:
                        return
                    output.extend(data)
                    if len(output) > 2 * 1024 * 1024:
                        del output[:1024 * 1024]

        reader = threading.Thread(target=drain, daemon=True)
        reader.start()
        try:
            time.sleep(2)
            if process.poll() is not None:
                raise RuntimeError(output.decode(errors='replace'))
            ordinary = sample(process.pid, args.seconds)
            output.clear()
            started = time.monotonic()
            os.write(master, b':explore\r')
            deadline = started + 10
            while True:
                # Terminal diff rendering can split the status at cursor moves.
                # Observe the loaded count instead of a particular map glyph.
                visible = re.sub(rb'\x1b\[[0-?]*[ -/]*[@-~]', b'', bytes(output))
                if re.search(rb'\d+\s*stations\s*in\s*cell', visible):
                    break
                if process.poll() is not None or time.monotonic() > deadline:
                    raise RuntimeError('Map did not load: ' + output.decode(errors='replace'))
                time.sleep(0.01)
            first = (time.monotonic() - started) * 1000
            time.sleep(0.5)
            explore = sample(process.pid, args.seconds)
            for _ in range(10):
                os.write(master, b'zlljjZhhkk')
                time.sleep(0.05)
            time.sleep(1)
            after_navigation = sample(process.pid, args.seconds)
            print(json.dumps({'ordinary': ordinary, 'explore': explore, 'after_navigation': after_navigation, 'first_map_ms': round(first, 2), 'rss_delta_mib': round(explore['rss_mib'] - ordinary['rss_mib'], 2)}, indent=2))
        finally:
            os.write(master, b'\x03')
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()
            stop.set()
            reader.join(timeout=1)
            os.close(master)


if __name__ == '__main__':
    main()
