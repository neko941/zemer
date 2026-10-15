#!/usr/bin/env python3
"""Decode one audio file to signed 32-bit little-endian PCM with libsndfile."""

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


library_path = os.environ.get("ZEMER_SNDFILE_LIBRARY") or ctypes.util.find_library("sndfile")
if not library_path:
    raise SystemExit("libsndfile was not found; set ZEMER_SNDFILE_LIBRARY")
sndfile = ctypes.CDLL(library_path)
sndfile.sf_open.argtypes = [ctypes.c_char_p, ctypes.c_int, ctypes.POINTER(SfInfo)]
sndfile.sf_open.restype = ctypes.c_void_p
sndfile.sf_readf_int.argtypes = [ctypes.c_void_p, ctypes.POINTER(ctypes.c_int), ctypes.c_longlong]
sndfile.sf_readf_int.restype = ctypes.c_longlong
sndfile.sf_close.argtypes = [ctypes.c_void_p]
sndfile.sf_close.restype = ctypes.c_int

info = SfInfo()
handle = sndfile.sf_open(os.fsencode(sys.argv[1]), 0x10, ctypes.byref(info))
if not handle:
    raise SystemExit("libsndfile could not open the audio file")
try:
    count = info.frames * info.channels
    samples = (ctypes.c_int * count)()
    frames_read = sndfile.sf_readf_int(handle, samples, info.frames)
    if frames_read != info.frames:
        raise SystemExit(f"libsndfile decoded {frames_read} of {info.frames} frames")
    sys.stdout.buffer.write(struct.pack(f"<{count}i", *samples))
finally:
    if sndfile.sf_close(handle) != 0:
        raise SystemExit("libsndfile reported an error while closing the file")
