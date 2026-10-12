Native FLAC
===========

The ``zemer-flac`` crate provides a streaming decoder for native FLAC files.
It implements ``zemer_core::AudioDecoder`` and yields the shared interleaved
PCM blocks, so applications can use it without the Zemer CLI.

.. code-block:: rust

   use std::{fs::File, io::BufReader};
   use flac::FlacDecoder;
   use zemer_core::AudioDecoder;

   let mut decoder = FlacDecoder::new(BufReader::new(File::open("input.flac")?))?;
   println!("{} Hz, {} channels", decoder.spec().sample_rate(), decoder.spec().channels());
   while let Some(block) = decoder.read_frames(4096)? {
       println!("{} frames", block.frames());
   }
   # Ok::<(), zemer_core::AudioError>(())

The decoder reads STREAMINFO and Vorbis comments, handles constant, verbatim,
fixed-predictor, and linear-predictor subframes, Rice residuals, wasted bits,
and stereo decorrelation. It verifies frame header and frame CRCs. It currently
supports native FLAC streams only; Ogg-FLAC, encoding, and seeking are not
implemented. The CLI can inspect native FLAC streams and convert them to WAVE.
It rejects streams that change sample rate, channel count,
or bit depth between frames because the shared decoder interface has one fixed
``AudioSpec`` per stream. It exposes the STREAMINFO MD5 value but does not yet
verify the decoded audio against it.

When STREAMINFO does not specify a maximum frame size, a frame is limited to
16,777,215 bytes to bound decoder memory. A valid file exceeding that limit
will be rejected.
