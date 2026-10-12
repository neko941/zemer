Command-line use
================

Zemer includes an optional ``zemer`` executable. The format crates remain
usable independently and do not depend on the CLI.

Install the CLI from the public GitHub repository with Cargo (Rust must be
installed):

.. code-block:: console

   cargo install --git https://github.com/neko941/zemer.git --branch main --locked zemer

This installs ``zemer`` in Cargo's binary directory, usually ``~/.cargo/bin``.
Alternatively, run it from a source checkout with ``cargo run -p zemer --``.

Inspect a WAVE, native FLAC, Ogg-FLAC, or WavPack file:

.. code-block:: console

   cargo run -p zemer -- inspect input.wav
   cargo run -p zemer -- inspect input.flac
   cargo run -p zemer -- inspect input.wv

Native DSD decoding/encoding is currently exposed through the library APIs and
is not yet available through the CLI.

For scripts and agents, request JSON:

.. code-block:: console

   cargo run -p zemer -- inspect --json input.wav
   cargo run -p zemer -- inspect --json input.flac
   cargo run -p zemer -- inspect --json input.wv

The JSON report contains the detected format, a WAVE container (``riff`` or
``rf64``) or ``null`` for FLAC, sample rate, channel count, optional speaker
mask, decoded sample format, frame count, exact duration as a
frame-count/sample-rate fraction, and supported metadata. It streams audio in
bounded blocks to validate and count frames. ``speaker_mask`` is a numeric WAVE
mask or JSON ``null`` when no speaker mask was supplied.

Convert WAVE, native FLAC, Ogg-FLAC, or WavPack input to a supported WAVE
sample format, native FLAC, or Ogg-FLAC:

.. code-block:: console

   cargo run -p zemer -- convert --format pcm_i16 input.wav output.wav
   cargo run -p zemer -- convert --format pcm_i16 input.flac output.wav
   cargo run -p zemer -- convert --format pcm_i16 input.wv output.wav
   cargo run -p zemer -- convert --format wavpack input.wav output.wv
   cargo run -p zemer -- convert --format wavpack --bitrate 128 input.wav output.wv
   cargo run -p zemer -- convert --format flac input.wav output.flac
   cargo run -p zemer -- convert --format ogg_flac input.wav output.oga
   cargo run -p zemer -- convert --format pcm_i16 --container rf64 input.wav large-output.wav

WAVE output defaults to classic RIFF; choose RF64 explicitly when needed.
With WavPack, ``--bitrate`` writes a hybrid ``.wv`` file and a companion
``.wvc`` correction file. JSON output reports the correction path.

The same command can explicitly select G.711 output:

.. code-block:: console

   cargo run -p zemer -- convert --format g711_alaw input.wav output-alaw.wav
   cargo run -p zemer -- convert --format g711_mulaw input.wav output-mulaw.wav

The output file must not already exist. Conversion keeps the input sample rate,
channel count, and supported RIFF INFO tags. It does not resample or remix.
Integer conversion rounds to the nearest sample without dithering. Float to
integer conversion fails for non-finite values or samples outside ``[-1, 1]``.
G.711 output accepts the same explicit format choices and converts to 16-bit
PCM before companding. Its JSON report includes ``output_encoding``. With
``--json``, failures are JSON objects on stderr with a stable ``kind`` field
and the process exits with status 2.

Metadata editing
----------------

Update RIFF INFO tags in WAVE, Vorbis comments in native FLAC, or APEv2 text
items in WavPack while preserving tags that are not named. Repeated ``--set`` options for one key
replace its existing values with the provided repeated values. ``--remove``
removes all values for a key. The output file must not already exist; encoded
audio bytes are copied without decoding or re-encoding.

.. code-block:: console

   zemer metadata edit --set "TITLE=New title" --set "ARTIST=Artist" --remove COMMENT input.flac updated.flac --json
   zemer metadata edit --set "TITLE=New title" input.wav updated.wav --json
   zemer metadata edit --set "TITLE=New title" input.wv updated.wv --json

FLAC keys may use printable ASCII other than ``=``; WAVE supports TITLE,
ARTIST, ALBUM, DATE, GENRE, and COMMENT through RIFF INFO; APEv2 keys use
printable ASCII and text values use UTF-8. WAVE edits preserve unrecognized
chunks and INFO subchunks. WavPack edits retain binary, external, and reserved
APEv2 items and any trailing ID3v1 bytes. If editing fails, the partial output
file created by the command is removed.

CUE/WAV workflows
-----------------

Split a CUE sheet that references WAVE files into numbered WAV tracks. The
destination directory must not already exist; the command also writes a CUE
sheet for the generated tracks, preserving track titles, performers, and
indexes.

.. code-block:: console

   zemer cue split --json album.cue album-tracks

Create a CUE sheet for one or more WAV tracks. File paths are stored relative
to the output CUE file, and the output file must not already exist.

.. code-block:: console

   zemer cue create --json album.cue track-01.wav track-02.wav

This workflow currently accepts UTF-8 or Shift-JIS CUE text, WAVE file entries,
and AUDIO tracks. Track boundaries must map exactly to sample frames; the
command errors rather than rounding timestamps. It reads and writes audio
through Zemer's WAVE crate.
