//! Exact telemetry for the premultiplied BGRA canvas, including alpha bytes.

const FNV_OFFSET: u64 = 0xcbf29ce484222325;
const FNV_PRIME: u64 = 0x100000001b3;
const ZERO_PIXEL_MULTIPLIER: u64 = FNV_PRIME.wrapping_pow(4);

/// Count nonzero BGRA pixels and hash their exact bytes, including premultiplied alpha.
///
/// Cursor canvases contain large transparent margins. Each zero byte leaves the
/// XOR unchanged, so a run of zero pixels can be hashed with one multiplication
/// by `FNV_PRIME^(4 * run_length)` modulo 2^64. Exponentiation by squaring avoids
/// both a byte-by-byte hash dependency chain and overflow in `4 * run_length`.
pub(crate) fn pixel_telemetry(pixels: &[u32]) -> (usize, u64) {
    let mut painted_pixels = 0;
    let mut hash = FNV_OFFSET;
    let mut index = 0;
    while index < pixels.len() {
        let pixel = pixels[index];
        index += 1;
        if pixel == 0 {
            let start = index - 1;
            while index < pixels.len() && pixels[index] == 0 {
                index += 1;
            }
            hash = hash.wrapping_mul(zero_run_multiplier(index - start));
        } else {
            painted_pixels += 1;
            for byte in pixel.to_ne_bytes() {
                hash = (hash ^ u64::from(byte)).wrapping_mul(FNV_PRIME);
            }
        }
    }
    (painted_pixels, hash)
}

fn zero_run_multiplier(mut pixels: usize) -> u64 {
    let mut factor = ZERO_PIXEL_MULTIPLIER;
    let mut multiplier = 1u64;
    while pixels != 0 {
        if pixels & 1 != 0 {
            multiplier = multiplier.wrapping_mul(factor);
        }
        pixels >>= 1;
        if pixels != 0 {
            factor = factor.wrapping_mul(factor);
        }
    }
    multiplier
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reference(pixels: &[u32]) -> (usize, u64) {
        (
            pixels.iter().filter(|pixel| **pixel != 0).count(),
            pixels
                .iter()
                .flat_map(|pixel| pixel.to_ne_bytes())
                .fold(FNV_OFFSET, |hash, byte| {
                    (hash ^ u64::from(byte)).wrapping_mul(FNV_PRIME)
                }),
        )
    }

    #[test]
    fn preserves_empty_zero_nonzero_and_alpha_byte_contracts() {
        for pixels in [
            vec![],
            vec![0],
            vec![0; 1024],
            vec![0x12345678; 1024],
            vec![0xff000000; 1024],
            vec![0, 0xff000000, 0, 0x00000001, 0, 0x00ffffff, 0],
        ] {
            assert_eq!(pixel_telemetry(&pixels), reference(&pixels));
        }
        assert_eq!(pixel_telemetry(&[]), (0, FNV_OFFSET));
        assert_eq!(pixel_telemetry(&[0xff000000]).0, 1);
    }

    #[test]
    fn preserves_zero_run_boundaries_and_wrapping_arithmetic() {
        for run in [
            1, 2, 3, 15, 16, 17, 31, 32, 33, 255, 256, 257, 1023, 1024, 1025, 65535, 65536,
        ] {
            let mut pixels = vec![0; run];
            pixels.push(0x80402010);
            pixels.extend(std::iter::repeat_n(0, run));
            pixels.push(0xff000000);
            pixels.extend(std::iter::repeat_n(0, run));
            assert_eq!(pixel_telemetry(&pixels), reference(&pixels), "run {run}");
        }
        assert_eq!(zero_run_multiplier(0), 1);
        // Exercise the full address-width exponent without allocating a canvas
        // that large (including exponents above u32 on 64-bit Windows).
        let a = usize::MAX / 2;
        let b = usize::MAX - a;
        assert_eq!(
            zero_run_multiplier(a + b),
            zero_run_multiplier(a).wrapping_mul(zero_run_multiplier(b))
        );
    }

    #[test]
    fn matches_reference_for_maximum_canvas_patterns() {
        let mut random = 0x12345678u32;
        let dense: Vec<u32> = (0..1024 * 1024)
            .map(|_| {
                random ^= random << 13;
                random ^= random >> 17;
                random ^= random << 5;
                random
            })
            .collect();
        assert_eq!(pixel_telemetry(&dense), reference(&dense));
        let sparse: Vec<u32> = dense
            .iter()
            .enumerate()
            .map(|(index, pixel)| if index % 997 == 0 { *pixel } else { 0 })
            .collect();
        assert_eq!(pixel_telemetry(&sparse), reference(&sparse));
        let mixed: Vec<u32> = dense
            .iter()
            .map(|pixel| if pixel & 3 == 0 { 0 } else { *pixel })
            .collect();
        assert_eq!(pixel_telemetry(&mixed), reference(&mixed));
        let zero = vec![0; 1024 * 1024];
        assert_eq!(pixel_telemetry(&zero), reference(&zero));
    }
}
