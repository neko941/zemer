use zemer_core::AudioError;

use crate::{VorbisCodebook, VorbisResidue, bitstream::BitReader};

/// Adds one configured residue bundle to the supplied zeroed spectral vectors.
pub(crate) fn decode_residue(
    residue: &VorbisResidue,
    codebooks: &[VorbisCodebook],
    bits: &mut BitReader<'_>,
    vectors: &mut [Vec<f64>],
    do_not_decode: &[bool],
) -> Result<(), AudioError> {
    if vectors.len() != do_not_decode.len() {
        return Err(AudioError::InvalidData(
            "Vorbis residue channel flag count is invalid",
        ));
    }
    let Some(vector_size) = vectors.first().map(Vec::len) else {
        return Ok(());
    };
    if vectors.iter().any(|vector| vector.len() != vector_size) {
        return Err(AudioError::InvalidData(
            "Vorbis residue vectors have different lengths",
        ));
    }
    if vector_size > 4096 || residue.partition_size == 0 || residue.cascades.is_empty() {
        return Err(AudioError::InvalidData("invalid Vorbis residue bounds"));
    }
    if residue.kind > 2 {
        return Err(AudioError::InvalidData("invalid Vorbis residue type"));
    }
    if residue.kind != 2 {
        return decode_partitioned(
            residue,
            codebooks,
            bits,
            vectors,
            do_not_decode,
            residue.kind == 0,
        );
    }
    if do_not_decode.iter().all(|&skip| skip) {
        return Ok(());
    }

    let total_size = vector_size
        .checked_mul(vectors.len())
        .ok_or(AudioError::InvalidData(
            "Vorbis residue vector size overflow",
        ))?;
    if total_size > 1_048_576 {
        return Err(AudioError::InvalidData(
            "Vorbis interleaved residue exceeds 1048576 values",
        ));
    }
    let mut interleaved = Vec::new();
    interleaved
        .try_reserve_exact(total_size)
        .map_err(|_| AudioError::InvalidData("Vorbis residue allocation failed"))?;
    for bin in 0..vector_size {
        for vector in vectors.iter() {
            interleaved.push(vector[bin]);
        }
    }
    decode_partitioned(
        residue,
        codebooks,
        bits,
        std::slice::from_mut(&mut interleaved),
        &[false],
        true,
    )?;
    for (flat_index, value) in interleaved.into_iter().enumerate() {
        let channel = flat_index % vectors.len();
        let bin = flat_index / vectors.len();
        vectors[channel][bin] = value;
    }
    Ok(())
}

fn decode_partitioned(
    residue: &VorbisResidue,
    codebooks: &[VorbisCodebook],
    bits: &mut BitReader<'_>,
    vectors: &mut [Vec<f64>],
    do_not_decode: &[bool],
    interleave_type_zero: bool,
) -> Result<(), AudioError> {
    let actual_size = vectors.first().map_or(0, Vec::len);
    let begin = usize::try_from(residue.begin)
        .unwrap_or(usize::MAX)
        .min(actual_size);
    let end = usize::try_from(residue.end)
        .unwrap_or(usize::MAX)
        .min(actual_size);
    if end <= begin {
        return Ok(());
    }
    let partition_size = usize::try_from(residue.partition_size)
        .map_err(|_| AudioError::InvalidData("Vorbis residue partition size overflow"))?;
    let partitions = (end - begin) / partition_size;
    if partitions == 0 {
        return Ok(());
    }
    let classbook =
        codebooks
            .get(usize::from(residue.classbook))
            .ok_or(AudioError::InvalidFormat(
                "invalid Vorbis residue classbook",
            ))?;
    let classword_dimensions = usize::from(classbook.dimensions);
    if classword_dimensions == 0 || residue.cascades.len() > 64 {
        return Err(AudioError::InvalidFormat(
            "invalid Vorbis residue classifications",
        ));
    }
    let mut classifications = Vec::new();
    classifications
        .try_reserve_exact(vectors.len())
        .map_err(|_| AudioError::InvalidData("Vorbis residue allocation failed"))?;
    for _ in vectors.iter() {
        let mut classes = Vec::new();
        classes
            .try_reserve_exact(partitions)
            .map_err(|_| AudioError::InvalidData("Vorbis residue allocation failed"))?;
        classes.resize(partitions, 0_u8);
        classifications.push(classes);
    }

    let mut scratch = Vec::new();
    for pass in 0..8 {
        let mut partition_count = 0;
        while partition_count < partitions {
            if pass == 0 {
                for vector_index in 0..vectors.len() {
                    if do_not_decode[vector_index] {
                        continue;
                    }
                    let mut classword = match classbook.huffman.decode_entry(bits) {
                        Ok(value) => value,
                        Err(AudioError::InvalidFormat("truncated Vorbis packet")) => return Ok(()),
                        Err(error) => return Err(error),
                    };
                    for digit in (0..classword_dimensions).rev() {
                        let class = (classword % residue.cascades.len() as u32) as u8;
                        classword /= residue.cascades.len() as u32;
                        if partition_count + digit < partitions {
                            classifications[vector_index][partition_count + digit] = class;
                        }
                    }
                    if classword != 0 {
                        return Err(AudioError::InvalidFormat(
                            "Vorbis residue classword exceeds its dimensions",
                        ));
                    }
                }
            }

            for _ in 0..classword_dimensions {
                if partition_count == partitions {
                    break;
                }
                for vector_index in 0..vectors.len() {
                    if do_not_decode[vector_index] {
                        continue;
                    }
                    let class = usize::from(classifications[vector_index][partition_count]);
                    let book_index = residue.cascades[class][pass];
                    if let Some(book_index) = book_index {
                        let book = codebooks.get(usize::from(book_index)).ok_or(
                            AudioError::InvalidFormat("invalid Vorbis residue value codebook"),
                        )?;
                        if book.dimensions == 0
                            || partition_size % usize::from(book.dimensions) != 0
                        {
                            return Err(AudioError::InvalidFormat(
                                "Vorbis residue partition is not divisible by its codebook dimension",
                            ));
                        }
                        if !decode_partition(
                            book,
                            bits,
                            &mut vectors[vector_index],
                            begin + partition_count * partition_size,
                            partition_size,
                            interleave_type_zero && residue.kind == 0,
                            &mut scratch,
                        )? {
                            return Ok(());
                        }
                    }
                }
                partition_count += 1;
            }
        }
    }
    Ok(())
}

fn decode_partition(
    book: &VorbisCodebook,
    bits: &mut BitReader<'_>,
    output: &mut [f64],
    offset: usize,
    partition_size: usize,
    interleave_type_zero: bool,
    scratch: &mut Vec<f64>,
) -> Result<bool, AudioError> {
    let dimensions = usize::from(book.dimensions);
    if dimensions == 0 || partition_size % dimensions != 0 {
        return Err(AudioError::InvalidFormat(
            "Vorbis residue partition is not divisible by its codebook dimension",
        ));
    }
    if scratch.len() != dimensions {
        scratch
            .try_reserve_exact(dimensions.saturating_sub(scratch.len()))
            .map_err(|_| AudioError::InvalidData("Vorbis residue allocation failed"))?;
        scratch.resize(dimensions, 0.0);
    }
    if interleave_type_zero {
        let step = partition_size / dimensions;
        for index in 0..step {
            if !decode_vector(book, bits, scratch)? {
                return Ok(false);
            }
            for (dimension, &value) in scratch.iter().enumerate() {
                accumulate(output, offset + index + dimension * step, value)?;
            }
        }
    } else {
        for index in (0..partition_size).step_by(dimensions) {
            if !decode_vector(book, bits, scratch)? {
                return Ok(false);
            }
            for (dimension, &value) in scratch.iter().enumerate() {
                accumulate(output, offset + index + dimension, value)?;
            }
        }
    }
    Ok(true)
}

fn accumulate(output: &mut [f64], index: usize, value: f64) -> Result<(), AudioError> {
    let sum = output[index] + value;
    if !sum.is_finite() {
        return Err(AudioError::InvalidData("non-finite Vorbis residue value"));
    }
    output[index] = sum;
    Ok(())
}

fn decode_vector(
    book: &VorbisCodebook,
    bits: &mut BitReader<'_>,
    output: &mut [f64],
) -> Result<bool, AudioError> {
    let entry = match book.huffman.decode_entry(bits) {
        Ok(entry) => entry,
        Err(AudioError::InvalidFormat("truncated Vorbis packet")) => return Ok(false),
        Err(error) => return Err(error),
    };
    book.lookup_vector(entry, output)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use crate::{
        VorbisLookup,
        codebook::{AllocationBudget, HuffmanCodebook},
    };

    use super::decode_residue;
    use crate::{VorbisCodebook, VorbisResidue, bitstream::BitReader};

    fn codebook(
        dimensions: u16,
        entries: u32,
        lookup: VorbisLookup,
        lengths: &[u8],
    ) -> VorbisCodebook {
        VorbisCodebook {
            dimensions,
            entries,
            codeword_lengths: lengths.to_vec(),
            lookup,
            huffman: HuffmanCodebook::new(lengths, &mut AllocationBudget::new()).unwrap(),
        }
    }

    fn books() -> Vec<VorbisCodebook> {
        vec![
            codebook(
                1,
                1,
                VorbisLookup {
                    kind: 0,
                    minimum: 0,
                    delta: 0,
                    value_bits: 0,
                    sequence: false,
                    values: 0,
                    multiplicands: Vec::new(),
                },
                &[1],
            ),
            codebook(
                2,
                1,
                VorbisLookup {
                    kind: 2,
                    minimum: 0,
                    delta: (788 << 21) | 1,
                    value_bits: 2,
                    sequence: false,
                    values: 2,
                    multiplicands: vec![1, 2],
                },
                &[1],
            ),
            codebook(
                2,
                2,
                VorbisLookup {
                    kind: 2,
                    minimum: 0,
                    delta: (788 << 21) | 1,
                    value_bits: 2,
                    sequence: false,
                    values: 4,
                    multiplicands: vec![1, 2, 3, 4],
                },
                &[1, 1],
            ),
        ]
    }

    fn residue(kind: u16) -> VorbisResidue {
        VorbisResidue {
            kind,
            begin: 0,
            end: 4,
            partition_size: 4,
            classbook: 0,
            cascades: vec![[Some(1), None, None, None, None, None, None, None]],
        }
    }

    #[test]
    fn decodes_residue_zero_one_and_interleaved_two() {
        let codebooks = books();
        let mut type_zero = vec![vec![0.0; 4]];
        decode_residue(
            &residue(0),
            &codebooks,
            &mut BitReader::new(&[0]),
            &mut type_zero,
            &[false],
        )
        .unwrap();
        assert_eq!(type_zero[0], [1.0, 1.0, 2.0, 2.0]);

        let mut type_one = vec![vec![0.0; 4]];
        decode_residue(
            &residue(1),
            &codebooks,
            &mut BitReader::new(&[0]),
            &mut type_one,
            &[false],
        )
        .unwrap();
        assert_eq!(type_one[0], [1.0, 2.0, 1.0, 2.0]);

        let mut type_two = vec![vec![0.0; 2], vec![0.0; 2]];
        decode_residue(
            &residue(2),
            &codebooks,
            &mut BitReader::new(&[0, 0]),
            &mut type_two,
            &[false, false],
        )
        .unwrap();
        assert_eq!(type_two, [[1.0, 1.0], [2.0, 2.0]]);

        let mut type_two_partial_skip = vec![vec![0.0; 2], vec![0.0; 2]];
        decode_residue(
            &residue(2),
            &codebooks,
            &mut BitReader::new(&[0, 0]),
            &mut type_two_partial_skip,
            &[true, false],
        )
        .unwrap();
        assert_eq!(type_two_partial_skip, [[1.0, 1.0], [2.0, 2.0]]);
    }

    #[test]
    fn skips_flagged_vectors_and_stops_cleanly_on_truncated_residue() {
        let codebooks = books();
        let mut vectors = vec![vec![0.0; 4], vec![0.0; 4]];
        decode_residue(
            &residue(1),
            &codebooks,
            &mut BitReader::new(&[0]),
            &mut vectors,
            &[false, true],
        )
        .unwrap();
        assert_eq!(vectors, [[1.0, 2.0, 1.0, 2.0], [0.0; 4]]);

        let mut vectors = vec![vec![0.0; 4]];
        let mut truncated = residue(1);
        truncated.cascades[0][0] = Some(2);
        decode_residue(
            &truncated,
            &codebooks,
            &mut BitReader::new(&[]),
            &mut vectors,
            &[false],
        )
        .unwrap();
        assert_eq!(vectors, [vec![0.0; 4]]);
    }
}
