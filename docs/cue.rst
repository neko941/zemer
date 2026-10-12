CUE sheets
==========

The ``cue`` crate exposes a CUE sheet model separately from the WAV crate.
The optional CLI connects WAVE audio to CUE track boundaries:

.. code-block:: console

   zemer cue split album.cue album-tracks
   zemer cue create album.cue track-01.wav track-02.wav

``split`` reads CUE-referenced WAVE files and writes one numbered WAVE file per
audio track, plus a CUE sheet for those outputs. It copies decoded sample
values at the original sample format and rate. Existing destination directories
and files are preserved; the command requires a new destination directory.

``create`` accepts one or more WAVE tracks and writes a CUE file whose file
references are relative to the CUE file. Track titles and performers are read
from RIFF INFO tags when present; otherwise the file name supplies the title.
The output CUE file must not already exist.

The current workflow accepts UTF-8 or Shift-JIS text, ``FILE ... WAVE``, and
``TRACK ... AUDIO`` entries. CUE positions use 75 frames per second. A position
must land exactly on a PCM sample frame; Zemer reports an error instead of
rounding. The parser does not decode non-WAV files referenced by a CUE sheet or
non-audio disc tracks.
