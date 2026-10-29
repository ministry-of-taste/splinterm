//! Narrow safe-Rust translation of Foot's terminal box-drawing geometry.
//!
//! Source: Foot 1.27.0 `box-drawing.c` at commit
//! `3c5b584b0eafa772eb4376fb6eaf6643399e190e` (MIT). U+2500,
//! U+250C, U+2510, U+253C, and the U+2800–U+28FF Braille range are translated.
//! The integer midpoint and Braille dot formulas intentionally match Foot's
//! helpers. The common light-line set is generated across complete cell edges
//! so adjacent terminal cells cannot expose rasterizer-bearing gaps.

/// An 8-bit alpha mask occupying one complete terminal cell.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct BoxMask {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) data: Vec<u8>,
}

/// Returns Foot's default scale- and cell-relative light-line thickness.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "bounded positive Foot formula intentionally truncates before clamping"
)]
pub(crate) fn default_thickness(width: u32, height: u32, scale_120: u16) -> u32 {
    if width == 0 || height == 0 || scale_120 == 0 {
        return 1;
    }
    let width = f64::from(width);
    let height = f64::from(height);
    let scale = f64::from(scale_120) / 120.0;
    ((0.04 * scale * width.hypot(height) * 96.0 / 72.0) as u32).max(1)
}

/// Draws a Braille character for trusted chrome, such as the agent spinner:
/// the same eight dot positions as [`generate`], but with round, smoothed
/// dots somewhat larger than Foot's square terminal dots, so a lone spinner
/// frame reads beside the text without turning into a block. Terminal cells
/// always use [`generate`]. `None` outside U+2800–U+28FF or for a cell too
/// small for a one-pixel dot.
pub(crate) fn chrome_braille(character: char, width: u32, height: u32) -> Option<BoxMask> {
    // Geometry is in sixteenths of a pixel; each pixel samples a 4x4 grid.
    const UNIT: i64 = 16;
    if !('\u{2800}'..='\u{28ff}').contains(&character) {
        return None;
    }
    let dots = u8::try_from(u32::from(character) - 0x2800).ok()?;
    let (cell_width, cell_height) = (i64::from(width), i64::from(height));
    // Four rows on an even pitch with margins above and below; two columns
    // no further apart than the rows. A dot's radius is about 0.38 of the
    // pitch, so neighbouring dots keep a clear gap.
    let row_pitch = cell_height * UNIT * 10 / 46;
    let column_pitch = row_pitch.min(cell_width * UNIT * 10 / 23);
    let radius = (row_pitch * 38 / 100).min(column_pitch * 40 / 100);
    if radius < UNIT / 2 {
        return None;
    }
    let left = (cell_width * UNIT - column_pitch) / 2;
    let top = (cell_height * UNIT - 3 * row_pitch) / 2;
    let mut mask = BoxMask {
        width,
        height,
        data: vec![0; usize::try_from(width.checked_mul(height)?).ok()?],
    };
    // Bit order as in `draw_braille`: dots 1-3 and 7 down the left column,
    // 4-6 and 8 down the right.
    let positions = [
        (0_u32, 0_u32, 0_u8),
        (0, 1, 1),
        (0, 2, 2),
        (1, 0, 3),
        (1, 1, 4),
        (1, 2, 5),
        (0, 3, 6),
        (1, 3, 7),
    ];
    for (column, row, bit) in positions {
        if dots & (1 << bit) == 0 {
            continue;
        }
        let centre_x = left + i64::from(column) * column_pitch;
        let centre_y = top + i64::from(row) * row_pitch;
        mask.fill_disc(centre_x, centre_y, radius, UNIT);
    }
    Some(mask)
}

/// Generates a translated box or Braille glyph.
pub(crate) fn generate(
    character: char,
    width: u32,
    height: u32,
    thickness: u32,
) -> Option<BoxMask> {
    if width == 0 || height == 0 || thickness == 0 {
        return None;
    }
    let mut mask = BoxMask {
        width,
        height,
        data: vec![0; usize::try_from(width.checked_mul(height)?).ok()?],
    };
    if let Some((x, y, rect_width, rect_height)) = opaque_block_rect(character, width, height) {
        mask.fill(x, y, x + rect_width, y + rect_height);
        return Some(mask);
    }
    if ('\u{2800}'..='\u{28ff}').contains(&character) {
        mask.draw_braille(u8::try_from(u32::from(character) - 0x2800).ok()?);
        return Some(mask);
    }
    match character {
        '░' => mask.draw_shade(1),
        '▒' => mask.draw_shade(2),
        '▓' => mask.draw_shade(3),
        _ => {}
    }
    if matches!(character, '░' | '▒' | '▓') {
        return Some(mask);
    }
    let horizontal_y = height.saturating_sub(thickness) / 2;
    let vertical_x = width.saturating_sub(thickness) / 2;
    let right_of_center = vertical_x.saturating_add(thickness);
    let below_center = horizontal_y.saturating_add(thickness);
    match character {
        '─' => mask.horizontal(0, width, horizontal_y, thickness),
        '│' => mask.vertical(0, height, vertical_x, thickness),
        '┌' => {
            mask.horizontal(vertical_x, width, horizontal_y, thickness);
            mask.vertical(horizontal_y, height, vertical_x, thickness);
        }
        '┐' => {
            mask.horizontal(0, right_of_center, horizontal_y, thickness);
            mask.vertical(horizontal_y, height, vertical_x, thickness);
        }
        '└' => {
            mask.horizontal(vertical_x, width, horizontal_y, thickness);
            mask.vertical(0, below_center, vertical_x, thickness);
        }
        '┘' => {
            mask.horizontal(0, right_of_center, horizontal_y, thickness);
            mask.vertical(0, below_center, vertical_x, thickness);
        }
        '╭' => mask.rounded_corner(RoundedCorner::TopLeft, vertical_x, horizontal_y, thickness),
        '╮' => mask.rounded_corner(RoundedCorner::TopRight, vertical_x, horizontal_y, thickness),
        '╰' => mask.rounded_corner(
            RoundedCorner::BottomLeft,
            vertical_x,
            horizontal_y,
            thickness,
        ),
        '╯' => mask.rounded_corner(
            RoundedCorner::BottomRight,
            vertical_x,
            horizontal_y,
            thickness,
        ),
        '├' => {
            mask.horizontal(vertical_x, width, horizontal_y, thickness);
            mask.vertical(0, height, vertical_x, thickness);
        }
        '┤' => {
            mask.horizontal(0, right_of_center, horizontal_y, thickness);
            mask.vertical(0, height, vertical_x, thickness);
        }
        '┬' => {
            mask.horizontal(0, width, horizontal_y, thickness);
            mask.vertical(horizontal_y, height, vertical_x, thickness);
        }
        '┴' => {
            mask.horizontal(0, width, horizontal_y, thickness);
            mask.vertical(0, below_center, vertical_x, thickness);
        }
        '┼' => {
            mask.horizontal(0, width, horizontal_y, thickness);
            mask.vertical(0, height, vertical_x, thickness);
        }
        _ => return None,
    }
    Some(mask)
}

/// Returns the exact opaque rectangle represented by a solid block element.
/// Coordinates are relative to the complete cell mask generated above.
pub(crate) fn opaque_block_rect(
    character: char,
    width: u32,
    height: u32,
) -> Option<(u32, u32, u32, u32)> {
    let lower_extent = |eighths: u64| {
        u32::try_from((u64::from(height) * eighths + 4) / 8)
            .expect("rounded fractional block extent fits u32")
    };
    match character {
        '▀' => Some((0, 0, width, height.div_ceil(2))),
        '▁' => Some((0, height - lower_extent(1), width, lower_extent(1))),
        '▂' => Some((0, height - lower_extent(2), width, lower_extent(2))),
        '▃' => Some((0, height - lower_extent(3), width, lower_extent(3))),
        '▄' => Some((0, height - lower_extent(4), width, lower_extent(4))),
        '▅' => Some((0, height - lower_extent(5), width, lower_extent(5))),
        '▆' => Some((0, height - lower_extent(6), width, lower_extent(6))),
        '▇' => Some((0, height - lower_extent(7), width, lower_extent(7))),
        '█' => Some((0, 0, width, height)),
        '▌' => Some((0, 0, width.div_ceil(2), height)),
        '▐' => Some((width / 2, 0, width - width / 2, height)),
        _ => None,
    }
}

#[derive(Clone, Copy)]
enum RoundedCorner {
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

impl BoxMask {
    fn draw_shade(&mut self, level: u32) {
        // A stable 2×2 ordered pattern gives 25%, 50%, and 75% coverage while
        // using the entire cell; font bearings must not introduce edge gaps.
        const BAYER: [[u32; 2]; 2] = [[0, 2], [3, 1]];
        for y in 0..self.height {
            for x in 0..self.width {
                if BAYER[(y % 2) as usize][(x % 2) as usize] < level {
                    let index = usize::try_from(y * self.width + x).expect("shade index fits");
                    self.data[index] = 0xff;
                }
            }
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the adjustment order is a direct translation of Foot's Braille geometry"
    )]
    fn draw_braille(&mut self, dots: u8) {
        let width = i32::try_from(self.width).expect("cell width fits i32");
        let height = i32::try_from(self.height).expect("cell height fits i32");
        let mut dot = (width / 4).min(height / 8);
        let mut x_spacing = width / 4;
        let mut y_spacing = height / 8;
        let mut x_margin = x_spacing / 2;
        let mut y_margin = y_spacing / 2;
        let mut x_left = width - 2 * x_margin - x_spacing - 2 * dot;
        let mut y_left = height - 2 * y_margin - 3 * y_spacing - 4 * dot;

        if x_left >= 2 && y_left >= 4 && dot == 0 {
            dot += 1;
            x_left -= 2;
            y_left -= 4;
        }
        if x_left >= 2 && x_margin == 0 {
            x_margin = 1;
            x_left -= 2;
        }
        if y_left >= 2 && y_margin == 0 {
            y_margin = 1;
            y_left -= 2;
        }
        if x_left >= 1 {
            x_spacing += 1;
            x_left -= 1;
        }
        if y_left >= 3 {
            y_spacing += 1;
            y_left -= 3;
        }
        if x_left >= 2 {
            x_margin += 1;
            x_left -= 2;
        }
        if y_left >= 2 {
            y_margin += 1;
            y_left -= 2;
        }
        if x_left >= 2 && y_left >= 4 {
            dot += 1;
        }
        if dot <= 0 {
            return;
        }

        let xs = [x_margin, x_margin + dot + x_spacing];
        let ys = [
            y_margin,
            y_margin + dot + y_spacing,
            y_margin + 2 * (dot + y_spacing),
            y_margin + 3 * (dot + y_spacing),
        ];
        let positions = [
            (0_usize, 0_usize, 0_u8),
            (0, 1, 1),
            (0, 2, 2),
            (1, 0, 3),
            (1, 1, 4),
            (1, 2, 5),
            (0, 3, 6),
            (1, 3, 7),
        ];
        for (column, row, bit) in positions {
            if dots & (1 << bit) == 0 {
                continue;
            }
            let x = u32::try_from(xs[column]).expect("Braille x is nonnegative");
            let y = u32::try_from(ys[row]).expect("Braille y is nonnegative");
            let dot = u32::try_from(dot).expect("Braille dot is positive");
            self.fill(x, y, x + dot, y + dot);
        }
    }

    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "bounded rounded-corner samples are clipped to the cell before conversion"
    )]
    fn rounded_corner(
        &mut self,
        corner: RoundedCorner,
        center_x: u32,
        center_y: u32,
        thickness: u32,
    ) {
        let radius = (self.width / 3).min(self.height / 6).max(2);
        match corner {
            RoundedCorner::TopLeft => {
                self.horizontal(center_x + radius, self.width, center_y, thickness);
                self.vertical(center_y + radius, self.height, center_x, thickness);
            }
            RoundedCorner::TopRight => {
                self.horizontal(
                    0,
                    center_x.saturating_sub(radius) + thickness,
                    center_y,
                    thickness,
                );
                self.vertical(center_y + radius, self.height, center_x, thickness);
            }
            RoundedCorner::BottomLeft => {
                self.horizontal(center_x + radius, self.width, center_y, thickness);
                self.vertical(
                    0,
                    center_y.saturating_sub(radius) + thickness,
                    center_x,
                    thickness,
                );
            }
            RoundedCorner::BottomRight => {
                self.horizontal(
                    0,
                    center_x.saturating_sub(radius) + thickness,
                    center_y,
                    thickness,
                );
                self.vertical(
                    0,
                    center_y.saturating_sub(radius) + thickness,
                    center_x,
                    thickness,
                );
            }
        }

        let (origin_x, origin_y, start) = match corner {
            RoundedCorner::TopLeft => (center_x + radius, center_y + radius, std::f64::consts::PI),
            RoundedCorner::TopRight => (
                center_x.saturating_sub(radius),
                center_y + radius,
                -std::f64::consts::FRAC_PI_2,
            ),
            RoundedCorner::BottomLeft => (
                center_x + radius,
                center_y.saturating_sub(radius),
                std::f64::consts::FRAC_PI_2,
            ),
            RoundedCorner::BottomRight => (
                center_x.saturating_sub(radius),
                center_y.saturating_sub(radius),
                0.0,
            ),
        };
        let samples = radius.saturating_mul(8);
        for sample in 0..=samples {
            let angle =
                start + f64::from(sample) / f64::from(samples) * std::f64::consts::FRAC_PI_2;
            let x = f64::from(origin_x) + f64::from(radius) * angle.cos();
            let y = f64::from(origin_y) + f64::from(radius) * angle.sin();
            let x = x.round().max(0.0) as u32;
            let y = y.round().max(0.0) as u32;
            self.fill(
                x,
                y,
                x.saturating_add(thickness),
                y.saturating_add(thickness),
            );
        }
    }

    fn horizontal(&mut self, x1: u32, x2: u32, y: u32, thickness: u32) {
        self.fill(x1, y, x2, y.saturating_add(thickness));
    }

    fn vertical(&mut self, y1: u32, y2: u32, x: u32, thickness: u32) {
        self.fill(x, y1, x.saturating_add(thickness), y2);
    }

    fn fill(&mut self, x1: u32, y1: u32, x2: u32, y2: u32) {
        for y in y1.min(self.height)..y2.min(self.height) {
            for x in x1.min(self.width)..x2.min(self.width) {
                let index =
                    usize::try_from(y * self.width + x).expect("cell mask index fits usize");
                self.data[index] = 0xff;
            }
        }
    }

    /// Adds a smoothed disc, in `unit`ths of a pixel, sampling each pixel it
    /// touches on a 4x4 grid. A pixel keeps its larger coverage.
    fn fill_disc(&mut self, centre_x: i64, centre_y: i64, radius: i64, unit: i64) {
        let step = unit / 4;
        let pixels = |centre: i64, size: u32| {
            let first = ((centre - radius) / unit).max(0);
            let last = ((centre + radius) / unit + 1).min(i64::from(size));
            first..last
        };
        for y in pixels(centre_y, self.height) {
            for x in pixels(centre_x, self.width) {
                let mut covered = 0_u32;
                for sample_y in 0..4 {
                    for sample_x in 0..4 {
                        let dx = x * unit + step / 2 + sample_x * step - centre_x;
                        let dy = y * unit + step / 2 + sample_y * step - centre_y;
                        if dx * dx + dy * dy <= radius * radius {
                            covered += 1;
                        }
                    }
                }
                if covered == 0 {
                    continue;
                }
                let alpha = u8::try_from(covered * 255 / 16).expect("coverage fits a byte");
                let index = usize::try_from(y * i64::from(self.width) + x)
                    .expect("cell mask index fits usize");
                self.data[index] = self.data[index].max(alpha);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pixel(mask: &BoxMask, x: u32, y: u32) -> u8 {
        mask.data[usize::try_from(y * mask.width + x).expect("index fits")]
    }

    #[test]
    fn default_thickness_matches_foot_scale_and_cell_vectors() {
        assert_eq!(default_thickness(7, 17, 120), 1);
        assert_eq!(default_thickness(9, 21, 150), 1);
        assert_eq!(default_thickness(11, 26, 180), 2);
        assert_eq!(default_thickness(14, 34, 240), 3);
    }

    #[test]
    fn horizontal_uses_foot_centering_for_odd_and_even_cells() {
        let odd = generate('─', 7, 9, 1).expect("supported");
        let even = generate('─', 8, 10, 2).expect("supported");
        assert!((0..7).all(|x| pixel(&odd, x, 4) == 0xff));
        assert_eq!(odd.data.iter().filter(|value| **value != 0).count(), 7);
        assert!((0..8).all(|x| pixel(&even, x, 4) == 0xff && pixel(&even, x, 5) == 0xff));
        assert_eq!(even.data.iter().filter(|value| **value != 0).count(), 16);
    }

    #[test]
    fn corners_and_cross_have_continuous_centered_joins() {
        for (width, height, thickness) in [(7, 9, 1), (8, 10, 2)] {
            let center_x = (width - thickness) / 2;
            let center_y = (height - thickness) / 2;
            for character in ['┌', '┐', '└', '┘', '├', '┤', '┬', '┴', '┼'] {
                let mask = generate(character, width, height, thickness).expect("supported");
                for y in center_y..center_y + thickness {
                    for x in center_x..center_x + thickness {
                        assert_eq!(pixel(&mask, x, y), 0xff, "gap in {character}");
                    }
                }
            }
        }
    }

    #[test]
    fn block_elements_cover_cell_edges_without_font_bearing_gaps() {
        for character in ['▀', '▁', '▂', '▃', '▄', '▅', '▆', '▇', '█', '▌', '▐']
        {
            let mask = generate(character, 13, 30, 1).expect("solid block");
            let (x, y, width, height) =
                opaque_block_rect(character, 13, 30).expect("opaque rectangle");
            for mask_y in 0..mask.height {
                for mask_x in 0..mask.width {
                    let expected =
                        mask_x >= x && mask_x < x + width && mask_y >= y && mask_y < y + height;
                    assert_eq!(pixel(&mask, mask_x, mask_y) == 0xff, expected);
                }
            }
        }
        let full = generate('█', 13, 30, 1).expect("full block");
        assert!(full.data.iter().all(|alpha| *alpha == 0xff));
        let upper = generate('▀', 13, 30, 1).expect("upper block");
        assert!((0..13).all(|x| pixel(&upper, x, 0) == 0xff));
        assert!((0..13).all(|x| pixel(&upper, x, 29) == 0));
        let lower = generate('▄', 13, 30, 1).expect("lower block");
        assert!((0..13).all(|x| pixel(&lower, x, 0) == 0));
        assert!((0..13).all(|x| pixel(&lower, x, 29) == 0xff));
        for (character, extent) in [
            ('▁', 4_u32),
            ('▂', 8),
            ('▃', 11),
            ('▄', 15),
            ('▅', 19),
            ('▆', 23),
            ('▇', 26),
        ] {
            let mask = generate(character, 13, 30, 1).expect("fractional lower block");
            let start = 30 - extent;
            assert!((0..13).all(|x| pixel(&mask, x, 29) == 0xff));
            assert!((start..30).all(|y| (0..13).all(|x| pixel(&mask, x, y) == 0xff)));
            if start > 0 {
                assert!((0..13).all(|x| pixel(&mask, x, start - 1) == 0));
            }
        }
        for shade in ['░', '▒', '▓'] {
            let mask = generate(shade, 13, 30, 1).expect("shade");
            assert!(mask.data.contains(&0));
            assert!(mask.data.contains(&0xff));
        }
    }

    #[test]
    fn rounded_corners_keep_line_weight_and_do_not_become_square() {
        let top_right = generate('╮', 13, 30, 1).expect("rounded corner");
        assert_eq!(pixel(&top_right, 0, 14), 0xff, "horizontal edge");
        assert_eq!(pixel(&top_right, 6, 29), 0xff, "vertical edge");
        assert_eq!(pixel(&top_right, 6, 14), 0, "sharp center must stay empty");
        assert!(top_right.data.iter().all(|alpha| matches!(alpha, 0 | 0xff)));
    }

    #[test]
    fn straight_lines_reach_opposite_cell_edges() {
        let horizontal = generate('─', 13, 30, 1).expect("horizontal");
        let vertical = generate('│', 13, 30, 1).expect("vertical");
        let center_x = 6;
        let center_y = 14;
        assert_eq!(pixel(&horizontal, 0, center_y), 0xff);
        assert_eq!(pixel(&horizontal, 12, center_y), 0xff);
        assert_eq!(pixel(&vertical, center_x, 0), 0xff);
        assert_eq!(pixel(&vertical, center_x, 29), 0xff);
    }

    #[test]
    fn braille_bits_map_to_foot_dot_positions() {
        let top_left = generate('\u{2801}', 14, 30, 1).expect("Braille supported");
        let bottom_right = generate('\u{2880}', 14, 30, 1).expect("Braille supported");
        assert!(top_left.data.iter().any(|alpha| *alpha != 0));
        assert!(bottom_right.data.iter().any(|alpha| *alpha != 0));
        let first_top = top_left
            .data
            .iter()
            .position(|alpha| *alpha != 0)
            .expect("top-left dot");
        let first_bottom = bottom_right
            .data
            .iter()
            .position(|alpha| *alpha != 0)
            .expect("bottom-right dot");
        assert!(first_bottom > first_top);
    }

    fn inked(mask: &BoxMask) -> usize {
        mask.data.iter().filter(|alpha| **alpha != 0).count()
    }

    /// Total coverage in whole pixels.
    fn ink(mask: &BoxMask) -> f64 {
        mask.data.iter().map(|alpha| f64::from(*alpha)).sum::<f64>() / 255.0
    }

    #[test]
    fn chrome_braille_dots_are_round_and_moderately_larger_than_the_terminal_dots() {
        // A 14 px chrome cell: Foot's terminal geometry gives 2 px square
        // dots (32 px of ink for all eight).
        let (width, height) = (8, 17);
        let full = chrome_braille('\u{28ff}', width, height).expect("Braille supported");
        assert_eq!((full.width, full.height), (width, height));
        let terminal = generate('\u{28ff}', width, height, 1).expect("Braille supported");
        let ratio = ink(&full) / ink(&terminal);
        assert!(
            (1.2..1.8).contains(&ratio),
            "chrome dots carry {ratio:.2}x the terminal ink, not a solid block"
        );
        assert!(
            full.data.iter().any(|alpha| (1..255).contains(alpha)),
            "dot edges are smoothed"
        );

        // Each spinner frame keeps its dot pattern: '⠋' is dots 1, 2 and 4.
        let frame = chrome_braille('⠋', width, height).expect("Braille supported");
        let per_dot = ink(&full) / 8.0;
        assert!((ink(&frame) - 3.0 * per_dot).abs() < per_dot * 0.3);
        assert_eq!(
            inked(&chrome_braille('\u{2800}', width, height).expect("Braille supported")),
            0
        );

        // Doubled output scale gives each dot about four times the ink.
        let small = ink(&chrome_braille('\u{2801}', width, height).expect("Braille supported"));
        let large = ink(&chrome_braille('\u{2801}', 16, 34).expect("Braille supported"));
        assert!((3.4..4.6).contains(&(large / small)));
    }

    #[test]
    fn chrome_braille_keeps_bit_positions_and_its_cell() {
        let (width, height) = (8_u32, 17_u32);
        let first = |character| {
            let mask = chrome_braille(character, width, height).expect("Braille supported");
            let index = mask
                .data
                .iter()
                .position(|alpha| *alpha != 0)
                .expect("a dot");
            let index = u32::try_from(index).unwrap();
            (index % width, index / width)
        };
        let (left_x, top_y) = first('\u{2801}');
        let (right_x, _) = first('\u{2808}');
        let (_, bottom_y) = first('\u{2840}');
        assert!(right_x > left_x, "dot 4 is right of dot 1");
        assert!(bottom_y > top_y, "dot 7 is below dot 1");
        let whole = chrome_braille('\u{28ff}', width, height).expect("Braille supported");
        assert_eq!(whole.data.len(), usize::try_from(width * height).unwrap());
    }

    #[test]
    fn chrome_braille_rejects_other_characters_and_tiny_cells() {
        assert!(chrome_braille('A', 8, 17).is_none());
        assert!(chrome_braille('─', 8, 17).is_none());
        assert!(chrome_braille('\u{28ff}', 4, 6).is_none());
        assert!(chrome_braille('\u{28ff}', 0, 17).is_none());
    }

    #[test]
    fn unsupported_or_invalid_requests_are_rejected() {
        assert!(generate('A', 8, 16, 1).is_none());
        assert!(generate('─', 0, 16, 1).is_none());
    }
}
