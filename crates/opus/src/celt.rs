use crate::range::{RangeDecoder, RangeError};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PostFilter {
    pub(crate) period: u16,
    pub(crate) gain: u8,
    pub(crate) tapset: u8,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct FrameHeader {
    pub(crate) silence: bool,
    pub(crate) postfilter: Option<PostFilter>,
    pub(crate) transient: bool,
    pub(crate) intra: bool,
}

pub(crate) fn decode_header(range: &mut RangeDecoder<'_>) -> Result<FrameHeader, RangeError> {
    let silence = range.decode_symbol(&[32_767, 1])? == 1;
    if silence {
        return Ok(FrameHeader {
            silence,
            postfilter: None,
            transient: false,
            intra: false,
        });
    }

    let postfilter = if range.decode_bit_logp(1)? {
        let octave = range.decode_uint(6)? as u8;
        let period = (16_u32 << octave) + range.read_raw_bits(4 + octave)? - 1;
        let gain = range.read_raw_bits(3)? as u8;
        let tapset = range.decode_symbol(&[2, 1, 1])? as u8;
        Some(PostFilter {
            period: period as u16,
            gain,
            tapset,
        })
    } else {
        None
    };
    let transient = range.decode_symbol(&[7, 1])? == 1;
    let intra = range.decode_symbol(&[7, 1])? == 1;
    Ok(FrameHeader {
        silence,
        postfilter,
        transient,
        intra,
    })
}

#[cfg(test)]
mod tests {
    use super::{FrameHeader, decode_header};
    use crate::range::RangeDecoder;

    #[test]
    fn zero_range_stream_decodes_regular_short_frame_header() {
        let mut range = RangeDecoder::new(&[0; 8]);
        assert_eq!(
            decode_header(&mut range),
            Ok(FrameHeader {
                silence: false,
                postfilter: None,
                transient: false,
                intra: false,
            })
        );
    }

    #[test]
    fn high_range_stream_decodes_the_silent_frame_flag() {
        let mut range = RangeDecoder::new(&[0xff; 8]);
        assert_eq!(
            decode_header(&mut range),
            Ok(FrameHeader {
                silence: true,
                postfilter: None,
                transient: false,
                intra: false,
            })
        );
    }

    #[test]
    fn postfilter_pitch_stays_within_the_specified_range() {
        let mut found_postfilter = false;
        for first in 0..=u8::MAX {
            for second in 0..=u8::MAX {
                let mut bytes = [0; 8];
                bytes[0] = first;
                bytes[1] = second;
                let mut range = RangeDecoder::new(&bytes);
                if let Ok(FrameHeader {
                    postfilter: Some(postfilter),
                    ..
                }) = decode_header(&mut range)
                {
                    assert!((15..=1022).contains(&postfilter.period));
                    assert!(postfilter.gain < 8);
                    assert!(postfilter.tapset < 3);
                    found_postfilter = true;
                }
            }
        }
        assert!(found_postfilter);
    }
}
