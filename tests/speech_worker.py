"""Deterministic worker queue tests; no speech packages or downloads needed."""
import importlib.util
import io
import struct
import sys
import unittest
from pathlib import Path

sys.dont_write_bytecode = True

spec = importlib.util.spec_from_file_location(
    "worker", Path(__file__).parents[1] / "src/speech/worker.py")
worker = importlib.util.module_from_spec(spec)
spec.loader.exec_module(worker)


class Queues(unittest.TestCase):
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

    def test_truncated_sample_remains_a_framing_error(self):
        with self.assertRaisesRegex(ValueError, "truncated PCM"):
            worker.read_pcm(io.BytesIO(b"\x00"), worker.AudioQueue(512),
                            [None], lambda kind: None)


if __name__ == "__main__":
    unittest.main()
