//! Entropy-coded data for JPEG admission (decision 0045, rule 3): canonical Huffman
//! tables, and the bit reader that the scan walk decodes them with.
//!
//! `BitReader` never reads ahead and never invents bits. It loads a byte only when a bit
//! is needed, so a scan that ends early is refused rather than zero-filled.

use artifree_domain::assets::AssetRejection;

/// The byte that starts every marker. Fill bytes are also `0xFF`.
pub(super) const PREFIX: u8 = 0xFF;

/// The byte that follows a `0xFF` data byte in entropy-coded data.
pub(super) const STUFF: u8 = 0x00;

/// Most symbols one table may define: one per byte value.
const MAX_SYMBOLS: usize = 256;

/// Most bits that one `read_bits` call returns. This is the longest code length.
const MAX_READ_BITS: u32 = 16;

/// A canonical Huffman table.
///
/// Each per-length array is indexed by code length, from 1 to 16, so entry 0 is unused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct HuffmanTable {
    /// The value of the first code of each length.
    first_code: [u32; 17],
    /// The number of codes of each length.
    counts: [u32; 17],
    /// The index in `symbols` of the first symbol of each length.
    first_index: [usize; 17],
    /// The symbols: shortest codes first, and in code order within a length.
    symbols: [u8; MAX_SYMBOLS],
}

impl HuffmanTable {
    /// Builds a table from the code counts and symbols of a DHT entry.
    ///
    /// `counts[n]` is the number of codes of length `n + 1`, and `symbols` lists the symbols in
    /// order of increasing code length. Codes are assigned as in T.81 Annex C: each length starts
    /// at the code after the last code of the length before it. Annex C also reserves the all-ones
    /// code word of every length, so a table whose codes of some length reach that word is refused.
    /// An incomplete table is accepted.
    ///
    /// # Errors
    /// `Invalid` when the table defines no symbols or more than 256, when `symbols` does not hold
    /// exactly one entry per code, or when the codes of some length are oversubscribed or reach the
    /// all-ones code word.
    pub(super) fn build(counts: &[u8; 16], symbols: &[u8]) -> Result<Self, AssetRejection> {
        let total = counts
            .iter()
            .try_fold(0_usize, |sum, &count| sum.checked_add(usize::from(count)))
            .ok_or(AssetRejection::Invalid)?;
        if total == 0 || total > MAX_SYMBOLS || symbols.len() != total {
            return Err(AssetRejection::Invalid);
        }
        let mut table = Self {
            first_code: [0; 17],
            counts: [0; 17],
            first_index: [0; 17],
            symbols: [0; MAX_SYMBOLS],
        };
        for (slot, &value) in table.symbols.iter_mut().zip(symbols) {
            *slot = value;
        }
        // Entry 0 of the per-length arrays is skipped, so length 1 pairs with the first count.
        let lengths = table
            .first_code
            .iter_mut()
            .zip(table.counts.iter_mut())
            .zip(table.first_index.iter_mut())
            .skip(1)
            .zip(counts);
        let mut next_code: u32 = 0;
        let mut next_index: usize = 0;
        for (length, (((first_code, count_slot), first_index_slot), &length_count)) in
            (1_u32..).zip(lengths)
        {
            let codes = u32::from(length_count);
            let end = next_code
                .checked_add(codes)
                .ok_or(AssetRejection::Invalid)?;
            // The last code of this length is `end - 1`. It must fit in `length` bits and must not
            // be the all-ones word, so `end` must be below 2^length.
            if end >= 1 << length {
                return Err(AssetRejection::Invalid);
            }
            *first_code = next_code;
            *count_slot = codes;
            *first_index_slot = next_index;
            next_index += usize::from(length_count);
            next_code = end << 1;
        }
        Ok(table)
    }

    /// Reads bits from `reader` until they form a code of this table, and returns its symbol.
    ///
    /// # Errors
    /// `Invalid` when no code matches within 16 bits, or when `reader` cannot supply the bits.
    pub(super) fn decode(&self, reader: &mut BitReader<'_>) -> Result<u8, AssetRejection> {
        let lengths = self
            .first_code
            .iter()
            .zip(&self.counts)
            .zip(&self.first_index)
            .skip(1);
        let mut code: u32 = 0;
        for ((&first_code, &count), &first_index) in lengths {
            code = (code << 1) | reader.read_bits(1)?;
            // Every shorter length failed to match, so `code` is at least `first_code`. The
            // `checked_sub` guards the subtraction, and a miss counts as no match.
            if let Some(offset) = code
                .checked_sub(first_code)
                .filter(|&offset| offset < count)
            {
                let index = first_index
                    .checked_add(usize::try_from(offset).map_err(|_| AssetRejection::Invalid)?)
                    .ok_or(AssetRejection::Invalid)?;
                return self
                    .symbols
                    .get(index)
                    .copied()
                    .ok_or(AssetRejection::Invalid);
            }
        }
        Err(AssetRejection::Invalid)
    }
}

/// Reads entropy-coded data one bit at a time, most significant bit first.
///
/// Byte stuffing is undone: `FF 00` is the data byte `FF`. Any other `FF` where a data byte is
/// needed is a marker or a fill byte, which cannot occur inside a scan, so it is refused. So is
/// the end of the input. The reader never supplies a bit that it has not read from the input.
#[derive(Debug)]
pub(super) struct BitReader<'a> {
    bytes: &'a [u8],
    /// The offset of the next byte to load.
    pos: usize,
    /// The byte whose bits are being read.
    byte: u8,
    /// The number of unread bits in `byte`. They are the low `left` bits of `byte`.
    left: u8,
}

impl<'a> BitReader<'a> {
    /// Starts reading at offset `start` of `bytes`.
    pub(super) fn new(bytes: &'a [u8], start: usize) -> Self {
        Self {
            bytes,
            pos: start,
            byte: 0,
            left: 0,
        }
    }

    /// Reads the next `count` bits and returns them as the low bits of the result.
    ///
    /// # Errors
    /// `Invalid` when `count` is more than 16, or when the input ends or a marker begins before
    /// the bits are read.
    pub(super) fn read_bits(&mut self, count: u32) -> Result<u32, AssetRejection> {
        if count > MAX_READ_BITS {
            return Err(AssetRejection::Invalid);
        }
        let mut value: u32 = 0;
        for _ in 0..count {
            if self.left == 0 {
                self.load_byte()?;
            }
            self.left -= 1;
            value = (value << 1) | u32::from((self.byte >> self.left) & 1);
        }
        Ok(value)
    }

    /// Skips the next `count` bits. The rules of `read_bits` apply.
    ///
    /// # Errors
    /// `Invalid` in the same cases as `read_bits`.
    pub(super) fn skip_bits(&mut self, count: u32) -> Result<(), AssetRejection> {
        self.read_bits(count)?;
        Ok(())
    }

    /// Ends the entropy-coded data of an interval, which is either the data before a restart
    /// marker or the data before the end-of-image marker.
    ///
    /// The unread bits of the current byte must all be 1, which is the padding that the standard
    /// requires. Fill bytes `FF` may follow, and the marker comes after the last of them. On
    /// success the reader sits just after the marker, so the next interval starts there.
    ///
    /// Returns the marker's code byte and the offset just after the marker.
    ///
    /// # Errors
    /// `Invalid` when a padding bit is 0, when a data byte comes where the marker should be, when
    /// the marker code is `0x00`, or when the input ends first.
    pub(super) fn finish_interval(&mut self) -> Result<(u8, usize), AssetRejection> {
        let unread_mask = (1_u32 << self.left) - 1;
        if (u32::from(self.byte) & unread_mask) != unread_mask {
            return Err(AssetRejection::Invalid);
        }
        self.left = 0;
        if self.bytes.get(self.pos) != Some(&PREFIX) {
            return Err(AssetRejection::Invalid);
        }
        while self.bytes.get(self.pos) == Some(&PREFIX) {
            self.pos += 1;
        }
        let &marker = self.bytes.get(self.pos).ok_or(AssetRejection::Invalid)?;
        if marker == STUFF {
            return Err(AssetRejection::Invalid);
        }
        self.pos += 1;
        Ok((marker, self.pos))
    }

    /// Loads the next data byte into the bit buffer, and undoes byte stuffing.
    ///
    /// # Errors
    /// `Invalid` at the end of the input, or when `FF` is followed by anything other than `00`.
    fn load_byte(&mut self) -> Result<(), AssetRejection> {
        let &byte = self.bytes.get(self.pos).ok_or(AssetRejection::Invalid)?;
        if byte == PREFIX {
            if self.bytes.get(self.pos + 1) != Some(&STUFF) {
                return Err(AssetRejection::Invalid);
            }
            self.pos += 2;
        } else {
            self.pos += 1;
        }
        self.byte = byte;
        self.left = 8;
        Ok(())
    }
}
