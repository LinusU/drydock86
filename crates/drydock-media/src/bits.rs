//! Little-endian bit reader and the prefix-code tables read with it.
//!
//! Bits are consumed starting at the least significant bit of each byte, as in
//! FFmpeg's `BITSTREAM_READER_LE`. Reads past the end of the data return zero bits
//! (FFmpeg pads its input with zeros); callers check `overrun` where it matters.

pub struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> BitReader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        BitReader { data, pos: 0 }
    }

    /// Position in bits from the start of the data.
    pub fn pos(&self) -> usize {
        self.pos
    }

    pub fn bits_left(&self) -> isize {
        (self.data.len() * 8) as isize - self.pos as isize
    }

    /// The next `n` (at most 25) bits without consuming them.
    pub fn peek(&self, n: u32) -> u32 {
        debug_assert!(n <= 25);
        let byte = self.pos >> 3;
        let mut word = 0u32;
        for i in 0..4 {
            if let Some(&b) = self.data.get(byte + i) {
                word |= (b as u32) << (8 * i);
            }
        }
        (word >> (self.pos & 7)) & ((1u32 << n) - 1)
    }

    pub fn skip(&mut self, n: usize) {
        self.pos += n;
    }

    /// Reads `n` (at most 25) bits.
    pub fn bits(&mut self, n: u32) -> u32 {
        if n == 0 {
            return 0;
        }
        let v = self.peek(n);
        self.pos += n as usize;
        v
    }

    pub fn bits32(&mut self) -> u32 {
        let lo = self.bits(16);
        let hi = self.bits(16);
        lo | (hi << 16)
    }

    pub fn bit(&mut self) -> bool {
        self.bits(1) != 0
    }

    /// Advances to the next byte boundary.
    pub fn align(&mut self) {
        self.pos = (self.pos + 7) & !7;
    }
}

/// A prefix-code lookup table for codes of at most `BITS` bits, indexed by the next
/// `BITS` stream bits. Code words are given most significant bit first: the first bit
/// read from the stream is the code's most significant bit.
pub struct Vlc {
    /// (symbol, length) per index; length 0 marks bit patterns no code starts with.
    table: Vec<(u16, u8)>,
}

pub const VLC_BITS: u32 = 13;

impl Vlc {
    /// Builds a table from (code, length) pairs, symbol = position in `codes`.
    pub fn new(codes: &[(u32, u8)]) -> Self {
        let mut table = vec![(0u16, 0u8); 1 << VLC_BITS];
        for (sym, &(code, len)) in codes.iter().enumerate() {
            let len = len as u32;
            debug_assert!((1..=VLC_BITS).contains(&len));
            // The stream holds the code's bits MSB first, read LSB first.
            let rev = code.reverse_bits() >> (32 - len);
            let step = 1usize << len;
            let mut i = rev as usize;
            while i < table.len() {
                table[i] = (sym as u16, len as u8);
                i += step;
            }
        }
        Vlc { table }
    }

    /// Decodes one symbol, or `None` for a bit pattern no code starts with (nothing
    /// is consumed then, as in FFmpeg).
    pub fn read(&self, br: &mut BitReader) -> Option<u32> {
        let (sym, len) = self.table[br.peek(VLC_BITS) as usize];
        if len == 0 {
            return None;
        }
        br.skip(len as usize);
        Some(sym as u32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_least_significant_bits_first() {
        let data = [0b1010_0110, 0xff, 0x01];
        let mut br = BitReader::new(&data);
        assert_eq!(br.bits(3), 0b110);
        assert_eq!(br.bits(5), 0b10100);
        assert_eq!(br.bits(9), 0x1ff);
        br.align();
        assert_eq!(br.pos(), 24);
        assert_eq!(br.bits(8), 0, "reads past the end return zeros");
    }

    #[test]
    fn decodes_codes_most_significant_bit_first() {
        // Codes: 0 -> "0", 1 -> "10", 2 -> "11".
        let vlc = Vlc::new(&[(0b0, 1), (0b10, 2), (0b11, 2)]);
        // Stream bits in order: 1,0 | 0 | 1,1 -> byte LSB first: 0b11001 = 0x19.
        let data = [0b0001_1001];
        let mut br = BitReader::new(&data);
        assert_eq!(vlc.read(&mut br), Some(1));
        assert_eq!(vlc.read(&mut br), Some(0));
        assert_eq!(vlc.read(&mut br), Some(2));
    }
}
