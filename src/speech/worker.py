"""Private v1 worker: float32 little-endian PCM in, bounded NDJSON events out.

The parent owns FFmpeg and both process lifetimes. No network calls or model
downloads happen here. Input draining and VAD continue during offline decoding.
"""
import array
import collections
import json
import math
import os
import sys
import threading
import time

RATE = 16000
WINDOW = 512


class AudioQueue:
    """Bounded sample queue; preserve absolute positions across dropped audio."""

    def __init__(self, limit):
        self.limit = limit
        self.items = collections.deque()
        self.samples = 0
        self.closed = False
        self.condition = threading.Condition()

    def put(self, start, samples):
        with self.condition:
            self.items.append((start, samples))
            self.samples += len(samples)
            dropped = False
            while self.samples > self.limit and len(self.items) > 1:
                self.samples -= len(self.items.popleft()[1])
                dropped = True
            self.condition.notify()
            return dropped

    def get(self):
        with self.condition:
            self.condition.wait_for(lambda: self.items or self.closed)
            if not self.items:
                return None
            start, samples = self.items.popleft()
            self.samples -= len(samples)
            return start, samples

    def close(self):
        with self.condition:
            self.closed = True
            self.condition.notify_all()


def stale(end_sample, origin, now):
    return now - origin - end_sample / RATE > 30


def read_pcm(source, pcm, origin, emit):
    """Skip corrupt frames while preserving the gap in absolute sample offsets."""
    offset = 0
    while True:
        data = source.read(WINDOW * 4)
        if not data:
            break
        if len(data) % 4:
            raise ValueError("truncated PCM sample")
        samples = array.array("f")
        samples.frombytes(data)
        if sys.byteorder != "little":
            samples.byteswap()
        if origin[0] is None:
            origin[0] = time.monotonic()
        if all(math.isfinite(sample) for sample in samples):
            pcm.put(offset, samples)
        else:
            emit("gap")
        offset += len(samples)


def emit_event(epoch, kind, **fields):
    # ASCII NDJSON remains writable under isolated Python's locale encoding.
    print(json.dumps(dict(v=1, epoch=epoch, type=kind, **fields),
                     ensure_ascii=True), flush=True)


def run(args):
    import numpy as np
    import sherpa_onnx as sherpa

    output_lock = threading.Lock()

    def emit(kind, **fields):
        with output_lock:
            emit_event(args["epoch"], kind, **fields)

    model = args["model_dir"]
    common = dict(num_threads=args["threads"], provider="cpu")
    if args["model"] == "sensevoice-small-yue-int8":
        recognizer = sherpa.OfflineRecognizer.from_sense_voice(
            model=os.path.join(model, "model.int8.onnx"),
            tokens=os.path.join(model, "tokens.txt"),
            language=args["language"], use_itn=True, **common)
    else:
        recognizer = sherpa.OfflineRecognizer.from_qwen3_asr(
            conv_frontend=os.path.join(model, "conv_frontend.onnx"),
            encoder=os.path.join(model, "encoder.int8.onnx"),
            decoder=os.path.join(model, "decoder.int8.onnx"),
            tokenizer=os.path.join(model, "tokenizer"),
            feature_dim=128, max_total_len=768, max_new_tokens=256, **common)

    config = sherpa.VadModelConfig()
    config.silero_vad.model = args["vad"]
    config.silero_vad.threshold = 0.5
    config.silero_vad.min_silence_duration = 0.5
    config.silero_vad.min_speech_duration = 0.25
    config.silero_vad.max_speech_duration = args["max_segment_secs"]
    config.silero_vad.window_size = WINDOW
    config.sample_rate = RATE
    config.num_threads = 1
    vad = sherpa.VoiceActivityDetector(config, buffer_size_in_seconds=40)
    pcm = AudioQueue(30 * RATE)
    speech = AudioQueue(20 * RATE)
    origin = [None]
    failures = []

    def read():
        try:
            read_pcm(sys.stdin.buffer, pcm, origin, emit)
        except Exception as exc:
            failures.append(str(exc))
        finally:
            pcm.close()

    def segment():
        expected = 0
        base = 0

        def drain():
            while not vad.empty():
                item = vad.front
                samples = item.samples.copy()
                start = base + item.start
                vad.pop()
                # Bound an unexpected oversized VAD result too.
                for cut in range(0, len(samples), 20 * RATE):
                    chunk = samples[cut:cut + 20 * RATE]
                    if speech.put(start + cut, chunk):
                        emit("gap")

        try:
            while True:
                item = pcm.get()
                if item is None:
                    break
                start, samples = item
                if start != expected:
                    vad.reset()
                    base = start
                    emit("gap")
                # Pad only a final partial window; retain its original timeline.
                frame = np.pad(samples, (0, WINDOW - len(samples)))
                vad.accept_waveform(frame)
                expected = start + len(samples)
                drain()
            vad.flush()
            drain()
        except Exception as exc:
            failures.append(str(exc))
        finally:
            speech.close()

    emit("ready")
    reader = threading.Thread(target=read, daemon=True)
    segmenter = threading.Thread(target=segment, daemon=True)
    reader.start()
    segmenter.start()
    while True:
        item = speech.get()
        if item is None:
            break
        start, samples = item
        end = start + len(samples)
        if stale(end, origin[0], time.monotonic()):
            emit("gap")
            continue
        emit("decoding")
        stream = recognizer.create_stream()
        stream.accept_waveform(RATE, samples)
        recognizer.decode_stream(stream)
        text = stream.result.text.strip()
        lag = max(0, int((time.monotonic() - origin[0] - end / RATE) * 1000))
        if text and lag <= 30000:
            emit("segment", start_ms=start * 1000 // RATE,
                 end_ms=end * 1000 // RATE, text=text, lag_ms=lag)
        elif lag > 30000:
            emit("gap")
        emit("listening")
    if failures:
        raise RuntimeError(failures[0])
    reader.join()
    segmenter.join()
    emit("done")


if __name__ == "__main__":
    try:
        run(json.loads(sys.argv[1]))
    except Exception as exc:
        # Native diagnostics use stderr; stdout is reserved for the protocol.
        print(str(exc), file=sys.stderr, flush=True)
        sys.exit(1)
