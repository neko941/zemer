Ogg container
=============

The ``zemer-ogg`` crate implements generic Ogg page and packet framing. It
validates page capture/version fields, page checksums, sequence numbers, stream
serial, and packet continuation, then exposes complete packets to codec-specific
readers. It does not decode audio by itself: FLAC, Vorbis, and Opus each define
their own packet mappings and codec rules.

The extensions ``.ogg`` and ``.oga`` are container naming conventions, not
reliable codec identifiers. Zemer identifies supported input by its contents.
At present, Ogg-FLAC decoding uses this crate; Vorbis and Opus decoding remain
planned.

.. code-block:: rust

   use std::{fs::File, io::BufReader};
   use ogg::OggPacketReader;

   let mut packets = OggPacketReader::new(BufReader::new(File::open("input.oga")?));
   while let Some(packet) = packets.next_packet()? {
       println!("packet: {} bytes, granule {}", packet.bytes.len(), packet.page_granule_position);
   }
   # Ok::<(), std::io::Error>(())

Packets are limited to 64 MiB by default. Use ``with_packet_limit`` when a
caller needs a different bound. The reader handles one logical stream and
rejects chained streams or trailing bytes after EOS; multiplexed or chained
streams require a higher-level demultiplexer.
