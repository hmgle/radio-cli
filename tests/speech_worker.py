"""Deterministic worker queue tests; no speech packages or downloads needed."""
import importlib.util
import io
import json
import struct
import sys
import unittest
import types
import tempfile
from unittest.mock import patch
from pathlib import Path

sys.dont_write_bytecode = True

spec = importlib.util.spec_from_file_location(
    "worker", Path(__file__).parents[1] / "src/speech/worker.py")
worker = importlib.util.module_from_spec(spec)
spec.loader.exec_module(worker)


class Queues(unittest.TestCase):
    def test_unicode_segments_round_trip_through_ascii_stdout(self):
        output = io.BytesIO()
        stream = io.TextIOWrapper(output, encoding="ascii")
        with patch.object(sys, "stdout", stream):
            worker.emit_event(7, "segment", text="廣東話字幕 🎵")
        event = json.loads(output.getvalue().decode("ascii"))
        self.assertEqual(event["text"], "廣東話字幕 🎵")
        self.assertEqual((event["v"], event["epoch"], event["type"]),
                         (1, 7, "segment"))

    def test_dropped_audio_keeps_absolute_sample_positions(self):
        queue = worker.AudioQueue(6)
        self.assertFalse(queue.put(0, [1, 2, 3]))
        self.assertFalse(queue.put(3, [4, 5, 6]))
        self.assertTrue(queue.put(6, [7, 8, 9]))
        self.assertEqual(queue.samples, 6)
        self.assertEqual(queue.get(), (3, [4, 5, 6]))
        self.assertEqual(queue.get(), (6, [7, 8, 9]))
        queue.close()
        self.assertIsNone(queue.get())

    def test_expiry_is_based_on_audio_end_not_decode_start(self):
        self.assertFalse(worker.stale(160000, 100, 140))
        self.assertTrue(worker.stale(160000, 100, 140.001))

    def test_invalid_pcm_is_skipped_without_joining_audio_across_the_gap(self):
        for invalid in [float("nan"), float("inf"), -float("inf")]:
            frame = [0.1] * worker.WINDOW
            data = struct.pack("<" + "f" * worker.WINDOW, *frame)
            corrupt = struct.pack("<" + "f" * worker.WINDOW,
                                  invalid, *frame[1:])
            pcm = worker.AudioQueue(4 * worker.WINDOW)
            events = []
            origin = [None]
            worker.read_pcm(io.BytesIO(data + corrupt + data), pcm, origin,
                            events.append)
            self.assertEqual(events, ["gap"])
            self.assertIsNotNone(origin[0])
            self.assertEqual(pcm.get()[0], 0)
            self.assertEqual(pcm.get()[0], 2 * worker.WINDOW)
            pcm.close()
            self.assertIsNone(pcm.get())

    def test_file_mode_preserves_long_audio_and_flushes_final_speech(self):
        class Vad:
            def __init__(self):
                self.offset = 0
                self.pending = []
                self.waiting = []

            def accept_waveform(self, samples):
                # Keep the final frame buffered until flush().
                self.waiting.append(types.SimpleNamespace(
                    start=self.offset, samples=samples))
                self.offset += len(samples)
                if len(self.waiting) > 1:
                    self.pending.append(self.waiting.pop(0))

            def flush(self):
                self.pending.extend(self.waiting)
                self.waiting = []

            def empty(self):
                return not self.pending

            @property
            def front(self):
                return self.pending[0]

            def pop(self):
                self.pending.pop(0)

        class Recognizer:
            def create_stream(self):
                return types.SimpleNamespace(
                    result=types.SimpleNamespace(text="caption"),
                    accept_waveform=lambda rate, samples: None)

            def decode_stream(self, stream):
                pass

        frames = 1300  # More than 30 seconds: live queues would drop samples.
        data = struct.pack("<f", 0.1) * worker.WINDOW * frames
        vad = Vad()
        events = []
        np = types.SimpleNamespace(pad=lambda samples, padding: list(samples))
        worker.run_file(io.BytesIO(data), vad, Recognizer(),
                        lambda kind, **fields: events.append((kind, fields)), np)
        segments = [fields for kind, fields in events if kind == "segment"]
        self.assertEqual(len(segments), frames)
        self.assertEqual(segments[-1]["end_ms"],
                         frames * worker.WINDOW * 1000 // worker.RATE)
        self.assertEqual(events[0][0], "ready")
        self.assertEqual(events[-1][0], "done")
        self.assertFalse(any(kind == "gap" for kind, _ in events))

        class OversizedVad(Vad):
            def accept_waveform(self, samples):
                self.waiting.append(types.SimpleNamespace(
                    start=0, samples=[0.1] * (21 * worker.RATE)))

        events = []
        worker.run_file(io.BytesIO(data[:worker.WINDOW * 4]),
                        OversizedVad(), Recognizer(),
                        lambda kind, **fields: events.append((kind, fields)), np)
        segments = [fields for kind, fields in events if kind == "segment"]
        self.assertEqual([(s["start_ms"], s["end_ms"]) for s in segments],
                         [(0, 20000), (20000, 21000)])

    def test_truncated_sample_remains_a_framing_error(self):
        with self.assertRaisesRegex(ValueError, "truncated PCM"):
            worker.read_pcm(io.BytesIO(b"\x00"), worker.AudioQueue(512),
                            [None], lambda kind: None)

    def test_chunk_mode_reuses_model_and_resets_vad_for_each_window(self):
        resets = []
        vad = types.SimpleNamespace(reset=lambda: resets.append(True))
        recognizer = object()
        events = []
        def file_mode(audio, detector, model, emit, np, announce):
            self.assertIs(detector, vad)
            self.assertIs(model, recognizer)
            self.assertFalse(announce)
            self.assertEqual(audio.read(), b"\x00" * 16)
            emit("segment", start_ms=0, end_ms=1, text="caption")
            emit("done")
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "chunk.pcm"
            path.write_bytes(b"\x00" * 16)
            line = json.dumps({"path": str(path)}).encode() + b"\n"
            with patch.object(worker, "run_file", file_mode):
                worker.run_chunks(io.BytesIO(line * 2), vad, recognizer,
                    lambda kind, **fields: events.append(kind), None)
        self.assertEqual(len(resets), 2)
        self.assertEqual(events, ["ready", "segment", "done", "segment", "done"])


if __name__ == "__main__":
    unittest.main()
