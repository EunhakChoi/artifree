//! Checks for JPEG uploads (decision 0045, rule 3).
//!
//! `validate_jpeg` runs three layers in order. `read_scan_input` parses the marker segments
//! before the scan. It admits baseline (SOF0) and extended sequential (SOF1) frames with 8-bit
//! samples and one scan that holds every component, and it refuses every other marker before the
//! scan. `scan::walk_scan` then follows the entropy-coded data, so a truncated scan is refused.
//! `decode_with_zune` decodes the image with zune-jpeg in strict mode as an independent check.

mod entropy;
mod scan;
#[cfg(test)]
mod scan_tests;
#[cfg(test)]
mod tests;

use artifree_domain::assets::AssetRejection;
use zune_jpeg::JpegDecoder;
use zune_jpeg::zune_core::bytestream::ZCursor;
use zune_jpeg::zune_core::options::DecoderOptions;

use super::{MAX_IMAGE_AXIS, MAX_IMAGE_PIXELS};
use entropy::HuffmanTable;
use scan::{HuffmanTables, ScanPlan};

/// The first three bytes of every admitted file: the start-of-image marker `FF D8`, then `FF`,
/// which begins the next marker.
pub(super) const SIGNATURE: [u8; 3] = [0xFF, 0xD8, 0xFF];

/// The byte that starts every marker. Fill bytes are also `FF`.
const PREFIX: u8 = 0xFF;

/// The marker code `00`. In entropy-coded data `FF 00` is a stuffed data byte, so this code is
/// never a marker and is refused.
const STUFF: u8 = 0x00;

// Marker codes that the header parser reads. Every other marker before the scan is refused.
const SOF0: u8 = 0xC0;
const SOF1: u8 = 0xC1;
const DHT: u8 = 0xC4;
const DQT: u8 = 0xDB;
const SOS: u8 = 0xDA;
const DRI: u8 = 0xDD;
const APP0: u8 = 0xE0;
const APP15: u8 = 0xEF;
const COM: u8 = 0xFE;

/// Most components in a frame. The admitted subset has one, three or four.
const MAX_COMPONENTS: usize = 4;

/// Quantization table slots. The selector `Tq` is at most 3.
const QUANT_SLOTS: usize = 4;

/// Most blocks in one MCU. The scan walk keeps its own copy of this limit, which is private to it.
const MAX_BLOCKS_PER_MCU: usize = 10;

/// Largest decoder output that is allowed: four 8-bit channels at the largest admitted size.
const MAX_DECODED_BYTES: usize = 4096 * 4096 * 4;

/// Checks a JPEG upload: its header segments, its entropy-coded scan, and then a decode with
/// zune-jpeg in strict mode.
///
/// The caller checks the signature too. This function checks it again so that it stays sound on
/// its own. Nothing may follow the end-of-image marker.
///
/// # Errors
/// `Invalid` for any input that is not a complete baseline or extended sequential 8-bit JPEG
/// within the admitted subset.
pub(super) fn validate_jpeg(bytes: &[u8]) -> Result<(), AssetRejection> {
    if !bytes.starts_with(&SIGNATURE) {
        return Err(AssetRejection::Invalid);
    }
    let mut reader = Reader::new(bytes);
    // The start-of-image marker. The next marker begins at the third byte.
    reader.take_bytes(2)?;
    let input = read_scan_input(&mut reader)?;
    let end = scan::walk_scan(bytes, reader.offset, &input.plan, &input.tables)?;
    if end != bytes.len() {
        return Err(AssetRejection::Invalid);
    }
    decode_with_zune(bytes, input.width, input.height)
}

/// What the scan walk and the decoder need from the headers.
#[derive(Debug)]
struct ScanInput {
    /// The scan's MCU layout and table slots.
    plan: ScanPlan,
    /// The Huffman tables that the scan may use.
    tables: HuffmanTables,
    /// The frame's width, which the decoder must report.
    width: u32,
    /// The frame's height, which the decoder must report.
    height: u32,
}

/// Reads the marker segments up to the start of the scan, and returns what the walk needs. On
/// return, `reader` sits at the first byte of the entropy-coded data.
///
/// # Errors
/// `Invalid` for a marker that is not admitted before the scan, a malformed segment, or input that
/// ends first.
fn read_scan_input(reader: &mut Reader<'_>) -> Result<ScanInput, AssetRejection> {
    let mut headers = Headers::default();
    loop {
        let code = take_marker(reader)?;
        let payload = take_segment(reader)?;
        match code {
            SOS => return headers.into_scan_input(payload),
            SOF0 | SOF1 => headers.accept_frame(code, payload)?,
            DHT => headers.accept_huffman(payload)?,
            DQT => headers.accept_quant(payload)?,
            DRI => headers.accept_restart(payload)?,
            // Application and comment segments are skipped. Their framing has been checked.
            APP0..=APP15 | COM => {}
            _ => return Err(AssetRejection::Invalid),
        }
    }
}

/// The marker segments read so far, from the start of the image up to the scan.
#[derive(Debug, Default)]
struct Headers {
    /// The frame, once its SOF segment has been read.
    frame: Option<Frame>,
    /// Whether each quantization table slot has been defined.
    quant_defined: [bool; QUANT_SLOTS],
    /// The Huffman tables defined so far.
    tables: HuffmanTables,
    /// The restart interval from the last DRI segment, in MCUs. Zero means no restart markers.
    restart_interval: u16,
}

impl Headers {
    /// Accepts a SOF0 or SOF1 segment. A scan may follow only one frame.
    ///
    /// # Errors
    /// `Invalid` for a second frame, or for a frame that `Frame::parse` refuses.
    fn accept_frame(&mut self, sof: u8, payload: &[u8]) -> Result<(), AssetRejection> {
        if self.frame.is_some() {
            return Err(AssetRejection::Invalid);
        }
        self.frame = Some(Frame::parse(sof, payload)?);
        Ok(())
    }

    /// Records the quantization tables of a DQT segment. Each table is a precision and selector
    /// byte, then 64 values. Only 8-bit tables with selectors up to 3 are admitted, and each value
    /// must be from 1 to 255.
    ///
    /// # Errors
    /// `Invalid` for an empty segment, a precision above 0, a selector above 3, a zero value, or
    /// entries that do not fill the segment.
    fn accept_quant(&mut self, payload: &[u8]) -> Result<(), AssetRejection> {
        if payload.is_empty() {
            return Err(AssetRejection::Invalid);
        }
        let mut rest = payload;
        while let Some((&spec, after_spec)) = rest.split_first() {
            let (values, after) = after_spec
                .split_first_chunk::<64>()
                .ok_or(AssetRejection::Invalid)?;
            // The byte is Pq * 16 + Tq. Pq must be 0 and Tq at most 3, so the byte is at most 3.
            let defined = self
                .quant_defined
                .get_mut(usize::from(spec))
                .ok_or(AssetRejection::Invalid)?;
            if values.contains(&0) {
                return Err(AssetRejection::Invalid);
            }
            *defined = true;
            rest = after;
        }
        Ok(())
    }

    /// Records the Huffman tables of a DHT segment. Each table is a class and selector byte, 16
    /// code counts, then one symbol per code. A later table in the same slot replaces the earlier.
    ///
    /// # Errors
    /// `Invalid` for an empty segment, a class above 1, a selector above 3, a table that
    /// `HuffmanTable::build` refuses, or entries that do not fill the segment.
    fn accept_huffman(&mut self, payload: &[u8]) -> Result<(), AssetRejection> {
        if payload.is_empty() {
            return Err(AssetRejection::Invalid);
        }
        let mut rest = payload;
        while let Some((&spec, after_spec)) = rest.split_first() {
            let (code_counts, after_counts) = after_spec
                .split_first_chunk::<16>()
                .ok_or(AssetRejection::Invalid)?;
            let symbol_count: usize = code_counts.iter().map(|&count| usize::from(count)).sum();
            let (symbols, after) = after_counts
                .split_at_checked(symbol_count)
                .ok_or(AssetRejection::Invalid)?;
            let table = HuffmanTable::build(code_counts, symbols)?;
            let class_tables = match spec >> 4 {
                0 => &mut self.tables.dc,
                1 => &mut self.tables.ac,
                _ => return Err(AssetRejection::Invalid),
            };
            let table_slot = class_tables
                .get_mut(usize::from(spec & 0x0F))
                .ok_or(AssetRejection::Invalid)?;
            *table_slot = Some(table);
            rest = after;
        }
        Ok(())
    }

    /// Records the restart interval of a DRI segment, which holds exactly two bytes.
    ///
    /// # Errors
    /// `Invalid` when the segment is not two bytes long.
    fn accept_restart(&mut self, payload: &[u8]) -> Result<(), AssetRejection> {
        let &[high, low] = payload else {
            return Err(AssetRejection::Invalid);
        };
        self.restart_interval = u16::from_be_bytes([high, low]);
        Ok(())
    }

    /// Checks the scan header against the frame, and returns what the walk needs. The scan must
    /// follow a frame.
    ///
    /// # Errors
    /// `Invalid` when no frame was read, or when the scan header or the MCU layout is refused.
    fn into_scan_input(self, payload: &[u8]) -> Result<ScanInput, AssetRejection> {
        let frame = self.frame.ok_or(AssetRejection::Invalid)?;
        let slots = self.scan_table_slots(&frame, payload)?;
        let plan = frame.scan_plan(slots, self.restart_interval)?;
        Ok(ScanInput {
            plan,
            tables: self.tables,
            width: frame.width,
            height: frame.height,
        })
    }

    /// Checks the scan header against the frame. The scan must name every component in frame
    /// order, with the spectral selection and approximation of a sequential scan (0, 63, 0, 0).
    /// Returns the DC and AC table slot of each component.
    ///
    /// # Errors
    /// `Invalid` when the component count or an identifier differs from the frame, a table selector
    /// is above 1 under SOF0 or above 3 under SOF1, a table or a quantization table that a component
    /// uses is undefined, or the spectral or approximation fields are not 0, 63, 0, 0.
    fn scan_table_slots(
        &self,
        frame: &Frame,
        payload: &[u8],
    ) -> Result<[(usize, usize); MAX_COMPONENTS], AssetRejection> {
        let (&count, rest) = payload.split_first().ok_or(AssetRejection::Invalid)?;
        if usize::from(count) != frame.components().len() {
            return Err(AssetRejection::Invalid);
        }
        let (selectors, tail) = rest
            .split_at_checked(2 * usize::from(count))
            .ok_or(AssetRejection::Invalid)?;
        let &[spectral_start, spectral_end, approximation] = tail else {
            return Err(AssetRejection::Invalid);
        };
        if (spectral_start, spectral_end, approximation) != (0, 63, 0) {
            return Err(AssetRejection::Invalid);
        }
        let max_selector = if frame.sof == SOF0 { 1 } else { 3 };
        let mut slots = [(0, 0); MAX_COMPONENTS];
        for ((slot, component), selector) in slots
            .iter_mut()
            .zip(frame.components())
            .zip(selectors.chunks_exact(2))
        {
            let &[component_id, table_byte] = selector else {
                return Err(AssetRejection::Invalid);
            };
            let dc = table_byte >> 4;
            let ac = table_byte & 0x0F;
            let dc_defined = self
                .tables
                .dc
                .get(usize::from(dc))
                .is_some_and(Option::is_some);
            let ac_defined = self
                .tables
                .ac
                .get(usize::from(ac))
                .is_some_and(Option::is_some);
            let quant_defined = self
                .quant_defined
                .get(usize::from(component.quant_table))
                .copied()
                .unwrap_or(false);
            let defined = dc_defined && ac_defined && quant_defined;
            if component_id != component.id || dc > max_selector || ac > max_selector || !defined {
                return Err(AssetRejection::Invalid);
            }
            *slot = (usize::from(dc), usize::from(ac));
        }
        Ok(slots)
    }
}

/// The frame header, from an SOF0 or SOF1 segment, after its checks.
#[derive(Debug, Clone, Copy)]
struct Frame {
    /// The marker code, `SOF0` or `SOF1`. It limits the table selectors that a scan may use.
    sof: u8,
    /// The width in pixels, from 1 to `MAX_IMAGE_AXIS`.
    width: u32,
    /// The height in pixels, from 1 to `MAX_IMAGE_AXIS`.
    height: u32,
    /// The components in frame order. Only the first `component_count` entries are used.
    entries: [Component; MAX_COMPONENTS],
    /// How many components the frame has: 1, 3 or 4.
    component_count: usize,
}

impl Frame {
    /// Parses the payload of an SOF segment, whose marker code is `sof`.
    ///
    /// # Errors
    /// `Invalid` for a precision other than 8, dimensions outside the admitted range, a component
    /// count other than 1, 3 or 4, a payload whose length does not fit that count, or a component
    /// entry or sampling rule that is refused.
    fn parse(sof: u8, payload: &[u8]) -> Result<Self, AssetRejection> {
        let mut reader = Reader::new(payload);
        let precision = reader.take_u8()?;
        let height = u32::from(reader.take_u16()?);
        let width = u32::from(reader.take_u16()?);
        let component_count = usize::from(reader.take_u8()?);
        let component_bytes = reader.take_rest();
        if precision != 8 {
            return Err(AssetRejection::Invalid);
        }
        check_dimensions(width, height)?;
        if !matches!(component_count, 1 | 3 | 4) || component_bytes.len() != 3 * component_count {
            return Err(AssetRejection::Invalid);
        }
        // Entries past `component_count` keep the default, which `components` never returns.
        let mut parsed = [Component::default(); MAX_COMPONENTS];
        for (target, entry) in parsed.iter_mut().zip(component_bytes.chunks_exact(3)) {
            *target = Component::parse(entry)?;
        }
        let frame = Self {
            sof,
            width,
            height,
            entries: parsed,
            component_count,
        };
        frame.check_components()?;
        Ok(frame)
    }

    /// The components in frame order.
    fn components(&self) -> &[Component] {
        self.entries.get(..self.component_count).unwrap_or_default()
    }

    /// The largest horizontal and vertical sampling factors, `Hmax` and `Vmax`.
    fn max_sampling(&self) -> (u8, u8) {
        let horizontal = self
            .components()
            .iter()
            .map(|component| component.horizontal)
            .max();
        let vertical = self
            .components()
            .iter()
            .map(|component| component.vertical)
            .max();
        (horizontal.unwrap_or(1), vertical.unwrap_or(1))
    }

    /// The blocks that `component` contributes to each MCU. A single component is scanned one
    /// block at a time, so it contributes one block.
    fn blocks_per_mcu(&self, component: Component) -> usize {
        if self.component_count == 1 {
            1
        } else {
            usize::from(component.horizontal) * usize::from(component.vertical)
        }
    }

    /// Checks the component identifiers and sampling factors, which depend on one another.
    ///
    /// # Errors
    /// `Invalid` for a repeated identifier, a factor that does not divide the largest factor, or
    /// more than `MAX_BLOCKS_PER_MCU` blocks in one MCU.
    fn check_components(&self) -> Result<(), AssetRejection> {
        let components = self.components();
        let (max_h, max_v) = self.max_sampling();
        for (index, component) in components.iter().enumerate() {
            let repeated = components
                .iter()
                .skip(index + 1)
                .any(|other| other.id == component.id);
            if repeated || max_h % component.horizontal != 0 || max_v % component.vertical != 0 {
                return Err(AssetRejection::Invalid);
            }
        }
        let blocks: usize = components
            .iter()
            .map(|&component| self.blocks_per_mcu(component))
            .sum();
        if blocks > MAX_BLOCKS_PER_MCU {
            return Err(AssetRejection::Invalid);
        }
        Ok(())
    }

    /// The number of MCUs in the scan. A single component is scanned as 8 by 8 blocks. Several
    /// components share MCUs of `8 * Hmax` by `8 * Vmax` samples.
    ///
    /// # Errors
    /// `Invalid` when the count does not fit in `u32`.
    fn mcu_count(&self) -> Result<u32, AssetRejection> {
        let (unit_width, unit_height) = if self.component_count == 1 {
            (8, 8)
        } else {
            let (max_h, max_v) = self.max_sampling();
            (8 * u32::from(max_h), 8 * u32::from(max_v))
        };
        let columns = self.width.div_ceil(unit_width);
        let rows = self.height.div_ceil(unit_height);
        columns.checked_mul(rows).ok_or(AssetRejection::Invalid)
    }

    /// Builds the scan plan from the table slots of each component, which are in frame order.
    ///
    /// # Errors
    /// `Invalid` when the MCU count overflows, or when the blocks of one MCU exceed
    /// `MAX_BLOCKS_PER_MCU`.
    fn scan_plan(
        &self,
        slots: [(usize, usize); MAX_COMPONENTS],
        restart_interval: u16,
    ) -> Result<ScanPlan, AssetRejection> {
        let mut units = [(0, 0); MAX_BLOCKS_PER_MCU];
        let mut unit_count: usize = 0;
        for (&component, slot) in self.components().iter().zip(slots) {
            for _ in 0..self.blocks_per_mcu(component) {
                let unit = units.get_mut(unit_count).ok_or(AssetRejection::Invalid)?;
                *unit = slot;
                unit_count += 1;
            }
        }
        Ok(ScanPlan {
            units,
            unit_count,
            mcu_count: self.mcu_count()?,
            restart_interval,
        })
    }
}

/// One component entry of a frame: its identifier, sampling factors and quantization table.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Component {
    /// The identifier, which a scan names to select the component.
    id: u8,
    /// The horizontal sampling factor, from 1 to 4.
    horizontal: u8,
    /// The vertical sampling factor, from 1 to 4.
    vertical: u8,
    /// The quantization table selector, from 0 to 3.
    quant_table: u8,
}

impl Component {
    /// Parses one three-byte component entry.
    ///
    /// # Errors
    /// `Invalid` when the entry is not three bytes, a sampling factor is outside 1 to 4, or the
    /// quantization table selector is above 3.
    fn parse(entry: &[u8]) -> Result<Self, AssetRejection> {
        let &[id, sampling, quant_table] = entry else {
            return Err(AssetRejection::Invalid);
        };
        let horizontal = sampling >> 4;
        let vertical = sampling & 0x0F;
        if !(1..=4).contains(&horizontal) || !(1..=4).contains(&vertical) || quant_table > 3 {
            return Err(AssetRejection::Invalid);
        }
        Ok(Self {
            id,
            horizontal,
            vertical,
            quant_table,
        })
    }
}

/// Forward-only reader over the upload. Every read is bounds-checked, so a declared length
/// cannot reach past the end of the input.
struct Reader<'a> {
    bytes: &'a [u8],
    /// The offset of the next unread byte.
    offset: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    /// Returns the next `len` bytes, or `Invalid` if they run past the end.
    fn take_bytes(&mut self, len: usize) -> Result<&'a [u8], AssetRejection> {
        let end = self
            .offset
            .checked_add(len)
            .ok_or(AssetRejection::Invalid)?;
        let taken = self
            .bytes
            .get(self.offset..end)
            .ok_or(AssetRejection::Invalid)?;
        self.offset = end;
        Ok(taken)
    }

    /// Returns the next `N` bytes as an array.
    fn take_array<const N: usize>(&mut self) -> Result<[u8; N], AssetRejection> {
        let taken = self.take_bytes(N)?;
        <[u8; N]>::try_from(taken).map_err(|_| AssetRejection::Invalid)
    }

    /// Returns the next byte.
    fn take_u8(&mut self) -> Result<u8, AssetRejection> {
        let [byte] = self.take_array()?;
        Ok(byte)
    }

    /// Returns the next two bytes as a big-endian `u16`.
    fn take_u16(&mut self) -> Result<u16, AssetRejection> {
        let bytes: [u8; 2] = self.take_array()?;
        Ok(u16::from_be_bytes(bytes))
    }

    /// Returns the next byte without consuming it, or `None` at the end of the input.
    fn peek_u8(&self) -> Option<u8> {
        self.bytes.get(self.offset).copied()
    }

    /// Returns every unread byte.
    fn take_rest(&mut self) -> &'a [u8] {
        let rest = self.bytes.get(self.offset..).unwrap_or_default();
        self.offset = self.bytes.len();
        rest
    }
}

/// Reads the next marker code. Fill bytes (`FF`) may come before it, and the code `00` is refused.
///
/// # Errors
/// `Invalid` when the next byte is not `FF`, when the code is `00`, or when the input ends.
fn take_marker(reader: &mut Reader<'_>) -> Result<u8, AssetRejection> {
    if reader.take_u8()? != PREFIX {
        return Err(AssetRejection::Invalid);
    }
    while reader.peek_u8() == Some(PREFIX) {
        reader.take_u8()?;
    }
    match reader.take_u8()? {
        STUFF => Err(AssetRejection::Invalid),
        code => Ok(code),
    }
}

/// Reads a segment's two-byte length, then returns its payload. The length counts its own two
/// bytes.
///
/// # Errors
/// `Invalid` when the length is below 2, or when the payload runs past the end of the input.
fn take_segment<'a>(reader: &mut Reader<'a>) -> Result<&'a [u8], AssetRejection> {
    let length = usize::from(reader.take_u16()?);
    let payload_len = length.checked_sub(2).ok_or(AssetRejection::Invalid)?;
    reader.take_bytes(payload_len)
}

/// Decodes the image with zune-jpeg in strict mode and discards the pixels. This is an independent
/// check after the walk, which alone decides truncation.
///
/// The decoder's own dimensions must match the frame's, and its output must fit in
/// `MAX_DECODED_BYTES`. The output buffer is allocated with `try_reserve_exact`, so a failed
/// allocation is an error and not an abort.
///
/// # Errors
/// `Invalid` when the decoder refuses the image, reports other dimensions, asks for more than
/// `MAX_DECODED_BYTES`, or the buffer cannot be allocated.
///
/// # Panics
/// zune-jpeg is not run under `catch_unwind`. A panic inside the decoder, on input that the walk
/// accepted, unwinds to the caller. The caller holds the decode lock, which such a panic poisons,
/// and the lock recovers its guard for the next holder.
fn decode_with_zune(bytes: &[u8], width: u32, height: u32) -> Result<(), AssetRejection> {
    let max_axis = usize::try_from(MAX_IMAGE_AXIS).map_err(|_| AssetRejection::Invalid)?;
    let options = DecoderOptions::default()
        .set_strict_mode(true)
        .set_max_width(max_axis)
        .set_max_height(max_axis);
    let mut decoder = JpegDecoder::new_with_options(ZCursor::new(bytes), options);
    decoder
        .decode_headers()
        .map_err(|_| AssetRejection::Invalid)?;
    let info = decoder.info().ok_or(AssetRejection::Invalid)?;
    if u32::from(info.width) != width || u32::from(info.height) != height {
        return Err(AssetRejection::Invalid);
    }
    let size = decoder
        .output_buffer_size()
        .ok_or(AssetRejection::Invalid)?;
    if size > MAX_DECODED_BYTES {
        return Err(AssetRejection::Invalid);
    }
    let mut output: Vec<u8> = Vec::new();
    output
        .try_reserve_exact(size)
        .map_err(|_| AssetRejection::Invalid)?;
    output.resize(size, 0);
    decoder
        .decode_into(&mut output)
        .map_err(|_| AssetRejection::Invalid)
}

/// Checks that each axis is in range and that the pixel count is within its limit.
///
/// # Errors
/// `Invalid` for a zero or oversize axis, or for a pixel count above `MAX_IMAGE_PIXELS`.
fn check_dimensions(width: u32, height: u32) -> Result<(), AssetRejection> {
    if width == 0 || height == 0 || width > MAX_IMAGE_AXIS || height > MAX_IMAGE_AXIS {
        return Err(AssetRejection::Invalid);
    }
    let pixels = u64::from(width)
        .checked_mul(u64::from(height))
        .ok_or(AssetRejection::Invalid)?;
    if pixels > MAX_IMAGE_PIXELS {
        return Err(AssetRejection::Invalid);
    }
    Ok(())
}
