# WavPack reference fixtures

The `dsd-*-stereo.wv` files were encoded with WavPack 5.6.0 from generated DSD
test signals. `dsd-adaptive-51.wv` is a generated six-channel adaptive DSD
stream. The `*.expected.dsd` files are raw native DSD output from that version's
`wvunpack -r`; each contains channel-interleaved octets, MSB-first.

`dsd-adaptive-stereo` uses the first 125 ms of Decaud's DSD64 Variatio 5 sample.
Decaud identifies the recording as Kimiko Ishizaka's *Open Goldberg Variations*,
released under CC0, and the DSD sample as public domain:
<https://www.decaud.io/dsd.html>. The direct and fast fixtures are generated
signals and contain no source audio recording.

The hybrid PCM24, PCM32, float32, and custom-rate PCM16 fixtures were encoded
from deterministic generated test signals with WavPack 5.6.0. The 5.1 PCM24/32
fixtures were made from generated six-channel WAVE_FORMAT_EXTENSIBLE inputs.
Their `.expected.*` files are the corresponding raw audio emitted by
`wvunpack`; the hybrid float oracle was decoded with its `.wvc` correction file.
`tagged-custom-rate-stereo.wv` is the custom-rate fixture with an official
WavPack APEv2 title tag appended; it verifies decoding stops at the declared
audio length instead of trying to parse trailing tags as audio blocks.
`false-stereo.wv` is a generated mono signal encoded with the WavPack
false-stereo flag; the expected file confirms the decoder duplicates the mono
payload into left and right output channels.

SHA-256:

| File | SHA-256 |
|---|---|
| `dsd-adaptive-stereo.wv` | `021A88F9162E28B71D2F54B81EF05DD399D981706B9835DD3AEF6C495B7CA9E6` |
| `dsd-adaptive-stereo.expected.dsd` | `F52897739CA4A55C9BAD6C8748C1239C9DE305ED9A6784D1CAC38F8AF2069348` |
| `dsd-fast-stereo.wv` | `081133D189E003FF4641198F025ED10C325B4E271D64BB59B2B6F9550B9B958C` |
| `dsd-fast-stereo.expected.dsd` | `0EE4DA6E0F3FB21C154ABC26E49C6C436E39789E28EDE5959F45E0722DF3448D` |
| `dsd-direct-stereo.wv` | `703F05121870D1E2FD09EC468CEF5699066625DB52A07A1CAD271DE08914FCD5` |
| `dsd-direct-stereo.expected.dsd` | `C00562C1CCCB171744B56F2A39108EA6CA89DE2865A3DC92419DF15352D8827E` |
| `dsd-adaptive-51.expected.dsd` | `FF57F9A9AD265BBEAE4F6E3D193C83CBBEBBEF5758490D8C70C73E2603B929D5` |
| `dsd-adaptive-51.wv` | `97F7CBA26B15ADE82D599D30CAE4159E5BBBE39620C3A931F4B0EFAAF4F78435` |
| `hybrid-float-stereo.expected.f32le` | `5AACBA072A65D8730FBBDC614B6EF161AC17B5796AB19966E4E5A358E35BA7F3` |
| `hybrid-float-stereo.wv` | `EDC89E28D32738D941C457EF471D56529AFD1767CD83321ED4D1D78D8D77BCDE` |
| `hybrid-float-stereo.wvc` | `86C114F2C52FB13DEAC65FFDA741CE4BD99A4E2D8257D4AD3AE83CC6A65F412F` |
| `custom-rate-stereo.expected.pcm` | `2C575968F0E24972CECB4DF7D1018B4200DA57933EC2880768E7E3F13CB02F60` |
| `custom-rate-stereo.wv` | `A164EB940794D9E23A46D032849B5365C660D0E98D069955AA7FD40205324A92` |
| `tagged-custom-rate-stereo.expected.pcm` | `2C575968F0E24972CECB4DF7D1018B4200DA57933EC2880768E7E3F13CB02F60` |
| `tagged-custom-rate-stereo.wv` | `C815B82BCFD1C02E0113A2A4B628EC46458F461CD840637C17BF7B5C4707A5D1` |
| `surround51-24.expected.pcm` | `C65809444895591061A77294FF4DDF0FCBDFBBC0ABE1AD9A261CF4D5E26A8F79` |
| `surround51-24.wv` | `EA61A0B7E61C125F567BC9D71AF5DBB7AFDAE8313DBB822F77BA0C22D6E55C8E` |
| `surround51-32.expected.pcm` | `CA53F19A873063A703A50FD20CCC3652A214958515FDAC9A637B8264E9960D7E` |
| `surround51-32.wv` | `4731B223700196AC2442617619632221FFAA7BABCC86B884AFEA2B4850FC459F` |
| `false-stereo.expected.pcm` | `8D9EE8A1665F5BF5DBDB1731BD4289F2789548AE0A6799BCFFB96B221852D01F` |
| `false-stereo.wv` | `746BC2F263073226B61C514E5AD8A2468F8CC75582DD557C15C5D84D643C00A3` |
