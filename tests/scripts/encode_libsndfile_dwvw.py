#!/usr/bin/env python3
"""Encode mono signed 32-bit PCM to AIFC DWVW with libsndfile."""

import ctypes
import ctypes.util
import os
import struct
import sys


class SfInfo(ctypes.Structure):
    _fields_ = [
        ("frames", ctypes.c_longlong),
        ("samplerate", ctypes.c_int),
        ("channels", ctypes.c_int),
        ("format", ctypes.c_int),
        ("sections", ctypes.c_int),
        ("seekable", ctypes.c_int),
    ]


def main():
    raw_path, output_path, bit_depth = sys.argv[1:]
    bit_depth = int(bit_depth)
    library_path = os.environ.get("ZEMER_SNDFILE_LIBRARY") or ctypes.util.find_library("sndfile")
    if not library_path:
        raise SystemExit("libsndfile was not found; set ZEMER_SNDFILE_LIBRARY")

    sndfile = ctypes.CDLL(library_path)
    sndfile.sf_open.argtypes = [ctypes.c_char_p, ctypes.c_int, ctypes.POINTER(SfInfo)]
    sndfile.sf_open.restype = ctypes.c_void_p
    sndfile.sf_writef_int.argtypes = [ctypes.c_void_p, ctypes.POINTER(ctypes.c_int), ctypes.c_longlong]
    sndfile.sf_writef_int.restype = ctypes.c_longlong
    sndfile.sf_close.argtypes = [ctypes.c_void_p]
    sndfile.sf_close.restype = ctypes.c_int

    raw = open(raw_path, "rb").read()
    if len(raw) % 4:
        raise SystemExit("signed 32-bit PCM input has a partial sample")
    frames = len(raw) // 4
    samples = (ctypes.c_int * frames)(*struct.unpack(f"<{frames}i", raw))
    info = SfInfo(frames, 44_100, 1, 0x20000 | {16: 0x41, 24: 0x42}[bit_depth], 0, 0)
    handle = sndfile.sf_open(os.fsencode(output_path), 0x20, ctypes.byref(info))
    if not handle:
        raise SystemExit("libsndfile could not create the mono AIFC DWVW oracle")
    try:
        written = sndfile.sf_writef_int(handle, samples, frames)
        if written != frames:
            raise SystemExit(f"libsndfile wrote {written} of {frames} frames")
    finally:
        if sndfile.sf_close(handle) != 0:
            raise SystemExit("libsndfile reported an error while closing the oracle")


if __name__ == "__main__":
    main()
