//! Font and metric variation tables.

use super::{fixed::Fixed, raw_tag, Array, Bytes, RawFont, RawTag, U24};

pub const FVAR: RawTag = raw_tag(b"fvar");
pub const AVAR: RawTag = raw_tag(b"avar");
pub const HVAR: RawTag = raw_tag(b"HVAR");
pub const VVAR: RawTag = raw_tag(b"VVAR");
pub const MVAR: RawTag = raw_tag(b"MVAR");

/// Font variations table.
#[derive(Copy, Clone)]
pub struct Fvar<'a> {
    data: Bytes<'a>,
    axis_offset: u16,
    axis_count: u16,
    axis_size: u16,
    inst_count: u16,
    inst_size: u16,
}

impl<'a> Fvar<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        let b = Bytes::new(data);
        let axis_offset = b.read_or_default::<u16>(4);
        let axis_count = b.read_or_default::<u16>(8);
        let axis_size = b.read_or_default::<u16>(10);
        let inst_count = b.read_or_default::<u16>(12);
        let inst_size = b.read_or_default::<u16>(14);
        Self {
            data: b,
            axis_offset,
            axis_count,
            axis_size,
            inst_count,
            inst_size,
        }
    }

    pub fn from_font(font: impl RawFont<'a>) -> Option<Self> {
        Some(Self::new(font.table_data(FVAR)?))
    }

    pub fn axis_count(&self) -> u16 {
        self.axis_count
    }

    pub fn get_axis(&self, index: u16) -> Option<VarAxis> {
        if index >= self.axis_count {
            return None;
        }
        let b = &self.data;
        let base = self.axis_offset as usize;
        let offset = base + index as usize * self.axis_size as usize;
        let tag = b.read::<u32>(offset)?;
        let min = Fixed(b.read::<i32>(offset + 4)?);
        let default = Fixed(b.read::<i32>(offset + 8)?);
        let max = Fixed(b.read::<i32>(offset + 12)?);
        let flags = b.read::<u16>(offset + 16)?;
        let name_id = b.read::<u16>(offset + 18)?;
        Some(VarAxis {
            index,
            tag,
            name_id,
            flags,
            min,
            default,
            max,
        })
    }

    pub fn get_axis_by_tag(&self, tag: RawTag) -> Option<VarAxis> {
        let b = &self.data;
        let base = self.axis_offset as usize;
        let axis_size = self.axis_size as usize;
        for i in 0..self.axis_count as usize {
            let tag_offset = base + i * axis_size;
            if b.read_u32(tag_offset) == Some(tag) {
                return self.get_axis(i as u16);
            }
        }
        None
    }

    pub fn instance_count(&self) -> u16 {
        self.inst_count
    }

    pub fn get_instance(&self, index: u16) -> Option<VarInstance<'a>> {
        if index >= self.inst_count {
            return None;
        }
        let b = &self.data;
        let base = self.axis_offset as usize + (self.axis_count as usize * self.axis_size as usize);
        let offset = base + index as usize * self.inst_size as usize;
        let name_id = b.read::<u16>(offset)?;
        let values = b.read_array::<Fixed>(offset + 4, self.axis_count as usize)?;
        let ps_name_offset = 4 + self.axis_count as usize * 4;
        let postscript_name_id = if ps_name_offset == self.inst_size as usize - 2 {
            b.read::<u16>(ps_name_offset)
        } else {
            None
        };
        Some(VarInstance {
            index,
            name_id,
            postscript_name_id,
            values,
        })
    }
}

/// Axis of variation in a variable font.
#[derive(Copy, Clone, Default)]
pub struct VarAxis {
    pub index: u16,
    pub tag: RawTag,
    pub name_id: u16,
    pub flags: u16,
    pub min: Fixed,
    pub default: Fixed,
    pub max: Fixed,
}

impl VarAxis {
    /// Returns true if the axis should be hidden in a user interface.
    pub fn is_hidden(&self) -> bool {
        self.flags & 1 != 0
    }

    /// Returns a normalized axis coordinate for the specified value in 2.14
    /// fixed point format.
    pub fn normalized_coord(&self, mut value: Fixed, avar: Option<(&[u8], u32)>) -> i16 {
        use core::cmp::Ordering::*;
        if value < self.min {
            value = self.min;
        } else if value > self.max {
            value = self.max;
        }
        value = match value.cmp(&self.default) {
            Less => -((self.default - value) / (self.default - self.min)),
            Greater => (value - self.default) / (self.max - self.default),
            Equal => Fixed(0),
        };
        value = value.min(Fixed::ONE).max(-Fixed::ONE);
        value = avar
            .and_then(|(data, avar)| adjust_axis(data, avar, self.index, value))
            .unwrap_or(value);
        value.to_f2dot14()
    }
}

/// Named instance in a variable font.
#[derive(Copy, Clone)]
pub struct VarInstance<'a> {
    pub index: u16,
    pub name_id: u16,
    pub postscript_name_id: Option<u16>,
    pub values: Array<'a, Fixed>,
}

/// Metrics variation table.
pub struct Mvar<'a> {
    data: Bytes<'a>,
    coords: &'a [i16],
    rec_size: usize,
    rec_count: usize,
    store: u32,
}

impl<'a> Mvar<'a> {
    pub fn new(data: &'a [u8], mvar: u32, coords: &'a [i16]) -> Option<Self> {
        let b = Bytes::with_offset(data, mvar as usize)?;
        let rec_size = b.read::<u16>(6)? as usize;
        let rec_count = b.read::<u16>(8)? as usize;
        let store = b.read::<u16>(10)? as u32;
        if rec_count == 0 || store == 0 {
            return None;
        }
        Some(Self {
            data: b,
            coords,
            rec_size,
            rec_count,
            store,
        })
    }

    pub fn delta(&self, metric: RawTag) -> f32 {
        self.read_delta(metric).map(|d| d.to_f32()).unwrap_or(0.)
    }

    #[inline(always)]
    fn read_delta(&self, metric: RawTag) -> Option<Fixed> {
        let base = 12;
        let b = &self.data;
        let rec_size = self.rec_size;
        let mut l = 0;
        let mut h = self.rec_count;
        while l < h {
            use core::cmp::Ordering::*;
            let i = (l + h) / 2;
            let offset = base + i * rec_size;
            let t = b.read::<u32>(offset)?;
            match metric.cmp(&t) {
                Less => h = i,
                Greater => l = i + 1,
                Equal => {
                    let inner = b.read::<u16>(offset + 4)?;
                    let outer = b.read::<u16>(offset + 6)?;
                    return item_delta(b.data(), self.store, outer, inner, self.coords);
                }
            }
        }
        None
    }
}

/// Returns the advance delta for the specified glyph.
pub fn advance_delta(data: &[u8], xvar: u32, glyph_id: u16, coords: &[i16]) -> f32 {
    metric_delta(data, xvar, 8, glyph_id, coords)
        .map(|d| d.to_f32())
        .unwrap_or(0.)
}

/// Returns the side bearing delta for the specified glyph.
pub fn sb_delta(data: &[u8], xvar: u32, glyph_id: u16, coords: &[i16]) -> f32 {
    metric_delta(data, xvar, 12, glyph_id, coords)
        .map(|d| d.to_f32())
        .unwrap_or(0.)
}

/// Applies adjustments to a coordinate according to the optional axis
/// variation table.
pub fn adjust_axis(data: &[u8], avar: u32, axis: u16, coord: Fixed) -> Option<Fixed> {
    use skrifa::raw::{tables::avar::Avar, types::Fixed, FontData, FontRead};

    if avar == 0 {
        return None;
    }
    let avar = Avar::read(FontData::new(data.get(avar as usize..)?)).ok()?;
    let mapping = avar
        .axis_segment_maps()
        .get(axis as usize)
        .transpose()
        .ok()??;
    Some(Fixed(mapping.apply(Fixed::from_bits(coord.0)).to_bits()))
}

/// Maximum number of variation axes for which deltas from an `avar`
/// version 2 table are applied. Evaluation uses a fixed size buffer to
/// avoid allocation; coordinates for fonts with more axes are left
/// unmodified.
pub const MAX_AVAR2_AXES: usize = 64;

/// Errors that can occur when evaluating an `avar` version 2 table.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Avar2Error {
    /// Version of the `avar` table is not supported. Only versions 1.x
    /// and 2.0 are recognized.
    UnsupportedVersion,
    /// The axis index map does not contain an entry for a requested axis
    /// or is malformed.
    MissingMapping,
    /// The table is truncated or the item variation store offset is out
    /// of bounds or the store is malformed.
    VarStoreOutOfBounds,
}

impl core::fmt::Display for Avar2Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let s = match self {
            Self::UnsupportedVersion => "unsupported avar table version",
            Self::MissingMapping => "missing axis index map entry",
            Self::VarStoreOutOfBounds => "item variation store out of bounds",
        };
        f.write_str(s)
    }
}

/// Header of an `avar` version 2 table with absolute offsets to the axis
/// index map and item variation store.
struct Avar2 {
    index_map: u32,
    var_store: u32,
}

impl Avar2 {
    /// Parses the avar table header. Returns `Ok(None)` if the table is
    /// absent, unreadable or is not version 2.0 (version 1 tables carry
    /// no deltas and are handled by per-axis segment maps).
    fn new(data: &[u8], avar: u32) -> Result<Option<Self>, Avar2Error> {
        if avar == 0 {
            return Ok(None);
        }
        let b = Bytes::new(data);
        let base = avar as usize;
        let (major, minor) = match (b.read::<u16>(base), b.read::<u16>(base + 2)) {
            (Some(major), Some(minor)) => (major, minor),
            _ => return Ok(None),
        };
        match (major, minor) {
            (1, _) => return Ok(None),
            (2, 0) => {}
            _ => return Err(Avar2Error::UnsupportedVersion),
        }
        use Avar2Error::*;
        let axis_count = b
            .read::<u16>(base + 6)
            .ok_or(VarStoreOutOfBounds)? as usize;
        // The axis index map and item variation store offsets are located
        // after the axis segment maps.
        let mut offset = base + 8;
        for _ in 0..axis_count {
            let count = b.read::<u16>(offset).ok_or(VarStoreOutOfBounds)? as usize;
            offset = offset.checked_add(2 + count * 4).ok_or(VarStoreOutOfBounds)?;
        }
        let index_map = b.read::<u32>(offset).ok_or(VarStoreOutOfBounds)?;
        let var_store = b.read::<u32>(offset + 4).ok_or(VarStoreOutOfBounds)?;
        // Both offsets are from the beginning of the avar table.
        let index_map = if index_map != 0 {
            avar.checked_add(index_map).ok_or(VarStoreOutOfBounds)?
        } else {
            0
        };
        let var_store = if var_store != 0 {
            avar.checked_add(var_store).ok_or(VarStoreOutOfBounds)?
        } else {
            0
        };
        Ok(Some(Self {
            index_map,
            var_store,
        }))
    }
}

/// Returns the (outer, inner) delta set index for the specified axis from
/// a delta set index map. Returns the identity mapping if the map is not
/// present and `None` if the map is malformed or does not contain an
/// entry for the axis.
fn axis_map_entry(data: &[u8], map: u32, axis: usize) -> Option<(u16, u16)> {
    if map == 0 {
        // Identity mapping when the axis index map is not present.
        return Some((0, axis as u16));
    }
    let b = Bytes::new(data);
    let map = map as usize;
    let format = b.read::<u8>(map)?;
    let entry_format = b.read::<u8>(map + 1)?;
    let (count, entries) = match format {
        0 | 1 => (b.read::<u16>(map + 2)? as usize, map + 4),
        2 => (b.read::<u32>(map + 2)? as usize, map + 6),
        _ => return None,
    };
    if axis >= count {
        return None;
    }
    let inner_bits = (entry_format & 0xF) as u32 + 1;
    let entry_size = ((entry_format >> 4) & 0x3) as usize + 1;
    let offset = entries + axis * entry_size;
    let entry = match entry_size {
        1 => b.read::<u8>(offset)? as u32,
        2 => b.read::<u16>(offset)? as u32,
        3 => b.read::<U24>(offset)?.0,
        _ => b.read::<u32>(offset)?,
    };
    Some((
        (entry >> inner_bits) as u16,
        (entry & ((1 << inner_bits) - 1)) as u16,
    ))
}

/// Applies deltas from an `avar` version 2 table to a set of normalized
/// coordinates in 2.14 fixed point format.
///
/// The coordinates are expected to be fully normalized, including
/// application of `avar` version 1 segment maps. Deltas from the item
/// variation store are computed from the input coordinates as a whole
/// (regions may span multiple axes), added to each axis and the results
/// are clamped to the range \[-1, 1\].
///
/// If the font does not contain an `avar` version 2 table, this is a
/// no-op and returns `Ok(())`, leaving the coordinates bit-for-bit
/// unchanged. The coordinates are also left unchanged if an error is
/// returned or if there are more than [`MAX_AVAR2_AXES`] coordinates.
///
/// Evaluation is deterministic (repeated evaluation of the same
/// coordinates yields the same result) and uses a fixed amount of stack
/// memory regardless of the font.
pub fn apply_avar2(data: &[u8], avar: u32, coords: &mut [i16]) -> Result<(), Avar2Error> {
    let table = Avar2::new(data, avar)?;
    let table = match table {
        Some(table) => table,
        None => return Ok(()),
    };
    let len = coords.len();
    if len == 0 || len > MAX_AVAR2_AXES || table.var_store == 0 {
        return Ok(());
    }
    let b = Bytes::new(data);
    let store = table.var_store as usize;
    // Ensure the store offset is within bounds and its header (through
    // the item variation data count field) is readable.
    if b.read::<u16>(store + 6).is_none() {
        return Err(Avar2Error::VarStoreOutOfBounds);
    }
    // Compute all deltas from the input coordinates before committing so
    // that cross axis regions observe unmodified values and the output
    // remains untouched when an error occurs.
    let mut result = [0i16; MAX_AVAR2_AXES];
    for (i, out) in result[..len].iter_mut().enumerate() {
        let (outer, inner) =
            axis_map_entry(data, table.index_map, i).ok_or(Avar2Error::MissingMapping)?;
        let delta = item_delta(data, table.var_store, outer, inner, coords)
            .ok_or(Avar2Error::VarStoreOutOfBounds)?;
        // item_delta treats delta values as integers; avar2 deltas are in
        // 2.14 format so rescale to 16.16.
        let delta = Fixed(delta.0 / 16384);
        let value = Fixed::from_f2dot14(coords[i]) + delta;
        *out = value.min(Fixed::ONE).max(-Fixed::ONE).to_f2dot14();
    }
    coords.copy_from_slice(&result[..len]);
    Ok(())
}

/// Returns a delta from an item variation store.
pub fn item_delta(
    data: &[u8],
    offset: u32,
    outer: u16,
    inner: u16,
    coords: &[i16],
) -> Option<Fixed> {
    if offset == 0 {
        return None;
    }
    let b = Bytes::new(data);
    let store = offset as usize;
    if outer >= b.read::<u16>(store + 6)? {
        return None;
    }
    let region_base = store + b.read::<u32>(store + 2)? as usize;
    let axis_count = b.read::<u16>(region_base)? as usize;
    let region_record_size = axis_count * 6;
    let region_count = b.read::<u16>(region_base + 2)? as usize;
    let data_base = store + b.read::<u32>(store + 8 + outer as usize * 4)? as usize;
    let region_index_base = data_base + 6;
    let region_index_count = b.read::<u16>(data_base + 4)? as usize;
    let (short_count, mut delta_base) = {
        let inner = inner as usize;
        let short_count = b.read::<u16>(data_base + 2)? as usize;
        let count = region_index_count;
        let base = data_base + 6 + count * 2;
        let elem_len = (count - short_count) + short_count * 2;
        let offset = base + inner * elem_len;
        (short_count, offset)
    };
    const ZERO: Fixed = Fixed::ZERO;
    const ONE: Fixed = Fixed::ONE;
    let mut idx = 0;
    let mut delta = ZERO;
    for i in 0..region_index_count {
        let region_index = b.read::<u16>(region_index_base + i * 2)? as usize;
        if region_index >= region_count {
            return None;
        }
        let region_offset = region_base + 4 + region_index * region_record_size;
        let mut scalar = ONE;
        for axis in 0..axis_count {
            let region_axis_base = region_offset + axis * 6;
            let start = Fixed::from_f2dot14(b.read::<i16>(region_axis_base)?);
            let peak = Fixed::from_f2dot14(b.read::<i16>(region_axis_base + 2)?);
            let end = Fixed::from_f2dot14(b.read::<i16>(region_axis_base + 4)?);
            let coord = coords
                .get(axis)
                .map(|c| Fixed::from_f2dot14(*c))
                .unwrap_or(ZERO);
            if start > peak || peak > end || peak == ZERO || start < ZERO && end > ZERO {
                continue;
            } else if coord < start || coord > end {
                scalar = ZERO;
                break;
            } else if coord == peak {
                continue;
            } else if coord < peak {
                scalar = scalar * (coord - start) / (peak - start);
            } else {
                scalar = scalar * (end - coord) / (end - peak);
            };
        }
        let val = if idx >= short_count {
            delta_base += 1;
            b.read::<i8>(delta_base - 1)? as i16
        } else {
            delta_base += 2;
            b.read::<i16>(delta_base - 2)?
        };
        idx += 1;
        delta += scalar * Fixed::from_i32(val as i32);
    }
    Some(delta)
}

#[inline(always)]
fn metric_delta(
    data: &[u8],
    base: u32,
    which: usize,
    glyph_id: u16,
    coords: &[i16],
) -> Option<Fixed> {
    if base == 0 {
        return None;
    }
    let b = Bytes::new(data);
    let mut store = b.read::<u32>(base as usize + 4)?;
    if store == 0 {
        return None;
    }
    store += base;
    let mut offset = b.read::<u32>(base as usize + which)? as usize;
    if offset == 0 {
        if which == 8 {
            return item_delta(data, store, 0, glyph_id, coords);
        } else {
            return None;
        }
    }
    offset += base as usize;
    let format = b.read::<u16>(offset)? as u32;
    let count = b.read::<u16>(offset + 2)?;
    let bit_count = (format & 0xF) + 1;
    let entry_size = ((format & 0x30) >> 4) + 1;
    let base = offset + 4;
    let index = if glyph_id >= count {
        count - 1
    } else {
        glyph_id
    } as usize;
    let entry = match entry_size {
        1 => b.read::<u8>(base + index)? as u32,
        2 => b.read::<u16>(base + index * 2)? as u32,
        3 => b.read::<U24>(base + index * 3)?.0,
        4 => b.read::<u32>(base + index * 4)?,
        _ => return None,
    };
    let outer = entry >> bit_count;
    let inner = entry & ((1 << bit_count) - 1);
    item_delta(data, store, outer as u16, inner as u16, coords)
}

/// Tags for metrics from the `MVAR` table.
pub mod mvar_tags {
    use super::{raw_tag, RawTag};

    /// Horizontal ascender.
    pub const HASC: RawTag = raw_tag(b"hasc");
    /// Horizontal descender.
    pub const HDSC: RawTag = raw_tag(b"hdsc");
    /// Horizontal line gap.
    pub const HLGP: RawTag = raw_tag(b"hlgp");

    /// Horizontal caret rise.
    pub const HCRS: RawTag = raw_tag(b"hcrs");
    /// Horizontal caret run.
    pub const HCRN: RawTag = raw_tag(b"hcrn");
    /// Horizontal caret offset.
    pub const HCOF: RawTag = raw_tag(b"hcof");

    /// Horizontal clipping ascent.
    pub const HCLA: RawTag = raw_tag(b"hcla");
    /// Horizontal clipping descent.
    pub const HCLD: RawTag = raw_tag(b"hcld");

    /// Vertical ascender.
    pub const VASC: RawTag = raw_tag(b"vasc");
    /// Vertical descender.
    pub const VDSC: RawTag = raw_tag(b"vdsc");
    /// Vertical line gap.
    pub const VLGP: RawTag = raw_tag(b"vlgp");

    /// Vertical caret rise.
    pub const VCRS: RawTag = raw_tag(b"vcrs");
    /// Vertical caret run.
    pub const VCRN: RawTag = raw_tag(b"vcrn");
    /// Vertical caret offset.
    pub const VCOF: RawTag = raw_tag(b"vcof");

    /// X-height.
    pub const XHGT: RawTag = raw_tag(b"xhgt");
    /// Cap height.
    pub const CPHT: RawTag = raw_tag(b"cpht");

    /// Underline offset.
    pub const UNDO: RawTag = raw_tag(b"undo");
    /// Underline size.
    pub const UNDS: RawTag = raw_tag(b"unds");

    /// Strikeout offset.
    pub const STRO: RawTag = raw_tag(b"stro");
    /// Strikeout size.
    pub const STRS: RawTag = raw_tag(b"strs");

    /// Subscript x-offset.
    pub const SBXO: RawTag = raw_tag(b"sbxo");
    /// Subscript y-offset.
    pub const SBYO: RawTag = raw_tag(b"sbyo");
    /// Subscript x-size.
    pub const SBXS: RawTag = raw_tag(b"sbxs");
    /// Subscript y-size.
    pub const SBYS: RawTag = raw_tag(b"sbys");

    /// Superscript x-offset.
    pub const SPXO: RawTag = raw_tag(b"spxo");
    /// Superscript y-offset.
    pub const SPYO: RawTag = raw_tag(b"spyo");
    /// Superscript x-size.
    pub const SPXS: RawTag = raw_tag(b"spxs");
    /// Superscript y-size.
    pub const SPYS: RawTag = raw_tag(b"spys");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds an avar table with the specified version and the given
    /// number of identity (empty) segment maps. If `v2` is provided,
    /// appends the axis index map and item variation store offsets along
    /// with the associated data.
    fn build_avar(major: u16, minor: u16, axis_count: u16, v2: Option<(u32, u32, Vec<u8>)>) -> Vec<u8> {
        let mut data = Vec::new();
        data.extend_from_slice(&major.to_be_bytes());
        data.extend_from_slice(&minor.to_be_bytes());
        data.extend_from_slice(&[0; 2]); // reserved
        data.extend_from_slice(&axis_count.to_be_bytes());
        for _ in 0..axis_count {
            data.extend_from_slice(&[0; 2]); // positionMapCount = 0
        }
        if let Some((map_offset, store_offset, extra)) = v2 {
            data.extend_from_slice(&map_offset.to_be_bytes());
            data.extend_from_slice(&store_offset.to_be_bytes());
            data.extend_from_slice(&extra);
        }
        data
    }

    /// Delta set index map (format 1, two byte entries with 8 inner bits)
    /// mapping axes to the specified (outer, inner) delta set indices.
    fn build_index_map(entries: &[(u16, u16)]) -> Vec<u8> {
        let mut data = Vec::new();
        data.push(1); // format
        data.push(0x17); // entryFormat: 2 byte entries, 8 inner bits
        data.extend_from_slice(&(entries.len() as u16).to_be_bytes());
        for (outer, inner) in entries {
            data.extend_from_slice(&(outer << 8 | inner).to_be_bytes());
        }
        data
    }

    /// Item variation store with a single region:
    /// axis 0: start=0, peak=1, end=1; axis 1: peak=0 (neutral).
    /// Contains a single variation data subtable with the specified
    /// i16 (short) delta values, one item per value.
    fn build_var_store(deltas: &[i16]) -> Vec<u8> {
        let mut data = Vec::new();
        data.extend_from_slice(&1u16.to_be_bytes()); // format
        data.extend_from_slice(&12u32.to_be_bytes()); // region list offset
        data.extend_from_slice(&1u16.to_be_bytes()); // data count
        data.extend_from_slice(&28u32.to_be_bytes()); // data offset[0]
        // Variation region list at offset 12.
        data.extend_from_slice(&2u16.to_be_bytes()); // axis count
        data.extend_from_slice(&1u16.to_be_bytes()); // region count
        data.extend_from_slice(&[0, 0, 0x40, 0, 0x40, 0]); // axis 0: 0, 1, 1
        data.extend_from_slice(&[0; 6]); // axis 1: 0, 0, 0
        // Item variation data at offset 28.
        data.extend_from_slice(&(deltas.len() as u16).to_be_bytes()); // item count
        data.extend_from_slice(&1u16.to_be_bytes()); // short delta count
        data.extend_from_slice(&1u16.to_be_bytes()); // region index count
        data.extend_from_slice(&0u16.to_be_bytes()); // region index[0]
        for delta in deltas {
            data.extend_from_slice(&delta.to_be_bytes());
        }
        data
    }

    /// The avar2 test table: two axes, explicit index map [(0,0), (0,1)]
    /// and a var store with deltas [8192, -4096] selected by axis.
    /// Returns the font data and the offset of the avar table within it.
    fn avar2_table() -> (Vec<u8>, u32) {
        let map = build_index_map(&[(0, 0), (0, 1)]);
        let store = build_var_store(&[8192, -4096]);
        let mut extra = map;
        extra.extend_from_slice(&store);
        // Header (8) + two empty segment maps (4) + two offsets (8) = 20.
        embed(build_avar(2, 0, 2, Some((20, 28, extra))))
    }

    /// Places a table at a non-zero offset within a larger buffer to
    /// exercise table relative offsets.
    fn embed(table: Vec<u8>) -> (Vec<u8>, u32) {
        let mut data = vec![0xFF; 16];
        data.extend_from_slice(&table);
        (data, 16)
    }

    #[test]
    fn avar2_v1_and_default_coords_unchanged() {
        let (data, avar) = avar2_table();
        let coords = &[8192i16, -2048];
        // No avar table.
        let mut c = *coords;
        assert_eq!(apply_avar2(&data, 0, &mut c), Ok(()));
        assert_eq!(&c, coords);
        // avar version 1 table.
        let (v1, v1_offset) = embed(build_avar(1, 0, 2, None));
        let mut c = *coords;
        assert_eq!(apply_avar2(&v1, v1_offset, &mut c), Ok(()));
        assert_eq!(&c, coords);
        // avar2 with null var store offset.
        let (no_store, no_store_offset) = embed(build_avar(
            2,
            0,
            2,
            Some((20, 0, build_index_map(&[(0, 0), (0, 1)]))),
        ));
        let mut c = *coords;
        assert_eq!(apply_avar2(&no_store, no_store_offset, &mut c), Ok(()));
        assert_eq!(&c, coords);
        // Default (zero) coordinates produce zero deltas.
        let mut c = [0i16; 2];
        assert_eq!(apply_avar2(&data, avar, &mut c), Ok(()));
        assert_eq!(c, [0; 2]);
        // More than MAX_AVAR2_AXES coordinates are left unchanged.
        let mut c = [1i16; MAX_AVAR2_AXES + 1];
        assert_eq!(apply_avar2(&data, avar, &mut c), Ok(()));
        assert_eq!(c, [1; MAX_AVAR2_AXES + 1]);
    }

    #[test]
    fn avar2_applies_cross_axis_deltas() {
        let (data, avar) = avar2_table();
        // coords = [0.5, 0]. Region scalar = 0.5 (from axis 0). Axis 0
        // delta = 0.5 * 0.5 = 0.25; axis 1 delta = 0.5 * -0.25 = -0.125.
        let mut coords = [8192i16, 0];
        assert_eq!(apply_avar2(&data, avar, &mut coords), Ok(()));
        assert_eq!(coords, [12288, -2048]);
        // Repeated evaluation of the same coordinates is stable.
        let mut again = [8192i16, 0];
        assert_eq!(apply_avar2(&data, avar, &mut again), Ok(()));
        assert_eq!(coords, again);
        // Results are clamped to [-1, 1]: axis 0 = 1 + 0.5 -> 1.
        let mut coords = [16384i16, 16384];
        assert_eq!(apply_avar2(&data, avar, &mut coords), Ok(()));
        assert_eq!(coords, [16384, 12288]);
    }

    #[test]
    fn avar2_identity_map_when_absent() {
        let store = build_var_store(&[8192, -4096]);
        // Null axis index map offset: identity mapping (0, axis).
        let (data, avar) = embed(build_avar(2, 0, 2, Some((0, 20, store))));
        let mut coords = [8192i16, 0];
        assert_eq!(apply_avar2(&data, avar, &mut coords), Ok(()));
        assert_eq!(coords, [12288, -2048]);
    }

    #[test]
    fn avar2_errors_are_distinguishable() {
        let (data, avar) = avar2_table();
        let mut coords = [8192i16, 0];
        // Unsupported versions.
        for (major, minor) in [(3, 0), (2, 1), (0, 0)] {
            let (t, offset) = embed(build_avar(major, minor, 2, None));
            assert_eq!(
                apply_avar2(&t, offset, &mut coords),
                Err(Avar2Error::UnsupportedVersion)
            );
        }
        // Var store offset out of bounds.
        let map = build_index_map(&[(0, 0), (0, 1)]);
        let (t, offset) = embed(build_avar(2, 0, 2, Some((20, 0x7FFF_FF00, map))));
        assert_eq!(
            apply_avar2(&t, offset, &mut coords),
            Err(Avar2Error::VarStoreOutOfBounds)
        );
        // Truncated table (offsets not readable).
        let mut t = build_avar(2, 0, 2, None);
        t.truncate(10);
        let (t, offset) = embed(t);
        assert_eq!(
            apply_avar2(&t, offset, &mut coords),
            Err(Avar2Error::VarStoreOutOfBounds)
        );
        // Map with fewer entries than axes.
        let map = build_index_map(&[(0, 0)]);
        let mut extra = map;
        extra.extend_from_slice(&build_var_store(&[8192, -4096]));
        let (t, offset) = embed(build_avar(2, 0, 2, Some((20, 26, extra))));
        assert_eq!(
            apply_avar2(&t, offset, &mut coords),
            Err(Avar2Error::MissingMapping)
        );
        // Map entry referencing a nonexistent outer delta set index.
        let map = build_index_map(&[(5, 0), (0, 1)]);
        let mut extra = map;
        extra.extend_from_slice(&build_var_store(&[8192, -4096]));
        let (t, offset) = embed(build_avar(2, 0, 2, Some((20, 28, extra))));
        assert_eq!(
            apply_avar2(&t, offset, &mut coords),
            Err(Avar2Error::VarStoreOutOfBounds)
        );
        // Coordinates are never modified when an error occurs.
        assert_eq!(coords, [8192, 0]);
        // Sanity: the well formed table still evaluates.
        assert_eq!(apply_avar2(&data, avar, &mut coords), Ok(()));
        assert_eq!(coords, [12288, -2048]);
    }
}
