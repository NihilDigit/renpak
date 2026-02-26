from dataclasses import dataclass
from pathlib import Path
import pickle
import zlib
import random


@dataclass
class RpaEntry:
    name: str
    offset: int
    length: int
    prefix: bytes


class RpaReader:
    """Read RPA-3.0 archives."""

    def __init__(self, path: Path):
        self._path = Path(path)
        self._file = open(self._path, 'rb')
        self._parse_header()

    def _parse_header(self):
        header = self._file.read(40)
        if not header.startswith(b'RPA-3.0 '):
            raise ValueError(f"Not an RPA-3.0 archive: {self._path}")
        self._index_offset = int(header[8:24], 16)
        self._key = int(header[25:33], 16)

    def read_index(self) -> dict[str, RpaEntry]:
        self._file.seek(self._index_offset)
        raw = pickle.loads(zlib.decompress(self._file.read()))
        entries = {}
        for name, tuples in raw.items():
            t = tuples[0]
            if len(t) == 2:
                offset, length = t
                prefix = b''
            else:
                offset, length, prefix = t
                if prefix is None:
                    prefix = b''
                elif not isinstance(prefix, bytes):
                    prefix = prefix.encode('latin-1')
            offset ^= self._key
            length ^= self._key
            entries[name] = RpaEntry(name=name, offset=offset, length=length, prefix=prefix)
        return entries

    def read_file(self, entry: RpaEntry) -> bytes:
        self._file.seek(entry.offset)
        data = self._file.read(entry.length)
        if entry.prefix:
            data = entry.prefix + data
        return data

    def close(self):
        self._file.close()

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        self.close()


class RpaWriter:
    """Write RPA-3.0 archives."""

    def __init__(self, path: Path, key: int = None):
        self._path = Path(path)
        self._path.parent.mkdir(parents=True, exist_ok=True)
        self._key = key if key is not None else random.randint(0, 0xFFFFFFFF)
        self._file = open(self._path, 'wb')
        self._file.write(b'\x00' * 40)
        self._entries = {}
        self._finished = False

    def add_file(self, name: str, data: bytes):
        offset = self._file.tell()
        self._file.write(data)
        self._entries[name] = (offset, len(data))

    def finish(self):
        if self._finished:
            return
        self._finished = True
        index_offset = self._file.tell()
        index = {}
        for name, (offset, length) in self._entries.items():
            index[name] = [(offset ^ self._key, length ^ self._key, b'')]
        index_data = zlib.compress(pickle.dumps(index, 2))
        self._file.write(index_data)
        self._file.seek(0)
        header = f"RPA-3.0 {index_offset:016x} {self._key:08x}\n".encode('ascii')
        header = header + b'\x00' * (40 - len(header))
        self._file.write(header[:40])
        self._file.close()

    def close(self):
        if not self._file.closed:
            self.finish()

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        self.close()
