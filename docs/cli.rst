Command-line use
================

Zemer includes an optional ``zemer`` executable. The format crates remain
usable independently and do not depend on the CLI.

Install the CLI from the public GitHub repository with Cargo (Rust must be
installed):

.. code-block:: console

   cargo install --git https://github.com/neko941/zemer.git --branch main --package zemer-cli --locked

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
