"""Telemetry sink: the module other fixtures import from."""
import json


class Sink:
    def __init__(self, name):
        self.name = name
        self.buffer = []

    def flush(self, batch):
        payload = json.dumps(batch, default=str) # @eval dumps=-
        return f"{self.name}:{payload}"

    def queue(self, item):
        self.buffer.append(item)
        return len(self.buffer)


def compress_batch(batch):
    seen = dedupe(batch) # @eval dedupe=dedupe@app/sink.py
    return [x for x in seen if x is not None]


def dedupe(items):
    return list(dict.fromkeys(items))
