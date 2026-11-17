"""Pipeline: cross-module calls, a same-name collision, and a broken import."""
from app.sink import Sink, compress_batch
from app.helpers import math_hash
import os

# NOTE: app/altmath.py also defines math_hash. The explicit import above
# means every bare math_hash() call here must bind to app/helpers.py,
# not app/altmath.py.


class Pipeline:
    def __init__(self, sink):
        self.sink = sink

    def run(self, events):
        for e in events:
            self.sink.queue(e) # @eval queue=Sink::queue@app/sink.py
        out = compress_batch(events) # @eval compress_batch=compress_batch@app/sink.py
        tag = math_hash(out) # @eval math_hash=math_hash@app/helpers.py
        size = os.path.getsize(__file__) # @eval getsize=-
        return tag, os.path.normpath(out and str(size)) # @eval normpath=-


def fake_flush(events):
    # gzip_compress is imported nowhere. Semantically this NameError call has
    # no in-repo target: it must stay unbound even though a symbol of the
    # same name could exist in another language's fixture.
    return gzip_compress(events) # @eval gzip_compress=-


def describe(pipeline):
    return pipeline.sink.flush(["ready"]) # @eval flush=Sink::flush@app/sink.py
