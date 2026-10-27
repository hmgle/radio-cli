"""Deterministic worker queue tests; no speech packages or downloads needed."""
import importlib.util
from pathlib import Path
import unittest
import sys

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


if __name__ == "__main__":
    unittest.main()
