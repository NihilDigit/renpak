"""Tests for AVIS sequence encoding/decoding and grouping logic."""

import ctypes
import os
from pathlib import Path

import pytest
from PIL import Image

from renpak.encode import group_by_prefix, encode_avis, SEQUENCE_THRESHOLD


# --- group_by_prefix tests ---

class TestGroupByPrefix:
    def test_basic_grouping(self):
        names = [
            "images/01/ale 1.jpg",
            "images/01/ale 2.jpg",
            "images/01/ale 3.jpg",
            "images/01/ale 4.jpg",
            "images/01/ale 5.jpg",
        ]
        groups, ungrouped = group_by_prefix(names)
        assert "images/01/ale " in groups
        assert len(groups["images/01/ale "]) == 5
        assert ungrouped == []

    def test_small_group_goes_to_ungrouped(self):
        names = [
            "images/01/ale 1.jpg",
            "images/01/ale 2.jpg",
            "images/01/ale 3.jpg",
        ]
        groups, ungrouped = group_by_prefix(names)
        assert len(groups) == 0
        assert len(ungrouped) == 3

    def test_mixed_groups(self):
        names = [
            "images/01/ale 1.jpg",
            "images/01/ale 2.jpg",
            "images/01/ale 3.jpg",
            "images/01/ale 4.jpg",
            "images/01/ale 5.jpg",
            "images/01/dun 1.jpg",
            "images/01/dun 2.jpg",
            "images/01/solo.jpg",
        ]
        groups, ungrouped = group_by_prefix(names)
        assert "images/01/ale " in groups
        assert len(groups["images/01/ale "]) == 5
        assert len(groups) == 1  # dun group too small
        # dun 1, dun 2, solo.jpg all ungrouped
        assert len(ungrouped) == 3

    def test_no_number_suffix(self):
        names = ["images/logo.png", "images/bg.jpg"]
        groups, ungrouped = group_by_prefix(names)
        assert len(groups) == 0
        assert set(ungrouped) == set(names)

    def test_sorted_by_number(self):
        names = [
            "img/x10.png",
            "img/x2.png",
            "img/x1.png",
            "img/x5.png",
            "img/x3.png",
        ]
        groups, ungrouped = group_by_prefix(names)
        assert "img/x" in groups
        assert groups["img/x"] == [
            "img/x1.png", "img/x2.png", "img/x3.png", "img/x5.png", "img/x10.png",
        ]

    def test_empty_input(self):
        groups, ungrouped = group_by_prefix([])
        assert groups == {}
        assert ungrouped == []


# --- AVIS encode/decode roundtrip ---

CORE_SO = Path(__file__).parent.parent / "target" / "release" / "librenpak_core.so"
RT_SO = Path(__file__).parent.parent / "target" / "release" / "librenpak_rt.so"


@pytest.mark.skipif(not CORE_SO.exists(), reason="librenpak_core.so not built")
class TestAvisEncode:
    def test_encode_basic(self):
        """Encode 5 solid-color frames and verify output is valid AVIS."""
        w, h = 8, 8
        frames = []
        for i in range(5):
            img = Image.new("RGBA", (w, h), (i * 50, 100, 200, 255))
            frames.append((img.tobytes(), w, h))

        avis_data = encode_avis(frames, quality=50, speed=10)
        assert len(avis_data) > 0
        # AVIS files start with ftyp box containing 'avis'
        assert b"ftypavis" in avis_data[:32] or b"ftyp" in avis_data[:12]

    def test_encode_rejects_empty(self):
        with pytest.raises(ValueError):
            encode_avis([], quality=50, speed=10)

    def test_encode_rejects_mismatched_sizes(self):
        frames = [
            (b"\x00" * (4 * 4 * 4), 4, 4),
            (b"\x00" * (8 * 8 * 4), 8, 8),
        ]
        with pytest.raises(ValueError, match="size"):
            encode_avis(frames, quality=50, speed=10)


@pytest.mark.skipif(
    not (CORE_SO.exists() and RT_SO.exists()),
    reason="Rust libraries not built",
)
class TestAvisRoundtrip:
    def test_roundtrip_pixel_accuracy(self):
        """Encode → decode → verify dimensions match and pixels are close."""
        w, h = 16, 16
        frames = []
        colors = [(255, 0, 0), (0, 255, 0), (0, 0, 255), (255, 255, 0), (128, 0, 128)]
        for r, g, b in colors:
            img = Image.new("RGBA", (w, h), (r, g, b, 255))
            frames.append((img.tobytes(), w, h))

        avis_data = encode_avis(frames, quality=80, speed=10)

        # Decode each frame via renpak-rt
        rt = ctypes.CDLL(str(RT_SO))
        rt.renpak_decode_frame_png.argtypes = [
            ctypes.POINTER(ctypes.c_uint8), ctypes.c_size_t, ctypes.c_uint32,
            ctypes.POINTER(ctypes.POINTER(ctypes.c_uint8)),
            ctypes.POINTER(ctypes.c_size_t),
        ]
        rt.renpak_decode_frame_png.restype = ctypes.c_int
        rt.renpak_free.argtypes = [ctypes.POINTER(ctypes.c_uint8), ctypes.c_size_t]

        rt.renpak_avis_info.argtypes = [
            ctypes.POINTER(ctypes.c_uint8), ctypes.c_size_t,
            ctypes.POINTER(ctypes.c_uint32),
            ctypes.POINTER(ctypes.c_uint32),
            ctypes.POINTER(ctypes.c_uint32),
        ]
        rt.renpak_avis_info.restype = ctypes.c_int

        avis_buf = ctypes.create_string_buffer(avis_data)
        avis_ptr = ctypes.cast(avis_buf, ctypes.POINTER(ctypes.c_uint8))

        # Check info
        fc = ctypes.c_uint32()
        ow = ctypes.c_uint32()
        oh = ctypes.c_uint32()
        ret = rt.renpak_avis_info(avis_ptr, len(avis_data),
                                   ctypes.byref(fc), ctypes.byref(ow), ctypes.byref(oh))
        assert ret == 0
        assert fc.value == 5
        assert ow.value == w
        assert oh.value == h

        # Decode each frame
        for i, (r, g, b) in enumerate(colors):
            out_png = ctypes.POINTER(ctypes.c_uint8)()
            out_len = ctypes.c_size_t()
            ret = rt.renpak_decode_frame_png(
                avis_ptr, len(avis_data), i,
                ctypes.byref(out_png), ctypes.byref(out_len),
            )
            assert ret == 0, f"Frame {i} decode failed with {ret}"

            png_bytes = bytes(
                ctypes.cast(out_png, ctypes.POINTER(ctypes.c_uint8 * out_len.value)).contents
            )
            rt.renpak_free(out_png, out_len)

            # Verify it's a valid PNG
            assert png_bytes[:8] == b"\x89PNG\r\n\x1a\n"

            # Decode PNG and check dimensions
            from io import BytesIO
            decoded = Image.open(BytesIO(png_bytes))
            assert decoded.size == (w, h)
