"""`Dataset.load` from a file object: the loader reads the stream once as it arrives,
with no temporary copy, in every syntax."""

from __future__ import annotations

import _thread
import gzip
import io
import os
import signal
import subprocess
import sys
import textwrap
from pathlib import Path

import pytest

import sparkles
from sparkles import Dataset, RdfFormat

TRIG = b"""
@prefix ex: <http://example.org/> .
ex:a ex:p "one" , "deux"@fr .
ex:b ex:p ex:a .
ex:g { ex:a ex:q 1 . ex:c ex:q [ ex:r "x" ] }
"""

FORMATS = [
    ("turtle", True),
    ("nt", True),
    ("nq", True),
    ("trig", True),
    ("rdfxml", True),
    ("jsonld", True),
    (RdfFormat.RDF_THRIFT, False),
    (RdfFormat.RDF_PROTOBUF, False),
]


class Pieces(io.RawIOBase):
    """A binary file object that hands out its data in small pieces and records the sizes
    it is asked for."""

    def __init__(self, data: bytes, size: int = 997) -> None:
        self.data = data
        self.size = size
        self.pos = 0
        self.asked: list[int] = []

    def readable(self) -> bool:
        return True

    def read(self, n: int = -1) -> bytes:
        self.asked.append(n)
        chunk = self.data[self.pos : self.pos + min(self.size, n if n >= 0 else self.size)]
        self.pos += len(chunk)
        return chunk


def sample(fmt) -> tuple[bytes, int]:
    """The sample in `fmt` (its default graph for a triple syntax) and its quad count."""
    ds = Dataset()
    ds.load(TRIG, "trig")
    if fmt in ("turtle", "nt", "rdfxml"):
        data = ds.dump(format=fmt, from_graph=None)
    else:
        data = ds.dump(format=fmt)
    return data, Dataset().load(data, fmt)


def nt(n: int) -> bytes:
    return "".join(f'<http://ex.org/s{i}> <http://ex.org/p> "{i}" .\n' for i in range(n)).encode()


@pytest.mark.parametrize("fmt,text", FORMATS)
def test_every_syntax_loads_from_binary_and_text_file_objects(fmt, text) -> None:
    data, n = sample(fmt)
    assert n > 0
    want = Dataset()
    want.load(data, fmt)
    # a second load of bytes adds the quads of its own blank nodes
    again = Dataset()
    again.load(data, fmt)
    again = again.load(data, fmt)
    for mode in ["auto", "streaming", "buffered"]:
        src = Pieces(data)
        ds = Dataset()
        assert ds.load(src, fmt, parse_mode=mode) == n, (fmt, mode)
        assert src.pos == len(data)
        # never more than one bounded read at a time
        assert all(0 < k <= 64 << 10 for k in src.asked), src.asked
        assert ds.dump(format="nq") == want.dump(format="nq"), (fmt, mode)
        # into a dataset that is not empty, through a transaction
        assert ds.load(Pieces(data), fmt, parse_mode=mode) == again
        if text:
            assert Dataset().load(io.StringIO(data.decode()), fmt, parse_mode=mode) == n


def test_a_text_file_object_keeps_characters_that_span_reads() -> None:
    # multibyte characters, read 64 KiB of characters at a time from a text stream
    line = '<http://ex.org/s> <http://ex.org/p> "Grüße, 東京, 🎉 %d" .\n'
    data = "".join(line % i for i in range(20_000))
    ds = Dataset()
    assert ds.load(io.StringIO(data), "nt") == 20_000
    assert ds.ask('ASK { ?s ?p "Grüße, 東京, 🎉 19999" }')
    path_ds = Dataset()
    path_ds.load(data.encode(), "nt")
    assert sorted(ds.dump(format="nq").splitlines()) == sorted(path_ds.dump(format="nq").splitlines())


@pytest.mark.parametrize("fmt", ["turtle", "rdfxml", "nt", RdfFormat.RDF_THRIFT])
@pytest.mark.parametrize("allowance", [None, 0, 64])
def test_compression_is_sniffed_from_the_stream(fmt, allowance) -> None:
    data, n = sample(fmt)
    for packed in [gzip.compress(data), data]:
        src = Pieces(packed, size=5)
        assert Dataset().load(src, fmt, auto_buffer_bytes=allowance) == n
        assert src.pos == len(packed)
    # an explicit codec is checked against the data
    with pytest.raises(sparkles.InvalidInputError):
        Dataset().load(Pieces(data), fmt, compression="zstd")


def test_a_large_line_based_stream_is_parsed_in_parallel_blocks() -> None:
    # larger than one parallel chunk of the loader
    data = gzip.compress(nt(300_000))
    src = Pieces(data, size=1 << 16)
    ds = Dataset()
    assert ds.load(src, "nt") == 300_000
    assert len(ds) == 300_000


class Broken(Exception):
    pass


class FailsAfter(Pieces):
    def __init__(self, data: bytes, reads: int) -> None:
        super().__init__(data)
        self.reads = reads

    def read(self, n: int = -1) -> bytes:
        if len(self.asked) >= self.reads:
            raise Broken("the stream broke")
        return super().read(n)


@pytest.mark.parametrize("fmt", ["nt", "turtle", "rdfxml", RdfFormat.RDF_THRIFT])
@pytest.mark.parametrize("mode", ["auto", "streaming", "buffered"])
def test_an_exception_of_read_propagates_as_itself(fmt, mode) -> None:
    ds = Dataset()
    ds.load(nt(10_000), "nt")
    data = ds.dump(format=fmt)
    for target in [Dataset(), ds]:
        before = len(target)
        with pytest.raises(Broken, match="the stream broke"):
            target.load(FailsAfter(data, 3), fmt, parse_mode=mode)
        assert len(target) == before


class Endless(io.RawIOBase):
    """N-Triples without end, which interrupts the main thread after `after` reads."""

    def __init__(self, after: int) -> None:
        self.after = after
        self.reads = 0
        self.at_interrupt: int | None = None
        self.n = 0

    def readable(self) -> bool:
        return True

    def read(self, n: int = -1) -> bytes:
        self.reads += 1
        if self.reads == self.after:
            self.at_interrupt = self.reads
            _thread.interrupt_main()
        if self.reads > 1_000_000:
            return b""  # end the test rather than hang
        out = []
        size = 0
        while size < n:
            line = b'<http://ex.org/s%d> <http://ex.org/p> "%d" .\n' % (self.n, self.n)
            self.n += 1
            out.append(line)
            size += len(line)
        return b"".join(out)


@pytest.mark.parametrize("fmt", ["nt", "turtle"])
def test_ctrl_c_stops_the_reading_of_a_file_object(fmt) -> None:
    filled = Dataset()
    filled.load(nt(5), "nt")
    # the bulk loader and a transaction
    for target in [Dataset(), filled]:
        before = len(target)
        src = Endless(after=50)
        with pytest.raises(KeyboardInterrupt):
            target.load(src, fmt)
        assert src.at_interrupt == 50
        # the reads stopped at once, although the reader calls read in a tight loop
        assert src.reads < 50 + 20, src.reads
        assert len(target) == before


def test_a_file_object_writes_no_temporary_copy(tmp_path: Path) -> None:
    """With TMPDIR at a directory that does not exist, a load from a file object succeeds
    in every syntax, in automatic and streaming mode and in buffered mode within its
    allowance, so it wrote no temporary file."""
    names = {RdfFormat.RDF_THRIFT: "rt", RdfFormat.RDF_PROTOBUF: "rpb"}
    data = {names.get(f, f): sample(f)[0] for f, _ in FORMATS}
    script = textwrap.dedent(
        f"""
        import io, sys, tempfile
        import sparkles
        data = {data!r}
        db = sys.argv[1]
        total = 0
        for i, (fmt, raw) in enumerate(data.items()):
            for mode in ["auto", "streaming", "buffered"]:
                ds = sparkles.Dataset(f"{{db}}-{{i}}-{{mode}}")
                total += ds.load(io.BytesIO(raw), fmt, parse_mode=mode)
                total += ds.load(io.BytesIO(raw), fmt, parse_mode=mode)
                ds.close()
        print(total)
        """
    )
    missing = tmp_path / "no-such-dir"
    env = dict(os.environ, TMPDIR=str(missing), PYTHONPATH=os.pathsep.join(sys.path))
    out = subprocess.run(
        [sys.executable, "-c", script, str(tmp_path / "db")],
        env=env,
        capture_output=True,
        text=True,
    )
    assert out.returncode == 0, out.stderr
    assert int(out.stdout) > 0
    assert not missing.exists()


class Signals(Pieces):
    """A file object that raises SIGUSR1 at every tenth read."""

    def read(self, n: int = -1) -> bytes:
        if len(self.asked) % 10 == 9:
            signal.raise_signal(signal.SIGUSR1)
        return super().read(n)


@pytest.mark.skipif(not hasattr(signal, "SIGUSR1"), reason="no SIGUSR1")
def test_a_signal_whose_handler_returns_leaves_the_load_running() -> None:
    seen = []
    old = signal.signal(signal.SIGUSR1, lambda *_: seen.append(1))
    try:
        data = nt(20_000)
        for fmt, packed in [("nt", data), ("nt", gzip.compress(data)), ("turtle", data)]:
            seen.clear()
            assert Dataset().load(Signals(packed, size=1 << 12), fmt) == 20_000
            assert seen
    finally:
        signal.signal(signal.SIGUSR1, old)


def run_child(script: str, *args: str, env: dict | None = None, timeout: float = 120):
    """Run `script` in a child Python with this package, failing instead of hanging."""
    full = dict(os.environ, PYTHONPATH=os.pathsep.join(sys.path), **(env or {}))
    return subprocess.run(
        [sys.executable, "-c", textwrap.dedent(script), *args],
        env=full,
        capture_output=True,
        text=True,
        timeout=timeout,
    )


class InterruptAtEnd(Pieces):
    """A file object that interrupts the main thread in the read that finds its end."""

    def read(self, n: int = -1) -> bytes:
        chunk = super().read(n)
        if not chunk:
            _thread.interrupt_main()
        return chunk


@pytest.mark.parametrize("mode", ["auto", "streaming", "buffered"])
@pytest.mark.parametrize("fmt", ["nt", "turtle", RdfFormat.RDF_THRIFT])
def test_an_interrupt_at_the_end_of_the_stream_commits_nothing(mode, fmt) -> None:
    data, _ = sample(fmt)
    filled = Dataset()
    filled.load(nt(5), "nt")
    for target in [Dataset(), filled]:
        before = target.dump(format="nq")
        with pytest.raises(KeyboardInterrupt):
            target.load(InterruptAtEnd(data), fmt, parse_mode=mode)
        assert target.dump(format="nq") == before
    with pytest.raises(KeyboardInterrupt):
        filled.replace(InterruptAtEnd(data), format=fmt)
    assert len(filled) == 5


@pytest.mark.skipif(not hasattr(signal, "SIGUSR1"), reason="no SIGUSR1")
def test_a_signal_handler_can_query_while_a_file_object_loads() -> None:
    out = run_child(
        """
        import io, signal
        import sparkles
        other = sparkles.Dataset()
        other.load('<http://e/a> <http://e/b> <http://e/c> .', "nt")
        answers = []
        signal.signal(signal.SIGUSR1, lambda *_: answers.append(other.ask("ASK { ?s ?p ?o }")))
        data = "".join(f'<http://e/s{i}> <http://e/p> "{i}" .\\n' for i in range(20000)).encode()

        class Signals(io.RawIOBase):
            def __init__(self):
                self.pos = 0
                self.reads = 0
            def readable(self):
                return True
            def read(self, n=-1):
                self.reads += 1
                if self.reads % 5 == 0:
                    signal.raise_signal(signal.SIGUSR1)
                chunk = data[self.pos : self.pos + min(n, 4096)]
                self.pos += len(chunk)
                return chunk

        ds = sparkles.Dataset()
        n = ds.load(Signals(), "nt")
        ds.replace(Signals(), format="nt")
        print(n, len(ds), len(answers) > 0 and all(answers))
        """,
        timeout=60,
    )
    assert out.returncode == 0, out.stderr
    assert out.stdout.split() == ["20000", "20000", "True"]


def test_ctrl_c_and_a_token_stop_a_replacement_from_a_file_object() -> None:
    out = run_child(
        """
        import _thread, io
        import sparkles

        class Endless(io.RawIOBase):
            def __init__(self, after, token=None):
                self.after, self.token, self.reads, self.n = after, token, 0, 0
            def readable(self):
                return True
            def read(self, n=-1):
                self.reads += 1
                if self.reads == self.after:
                    if self.token:
                        self.token.cancel()
                    else:
                        _thread.interrupt_main()
                if self.reads > 100000:
                    return b""
                out, size = [], 0
                while size < n:
                    line = b'<http://e/s%d> <http://e/p> "%d" .\\n' % (self.n, self.n)
                    self.n += 1
                    out.append(line)
                    size += len(line)
                return b"".join(out)

        ds = sparkles.Dataset()
        ds.load('<http://e/a> <http://e/b> <http://e/c> .', "nt")
        for fmt in ["nt", "turtle"]:
            src = Endless(50)
            try:
                ds.replace(src, format=fmt)
                print("no-interrupt")
            except KeyboardInterrupt:
                print("interrupted", src.reads < 70, len(ds))
            token = sparkles.CancelToken()
            src = Endless(50, token)
            try:
                ds.replace(src, format=fmt, cancel=token)
                print("no-cancel")
            except sparkles.CancelledError:
                print("cancelled", src.reads < 70, len(ds))
        """,
        timeout=60,
    )
    assert out.returncode == 0, out.stderr
    assert out.stdout.splitlines() == ["interrupted True 1", "cancelled True 1"] * 2


def test_a_tiny_file_object_allocates_no_block() -> None:
    out = run_child(
        """
        import io, resource, sys
        import sparkles
        def peak():
            r = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
            return r if sys.platform == "darwin" else r * 1024
        line = b'<http://e/a> <http://e/b> <http://e/c> .\\n'
        sparkles.Dataset().load(line, "nt")
        before = peak()
        for compress in [False, True]:
            import gzip
            data = gzip.compress(line) if compress else line
            assert sparkles.Dataset().load(io.BytesIO(data), "nt") == 1
        print(peak() - before)
        """
    )
    assert out.returncode == 0, out.stderr
    assert int(out.stdout) < 64 << 20, int(out.stdout)


def test_a_large_buffered_stream_spills_beyond_its_allowance(tmp_path: Path) -> None:
    script = """
        import io, sys
        import sparkles
        data = "".join(f'<http://e/s{i}> <http://e/p> "{i}" .\\n' for i in range(5000)).encode()
        ds = sparkles.Dataset(sys.argv[2])
        print(ds.load(io.BytesIO(data), "nt", parse_mode="buffered", auto_buffer_bytes=int(sys.argv[1])))
        """
    spool = tmp_path / "spool"
    spool.mkdir()
    # within the allowance, nothing touches the temporary directory
    out = run_child(script, str(1 << 20), str(tmp_path / "a"), env={"TMPDIR": str(tmp_path / "missing")})
    assert out.returncode == 0, out.stderr
    assert out.stdout.split() == ["5000"]
    # past it, the stream goes to an unnamed temporary file
    out = run_child(script, "1024", str(tmp_path / "b"), env={"TMPDIR": str(tmp_path / "missing")})
    assert out.returncode != 0
    out = run_child(script, "1024", str(tmp_path / "c"), env={"TMPDIR": str(spool)})
    assert out.returncode == 0, out.stderr
    assert out.stdout.split() == ["5000"]
    assert list(spool.iterdir()) == []


def test_an_interrupt_in_the_first_read_of_a_cold_process_commits_nothing() -> None:
    """The first file-object load of a process starts the signal watcher, which imports
    `signal`. An interrupt from the reader's first read must not be lost to that."""
    out = run_child(
        """
        import _thread, io
        import sparkles

        class First(io.RawIOBase):
            def __init__(self, data):
                self.data, self.pos, self.reads = data, 0, 0
            def readable(self):
                return True
            def read(self, n=-1):
                self.reads += 1
                if self.reads == 1:
                    _thread.interrupt_main()
                chunk = self.data[self.pos : self.pos + n]
                self.pos += len(chunk)
                return chunk

        ds = sparkles.Dataset()
        ds.load('<http://e/a> <http://e/b> <http://e/c> .', "nt")
        try:
            ds.load(First(b'<http://e/x> <http://e/y> <http://e/z> .\\n'), "nt")
            print("committed", len(ds))
        except KeyboardInterrupt:
            print("interrupted", len(ds))
        """,
        timeout=60,
    )
    assert out.returncode == 0, out.stderr
    assert out.stdout.split() == ["interrupted", "1"]


def test_an_interrupt_in_the_last_compressed_read_commits_nothing(tmp_path: Path) -> None:
    """A decoder that ends without a further raw read gives the reader no end-of-stream
    read, so the store's check before publication must run the handlers."""
    data = nt(2000)
    ds = Dataset()
    ds.load(data, "nt")
    packed = {}
    for codec, ext in [("gzip", "gz"), ("brotli", "br"), ("zstd", "zst")]:
        path = tmp_path / f"d.nt.{ext}"
        ds.dump(path, format="nt")
        packed[codec] = path.read_bytes()
    out = run_child(
        """
        import _thread, io, sys
        import sparkles
        packed = %s

        class InterruptAtLastData(io.RawIOBase):
            def __init__(self, data):
                self.data, self.pos = data, 0
            def readable(self):
                return True
            def read(self, n=-1):
                chunk = self.data[self.pos : self.pos + min(n, 1000)]
                self.pos += len(chunk)
                if chunk and self.pos == len(self.data):
                    _thread.interrupt_main()
                return chunk

        for codec, raw in packed.items():
            for mode in ["auto", "streaming", "buffered"]:
                for kind in ["load", "replace"]:
                    ds = sparkles.Dataset()
                    ds.load('<http://e/a> <http://e/b> <http://e/c> .', "nt")
                    try:
                        src = InterruptAtLastData(raw)
                        # Brotli has no magic bytes to sniff
                        if kind == "load":
                            ds.load(src, "nt", parse_mode=mode, compression=codec)
                        else:
                            ds.replace(src, format="nt", compression=codec)
                        print(codec, mode, kind, "committed", len(ds))
                    except KeyboardInterrupt:
                        print(codec, mode, kind, "interrupted", len(ds))
        """
        % repr(packed),
        timeout=120,
    )
    assert out.returncode == 0, out.stderr
    lines = out.stdout.splitlines()
    assert len(lines) == 18, lines
    assert all(line.endswith("interrupted 1") for line in lines), lines


def test_a_slow_stream_stops_at_the_replacement_timeout() -> None:
    out = run_child(
        """
        import io, time
        import sparkles

        class Trickle(io.RawIOBase):
            def __init__(self):
                self.reads = 0
            def readable(self):
                return True
            def read(self, n=-1):
                self.reads += 1
                time.sleep(0.025)
                return b" "

        ds = sparkles.Dataset()
        ds.load('<http://e/a> <http://e/b> <http://e/c> .', "nt")
        src = Trickle()
        t = time.perf_counter()
        try:
            ds.replace(src, format="nt", timeout=0.03)
            print("replaced")
        except sparkles.QueryTimeoutError:
            print("timeout", time.perf_counter() - t < 0.5, src.reads < 10, len(ds))
        """,
        timeout=60,
    )
    assert out.returncode == 0, out.stderr
    assert out.stdout.split() == ["timeout", "True", "True", "1"]
