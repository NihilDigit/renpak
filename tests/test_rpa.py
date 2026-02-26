"""Tests for RPA-3.0 archive read/write."""

import pytest
from pathlib import Path
from renpak.rpa import RpaReader, RpaWriter, RpaEntry


def test_roundtrip_basic(tmp_path):
    """Write files to RPA, read back, verify contents match."""
    rpa_path = tmp_path / "test.rpa"
    files = {
        "images/test.png": b"fake png data here",
        "scripts/main.rpy": b"label start:\n    pass\n",
        "audio/bgm.ogg": b"\x00" * 100,
    }

    with RpaWriter(rpa_path) as writer:
        for name, data in files.items():
            writer.add_file(name, data)

    with RpaReader(rpa_path) as reader:
        index = reader.read_index()
        assert set(index.keys()) == set(files.keys())
        for name, entry in index.items():
            data = reader.read_file(entry)
            assert data == files[name], f"Data mismatch for {name}"


def test_roundtrip_binary(tmp_path):
    """Test with random binary data."""
    import os
    rpa_path = tmp_path / "binary.rpa"
    files = {
        f"file_{i}.bin": os.urandom(1024 * (i + 1))
        for i in range(5)
    }

    with RpaWriter(rpa_path) as writer:
        for name, data in files.items():
            writer.add_file(name, data)

    with RpaReader(rpa_path) as reader:
        index = reader.read_index()
        assert len(index) == len(files)
        for name, entry in index.items():
            data = reader.read_file(entry)
            assert data == files[name]


def test_roundtrip_many_files(tmp_path):
    """Test with many small files."""
    rpa_path = tmp_path / "many.rpa"
    files = {f"dir/subdir/file_{i:04d}.txt": f"content {i}".encode() for i in range(200)}

    with RpaWriter(rpa_path) as writer:
        for name, data in files.items():
            writer.add_file(name, data)

    with RpaReader(rpa_path) as reader:
        index = reader.read_index()
        assert len(index) == 200
        # Spot check a few
        for name in ["dir/subdir/file_0000.txt", "dir/subdir/file_0100.txt", "dir/subdir/file_0199.txt"]:
            data = reader.read_file(index[name])
            assert data == files[name]


def test_roundtrip_empty_data(tmp_path):
    """Test with empty file data."""
    rpa_path = tmp_path / "empty.rpa"

    with RpaWriter(rpa_path) as writer:
        writer.add_file("empty.txt", b"")
        writer.add_file("notempty.txt", b"hello")

    with RpaReader(rpa_path) as reader:
        index = reader.read_index()
        assert reader.read_file(index["empty.txt"]) == b""
        assert reader.read_file(index["notempty.txt"]) == b"hello"


def test_header_format(tmp_path):
    """Verify the RPA header format is correct."""
    rpa_path = tmp_path / "header.rpa"

    with RpaWriter(rpa_path, key=0x42424242) as writer:
        writer.add_file("test.txt", b"hello")

    with open(rpa_path, "rb") as f:
        header = f.read(40)

    assert header.startswith(b"RPA-3.0 ")
    # Should be parseable
    offset = int(header[8:24], 16)
    key = int(header[25:33], 16)
    assert key == 0x42424242
    assert offset > 40  # index comes after header + data


def test_writer_explicit_key(tmp_path):
    """Test that explicit key is used correctly."""
    rpa_path = tmp_path / "keyed.rpa"

    with RpaWriter(rpa_path, key=0xDEADBEEF) as writer:
        writer.add_file("test.txt", b"data")

    with RpaReader(rpa_path) as reader:
        assert reader._key == 0xDEADBEEF
        index = reader.read_index()
        assert reader.read_file(index["test.txt"]) == b"data"


def test_invalid_rpa(tmp_path):
    """Test that invalid RPA files raise ValueError."""
    bad_path = tmp_path / "bad.rpa"
    bad_path.write_bytes(b"NOT-AN-RPA-FILE" + b"\x00" * 40)

    with pytest.raises(ValueError, match="Not an RPA-3.0"):
        RpaReader(bad_path)


REAL_RPA = Path("/home/spencer/Games/Eternum-0.9.5-pc/game/archive_0.09.05.rpa")

@pytest.mark.skipif(not REAL_RPA.exists(), reason="Real game RPA not found")
def test_read_real_rpa():
    """Read the real game RPA index and verify it has entries."""
    with RpaReader(REAL_RPA) as reader:
        index = reader.read_index()
        assert len(index) > 100, f"Expected many entries, got {len(index)}"
        # Verify we can read at least one file
        first_name = next(iter(index))
        first_entry = index[first_name]
        data = reader.read_file(first_entry)
        assert len(data) == first_entry.length or (first_entry.prefix and len(data) == first_entry.length + len(first_entry.prefix))
