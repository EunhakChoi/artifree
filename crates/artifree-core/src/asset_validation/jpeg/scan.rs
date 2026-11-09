//! The scan walk for JPEG admission (decision 0045, rule 3).
//!
//! `walk_scan` follows the MCUs and restart markers of one baseline scan. For each block it
//! decodes the DC category and the AC run/size symbols, and skips their magnitude bits. It keeps
//! no coefficient values, so its memory does not depend on the image size.

use artifree_domain::assets::AssetRejection;

use super::entropy::{BitReader, HuffmanTable};

/// Most blocks in one MCU. With more than one component, the blocks of an MCU are the sum of each
/// component's horizontal times vertical sampling factor, and that sum is at most 10. A single
/// component has one block per MCU.
const MAX_BLOCKS_PER_MCU: usize = 10;

/// Coefficients in one block. Index 0 is the DC coefficient, and AC symbols code indices 1 to 63.
const BLOCK_SIZE: u32 = 64;

/// Largest DC category at 8-bit sample precision.
const MAX_DC_CATEGORY: u8 = 11;

/// Largest AC magnitude size at 8-bit sample precision.
const MAX_AC_SIZE: u8 = 10;

/// The first restart marker code. The restart markers RST0 to RST7 repeat in that order.
const RST0: u8 = 0xD0;

/// How many restart markers are in one cycle before the codes repeat.
const RESTART_CYCLE: u8 = 8;

/// The end-of-image marker code.
const EOI: u8 = 0xD9;

/// What the walker needs to know about one scan, taken from the frame and scan headers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ScanPlan {
    /// One `(DC table, AC table)` pair per block of an MCU, in decoding order. A component with
    /// several blocks per MCU repeats its pair once per block. Only the first `unit_count`
    /// entries are used.
    pub(super) units: [(usize, usize); MAX_BLOCKS_PER_MCU],
    /// How many entries of `units` are used.
    pub(super) unit_count: usize,
    /// The number of MCUs in the scan.
    pub(super) mcu_count: u32,
    /// The number of MCUs between restart markers, or 0 when the scan has none.
    pub(super) restart_interval: u16,
}

/// The Huffman tables that a scan may use. Each class has four slots, numbered 0 to 3. An empty
/// slot is an undefined table, and a scan that names one is refused.
#[derive(Debug, Default)]
pub(super) struct HuffmanTables {
    /// The DC tables.
    pub(super) dc: [Option<HuffmanTable>; 4],
    /// The AC tables.
    pub(super) ac: [Option<HuffmanTable>; 4],
}

/// Walks the entropy-coded data of one scan, which starts at offset `start` of `bytes`, and
/// returns the offset just after the scan's end-of-image marker.
///
/// Before each MCU after the first, where the MCU number is a multiple of the restart interval,
/// the next restart marker must come in the cycle RST0 to RST7. After the last MCU the walk
/// expects the end-of-image marker and nothing else.
///
/// # Errors
/// `Invalid` when `unit_count` is 0 or above 10, when a unit names an empty table slot, when a
/// block's DC category is above 11, when an AC symbol is undefined or has a size above 10, when a
/// block's coefficients run past index 63, when a restart marker is missing or out of order, when
/// the input ends, or when the scan has bytes that are not part of its data.
pub(super) fn walk_scan(
    bytes: &[u8],
    start: usize,
    plan: &ScanPlan,
    tables: &HuffmanTables,
) -> Result<usize, AssetRejection> {
    let units = plan
        .units
        .get(..plan.unit_count)
        .filter(|entries| !entries.is_empty())
        .ok_or(AssetRejection::Invalid)?;
    let restart_interval = u32::from(plan.restart_interval);
    let mut reader = BitReader::new(bytes, start);
    // The position in the RST0 to RST7 cycle of the next restart marker.
    let mut next_restart: u8 = 0;
    for mcu in 0..plan.mcu_count {
        if restart_interval > 0 && mcu > 0 && mcu.is_multiple_of(restart_interval) {
            let (marker, _) = reader.finish_interval()?;
            if marker != RST0 + next_restart {
                return Err(AssetRejection::Invalid);
            }
            next_restart = (next_restart + 1) % RESTART_CYCLE;
        }
        for &(dc_index, ac_index) in units {
            let (dc_table, ac_table) = table_pair(tables, dc_index, ac_index)?;
            walk_block(&mut reader, dc_table, ac_table)?;
        }
    }
    let (marker, end) = reader.finish_interval()?;
    if marker != EOI {
        return Err(AssetRejection::Invalid);
    }
    Ok(end)
}

/// Returns the DC and AC tables that a unit names.
///
/// # Errors
/// `Invalid` when an index is outside its four slots, or names an empty slot.
fn table_pair(
    tables: &HuffmanTables,
    dc_index: usize,
    ac_index: usize,
) -> Result<(&HuffmanTable, &HuffmanTable), AssetRejection> {
    let dc_table = tables
        .dc
        .get(dc_index)
        .and_then(Option::as_ref)
        .ok_or(AssetRejection::Invalid)?;
    let ac_table = tables
        .ac
        .get(ac_index)
        .and_then(Option::as_ref)
        .ok_or(AssetRejection::Invalid)?;
    Ok((dc_table, ac_table))
}

/// Walks one block: its DC category, then its AC symbols up to the end of the block.
///
/// The magnitude bits are skipped, not decoded. The block ends at an end-of-block symbol, or
/// once index 63 has been coded.
///
/// # Errors
/// `Invalid` for a DC category above 11, an undefined AC symbol, an AC size above 10, a run that
/// reaches past index 63, or a ZRL whose skip reaches index 64. Bits that match no code of the
/// table, and input that ends mid-block, are `Invalid` too.
fn walk_block(
    reader: &mut BitReader<'_>,
    dc_table: &HuffmanTable,
    ac_table: &HuffmanTable,
) -> Result<(), AssetRejection> {
    let category = dc_table.decode(reader)?;
    if category > MAX_DC_CATEGORY {
        return Err(AssetRejection::Invalid);
    }
    reader.skip_bits(u32::from(category))?;
    // The index of the next AC coefficient, in zigzag order.
    let mut index: u32 = 1;
    while index < BLOCK_SIZE {
        let symbol = ac_table.decode(reader)?;
        let run = u32::from(symbol >> 4);
        let size = symbol & 0x0F;
        match (run, size) {
            (0, 0) => return Ok(()),
            // ZRL skips sixteen zero coefficients. The skip must leave an index to code, so it
            // may not reach index 64.
            (15, 0) => {
                index += 16;
                if index >= BLOCK_SIZE {
                    return Err(AssetRejection::Invalid);
                }
            }
            (_, 0) => return Err(AssetRejection::Invalid),
            _ => {
                if size > MAX_AC_SIZE {
                    return Err(AssetRejection::Invalid);
                }
                index += run;
                if index >= BLOCK_SIZE {
                    return Err(AssetRejection::Invalid);
                }
                reader.skip_bits(u32::from(size))?;
                index += 1;
            }
        }
    }
    Ok(())
}
