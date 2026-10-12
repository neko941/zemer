Getting started
===============

Requirements
------------

Install the stable Rust toolchain. Zemer targets Windows and Linux and uses
stable Rust 1.85 or newer.

Build and test the workspace from the repository root:

.. code-block:: console

   cargo test --workspace

Use the workspace crates
------------------------

Zemer is not published on crates.io yet. Install the command-line tool directly
from GitHub:

.. code-block:: console

   cargo install --git https://github.com/neko941/zemer.git --branch main --locked zemer-cli

To use the library crates in an application, start from a source checkout. The
following example uses ``zemer-core`` and ``wav``:

.. code-block:: rust

   use std::fs::File;
   use zemer_core::AudioDecoder;
   use wav::WavDecoder;

   fn inspect(path: &str) -> Result<(), Box<dyn std::error::Error>> {
       let mut decoder = WavDecoder::new(File::open(path)?)?;
       println!("{} Hz, {} channels", decoder.spec().sample_rate(), decoder.spec().channels());

       while let Some(block) = decoder.read_frames(4096)? {
           println!("read {} frames", block.frames());
       }
       Ok(())
   }

   # fn main() {}

The decoder yields bounded PCM blocks, so a caller can process long recordings
without loading the entire audio payload into memory. See :doc:`wav` for the
current format limits and writer example.
