RIFF/WAVE
=========

Reading
-------

``WavDecoder`` accepts any ``Read + Seek`` input. It reads container chunks,
then returns decoded audio in frame blocks through ``AudioDecoder``.

.. code-block:: rust

   use std::{fs::File, io::BufReader};
   use zemer_core::AudioDecoder;
   use wav::WavDecoder;

   let input = BufReader::new(File::open("input.wav")?);
   let mut decoder = WavDecoder::new(input)?;
   while let Some(block) = decoder.read_frames(8192)? {
       // Process `block.samples()` here.
   }

Writing
-------

``WavWriter`` accepts a ``Write + Seek`` output because RIFF/RF64 sizes are
patched when writing finishes. It defaults to classic RIFF; choose RF64
explicitly for 64-bit file and data sizes. Call ``finish`` to write final sizes
and obtain the output stream; errors from finalization are returned to the
caller.

.. code-block:: rust

   use std::{fs::File, io::BufWriter};
   use zemer_core::{AudioEncoder, AudioSpec, SampleFormat};
   use wav::WavWriter;

   let spec = AudioSpec::new(48_000, 2, SampleFormat::PcmI16)?;
   let mut writer = WavWriter::new(BufWriter::new(File::create("output.wav")?), spec)?;
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

The reader selects the first ``data`` chunk, skips unknown chunks, and rejects
partial sample frames. It does not decode ADPCM, GSM, or other unsupported
codecs stored inside WAV.

Metadata rewriting
------------------

``wav::rewrite_metadata`` edits supported ``LIST/INFO`` text tags while
copying audio bytes, unknown RIFF/RF64 chunks, and unrecognized INFO subchunks
unchanged. It updates RIFF sizes and RF64 ``ds64`` size fields, including a
size-table entry for a rewritten sentinel-sized INFO chunk. It accepts a
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
size limit. The reader supports the first ``data`` chunk in both containers,
but not ``wavl``/``slnt`` audio lists or embedded metadata beyond the listed
INFO tags.
