Command-line use
================

Zemer includes an optional ``zemer`` executable. The format crates remain
usable independently and do not depend on the CLI.

Install the CLI from the public GitHub repository with Cargo (Rust must be
installed):

.. code-block:: console

   cargo install --git https://github.com/neko941/zemer.git --branch main --locked zemer-cli

This installs ``zemer`` in Cargo's binary directory, usually ``~/.cargo/bin``.
Alternatively, run it from a source checkout with ``cargo run -p zemer-cli --``.

Inspect a WAVE file:

.. code-block:: console

   cargo run -p zemer-cli -- inspect input.wav

For scripts and agents, request JSON:

.. code-block:: console

   cargo run -p zemer-cli -- inspect --json input.wav

The JSON report contains the WAVE container (``riff`` or ``rf64``), sample rate,
channel count, optional speaker mask,
decoded sample format, frame count, exact duration as a frame-count/sample-rate
fraction, and RIFF INFO metadata. It streams audio in bounded blocks to
validate and count frames. ``speaker_mask`` is a numeric WAVE mask or JSON
``null`` when no speaker mask was supplied.
Convert between supported WAVE sample formats. Output defaults to classic
RIFF; choose RF64 explicitly when needed:

.. code-block:: console

   cargo run -p zemer-cli -- convert --format pcm_i16 input.wav output.wav
   cargo run -p zemer-cli -- convert --format pcm_i16 --container rf64 input.wav large-output.wav

The same command can explicitly select G.711 output:

.. code-block:: console

   cargo run -p zemer-cli -- convert --format g711_alaw input.wav output-alaw.wav
   cargo run -p zemer-cli -- convert --format g711_mulaw input.wav output-mulaw.wav

The output file must not already exist. Conversion keeps the input sample rate,
channel count, and supported RIFF INFO tags. It does not resample or remix.
Integer conversion rounds to the nearest sample without dithering. Float to
integer conversion fails for non-finite values or samples outside ``[-1, 1]``.
G.711 output accepts the same explicit format choices and converts to 16-bit
PCM before companding. Its JSON report includes ``output_encoding``. With
``--json``, failures are JSON objects on stderr with a stable ``kind`` field
and the process exits with status 2.

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
