# Generated audio fixtures

`ogg-flac-granule-trim.oga` contains a generated 512-frame, 44.1 kHz, mono
16-bit sine wave. The native FLAC stream was encoded with the host's
`libsndfile` 1.x FLAC encoder, then wrapped according to Xiph's Ogg-FLAC 1.0
mapping; the EOS granule was set to 509 and the Ogg page CRC recomputed.

`ogg-flac-granule-oracle.pcm` is the full 512-frame little-endian signed
16-bit PCM output from libsndfile decoding that native FLAC stream. Tests
compare the Ogg-FLAC decoder's 509 output frames with the corresponding oracle
prefix. Both files contain synthetic signal data and are generated for Zemer;
no third-party audio recording or implementation code is included.
