Ogg container
=============

The ``zemer-ogg`` crate implements generic Ogg page and packet framing. Its
reader validates page capture/version fields, page checksums, sequence numbers,
stream serial, and packet continuation. Its writer emits bounded pages with
packet granules and BOS/EOS flags. It does not decode audio by itself: FLAC,
Vorbis, and Opus each define their own packet mappings and codec rules.

The extensions ``.ogg`` and ``.oga`` are container naming conventions, not
reliable codec identifiers. Zemer identifies supported input by its contents.
At present, Ogg-FLAC encoding and decoding use this crate; Vorbis and Opus
decoding remain planned.

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
when input ends exactly at the link boundary. Trailing bytes and truncated
pages are errors. The reader does not demultiplex concurrent serial numbers.
``OggPacketWriter`` writes one logical stream and requires ``finish()`` to
mark its final page with EOS.
