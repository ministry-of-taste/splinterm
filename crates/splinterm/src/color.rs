//! Splinterm's one color-math stack for chrome colors.
//!
//! Colors are packed `0xRRGGBB` values in encoded sRGB. Mixing happens in
//! linear light, and contrast is the WCAG 2 ratio of relative luminances.
//! Theme derivation (`crate::theme`) and the overlay palettes share these
//! functions, so a role and the panel painted from it agree on every value.
//!
//! [`legacy_srgb`] keeps the two encoded-sRGB mixes that some of today's
//! derived chrome colors still come from; see its documentation.

/// WCAG AA contrast for normal-size text.
pub const TEXT_MIN_CONTRAST: f64 = 4.5;

/// The contrast non-text chrome (glyphs, rails, the focus ring) keeps.
pub const NON_TEXT_MIN_CONTRAST: f64 = 3.0;

/// Decodes one sRGB channel to linear light.
#[must_use]
pub fn linear_channel(channel: u8) -> f64 {
    let value = f64::from(channel) / 255.0;
    if value <= 0.04045 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

/// Encodes one linear-light channel to sRGB, rounded and clamped.
#[must_use]
pub fn srgb_channel(channel: f64) -> u8 {
    let value = if channel <= 0.003_130_8 {
        channel * 12.92
    } else {
        1.055 * channel.powf(1.0 / 2.4) - 0.055
    };
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let encoded = (value.clamp(0.0, 1.0) * 255.0).round() as u8;
    encoded
}

/// The red, green and blue bytes of a packed color.
#[must_use]
pub const fn rgb_channels(color: u32) -> [u8; 3] {
    let [_, red, green, blue] = color.to_be_bytes();
    [red, green, blue]
}

/// Packs red, green and blue bytes.
#[must_use]
pub const fn packed_channels([red, green, blue]: [u8; 3]) -> u32 {
    u32::from_be_bytes([0, red, green, blue])
}

/// Mixes `right` into `left` in linear light: weight 0 keeps `left` and 1
/// gives `right`.
#[must_use]
pub fn mix_linear(left: u32, right: u32, right_weight: f64) -> u32 {
    let left = rgb_channels(left);
    let right = rgb_channels(right);
    let weight = right_weight.clamp(0.0, 1.0);
    packed_channels(std::array::from_fn(|index| {
        srgb_channel(
            linear_channel(left[index]) * (1.0 - weight) + linear_channel(right[index]) * weight,
        )
    }))
}

/// WCAG relative luminance.
#[must_use]
pub fn relative_luminance(color: u32) -> f64 {
    let [red, green, blue] = rgb_channels(color).map(linear_channel);
    red * 0.2126 + green * 0.7152 + blue * 0.0722
}

/// WCAG contrast ratio, from 1 to 21.
#[must_use]
pub fn contrast_ratio(left: u32, right: u32) -> f64 {
    let left = relative_luminance(left);
    let right = relative_luminance(right);
    (left.max(right) + 0.05) / (left.min(right) + 0.05)
}

/// White or black, whichever contrasts more with `background`.
#[must_use]
pub fn strongest_contrast_endpoint(background: u32) -> u32 {
    if contrast_ratio(0x00ff_ffff, background) >= contrast_ratio(0, background) {
        0x00ff_ffff
    } else {
        0
    }
}

/// `foreground`, or the least linear-light mix of it toward `target` that
/// keeps `floor` against `background`. When even `target` misses the floor,
/// the strongest endpoint for `background`.
#[must_use]
pub fn contrast_floor_toward(foreground: u32, target: u32, background: u32, floor: f64) -> u32 {
    if contrast_ratio(foreground, background) >= floor {
        return foreground;
    }
    if contrast_ratio(target, background) < floor {
        return strongest_contrast_endpoint(background);
    }
    let mut failing = 0.0_f64;
    let mut passing = 1.0_f64;
    for _ in 0..24 {
        let weight = f64::midpoint(failing, passing);
        if contrast_ratio(mix_linear(foreground, target, weight), background) >= floor {
            passing = weight;
        } else {
            failing = weight;
        }
    }
    mix_linear(foreground, target, passing)
}

/// [`contrast_floor_toward`] at the text floor, 4.5:1.
#[must_use]
pub fn contrast_corrected_toward(foreground: u32, target: u32, background: u32) -> u32 {
    contrast_floor_toward(foreground, target, background, TEXT_MIN_CONTRAST)
}

/// `foreground` corrected toward the strongest endpoint for `background`
/// until it keeps the text floor.
#[must_use]
pub fn contrast_corrected(foreground: u32, background: u32) -> u32 {
    contrast_corrected_toward(
        foreground,
        strongest_contrast_endpoint(background),
        background,
    )
}

/// Encoded-sRGB mixes that today's derivation of a few painted chrome colors
/// still uses, kept exactly so that moving to semantic roles changes no
/// pixel: the inactive-tab label's muted ramp and a JSON or built-in theme's
/// default pane border. Nothing new may use them. The semantic-key and
/// surface milestones of Plan 0064 move these derivations to linear light.
pub mod legacy_srgb {
    /// Mixes `second` into `first` in quarters of encoded sRGB: 0 keeps
    /// `first`, 4 gives `second`, rounding half up.
    #[must_use]
    pub fn mix_quarters(first: u32, second: u32, second_quarters: u32) -> u32 {
        let channel = |shift: u32| {
            let first = first >> shift & 0xff;
            let second = second >> shift & 0xff;
            (first * (4 - second_quarters) + second * second_quarters + 2) / 4
        };
        channel(16) << 16 | channel(8) << 8 | channel(0)
    }

    /// The encoded-sRGB midpoint of two colors, each channel rounded down.
    #[must_use]
    pub fn midpoint(first: u32, second: u32) -> u32 {
        let red = u32::midpoint(first >> 16 & 0xff, second >> 16 & 0xff);
        let green = u32::midpoint(first >> 8 & 0xff, second >> 8 & 0xff);
        let blue = u32::midpoint(first & 0xff, second & 0xff);
        red << 16 | green << 8 | blue
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linear_mixes_round_trip_their_endpoints_and_brighten_the_midpoint() {
        assert_eq!(mix_linear(0x10_20_30, 0xf0_e0_d0, 0.0), 0x10_20_30);
        assert_eq!(mix_linear(0x10_20_30, 0xf0_e0_d0, 1.0), 0xf0_e0_d0);
        // Half of the light of white is brighter than the encoded midpoint.
        assert_eq!(mix_linear(0, 0x00ff_ffff, 0.5), 0x00bc_bcbc);
        assert_eq!(legacy_srgb::midpoint(0, 0x00ff_ffff), 0x007f_7f7f);
        for channel in 0..=u8::MAX {
            assert_eq!(srgb_channel(linear_channel(channel)), channel);
        }
    }

    #[test]
    fn contrast_floors_keep_readable_colors_and_mix_the_rest() {
        assert!((contrast_ratio(0, 0x00ff_ffff) - 21.0).abs() < 1e-9);
        assert_eq!(contrast_corrected(0x00ff_ffff, 0), 0x00ff_ffff);
        let corrected = contrast_corrected(0x20_20_20, 0x10_10_10);
        assert!(contrast_ratio(corrected, 0x10_10_10) >= TEXT_MIN_CONTRAST);
        let glyph = contrast_floor_toward(0x30_30_30, 0x00ff_ffff, 0, NON_TEXT_MIN_CONTRAST);
        assert!(contrast_ratio(glyph, 0) >= NON_TEXT_MIN_CONTRAST);
        assert!(contrast_ratio(glyph, 0) < TEXT_MIN_CONTRAST);
        // An unreadable target falls back to the strongest endpoint.
        assert_eq!(
            contrast_corrected_toward(0x10_10_10, 0x20_20_20, 0x18_18_18),
            0x00ff_ffff
        );
    }

    #[test]
    fn legacy_srgb_mixes_keep_their_exact_rounding() {
        assert_eq!(legacy_srgb::mix_quarters(0, 0x00ff_ffff, 0), 0);
        assert_eq!(legacy_srgb::mix_quarters(0, 0x00ff_ffff, 2), 0x0080_8080);
        assert_eq!(legacy_srgb::mix_quarters(0, 0x00ff_ffff, 4), 0x00ff_ffff);
        assert_eq!(legacy_srgb::midpoint(0x000e_1216, 0x00eb_ebeb), 0x007c_7e80);
    }
}
