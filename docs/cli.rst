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

Inspect a WAVE, AIFF/AIFC, AU, native FLAC, Ogg-FLAC, DSF, DSDIFF, or WavPack file:

.. code-block:: console

   cargo run -p zemer -- inspect input.wav
   cargo run -p zemer -- inspect input.aiff
   cargo run -p zemer -- inspect input.aifc
   cargo run -p zemer -- inspect input.au
   cargo run -p zemer -- inspect input.flac
   cargo run -p zemer -- inspect input.wv
   cargo run -p zemer -- inspect input.dsf
   cargo run -p zemer -- inspect input.dff

For scripts and agents, request JSON:

.. code-block:: console

   cargo run -p zemer -- inspect --json input.wav
   cargo run -p zemer -- inspect --json input.aiff
   cargo run -p zemer -- inspect --json input.au
   cargo run -p zemer -- inspect --json input.flac
   cargo run -p zemer -- inspect --json input.wv
   cargo run -p zemer -- inspect --json input.dsf
   cargo run -p zemer -- inspect --json input.dff

The JSON report contains the detected format, a WAVE container (``riff`` or
``rf64``) or ``null`` for FLAC, sample rate, channel count, optional speaker
mask, decoded sample format, frame count, exact duration as a
frame-count/sample-rate fraction, and supported metadata. It streams audio in
bounded blocks to validate and count frames. ``speaker_mask`` is a numeric WAVE
mask or JSON ``null`` when no speaker mask was supplied.
For native DSD, JSON reports bit rate, octet-frame count, exact samples per
channel, duration as a sample-count/bit-rate fraction, and supported metadata.
Inspection streams the entire file to validate and count audio.

Convert WAVE, AIFF/AIFC, AU, native FLAC, Ogg-FLAC, WavPack, DSF, or DSDIFF input to a supported target format:

.. code-block:: console

   cargo run -p zemer -- convert --format pcm_i16 input.wav output.wav
   cargo run -p zemer -- convert --format aiff input.wav output.aiff
   cargo run -p zemer -- convert --format aifc input.wav output.aifc
   cargo run -p zemer -- convert --format au input.wav output.au
   cargo run -p zemer -- convert --format au_mulaw input.wav output-mulaw.au
   cargo run -p zemer -- convert --format aifc_alaw input.wav output-alaw.aifc
   cargo run -p zemer -- convert --format aifc_mulaw input.wav output-mulaw.aifc
   cargo run -p zemer -- convert --format aifc_ima4 input.wav output-ima4.aifc
   cargo run -p zemer -- convert --format aifc_g726 --bitrate 40 input-8khz.wav output-g726.aifc
   cargo run -p zemer -- convert --format pcm_i16 input.flac output.wav
   cargo run -p zemer -- convert --format pcm_i16 input.wv output.wav
   cargo run -p zemer -- convert --format wavpack input.wav output.wv
   cargo run -p zemer -- convert --format wavpack --bitrate 128 input.wav output.wv
   cargo run -p zemer -- convert --format vorbis --bitrate 128 input.wav output.oga
   cargo run -p zemer -- convert --format flac input.wav output.flac
   cargo run -p zemer -- convert --format ogg_flac input.wav output.oga
   cargo run -p zemer -- convert --format pcm_i16 --container rf64 input.wav large-output.wav
   cargo run -p zemer -- convert --format dsf input.wv output.dsf
   cargo run -p zemer -- convert --format dsdiff input.dsf output.dff
   cargo run -p zemer -- convert --format dsdiff_dst input.dsf output.dst.dff
   cargo run -p zemer -- convert --format wavpack input.dsf output.wv

WAVE output defaults to classic RIFF; choose RF64 explicitly when needed.
``--bitrate`` is in kbit/s and accepts 24 through 9600 for Vorbis and
WavPack, or 16, 24, 32, or 40 for AIFC G.726. G.726 requires 8 kHz input and
encodes signed 16-bit PCM. With WavPack it writes a hybrid ``.wv`` file and a companion
``.wvc`` correction file. With Vorbis it selects the experimental per-packet
target-rate mode; sparse content can use fewer bits, and file headers raise
the average bitrate of short files. The effective minimum is profile-dependent:
Vorbis rejects targets that cannot fit the selected sample rate and channel
count's mandatory packet overhead. Its allocator optimizes unweighted spectral
squared error and is not perceptually tuned. JSON reports the WavPack correction
path when one is created.
WAVE output carries ``TITLE``, ``ARTIST``, ``ALBUM``, ``DATE``, ``GENRE``, and
``COMMENT`` tags; other source comments have no WAVE INFO mapping and are
omitted.

The same command can explicitly select G.711 output in WAVE, AIFF-C, or AU:

.. code-block:: console

   cargo run -p zemer -- convert --format g711_alaw input.wav output-alaw.wav
   cargo run -p zemer -- convert --format g711_mulaw input.wav output-mulaw.wav
   cargo run -p zemer -- convert --format aifc_alaw input.wav output-alaw.aifc
   cargo run -p zemer -- convert --format aifc_mulaw input.wav output-mulaw.aifc
   cargo run -p zemer -- convert --format au_alaw input.wav output-alaw.au
   cargo run -p zemer -- convert --format au_mulaw input.wav output-mulaw.au

   cargo run -p zemer -- convert --format aifc_ima4 input.wav output-ima4.aifc
   cargo run -p zemer -- convert --format aifc_g726 --bitrate 40 input-8khz.wav output-g726.aifc

AU G.721 output is available for mono 8 kHz signed 16-bit input:

.. code-block:: console

   cargo run -p zemer -- convert --format au_g721 voice.wav voice-g721.au

The output file must not already exist. Conversion keeps the input sample rate
and channel count and carries text tags supported by both formats. It does not
resample or remix.
Integer conversion rounds to the nearest sample without dithering. Float to
integer conversion fails for non-finite values or samples outside ``[-1, 1]``.
Float input converted to native FLAC, Ogg-FLAC, or AIFF uses 24-bit integer
PCM; choose AIFC to preserve IEEE float samples.
G.711 output accepts the same explicit format choices and converts to 16-bit
PCM before companding. Its JSON report includes ``output_encoding``. With
``--json``, failures are JSON objects on stderr with a stable ``kind`` field
and the process exits with status 2.

Native DSD conversion copies packed one-bit octets directly between DSF,
DSDIFF, and native DSD WavPack streams; it does not decode through PCM.
Conversion preserves the DSD rate, channel IDs, supported text metadata, and
exact DSF sample count. DSDIFF stores complete octets and cannot represent a
partial final octet. ``--format dsdiff`` writes raw DSDIFF; ``--format
dsdiff_dst`` writes compressed DST frames when the first-order predictor
reduces the frame and uses the specified mode-0 bypass otherwise. The CLI
does not convert between DSD and PCM.

Metadata editing
----------------

Update RIFF INFO tags in WAVE, NAME/AUTH/ANNO and ID3 text in AIFF/AIFC, AU
COMMENT annotations, DSF ID3 text frames, DSDIFF title/artist/general comments,
Vorbis comments in native
FLAC or Ogg-FLAC, or APEv2 text items in WavPack while preserving tags that are
not named. DSF edits retain artwork and opaque ID3 frames and copy the DSD
payload unchanged. DSDIFF edits copy audio, markers, typed comments, and
unchanged or unknown chunks byte-for-byte. Repeated ``--set`` options
for one key replace its existing
values with the provided repeated values. ``--remove`` removes all values for a
key. The output file must not already exist. Metadata edits do not decode or
re-encode audio; Ogg-FLAC packets are repaginated while their encoded packet
bytes stay identical.

.. code-block:: console

   zemer metadata edit --set "TITLE=New title" --set "ARTIST=Artist" --remove COMMENT input.flac updated.flac --json
   zemer metadata edit --set "TITLE=New title" input.wav updated.wav --json
   zemer metadata edit --set "TITLE=New title" input.aiff updated.aiff --json
   zemer metadata edit --set "COMMENT=New note" input.au updated.au --json
   zemer metadata edit --set "TITLE=New title" input.dsf updated.dsf --json
   zemer metadata edit --set "TITLE=New title" input.dff updated.dff --json
   zemer metadata edit --set "TITLE=New title" input.wv updated.wv --json
   zemer metadata edit --set "TITLE=New title" input.oga updated.oga --json

AU maps only COMMENT into its annotation. If the comment stays unchanged, the
original annotation bytes are retained. FLAC keys may use printable ASCII other than ``=``; WAVE supports TITLE,
ARTIST, ALBUM, DATE, GENRE, and COMMENT through RIFF INFO; AIFF/AIFC native
chunks support ASCII TITLE, ARTIST, and COMMENT, while the non-standard ID3
chunk supports common UTF-8 ID3 text. DSF writes ID3v2.4 UTF-8 text. DSDIFF title, artist, and
comment text is printable ASCII. WAVE edits preserve unrecognized
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
