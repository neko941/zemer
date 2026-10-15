RIFF/WAVE
=========

Reading
-------

``WavDecoder`` accepts any ``Read + Seek`` input. It reads container chunks,
then returns decoded audio in frame blocks through ``AudioDecoder``.
The typed ``bext`` accessor exposes the fixed Broadcast Wave fields as their
original bytes, preserving non-UTF-8 values and versioned reserved data.
``cue()`` exposes WAVE cue-point records and ``sampler()`` exposes sampler
header fields, sample-loop records, and opaque sampler-specific bytes. Each
typed chunk is limited to 16 MiB; duplicate cue names and sample-loop
identifiers are rejected.

.. code-block:: rust

   use std::{fs::File, io::BufReader};
   use zemer_core::AudioDecoder;
   use wav::WavDecoder;

   let input = BufReader::new(File::open("input.wav")?);
   let mut decoder = WavDecoder::new(input)?;
   while let Some(block) = decoder.read_frames(8192)? {
       // Process `block.samples()` here.
   }
   if let Some(bext) = decoder.bext() {
       let time_reference = bext.time_reference;
       let coding_history = &bext.coding_history;
   }
   if let Some(cue) = decoder.cue() {
       for point in &cue.points {
           let sample = point.sample_offset;
       }
   }
   if let Some(sampler) = decoder.sampler() {
       for sample_loop in &sampler.loops {
           let loop_start = sample_loop.start;
       }
   }

Writing
-------

``WavWriter`` accepts a ``Write + Seek`` output because RIFF/RF64 sizes are
patched when writing finishes. It defaults to classic RIFF; choose RF64
explicitly for 64-bit file and data sizes. Call ``finish`` to write final sizes
and obtain the output stream; errors from finalization are returned to the
caller.

Float and G.711 output includes the non-PCM ``fmt`` extension and ``fact``
sample count required by WAVE. The count is per channel; RF64 output uses the
32-bit sentinel when it exceeds that field's range and retains the full count
in ``ds64``.

.. code-block:: rust

   use std::{fs::File, io::BufWriter};
   use zemer_core::{AudioEncoder, AudioSpec, SampleFormat};
   use wav::WavWriter;

   let spec = AudioSpec::new(48_000, 2, SampleFormat::PcmI16)?;
   let mut writer = WavWriter::new(BufWriter::new(File::create("output.wav")?), spec)?;
   // Supply matching PcmBuffer blocks with writer.write_block(&block)?;
   writer.finish()?;

Set ``WavWriter::set_cue`` and ``set_sampler`` before finishing to write these
chunks after audio data. The chunk fields are preserved as supplied; cue
records use the format's 32-bit positions and offsets.

To create a Broadcast Wave file, provide a ``BextChunk`` when constructing the
writer. Its 602-byte fixed record, versioned reserved bytes, and variable coding
history are written before ``fmt`` (after ``ds64`` for RF64):

.. code-block:: rust

   use std::{fs::File, io::BufWriter};
   use zemer_core::{AudioEncoder, AudioSpec, SampleFormat};
   use wav::{BextChunk, WavWriter};

   let spec = AudioSpec::new(48_000, 2, SampleFormat::PcmI16)?;
   let mut bext = BextChunk::default();
   bext.description[..11].copy_from_slice(b"Live master");
   bext.version = 2;
   let mut writer = WavWriter::new_with_bext(
       BufWriter::new(File::create("broadcast.wav")?),
       spec,
       &bext,
   )?;
   // Supply matching PcmBuffer blocks with writer.write_block(&block)?;
   writer.finish()?;

For large files, select RF64:

.. code-block:: rust

   use std::{fs::File, io::BufWriter};
   use zemer_core::{AudioEncoder, AudioSpec, SampleFormat};
   use wav::{WavContainer, WavWriter};

   let spec = AudioSpec::new(48_000, 2, SampleFormat::PcmI16)?;
   let mut writer = WavWriter::new_with_container(
       BufWriter::new(File::create("large-output.wav")?),
       spec,
       WavContainer::Rf64,
   )?;
   // Supply matching PcmBuffer blocks.
   writer.finish()?;

Supported samples
-----------------

* Integer PCM: unsigned 8-bit, signed 16-bit, signed 24-bit, and signed 32-bit.
* IEEE float: 32-bit and 64-bit.
* G.711 A-law and μ-law decode to signed 16-bit PCM. ``WavWriter::new_g711``
  explicitly encodes signed 16-bit PCM blocks as either G.711 law; the WAVE
  header stores the corresponding 8-bit companded format.
* ``WAVE_FORMAT_EXTENSIBLE`` PCM with 1 through container-width valid bits, and
  float when valid bits match container bits. Reduced-valid-bit PCM remains
  left-aligned in its original container-width sample values. The WAVE channel
  mask is exposed through ``AudioSpec::speaker_mask`` as an optional raw ``u32``
  and preserved without inferring speaker positions or requiring a particular
  number of set bits.
* Read and write common text tags in RIFF ``LIST/INFO`` chunks: title, artist,
  album, date, genre, and comment. Other metadata keys are rejected by the WAV
  writer rather than silently discarded.
* Read and write typed ``cue `` cue-point and ``smpl`` sampler-loop chunks,
  preserving their fields and sampler-specific data bytes.

The reader concatenates top-level ``data`` chunks and ``LIST/wavl`` child
segments in file order. A ``wavl`` list may contain ``data`` sample bytes and
``slnt`` silent-frame counts; silence repeats the preceding interleaved sample
frame so transitions do not introduce clicks. If a file starts with ``slnt``,
the reader uses neutral PCM or G.711 sample codes. Segment reads remain bounded
by the caller's requested frame count, and at most one million audio segments
are retained while parsing. Unknown RIFF chunks and unknown ``wavl`` children
are skipped. Partial PCM sample frames and malformed ``slnt`` chunks are
rejected. The writer emits one conventional contiguous ``data`` chunk.
The reader does not decode ADPCM, GSM, or other unsupported codecs stored
inside WAV.

Metadata rewriting
------------------

``wav::rewrite_metadata`` edits supported ``LIST/INFO`` text tags while
copying audio bytes, unknown RIFF/RF64 chunks, and unrecognized INFO subchunks
unchanged. It preserves ``bext``, ``cue ``, and ``smpl`` chunks unchanged. It updates RIFF sizes and
RF64 ``ds64`` size fields, including a size-table entry for a rewritten
sentinel-sized INFO chunk. It accepts a
seekable input and output; callers should write to a new temporary file and
replace the original only after success. Supported keys are title, artist,
album, date, genre, and comment.

.. code-block:: rust

   use std::{fs::File, io::BufWriter};
   use zemer_core::Metadata;
   use wav::rewrite_metadata;

   let mut tags = Metadata::default();
   tags.insert("TITLE", "Updated title");
   rewrite_metadata(
       File::open("input.wav")?,
       BufWriter::new(File::create("updated.wav")?),
       &tags,
   )?;

Limits
------

The reader requires a seekable input and the writer requires a seekable output.
The writer produces classic RIFF/WAVE by default and ``WAVE_FORMAT_EXTENSIBLE``
when a channel mask is specified. RF64 output must be selected explicitly with
``WavContainer::Rf64``. Classic RIFF output errors before exceeding the 4 GiB
size limit. RIFF and RF64 readers support ``data`` and ``wavl``/``slnt`` audio
segments. The writer does not create segmented ``wavl`` audio; it writes a
contiguous ``data`` chunk. Each supported INFO value is limited to 1 MiB; the
reader retains at most 16 MiB and 65,536 supported INFO values across the file.
The first ``bext`` chunk is parsed and bounded to 16 MiB; its fixed fields are
602 bytes followed by coding history. A single ``cue `` or ``smpl`` chunk is
parsed; each is bounded to 16 MiB. Cue records have 32-bit fields and do not
provide RF64-scale marker positions. Writers emit at most one of each typed
chunk. Other embedded metadata chunks are not interpreted.
