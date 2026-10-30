//! Embedded Tabler and Material Design interface icons for trusted chrome
//! text.
//!
//! One subset face holds icons from Tabler's outline webfont and, for the
//! `-filled` current-row markers, its filled webfont, plus the three agent
//! access robots from the Material Design Icons webfont. The face is compiled
//! into the executable, copied once into a sealed mapping, and rasterized
//! through the same `FreeType` grayscale path as chrome text. Icons are placed only by trusted interface code through
//! [`super::ChromeSource`]; their assigned codepoints, mostly
//! private-use, are never interpreted in labels, titles, agent detail, or
//! terminal content. Provenance, hashes,
//! and the licenses are in `crates/splinterm/assets/icons/`.

use std::{
    cell::RefCell,
    collections::HashMap,
    sync::{Arc, OnceLock},
};

use splinterm_filemap::ReadOnlyFileMap;
use splinterm_freetype::{MAX_PIXEL_SIZE_26_6, MIN_PIXEL_SIZE_26_6, RasterFace};
use swash::{FontRef, scale::image::Content};

use super::{CachedGlyph, round_to_i32, u32_to_f32};
use crate::config::ExplorerCurrentMarker;

static ICON_FONT: &[u8] = include_bytes!("../../assets/icons/tabler-icons-subset.ttf");

/// Glyph-cache face for icons in chrome frames; terminal frames never use it.
pub(super) const ICON_FACE: usize = usize::MAX - 1;

/// Bounds retained rasters across font generations and output scales.
const ICON_RASTER_BUDGET: usize = 256;

/// One icon from the embedded subset, named after its Tabler or Material
/// Design icon.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) enum ChromeIcon {
    MessageQuestion,
    AlertCircle,
    CircleCheck,
    Point,
    Clock,
    ChevronRight,
    ChevronDown,
    /// A saved Lair.
    Pin,
    /// A pinned Lair.
    Pinned,
    /// An agent can reach the row (Material Design `robot-outline`).
    RobotOutline,
    /// An agent is connected to the row (Material Design `robot`).
    Robot,
    /// An agent is connected, but nothing covers the row (Material Design
    /// `robot-off-outline`).
    RobotOffOutline,
    /// An agent is asking for access.
    LockQuestion,
    // The `[explorer] current-marker` choices; the last five come from the
    // filled face.
    Eye,
    CurrentLocation,
    Focus2,
    Crosshair,
    ArrowNarrowRight,
    CornerDownRight,
    CaretRightFilled,
    ArrowBadgeRightFilled,
    PlayerPlayFilled,
    MapPinFilled,
    CircleDotFilled,
}

impl ChromeIcon {
    pub(crate) const ALL: [Self; 24] = [
        Self::MessageQuestion,
        Self::AlertCircle,
        Self::CircleCheck,
        Self::Point,
        Self::Clock,
        Self::ChevronRight,
        Self::ChevronDown,
        Self::Pin,
        Self::Pinned,
        Self::RobotOutline,
        Self::Robot,
        Self::RobotOffOutline,
        Self::LockQuestion,
        Self::Eye,
        Self::CurrentLocation,
        Self::Focus2,
        Self::Crosshair,
        Self::ArrowNarrowRight,
        Self::CornerDownRight,
        Self::CaretRightFilled,
        Self::ArrowBadgeRightFilled,
        Self::PlayerPlayFilled,
        Self::MapPinFilled,
        Self::CircleDotFilled,
    ];

    /// Chrome cells one icon occupies. Icons are square, so a two-cell slot
    /// lets them reach the text's ascender height instead of a narrow cell's
    /// width.
    pub(crate) const CELLS: u32 = 2;

    #[cfg(test)]
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::MessageQuestion => "message-question",
            Self::AlertCircle => "alert-circle",
            Self::CircleCheck => "circle-check",
            Self::Point => "point",
            Self::Clock => "clock",
            Self::ChevronRight => "chevron-right",
            Self::ChevronDown => "chevron-down",
            Self::Pin => "pin",
            Self::Pinned => "pinned",
            Self::RobotOutline => "robot-outline",
            Self::Robot => "robot",
            Self::RobotOffOutline => "robot-off-outline",
            Self::LockQuestion => "lock-question",
            Self::Eye => "eye",
            Self::CurrentLocation => "current-location",
            Self::Focus2 => "focus-2",
            Self::Crosshair => "crosshair",
            Self::ArrowNarrowRight => "arrow-narrow-right",
            Self::CornerDownRight => "corner-down-right",
            Self::CaretRightFilled => "caret-right-filled",
            Self::ArrowBadgeRightFilled => "arrow-badge-right-filled",
            Self::PlayerPlayFilled => "player-play-filled",
            Self::MapPinFilled => "map-pin-filled",
            Self::CircleDotFilled => "circle-dot-filled",
        }
    }

    /// Codepoint in `@tabler/icons-webfont` 3.49.0: in its outline face, or
    /// in its filled face for the `-filled` icons. The robots are from
    /// `@mdi/font` 7.4.47, in supplementary private use. No codepoint is in
    /// two faces.
    const fn codepoint(self) -> char {
        match self {
            Self::MessageQuestion => '\u{f98b}',
            Self::AlertCircle => '\u{ea05}',
            Self::CircleCheck => '\u{ea67}',
            Self::Point => '\u{eb0c}',
            Self::Clock => '\u{ea70}',
            Self::ChevronRight => '\u{ea61}',
            Self::ChevronDown => '\u{ea5f}',
            Self::Pin => '\u{ec9c}',
            Self::Pinned => '\u{ed60}',
            Self::RobotOutline => '\u{f167a}',
            Self::Robot => '\u{f06a9}',
            Self::RobotOffOutline => '\u{f167b}',
            Self::LockQuestion => '\u{f931}',
            Self::Eye => '\u{ea9a}',
            Self::CurrentLocation => '\u{ecef}',
            Self::Focus2 => '\u{ebd3}',
            Self::Crosshair => '\u{ec3e}',
            Self::ArrowNarrowRight => '\u{ea1c}',
            Self::CornerDownRight => '\u{ea7d}',
            Self::CaretRightFilled => '\u{fb2c}',
            Self::ArrowBadgeRightFilled => '\u{f7d8}',
            Self::PlayerPlayFilled => '\u{f691}',
            Self::MapPinFilled => '\u{f680}',
            Self::CircleDotFilled => '\u{f705}',
        }
    }
}

impl From<ExplorerCurrentMarker> for ChromeIcon {
    fn from(marker: ExplorerCurrentMarker) -> Self {
        match marker {
            ExplorerCurrentMarker::Eye => Self::Eye,
            ExplorerCurrentMarker::CurrentLocation => Self::CurrentLocation,
            ExplorerCurrentMarker::Focus2 => Self::Focus2,
            ExplorerCurrentMarker::Crosshair => Self::Crosshair,
            ExplorerCurrentMarker::ArrowNarrowRight => Self::ArrowNarrowRight,
            ExplorerCurrentMarker::CornerDownRight => Self::CornerDownRight,
            ExplorerCurrentMarker::CaretRightFilled => Self::CaretRightFilled,
            ExplorerCurrentMarker::ArrowBadgeRightFilled => Self::ArrowBadgeRightFilled,
            ExplorerCurrentMarker::PlayerPlayFilled => Self::PlayerPlayFilled,
            ExplorerCurrentMarker::MapPinFilled => Self::MapPinFilled,
            ExplorerCurrentMarker::CircleDotFilled => Self::CircleDotFilled,
        }
    }
}

fn icon_font() -> Option<&'static Arc<ReadOnlyFileMap>> {
    static FONT: OnceLock<Option<Arc<ReadOnlyFileMap>>> = OnceLock::new();
    FONT.get_or_init(|| {
        ReadOnlyFileMap::sealed_copy("splinterm-icon-font", ICON_FONT)
            .ok()
            .map(Arc::new)
    })
    .as_ref()
}

/// Fraction of the em square above the baseline (hhea ascent / units per em).
fn icon_ascent_ratio(data: &[u8]) -> Option<f32> {
    let metrics = FontRef::from_index(data, 0)?.metrics(&[]);
    (metrics.units_per_em > 0).then(|| metrics.ascent / f32::from(metrics.units_per_em))
}

/// Whether the embedded face loads and maps every icon. When it does not,
/// callers use their ASCII text instead of drawing a replacement glyph.
pub(crate) fn chrome_icons_available() -> bool {
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        icon_font().is_some_and(|data| {
            icon_ascent_ratio(data).is_some()
                && RasterFace::open_memory(Arc::clone(data), 0, MIN_PIXEL_SIZE_26_6).is_ok_and(
                    |face| {
                        ChromeIcon::ALL
                            .iter()
                            .all(|icon| face.glyph_index(icon.codepoint()) != 0)
                    },
                )
        })
    })
}

/// Cell metrics of the chrome frame an icon is placed in.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) struct IconCell {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) ascent: u32,
    pub(super) baseline: i32,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct IconRasterKey {
    generation: u64,
    font_size_bits: u32,
    cell: IconCell,
    icon: ChromeIcon,
}

thread_local! {
    static ICON_RASTERS: RefCell<HashMap<IconRasterKey, Option<Arc<CachedGlyph>>>> =
        RefCell::new(HashMap::new());
}

/// The icon as an alpha mask placed like a chrome glyph at its slot's first
/// column: centred in a slot [`ChromeIcon::CELLS`] cells wide and one cell
/// high, and cropped to it. Cached per font generation, scaled font size, and
/// cell metrics. `None` when the embedded face is unavailable.
pub(super) fn icon_glyph(
    icon: ChromeIcon,
    generation: u64,
    font_size: f32,
    cell: IconCell,
) -> Option<Arc<CachedGlyph>> {
    let key = IconRasterKey {
        generation,
        font_size_bits: font_size.to_bits(),
        cell,
        icon,
    };
    ICON_RASTERS.with(|rasters| {
        let mut rasters = rasters.borrow_mut();
        if let Some(glyph) = rasters.get(&key) {
            return glyph.clone();
        }
        if rasters.len() >= ICON_RASTER_BUDGET {
            rasters.clear();
        }
        let glyph = rasterize_icon(icon, cell).map(Arc::new);
        rasters.insert(key, glyph.clone());
        glyph
    })
}

fn rasterize_icon(icon: ChromeIcon, cell: IconCell) -> Option<CachedGlyph> {
    if !chrome_icons_available() {
        return None;
    }
    let data = icon_font()?;
    let slot_width = cell.width.checked_mul(ChromeIcon::CELLS)?;
    // Tabler draws within 20 of its 24 em units, so an em of 6/5 of the text
    // ascent puts the icon's ink on the ascender height, bounded by the slot.
    // Chevrons and the filled caret fill only 14 of the 24 units, and the
    // badge arrow 12, so they are drawn a third larger to read at a similar
    // size, with an em up to 3/2 of the slot that still keeps their ink
    // inside it.
    let em = cell.ascent.saturating_mul(6) / 5;
    let em = match icon {
        ChromeIcon::ChevronRight
        | ChromeIcon::ChevronDown
        | ChromeIcon::CaretRightFilled
        | ChromeIcon::ArrowBadgeRightFilled => {
            (em.saturating_mul(4) / 3).min(slot_width.min(cell.height).saturating_mul(3) / 2)
        }
        _ => em.min(cell.height).min(slot_width),
    };
    let pixel_size = isize::try_from(em).ok()?.checked_mul(64)?;
    let mut face = RasterFace::open_memory(
        Arc::clone(data),
        0,
        pixel_size.clamp(MIN_PIXEL_SIZE_26_6, MAX_PIXEL_SIZE_26_6),
    )
    .ok()?;
    let glyph_id = face.glyph_index(icon.codepoint());
    if glyph_id == 0 {
        return None;
    }
    let raster = face.rasterize_gray(glyph_id).ok()?;
    let ascent = round_to_i32(icon_ascent_ratio(data)? * u32_to_f32(em));
    let em = i32::try_from(em).ok()?;
    let slot_width = i32::try_from(slot_width).ok()?;
    let cell_height = i32::try_from(cell.height).ok()?;
    // Centre the whole em square, not the ink, so partial shapes such as the
    // pin keep the same optical centre as the other icons.
    let pen_x = (slot_width - em).div_euclid(2);
    let baseline = (cell_height - em).div_euclid(2) + ascent;
    let left = pen_x + raster.left;
    let top = baseline - raster.top;
    let width = i32::try_from(raster.width).ok()?;
    let height = i32::try_from(raster.height).ok()?;
    let (x0, x1) = (left.max(0), (left + width).min(slot_width));
    let (y0, y1) = (top.max(0), (top + height).min(cell_height));
    let cropped_width = u32::try_from(x1 - x0).ok().filter(|width| *width > 0)?;
    let cropped_height = u32::try_from(y1 - y0).ok().filter(|height| *height > 0)?;
    let mut data = Vec::with_capacity(usize::try_from(cropped_width * cropped_height).ok()?);
    for y in y0..y1 {
        let row = usize::try_from((y - top) * width).ok()?;
        let start = row + usize::try_from(x0 - left).ok()?;
        let end = row + usize::try_from(x1 - left).ok()?;
        data.extend_from_slice(raster.alpha.get(start..end)?);
    }
    Some(CachedGlyph {
        content: Content::Mask,
        left: x0,
        // Chrome glyphs are drawn at `baseline - top` below the cell origin.
        top: cell.baseline - y0,
        width: cropped_width,
        height: cropped_height,
        data,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Representative chrome cells for a 14 px UI font at scales 1, 1.25,
    /// 1.5, and 2, plus a narrow-cell face.
    const CELLS: [IconCell; 5] = [
        IconCell {
            width: 8,
            height: 17,
            ascent: 13,
            baseline: 13,
        },
        IconCell {
            width: 10,
            height: 21,
            ascent: 16,
            baseline: 16,
        },
        IconCell {
            width: 12,
            height: 25,
            ascent: 19,
            baseline: 19,
        },
        IconCell {
            width: 16,
            height: 34,
            ascent: 26,
            baseline: 26,
        },
        IconCell {
            width: 5,
            height: 20,
            ascent: 18,
            baseline: 16,
        },
    ];

    #[test]
    fn embedded_subset_loads_and_maps_every_icon_to_a_distinct_glyph() {
        assert!(chrome_icons_available());
        let data = icon_font().unwrap();
        assert_eq!(&***data, ICON_FONT);
        let face = RasterFace::open_memory(Arc::clone(data), 0, 16 * 64).unwrap();
        let mut glyphs = ChromeIcon::ALL
            .map(|icon| face.glyph_index(icon.codepoint()))
            .to_vec();
        assert!(glyphs.iter().all(|glyph| *glyph != 0));
        glyphs.sort_unstable();
        glyphs.dedup();
        assert_eq!(glyphs.len(), ChromeIcon::ALL.len());
        // Text characters are not in the icon face, nor are the stars and
        // bookmarks of either Tabler face, kept for other uses.
        for absent in ['?', '\u{eb2e}', '\u{ea3a}', '\u{f6a6}', '\u{fa88}'] {
            assert_eq!(face.glyph_index(absent), 0, "{absent:?}");
        }
    }

    #[test]
    fn every_current_marker_is_its_own_icon_under_its_tabler_name() {
        let icons = ExplorerCurrentMarker::ALL.map(ChromeIcon::from);
        for (marker, icon) in ExplorerCurrentMarker::ALL.into_iter().zip(icons) {
            assert_eq!(marker.name(), icon.name());
            assert!(ChromeIcon::ALL.contains(&icon), "{icon:?}");
        }
        let mut distinct = icons.to_vec();
        distinct.sort_unstable();
        distinct.dedup();
        assert_eq!(distinct.len(), ExplorerCurrentMarker::ALL.len());
        assert_eq!(
            ChromeIcon::from(ExplorerCurrentMarker::default()),
            ChromeIcon::Eye
        );
    }

    #[test]
    fn icon_rasters_stay_inside_their_slot_and_reach_the_ascender_height() {
        for cell in CELLS {
            for icon in ChromeIcon::ALL {
                let glyph = icon_glyph(icon, 7, 14.0, cell).unwrap();
                let slot_width = cell.width * ChromeIcon::CELLS;
                let top = cell.baseline - glyph.top;
                assert!(glyph.left >= 0 && top >= 0, "{icon:?} {cell:?}");
                assert!(
                    glyph.left + i32::try_from(glyph.width).unwrap()
                        <= i32::try_from(slot_width).unwrap()
                        && top + i32::try_from(glyph.height).unwrap()
                            <= i32::try_from(cell.height).unwrap(),
                    "{icon:?} leaves its slot at {cell:?}"
                );
                assert_eq!(glyph.data.len(), (glyph.width * glyph.height) as usize);
                assert!(glyph.data.iter().any(|alpha| *alpha != 0));
                let full_size = !matches!(
                    icon,
                    ChromeIcon::Point | ChromeIcon::ChevronRight | ChromeIcon::ChevronDown
                );
                if full_size && cell.width * 2 >= cell.ascent {
                    // Status icons are about as tall as the text ascent.
                    let bounds = glyph.ink_bounds().unwrap();
                    let ink = (bounds.bottom - bounds.top).max(bounds.right - bounds.left);
                    assert!(
                        ink * 10 >= cell.ascent * 7 && ink <= cell.height,
                        "{icon:?} is {ink}px at {cell:?}"
                    );
                }
                let chevron = matches!(
                    icon,
                    ChromeIcon::ChevronRight
                        | ChromeIcon::ChevronDown
                        | ChromeIcon::CaretRightFilled
                        | ChromeIcon::ArrowBadgeRightFilled
                );
                if chevron && cell.width * 2 * 5 >= cell.ascent * 8 {
                    // Enlarged chevrons read at over half of the ascent.
                    let bounds = glyph.ink_bounds().unwrap();
                    let ink = (bounds.bottom - bounds.top).max(bounds.right - bounds.left);
                    assert!(ink * 2 > cell.ascent, "{icon:?} is {ink}px at {cell:?}");
                }
            }
        }
    }

    /// Tabler draws each icon centred in its advance, and so does the face:
    /// a glyph whose left side bearing disagrees with its outline's left
    /// edge is drawn shifted, and cropped at the slot. Symmetric icons keep
    /// equal ink margins in their slot to within a pixel, and the filled
    /// caret, whose outline sits 1/24 em right of centre, to within two.
    #[test]
    fn icons_are_centred_in_their_slot_and_not_cropped() {
        for cell in CELLS {
            let slot_width = i32::try_from(cell.width * ChromeIcon::CELLS).unwrap();
            for (icon, slack) in [
                (ChromeIcon::Eye, 1),
                (ChromeIcon::Robot, 1),
                (ChromeIcon::RobotOutline, 1),
                (ChromeIcon::RobotOffOutline, 1),
                (ChromeIcon::ChevronDown, 1),
                (ChromeIcon::ChevronRight, 1),
                (ChromeIcon::AlertCircle, 1),
                (ChromeIcon::CircleDotFilled, 1),
                (ChromeIcon::ArrowBadgeRightFilled, 1),
                (ChromeIcon::CaretRightFilled, 2),
            ] {
                let glyph = icon_glyph(icon, 7, 14.0, cell).unwrap();
                let bounds = glyph.ink_bounds().unwrap();
                let left = glyph.left + i32::try_from(bounds.left).unwrap();
                let right = slot_width - glyph.left - i32::try_from(bounds.right).unwrap();
                assert!(
                    (left - right).abs() <= slack,
                    "{icon:?} has margins {left} and {right} px at {cell:?}"
                );
            }
        }
    }

    #[test]
    fn icon_rasters_are_cached_per_generation_size_and_cell() {
        let cell = CELLS[0];
        let first = icon_glyph(ChromeIcon::Clock, 1, 14.0, cell).unwrap();
        assert!(Arc::ptr_eq(
            &first,
            &icon_glyph(ChromeIcon::Clock, 1, 14.0, cell).unwrap()
        ));
        assert!(!Arc::ptr_eq(
            &first,
            &icon_glyph(ChromeIcon::Clock, 2, 14.0, cell).unwrap()
        ));
        let larger = icon_glyph(ChromeIcon::Clock, 1, 28.0, CELLS[3]).unwrap();
        assert!(larger.width > first.width);
    }
}
