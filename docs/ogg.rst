Ogg container
=============

The ``zemer-ogg`` crate implements generic Ogg page and packet framing. Its
reader validates page capture/version fields, page checksums, sequence numbers,
stream serials, and packet continuation. Chained streams must use distinct
serial numbers. Its writer emits bounded pages with packet granules and
BOS/EOS flags. It does not decode audio by itself: FLAC, Vorbis, and Opus each
define their own packet mappings and codec rules.

The extensions ``.ogg`` and ``.oga`` are container naming conventions, not
reliable codec identifiers. Zemer identifies supported input by its contents.
At present, Ogg-FLAC and Ogg Vorbis decoding use this crate. Ogg-FLAC is
supported; Vorbis is listed as experimental pending perceptual evaluation and
wider non-Xiph input conformance. Opus remains planned.

The experimental ``zemer-vorbis`` decoder returns Float32 PCM and reads
sequential chained logical streams, including links with different audio
specifications. Checked-in libvorbisenc/libsndfile fixtures compare complete
PCM streams for mono through 7.1 profiles at sample rates from 8 to 96 kHz.
The implementation exercises Vorbis I's 255-channel upper bound with a
synthetic decoder stream and a one-frame fixed-encoder round trip; these
boundary checks are not claims of high-channel interoperability coverage. The
`Vorbis I specification <https://xiph.org/vorbis/doc/Vorbis_I_spec.html>`_
allows up to 255 channels.

The experimental ``VorbisEncoder`` supports a fixed-precision scalar-residue
profile that omits 16-bin partitions quantized entirely to zero, and a nominal
target-bitrate profile. The bitrate profile caps each audio packet to the
corresponding rate budget after Ogg page overhead, then greedily spends bits on
the largest unweighted spectral squared-error reduction per bit. Sparse inputs
may use fewer bits, and container headers raise the average bitrate of short
files. This objective has not been perceptually validated. On a
12,347-frame 44.1-kHz stereo input, that reduced the encoded stream from
73,413 to 35,753 bytes while libsndfile produced bit-identical decoded Float32
PCM. Its 8–16-bit residue precision option changes quantizer resolution and
does not control bitrate. FFmpeg independently decodes two-second synthetic
target-rate profiles for every channel count from 1–8 plus 192-kHz stereo; ffprobe reports exact
durations, and channel-order-matched PCM has maximum per-channel RMS error
below 0.0043. These measurements verify decoding and sample output, not
perceived quality. Decoder interoperability fixtures also cover independent
libVorbis and two Rust encoder implementations against libsndfile PCM oracles.
Vorbis remains experimental until perceptual evaluation and wider external
corpus conformance are complete.

.. code-block:: rust

   use std::{fs::File, io::BufReader};
   use ogg::OggPacketReader;

   let mut packets = OggPacketReader::new(BufReader::new(File::open("input.oga")?));
   while let Some(packet) = packets.next_packet()? {
       println!("packet: {} bytes, granule {}", packet.bytes.len(), packet.page_granule_position);
   }
   # Ok::<(), std::io::Error>(())

Packets are limited to 64 MiB by default. Use ``with_packet_limit`` when a
caller needs a different bound. ``next_packet()`` stops at one logical stream's
EOS page. To read a chained stream, consume that stream and call
``next_stream()``; it validates the next BOS page and returns ``None`` only
when input ends exactly at the link boundary. A reader accepts at most 65,536
chained streams and rejects serial numbers reused by earlier links. Trailing
bytes and truncated pages are errors. The
reader does not demultiplex concurrent serial numbers.
``OggPacketWriter`` writes one logical stream and requires ``finish()`` to
mark its final page with EOS.
