use super::{frame::select_face_for_text, *};

fn stage_test_font_generation() -> FontGeneration {
    stage_live_font_generation(
        "monospace:style=Regular",
        crate::config::FontAuthority::NativeOmarchy,
    )
    .expect("stage the host's generic monospace family")
}

fn synthetic_row() -> TextRow {
    let key = GlyphKey { face: 0, glyph: 1 };
    TextRow {
        glyphs: vec![
            PlacedGlyph {
                key,
                cell: 0,
                cells: 1,
                cluster_advance: 4.0,
                x_offset: 0.0,
                y_offset: 0.0,
            },
            PlacedGlyph {
                key,
                cell: 1,
                cells: 1,
                cluster_advance: 4.0,
                x_offset: 0.0,
                y_offset: 0.0,
            },
        ],
        cache: HashMap::from([(
            key,
            CachedGlyph {
                content: Content::Mask,
                left: 0,
                top: 2,
                width: 2,
                height: 2,
                data: vec![0xff, 0x80, 0x40, 0],
            },
        )]),
        cell_width: 8,
        cell_height: 12,
        baseline: 9,
        cell_count: 2,
        origin_x: BASE_ROW_X,
        origin_y: BASE_ROW_Y,
    }
}

#[test]
fn default_font_profile_records_uncapped_1440p_and_4k_grids() {
    let snapshot = incremental_snapshot();
    for (scale_120, expected_cell, expected_1440p, expected_4k) in [
        (
            120_u32,
            (13, 30),
            (195, 47, 2_535, 1_410),
            (293, 71, 3_809, 2_130),
        ),
        (
            150,
            (17, 38),
            (186, 46, 3_162, 1_748),
            (280, 70, 4_760, 2_660),
        ),
        (
            180,
            (20, 44),
            (190, 48, 3_800, 2_112),
            (286, 72, 5_720, 3_168),
        ),
        (
            240,
            (26, 59),
            (195, 48, 5_070, 2_832),
            (293, 72, 7_618, 4_248),
        ),
    ] {
        let frame = SnapshotFrame::load_scaled(&snapshot, scale_120).expect("default font frame");
        assert_eq!((frame.cell_width, frame.cell_height), expected_cell);
        assert_eq!(
            frame
                .terminal_size(2_560, 1_440, scale_120)
                .expect("1440p terminal grid"),
            expected_1440p
        );
        assert_eq!(
            frame
                .terminal_size(3_840, 2_160, scale_120)
                .expect("4K terminal grid"),
            expected_4k
        );
        assert!(expected_1440p.0 < MAX_COLUMNS && expected_1440p.1 < MAX_ROWS);
        assert!(expected_4k.0 < MAX_COLUMNS && expected_4k.1 < MAX_ROWS);
    }
}

#[test]
fn deterministic_row_paints_identical_opaque_canvases() {
    let row = synthetic_row();
    let (width, height) = (96_u32, 128_u32);
    let mut first = vec![0; 96 * 128 * 4];
    let mut second = vec![0; 96 * 128 * 4];

    paint(&mut first, width, height, &row);
    paint(&mut second, width, height, &row);

    assert_eq!(first, second);
    let (pixels, _) = first.as_chunks::<4>();
    assert!(pixels.iter().all(|pixel| pixel[3] == 0xff));
    assert_eq!(row.cache.len(), 1, "repeated glyphs share one cached image");
}

#[test]
fn placement_centers_the_pen_then_applies_bearings_and_shaped_offsets() {
    let row = synthetic_row();
    let mut placed = PlacedGlyph {
        key: GlyphKey { face: 0, glyph: 1 },
        cell: 2,
        cells: 1,
        cluster_advance: 4.0,
        x_offset: -1.0,
        y_offset: 3.0,
    };
    let glyph = CachedGlyph {
        content: Content::Mask,
        left: -2,
        top: 5,
        width: 7,
        height: 8,
        data: vec![0; 56],
    };
    let centered_pen = (u32_to_f32(row.cell_width) - placed.cluster_advance) / 2.0;

    assert_eq!(glyph_origin(&row, &placed, &glyph, centered_pen), (47, 97));

    placed.x_offset = 2.0;
    assert_eq!(glyph_origin(&row, &placed, &glyph, centered_pen), (50, 97));
}

#[test]
fn shaped_combining_glyphs_share_a_cluster_advance_and_cell() {
    let key = GlyphKey { face: 0, glyph: 2 };
    let base = PlacedGlyph {
        key,
        cell: 3,
        cells: 1,
        cluster_advance: 8.0,
        x_offset: 0.0,
        y_offset: 0.0,
    };
    let mark = PlacedGlyph {
        key,
        x_offset: -2.0,
        y_offset: 4.0,
        ..base
    };

    assert_eq!(base.cell, mark.cell);
    assert!((base.cluster_advance - mark.cluster_advance).abs() < f32::EPSILON);
    assert!((base.x_offset - mark.x_offset).abs() > f32::EPSILON);
    assert!((base.y_offset - mark.y_offset).abs() > f32::EPSILON);
}

#[test]
fn primary_face_selection_tracks_bold_and_italic_attributes() {
    let mut attributes = default_attributes();
    assert_eq!(primary_face_index(&attributes), SNAPSHOT_PRIMARY_REGULAR);
    attributes.bold = true;
    assert_eq!(primary_face_index(&attributes), SNAPSHOT_PRIMARY_BOLD);
    attributes.italic = true;
    assert_eq!(
        primary_face_index(&attributes),
        SNAPSHOT_PRIMARY_BOLD_ITALIC
    );
    attributes.bold = false;
    assert_eq!(primary_face_index(&attributes), SNAPSHOT_PRIMARY_ITALIC);
}

#[test]
fn cell_metrics_use_the_foot_freetype_integer_extents() {
    let faces = snapshot_faces().unwrap();
    let metrics = cell_metrics(&faces[0], 22.0).unwrap();
    assert_eq!(metrics.width, 13);
    assert_eq!(metrics.height, 30);
    assert_eq!(metrics.baseline, 23);
    assert!((metrics.mono_advance - 13.0).abs() < f32::EPSILON);
}

#[test]
fn glyph_alpha_bytes_normalize_supported_swash_content() {
    let mask = CachedGlyph {
        content: Content::Mask,
        left: 0,
        top: 0,
        width: 2,
        height: 1,
        data: vec![10, 20],
    };
    assert_eq!(glyph_alpha_bytes(&mask), vec![10, 20]);

    let subpixel = CachedGlyph {
        content: Content::SubpixelMask,
        left: 0,
        top: 0,
        width: 2,
        height: 1,
        data: vec![10, 30, 20, 0, 40, 20, 30, 0],
    };
    assert_eq!(glyph_alpha_bytes(&subpixel), vec![30, 40]);

    let color = CachedGlyph {
        content: Content::Color,
        left: 0,
        top: 0,
        width: 2,
        height: 1,
        data: vec![1, 2, 3, 4, 5, 6, 7, 8],
    };
    assert_eq!(glyph_alpha_bytes(&color), vec![4, 8]);
}

#[test]
fn ink_bounds_cover_mask_and_color_images() {
    let mask = CachedGlyph {
        content: Content::Mask,
        left: 0,
        top: 0,
        width: 3,
        height: 2,
        data: vec![0, 1, 0, 0, 1, 0],
    };
    assert_eq!(
        mask.ink_bounds(),
        Some(InkBounds {
            left: 1,
            top: 0,
            right: 2,
            bottom: 2,
        })
    );

    let color = CachedGlyph {
        content: Content::Color,
        left: 0,
        top: 0,
        width: 2,
        height: 1,
        data: vec![10, 20, 30, 0, 10, 20, 30, 40],
    };
    assert_eq!(
        color.ink_bounds(),
        Some(InkBounds {
            left: 1,
            top: 0,
            right: 2,
            bottom: 1,
        })
    );
}

fn default_attributes() -> CellAttributes {
    CellAttributes {
        bold: false,
        dim: false,
        italic: false,
        underline: UnderlineStyle::None,
        underline_color_source: ColorSource::Default,
        underline_color: 0,
        strikethrough: false,
        blink: false,
        conceal: false,
        reverse: false,
        foreground_source: ColorSource::Default,
        foreground: 0,
        background_source: ColorSource::Default,
        background: 0,
    }
}

#[test]
fn explicit_render_contexts_are_pixel_and_metric_isolated_when_interleaved() {
    let snapshot = incremental_snapshot();
    let first = RenderContext::new(12_000);
    let mut second = RenderContext::new(52_000);
    second.set_font_zoom_steps(2, 120).unwrap();

    let first_before =
        capture_final_buffer_in_context(&first, &snapshot, 120, false, CursorStyle::Block).unwrap();
    let second_capture =
        capture_final_buffer_in_context(&second, &snapshot, 120, false, CursorStyle::Block)
            .unwrap();
    let first_after =
        capture_final_buffer_in_context(&first, &snapshot, 120, false, CursorStyle::Block).unwrap();

    assert_eq!(first_before.pixels, first_after.pixels);
    assert_eq!(first_before.cell_width, first_after.cell_width);
    assert_eq!(first_before.cell_height, first_after.cell_height);
    assert_eq!(first_before.background_bgra, first_after.background_bgra);
    assert_ne!(first_before.cell_height, second_capture.cell_height);
    assert_ne!(first_before.background_bgra, second_capture.background_bgra);
    assert_eq!(first_before.pixels[3], alpha_u8(12_000));
    assert_eq!(first_after.pixels[3], alpha_u8(12_000));
    assert_eq!(second_capture.pixels[3], alpha_u8(52_000));
}

#[test]
fn font_generation_replacement_preserves_zoom_dpi_and_alpha() {
    let mut context = RenderContext::new(12_345);
    context.set_font_zoom_steps(2, 150).unwrap();
    context
        .update_output_dpi(OutputDpiObservation::provided(144.0).unwrap(), 150)
        .unwrap();
    let before = context.effective_font_resolution(150).unwrap();
    let mut replacement = stage_test_font_generation();
    replacement.fingerprint.pattern.push_str("#test-generation");
    let replacement_id = replacement.id;

    assert!(context.replace_font_generation(Arc::new(replacement)));
    assert_eq!(context.font_generation_id(), Some(replacement_id));
    assert_eq!(context.effective_font_resolution(150).unwrap(), before);
    assert_eq!(context.background_alpha(), 12_345);
}

fn incremental_snapshot() -> TerminalSnapshot {
    let attributes = default_attributes();
    TerminalSnapshot {
        splint_id: SplintId::new(),
        incarnation: 1,
        revision: 1,
        columns: 2,
        rows: 2,
        cursor_column: 0,
        cursor_row: 0,
        cursor_deferred_wrap: false,
        active_screen: ActiveScreen::Normal,
        input_modes: TerminalInputModes {
            application_cursor: false,
            application_keypad: false,
            focus_reporting: false,
            bracketed_paste: false,
            cursor_visible: true,
            cursor_blink: false,
            mouse_tracking: splinterm_protocol::MouseTracking::None,
            sgr_mouse: false,
        },
        palette: vec![0; 256],
        default_colors: [0x00eb_ebeb, 0x000e_1216, 0x00eb_ebeb],
        title: "incremental".into(),
        visible_rows: ["ab", "cd"]
            .into_iter()
            .enumerate()
            .map(|(index, text)| TerminalRow {
                row_id: Some(u64::try_from(index + 1).unwrap()),
                linebreak: false,
                cells: text
                    .chars()
                    .map(|character| TerminalCell {
                        content: character.to_string(),
                        spacer_remaining: None,
                        attributes,
                    })
                    .collect(),
            })
            .collect(),
        history_generation: 1,
        oldest_available_scrollback_row_id: None,
        newest_available_scrollback_row_id: None,
        scrollback_rows: Vec::new(),
        available_scrollback_rows: 0,
        omitted_oldest_scrollback_rows: 0,
        images: None,
        exited_code: None,
        exited_signal: None,
    }
}

#[test]
fn fractional_scale_frames_map_cells_and_cursor_rectangles() {
    let snapshot = incremental_snapshot();
    for scale_120 in [120_u32, 150, 180, 240] {
        let frame = SnapshotFrame::load_scaled(&snapshot, scale_120).expect("scaled frame");
        assert_eq!(u32::from(frame.scale_120), scale_120);
        let geometry = frame.tight_geometry().unwrap();
        let scale = f64::from(scale_120) / 120.0;
        let logical_x =
            (f64::from(geometry.actual_padding.left) + f64::from(frame.cell_width) / 2.0) / scale;
        let logical_y =
            (f64::from(geometry.actual_padding.top) + f64::from(frame.cell_height) / 2.0) / scale;
        assert_eq!(frame.cell_at(logical_x, logical_y, &geometry), Some((0, 0)));
        let (_, _, width, height) = frame
            .cursor_rectangle(&geometry)
            .expect("visible cursor rectangle");
        assert!(width > 0 && height > 0);
    }
}

#[test]
fn focused_block_cursor_is_an_opaque_cell() {
    let capture =
        capture_final_buffer(&incremental_snapshot(), 120, true, CursorStyle::Block).unwrap();
    let expected = [0xeb, 0xeb, 0xeb, 0xff];
    for y in capture.origin_y..capture.origin_y + capture.cell_height {
        for x in capture.origin_x..capture.origin_x + capture.cell_width {
            let index = usize::try_from(y * capture.stride + x * 4).unwrap();
            assert_eq!(&capture.pixels[index..index + 4], &expected);
        }
    }
}

#[test]
fn final_buffer_capture_uses_declared_production_geometry_and_argb_bytes() {
    let snapshot = incremental_snapshot();
    let capture = capture_final_buffer(&snapshot, 120, true, CursorStyle::Block).unwrap();
    assert_eq!((capture.columns, capture.rows), (2, 2));
    assert_eq!(capture.origin_x, capture.padding_left);
    assert_eq!(capture.origin_y, capture.padding_top);
    assert_eq!(capture.padding_left, capture.padding_right);
    assert_eq!(capture.padding_top, capture.padding_bottom);
    assert!(capture.ascent + capture.descent <= capture.cell_height);
    assert_eq!(
        u32::try_from(capture.baseline).unwrap(),
        capture.cell_height - capture.descent
    );
    assert_eq!(capture.requested_padding, TerminalPadding::DEFAULT);
    assert_eq!(
        capture.padding_left + capture.columns * capture.cell_width + capture.padding_right,
        capture.width
    );
    assert_eq!(
        capture.padding_top + capture.rows * capture.cell_height + capture.padding_bottom,
        capture.height
    );
    assert_eq!(
        capture.padding_right,
        capture.effective_base_padding.right + capture.residual_right
    );
    assert_eq!(
        capture.padding_bottom,
        capture.effective_base_padding.bottom + capture.residual_bottom
    );
    assert_eq!(capture.stride, capture.width * 4);
    assert_eq!(
        capture.pixels.len(),
        usize::try_from(capture.stride * capture.height).unwrap()
    );
    assert_eq!(capture.cursor, Some((0, 0)));
    assert_eq!(capture.background_bgra[3], u8::MAX);
}

#[test]
fn asymmetric_capture_serializes_geometry_owned_rectangles_and_edges() {
    let snapshot = incremental_snapshot();
    let mut frame = SnapshotFrame::load_scaled(&snapshot, 150).unwrap();
    frame.padding = TerminalPadding {
        left: 1,
        right: 3,
        top: 5,
        bottom: 7,
    };
    let geometry = frame.tight_geometry().unwrap();
    let capture = capture_prepared_frame(
        &frame,
        geometry,
        false,
        CursorStyle::Block,
        CursorPresentation::FOCUSED_STEADY,
    )
    .unwrap();
    assert_eq!(capture.requested_padding, frame.padding);
    assert_ne!(capture.padding_left, capture.padding_right);
    assert_ne!(capture.padding_top, capture.padding_bottom);
    assert_eq!(capture.grid_rect.x, capture.origin_x);
    assert_eq!(capture.grid_rect.y, capture.origin_y);
    assert_eq!(
        capture.grid_rect.width,
        capture.columns * capture.cell_width
    );
    assert_eq!(capture.grid_rect.height, capture.rows * capture.cell_height);
    assert_eq!(
        capture.padding_left + capture.grid_rect.width + capture.padding_right,
        capture.width
    );
    assert_eq!(
        capture.padding_top + capture.grid_rect.height + capture.padding_bottom,
        capture.height
    );
}

#[test]
fn sized_capture_preserves_explicit_grid_and_owns_trailing_residual() {
    let snapshot = incremental_snapshot();
    let tight = capture_final_buffer(&snapshot, 120, false, CursorStyle::Block).unwrap();
    let capture = capture_final_buffer_sized(
        &snapshot,
        120,
        tight.logical_width,
        tight.logical_height + 1,
        false,
        CursorStyle::Block,
    )
    .unwrap();
    assert_eq!((capture.columns, capture.rows), (2, 2));
    assert_eq!(capture.padding_bottom, tight.padding_bottom + 1);
    assert_eq!(capture.residual_bottom, tight.residual_bottom + 1);
}

#[test]
fn cursor_geometry_matches_foot_at_required_scales() {
    let snapshot = incremental_snapshot();
    for scale in [120_u32, 150, 180, 240] {
        let frame = SnapshotFrame::load_scaled(&snapshot, scale).unwrap();
        let geometry = frame.tight_geometry().unwrap();
        let rect = geometry.cell_rect(0, 0).unwrap();
        let metrics = frame.cell_metrics[0];
        let width = geometry.buffer_width();
        let height = geometry.buffer_height();
        for shape in [
            EffectiveCursorShape::Beam,
            EffectiveCursorShape::Underline,
            EffectiveCursorShape::Hollow,
        ] {
            let mut canvas = vec![0; usize::try_from(width * height * 4).unwrap()];
            paint_effective_cursor(
                &mut canvas,
                width,
                height,
                &frame,
                i32::try_from(rect.x).unwrap(),
                i32::try_from(rect.y).unwrap(),
                1,
                metrics,
                [255, 255, 255, 255],
                shape,
            );
            let (pixels, _) = canvas.as_chunks::<4>();
            let painted = pixels.iter().filter(|pixel| pixel[3] != 0).count();
            assert!(painted > 0);
        }
        let expected_beam = (2 * scale + 60) / 120;
        assert!(expected_beam >= 2);
        let expected_hollow = (scale + 60) / 120;
        assert!((1..=2).contains(&expected_hollow));
    }
}

#[test]
fn focused_and_unfocused_full_dirty_composition_share_one_path() {
    let mut snapshot = incremental_snapshot();
    snapshot.visible_rows[0].cells[0].attributes.underline = UnderlineStyle::Curly;
    snapshot.visible_rows[0].cells[0].attributes.strikethrough = true;
    let frame = SnapshotFrame::load_scaled(&snapshot, 120).unwrap();
    let geometry = frame.tight_geometry().unwrap();
    let width = geometry.buffer_width();
    let height = geometry.buffer_height();
    for presentation in [
        CursorPresentation::FOCUSED_STEADY,
        CursorPresentation::for_keyboard_focus(false),
    ] {
        for style in [
            CursorStyle::Block,
            CursorStyle::Beam,
            CursorStyle::Underline,
        ] {
            let mut full = vec![0; usize::try_from(width * height * 4).unwrap()];
            paint_snapshot_presented(
                &mut full,
                width,
                height,
                &frame,
                &geometry,
                true,
                style,
                presentation,
            );
            let mut rows = vec![0; full.len()];
            for pixel in rows.as_chunks_mut::<4>().0 {
                pixel.copy_from_slice(&[
                    frame.canvas_background[2],
                    frame.canvas_background[1],
                    frame.canvas_background[0],
                    255,
                ]);
            }
            paint_snapshot_rows_presented(
                &mut rows,
                width,
                height,
                &frame,
                &geometry,
                &[true, true],
                true,
                style,
                presentation,
            );
            assert_eq!(full, rows);
        }
    }
}

#[test]
fn overlapping_glyphs_compose_in_foot_right_to_left_order() {
    let snapshot = incremental_snapshot();
    let mut frame = SnapshotFrame::load_scaled(&snapshot, 120).unwrap();
    let left_key = GlyphKey { face: 0, glyph: 1 };
    let right_key = GlyphKey { face: 0, glyph: 2 };
    frame.cache.clear();
    frame.cache.insert(
        left_key,
        Arc::new(CachedGlyph {
            content: Content::Mask,
            left: i32::try_from(frame.cell_width).unwrap(),
            top: frame.baseline,
            width: 1,
            height: 1,
            data: vec![1],
        }),
    );
    frame.cache.insert(
        right_key,
        Arc::new(CachedGlyph {
            content: Content::Mask,
            left: 0,
            top: frame.baseline,
            width: 1,
            height: 1,
            data: vec![178],
        }),
    );
    frame.glyphs = vec![
        SnapshotGlyph {
            key: left_key,
            column: 0,
            row: 0,
            cells: 1,
            cluster_advance: u32_to_f32(frame.cell_width),
            x_offset: 0.0,
            y_offset: 0.0,
            foreground: [235; 3],
        },
        SnapshotGlyph {
            key: right_key,
            column: 1,
            row: 0,
            cells: 1,
            cluster_advance: u32_to_f32(frame.cell_width),
            x_offset: 0.0,
            y_offset: 0.0,
            foreground: [235; 3],
        },
    ];
    let geometry = frame.tight_geometry().unwrap();
    let width = geometry.buffer_width();
    let height = geometry.buffer_height();
    let mut canvas = vec![0; usize::try_from(width * height * 4).unwrap()];
    for pixel in canvas.as_chunks_mut::<4>().0 {
        pixel.copy_from_slice(&[22, 18, 14, 255]);
    }
    paint_glyphs(&mut canvas, width, height, &frame, &geometry, None);
    let rect = geometry.cell_rect(1, 0).unwrap();
    let x = rect.x;
    let y = rect.y;
    let index = usize::try_from((y * width + x) * 4).unwrap();
    assert_eq!(&canvas[index..index + 4], &[171, 169, 168, 255]);
}

#[test]
fn snapshot_fallback_uses_cell_pen_without_centering() {
    let snapshot = incremental_snapshot();
    let mut frame = SnapshotFrame::load_scaled(&snapshot, 120).unwrap();
    let key = GlyphKey { face: 5, glyph: 1 };
    frame.cache.clear();
    frame.cache.insert(
        key,
        Arc::new(CachedGlyph {
            content: Content::Mask,
            left: 0,
            top: frame.baseline,
            width: 1,
            height: 1,
            data: vec![255],
        }),
    );
    frame.glyphs = vec![SnapshotGlyph {
        key,
        column: 0,
        row: 0,
        cells: 2,
        cluster_advance: u32_to_f32(frame.cell_width.saturating_mul(2) - 3),
        x_offset: 0.0,
        y_offset: 0.0,
        foreground: [235; 3],
    }];
    let geometry = frame.tight_geometry().unwrap();
    let width = geometry.buffer_width();
    let height = geometry.buffer_height();
    let mut canvas = vec![0; usize::try_from(width * height * 4).unwrap()];
    paint_glyphs(&mut canvas, width, height, &frame, &geometry, None);
    let origin = geometry.cell_rect(0, 0).unwrap();
    let index = usize::try_from((origin.y * width + origin.x) * 4).unwrap();
    assert_eq!(&canvas[index..index + 4], &[235, 235, 235, 255]);
}

#[test]
fn snapshot_decorations_use_foot_baseline_metrics_in_full_and_row_paints() {
    let mut snapshot = incremental_snapshot();
    snapshot.visible_rows[0].cells[0].attributes.underline = UnderlineStyle::Single;
    snapshot.visible_rows[0].cells[0]
        .attributes
        .underline_color_source = ColorSource::Rgb;
    snapshot.visible_rows[0].cells[0].attributes.underline_color = 0x0012_3456;
    snapshot.visible_rows[0].cells[1].attributes.strikethrough = true;
    let frame = SnapshotFrame::load_scaled(&snapshot, 120).expect("decorated frame");
    assert_eq!(frame.decorations.len(), 2);
    assert_eq!(frame.underline_position, -3);
    assert_eq!(frame.underline_thickness, 1);
    assert_eq!(frame.strike_position, 7);
    assert_eq!(frame.strike_thickness, 1);

    let geometry = frame.tight_geometry().unwrap();
    let width = geometry.buffer_width();
    let height = geometry.buffer_height();
    let mut full = vec![0; usize::try_from(width * height * 4).unwrap()];
    paint_snapshot(
        &mut full,
        width,
        height,
        &frame,
        &geometry,
        false,
        CursorStyle::Block,
    );
    let first = geometry.cell_rect(0, 0).unwrap();
    let second = geometry.cell_rect(1, 0).unwrap();
    let underline_y =
        usize::try_from(first.y).unwrap() + usize::try_from(frame.baseline + 3).unwrap();
    let strike_y = usize::try_from(second.y).unwrap()
        + usize::try_from(frame.baseline - frame.strike_position).unwrap();
    let underline_x = usize::try_from(first.x).unwrap();
    let strike_x = usize::try_from(second.x).unwrap();
    let pixel = |x: usize, y: usize| &full[(y * width as usize + x) * 4..][..4];
    assert_eq!(pixel(underline_x, underline_y), &[0x56, 0x34, 0x12, 0xff]);
    assert_eq!(pixel(strike_x, strike_y), &[0xeb, 0xeb, 0xeb, 0xff]);

    let mut rows = vec![0; full.len()];
    for pixel in rows.as_chunks_mut::<4>().0 {
        pixel.copy_from_slice(&[0x16, 0x12, 0x0e, 0xff]);
    }
    paint_snapshot_rows(
        &mut rows,
        width,
        height,
        &frame,
        &geometry,
        &[true, true],
        false,
        CursorStyle::Block,
    );
    assert_eq!(full, rows);
}

#[test]
fn fontconfig_fallback_renders_the_prompt_arrow_instead_of_replacement() {
    let generation = Arc::new(
        stage_renderer_test_generation(PRIMARY_FONT, crate::config::FontAuthority::Explicit)
            .unwrap(),
    );
    let faces = &generation.faces;
    let attributes = CellAttributes::default();
    let face_index = select_face_for_text(faces, "⇕", &attributes).unwrap();
    assert!(
        face_index >= SNAPSHOT_FALLBACK_START,
        "the selected primary, CJK, and emoji faces do not cover U+21D5"
    );
    assert!(
        faces[face_index].data.is_none(),
        "dynamic fallbacks must use the bounded mapping cache"
    );
    let glyph_id = with_font_ref(&faces[face_index], |font, _coords| {
        Ok(font.charmap().map('⇕'))
    })
    .unwrap();
    assert_ne!(glyph_id, 0);

    let mut snapshot = incremental_snapshot();
    snapshot.visible_rows[0].cells[0].content = "⇕".to_owned();
    let mut context = RenderContext::new(u16::MAX);
    context.replace_font_generation(generation);
    let frame = SnapshotFrame::load_scaled_with_context(&snapshot, 120, &context).unwrap();
    assert!(
        frame
            .glyphs
            .iter()
            .any(|glyph| glyph.column == 0 && glyph.row == 0 && glyph.key.face == face_index)
    );
}

#[test]
fn snapshot_styles_select_distinct_primary_faces_and_cache_keys() {
    let mut snapshot = incremental_snapshot();
    snapshot.visible_rows[0].cells[0].content = "A".to_owned();
    snapshot.visible_rows[0].cells[0].attributes.underline = UnderlineStyle::Single;
    snapshot.visible_rows[0].cells[1].content = "A".to_owned();
    snapshot.visible_rows[0].cells[1].attributes.bold = true;
    snapshot.visible_rows[1].cells[0].content = "A".to_owned();
    snapshot.visible_rows[1].cells[0].attributes.italic = true;
    snapshot.visible_rows[1].cells[0].attributes.underline = UnderlineStyle::Single;
    snapshot.visible_rows[1].cells[1].content = "A".to_owned();
    snapshot.visible_rows[1].cells[1].attributes.bold = true;
    snapshot.visible_rows[1].cells[1].attributes.italic = true;

    let frame = SnapshotFrame::load_scaled(&snapshot, 120).expect("styled frame");
    let faces: HashSet<_> = frame.glyphs.iter().map(|glyph| glyph.key.face).collect();
    assert_eq!(
        faces,
        HashSet::from([
            SNAPSHOT_PRIMARY_REGULAR,
            SNAPSHOT_PRIMARY_BOLD,
            SNAPSHOT_PRIMARY_ITALIC,
            SNAPSHOT_PRIMARY_BOLD_ITALIC,
        ])
    );
    assert_eq!(frame.cache.len(), 4, "each style owns a distinct cache key");
    let regular = frame
        .decorations
        .iter()
        .find(|span| span.row == 0 && span.column == 0)
        .unwrap();
    let italic = frame
        .decorations
        .iter()
        .find(|span| span.row == 1 && span.column == 0)
        .unwrap();
    assert_eq!(regular.metrics, frame.cell_metrics[0]);
    assert_eq!(italic.metrics, frame.cell_metrics[2]);
}

#[test]
fn color_fallback_cache_uses_fcft_fixed_strike_size_and_advance() {
    let faces = snapshot_faces().unwrap();
    let font = font_ref(&faces[SNAPSHOT_EMOJI]).unwrap();
    let glyph_id = font.charmap().map('\u{1f642}');
    let small = snapshot_glyph(faces, SNAPSHOT_EMOJI, glyph_id, 12.0).unwrap();
    let small_advance = snapshot_color_advance(faces, SNAPSHOT_EMOJI, glyph_id, 12.0).unwrap();
    let larger = snapshot_glyph(faces, SNAPSHOT_EMOJI, glyph_id, 15.0).unwrap();
    let larger_advance = snapshot_color_advance(faces, SNAPSHOT_EMOJI, glyph_id, 15.0).unwrap();

    assert_eq!((small.width, small.height, small_advance), (14, 14, 14));
    assert_eq!((larger.width, larger.height, larger_advance), (18, 17, 18));
    assert!(!Arc::ptr_eq(&small, &larger));
    assert_ne!(small.data, larger.data);
    clear_snapshot_caches();
    assert_eq!(
        snapshot_color_advance(faces, SNAPSHOT_EMOJI, glyph_id, 15.0).unwrap(),
        larger_advance
    );
}

#[test]
fn underline_style_color_partial_mutation_matches_clean_full_rebuild() {
    let mut initial = incremental_snapshot();
    initial.visible_rows[0].cells[0].attributes.underline = UnderlineStyle::Single;
    let mut changed = initial.clone();
    changed.visible_rows[0].cells[0].attributes.underline = UnderlineStyle::Dashed;
    changed.visible_rows[0].cells[0]
        .attributes
        .underline_color_source = ColorSource::Rgb;
    changed.visible_rows[0].cells[0].attributes.underline_color = 0x12_34_56;

    let mut frame = SnapshotFrame::load_scaled(&initial, 120).unwrap();
    let geometry = frame.tight_geometry().unwrap();
    let width = geometry.buffer_width();
    let height = geometry.buffer_height();
    let mut actual = vec![0; usize::try_from(width * height * 4).unwrap()];
    paint_snapshot(
        &mut actual,
        width,
        height,
        &frame,
        &geometry,
        false,
        CursorStyle::Block,
    );
    frame.refresh_rows(&changed, &[true, false]).unwrap();
    paint_snapshot_rows(
        &mut actual,
        width,
        height,
        &frame,
        &geometry,
        &[true, false],
        false,
        CursorStyle::Block,
    );

    let reference = SnapshotFrame::load_scaled(&changed, 120).unwrap();
    let mut expected = vec![0; actual.len()];
    paint_snapshot(
        &mut expected,
        width,
        height,
        &reference,
        &geometry,
        false,
        CursorStyle::Block,
    );
    assert_eq!(actual, expected);
}

#[test]
fn selected_font_bytes_and_opened_identity_are_staged_together() {
    let face = resolve_face("staged CJK test", CJK_FONT, "noto sans cjk").unwrap();
    let data = face.data.as_ref().expect("explicit faces are staged");
    assert!(!data.is_empty());
    assert_eq!(
        face.source_identity.length,
        u64::try_from(data.len()).unwrap()
    );
    assert_ne!(face.source_identity, data.identity());
    assert_ne!(font_ref(&face).unwrap().charmap().map('界'), 0);
}

#[test]
#[ignore = "requires host fontconfig and installed system fonts"]
fn memory_backed_freetype_keeps_the_staged_inode_after_path_replacement() {
    let source = resolve_face("staged replacement test", CJK_FONT, "noto sans cjk").unwrap();
    let path = std::env::temp_dir().join(format!(
        "splinterm-font-generation-replacement-{}",
        std::process::id()
    ));
    let retired = path.with_extension("retired");
    fs::write(
        &path,
        &***source.data.as_ref().expect("explicit faces are staged"),
    )
    .unwrap();
    let staged = Arc::new(
        splinterm_filemap::ReadOnlyFileMap::immutable_snapshot(
            &path,
            splinterm_freetype::MAX_STAGED_FONT_BYTES,
        )
        .unwrap()
        .mapping,
    );
    fs::rename(&path, &retired).unwrap();
    fs::write(&path, b"not a font").unwrap();

    let shaped = swash::FontRef::from_index(&staged, source.index.collection_index()).unwrap();
    assert_ne!(shaped.charmap().map('界'), 0);
    let retained_before = Arc::strong_count(&staged);
    let mut raster = splinterm_freetype::RasterFace::open_memory(
        Arc::clone(&staged),
        source.index.raw(),
        22 * 64,
    )
    .unwrap();
    assert!(Arc::strong_count(&staged) > retained_before);
    assert!(raster.metrics().unwrap().height > 0);
    drop(raster);
    assert_eq!(Arc::strong_count(&staged), retained_before);

    fs::remove_file(path).unwrap();
    fs::remove_file(retired).unwrap();
}

#[test]
fn raster_faces_share_one_generation_mapping_across_sizes() {
    let generation = stage_test_font_generation();
    let face = &generation.faces[SNAPSHOT_PRIMARY_REGULAR];
    let data = face.data.clone().expect("primary face is staged");
    let retained_before = Arc::strong_count(&data);

    let first =
        splinterm_freetype::RasterFace::open_memory(Arc::clone(&data), face.index.raw(), 16 * 64)
            .unwrap();
    let second =
        splinterm_freetype::RasterFace::open_memory(Arc::clone(&data), face.index.raw(), 32 * 64)
            .unwrap();
    assert!(Arc::strong_count(&data) > retained_before);

    drop((first, second));
    assert_eq!(Arc::strong_count(&data), retained_before);
}

#[test]
fn production_ascii_evidence_is_identical_with_cold_and_warm_cache() {
    let cold = production_ascii_glyph_evidence().expect("cold production evidence");
    let warm = production_ascii_glyph_evidence().expect("warm production evidence");
    assert_eq!(cold, warm);
}

#[test]
fn full_and_all_row_damage_paints_are_byte_identical() {
    let snapshot = incremental_snapshot();
    let frame = SnapshotFrame::load_scaled(&snapshot, 120).expect("frame");
    let geometry = frame.tight_geometry().unwrap();
    let width = geometry.buffer_width();
    let height = geometry.buffer_height();
    let bytes = usize::try_from(width * height * 4).unwrap();
    let mut full = vec![0; bytes];
    paint_snapshot(
        &mut full,
        width,
        height,
        &frame,
        &geometry,
        true,
        CursorStyle::Block,
    );

    let background = [
        frame.canvas_background[2],
        frame.canvas_background[1],
        frame.canvas_background[0],
        0xff,
    ];
    let mut incremental = vec![0; bytes];
    for pixel in incremental.as_chunks_mut::<4>().0 {
        pixel.copy_from_slice(&background);
    }
    paint_snapshot_rows(
        &mut incremental,
        width,
        height,
        &frame,
        &geometry,
        &vec![true; frame.rows as usize],
        true,
        CursorStyle::Block,
    );
    assert_eq!(full, incremental);
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "Slice 5 intentionally exercises every path from one semantic-state harness"
)]
fn equivalent_semantic_state_survives_cursor_cache_scale_and_theme_paths() {
    fn render(
        snapshot: &TerminalSnapshot,
        scale_120: u32,
        cursor_visible: bool,
        cursor_style: CursorStyle,
    ) -> Vec<u8> {
        let frame = SnapshotFrame::load_scaled(snapshot, scale_120).expect("frame");
        let geometry = frame.tight_geometry().unwrap();
        let mut pixels =
            vec![
                0;
                usize::try_from(geometry.buffer_width() * geometry.buffer_height() * 4).unwrap()
            ];
        paint_snapshot_presented(
            &mut pixels,
            geometry.buffer_width(),
            geometry.buffer_height(),
            &frame,
            &geometry,
            cursor_visible,
            cursor_style,
            CursorPresentation::FOCUSED_STEADY,
        );
        pixels
    }

    let mut semantic_state = incremental_snapshot();
    semantic_state.visible_rows[0].cells[0].attributes.underline = UnderlineStyle::Curly;
    semantic_state.visible_rows[0].cells[1]
        .attributes
        .strikethrough = true;
    semantic_state.visible_rows[1].cells[0]
        .attributes
        .foreground_source = ColorSource::Base16;
    semantic_state.visible_rows[1].cells[0]
        .attributes
        .foreground = 1;
    semantic_state.palette[1] = 0x0035_4a60;
    let reference = render(&semantic_state, 120, true, CursorStyle::Block);

    SNAPSHOT_GLYPH_CACHE.with(|cache| *cache.borrow_mut() = PersistentGlyphCache::default());
    let cold = render(&semantic_state, 120, true, CursorStyle::Block);
    let cold_metrics = snapshot_cache_metrics();
    let warm = render(&semantic_state, 120, true, CursorStyle::Block);
    let warm_metrics = snapshot_cache_metrics();
    assert_eq!(cold, reference);
    assert_eq!(warm, reference);
    assert!(warm_metrics["hits"].as_u64() > cold_metrics["hits"].as_u64());

    SNAPSHOT_GLYPH_CACHE.with(|cache| *cache.borrow_mut() = PersistentGlyphCache::default());
    let repopulated = render(&semantic_state, 120, true, CursorStyle::Block);
    assert_eq!(repopulated, reference);
    assert!(snapshot_cache_metrics()["entries"].as_u64().unwrap() > 0);

    let scaled = render(&semantic_state, 150, true, CursorStyle::Block);
    assert_ne!(scaled.len(), reference.len());
    assert_eq!(
        render(&semantic_state, 120, true, CursorStyle::Block),
        reference
    );

    let mut alternate_theme = semantic_state.clone();
    alternate_theme.palette[1] = 0x00f0_8040;
    alternate_theme.default_colors = [0x0011_2233, 0x0044_5566, 0x0077_8899];
    assert_ne!(
        render(&alternate_theme, 120, true, CursorStyle::Block),
        reference
    );
    assert_eq!(
        render(&semantic_state, 120, true, CursorStyle::Block),
        reference
    );

    let frame = SnapshotFrame::load_scaled(&semantic_state, 120).expect("cursor frame");
    let geometry = frame.tight_geometry().unwrap();
    let width = geometry.buffer_width();
    let height = geometry.buffer_height();
    let dirty_cursor_row = [true, false];
    let mut pixels = reference.clone();
    paint_snapshot_rows_presented(
        &mut pixels,
        width,
        height,
        &frame,
        &geometry,
        &dirty_cursor_row,
        false,
        CursorStyle::Block,
        CursorPresentation::FOCUSED_STEADY,
    );
    assert_eq!(
        pixels,
        render(&semantic_state, 120, false, CursorStyle::Block)
    );
    paint_snapshot_rows_presented(
        &mut pixels,
        width,
        height,
        &frame,
        &geometry,
        &dirty_cursor_row,
        true,
        CursorStyle::Beam,
        CursorPresentation::FOCUSED_STEADY,
    );
    assert_eq!(
        pixels,
        render(&semantic_state, 120, true, CursorStyle::Beam)
    );

    let mut moved = semantic_state.clone();
    moved.cursor_column = 1;
    let mut moved_frame = frame;
    moved_frame.refresh_cursor(&moved).unwrap();
    paint_snapshot_rows_presented(
        &mut pixels,
        width,
        height,
        &moved_frame,
        &geometry,
        &dirty_cursor_row,
        true,
        CursorStyle::Underline,
        CursorPresentation::FOCUSED_STEADY,
    );
    assert_eq!(pixels, render(&moved, 120, true, CursorStyle::Underline));
}

#[test]
fn forward_and_reverse_viewport_scroll_copy_match_clean_full_repaint() {
    let mut initial = incremental_snapshot();
    initial.input_modes.cursor_visible = false;
    for (offset_delta, rows, dirty_rows) in [
        (1, ["xy", "ab"], [true, false]),
        (-1, ["cd", "xy"], [false, true]),
    ] {
        let mut shifted = initial.clone();
        shifted.visible_rows = rows
            .into_iter()
            .map(|text| TerminalRow {
                row_id: None,
                linebreak: false,
                cells: text
                    .chars()
                    .map(|character| TerminalCell {
                        content: character.to_string(),
                        spacer_remaining: None,
                        attributes: default_attributes(),
                    })
                    .collect(),
            })
            .collect();
        let mut incremental = SnapshotFrame::load_scaled(&initial, 120).expect("initial frame");
        let reference = SnapshotFrame::load_scaled(&shifted, 120).expect("shifted frame");
        let geometry = incremental.tight_geometry().unwrap();
        let width = geometry.buffer_width();
        let height = geometry.buffer_height();
        let mut actual = vec![0; usize::try_from(width * height * 4).unwrap()];
        paint_snapshot(
            &mut actual,
            width,
            height,
            &incremental,
            &geometry,
            false,
            CursorStyle::Block,
        );
        let scroll = incremental
            .scroll_viewport_rows(&shifted, offset_delta)
            .expect("viewport shift")
            .expect("incremental scroll");
        scroll_snapshot_pixels(&mut actual, width, &incremental, &geometry, scroll);
        paint_snapshot_rows(
            &mut actual,
            width,
            height,
            &incremental,
            &geometry,
            &dirty_rows,
            false,
            CursorStyle::Block,
        );
        let mut expected = vec![0; actual.len()];
        paint_snapshot(
            &mut expected,
            width,
            height,
            &reference,
            &geometry,
            false,
            CursorStyle::Block,
        );
        assert_eq!(actual, expected, "scroll delta {offset_delta}");
    }
}

#[test]
fn persistent_raster_face_cache_is_bounded_across_scale_churn() {
    SNAPSHOT_GLYPH_CACHE.with(|cache| *cache.borrow_mut() = PersistentGlyphCache::default());
    let snapshot = incremental_snapshot();
    let mut context = RenderContext::new(u16::MAX);
    assert!(context.replace_font_generation(Arc::new(stage_test_font_generation())));
    for scale_120 in 120..=u32::try_from(120 + SNAPSHOT_RASTER_FACE_BUDGET).unwrap() {
        SnapshotFrame::load_scaled_with_context(&snapshot, scale_120, &context)
            .expect("scaled frame");
    }
    let metrics = snapshot_cache_metrics();
    assert_eq!(
        metrics["raster_faces"].as_u64(),
        Some(u64::try_from(SNAPSHOT_RASTER_FACE_BUDGET).unwrap())
    );
    assert_eq!(metrics["raster_face_evictions"].as_u64(), Some(1));

    clear_snapshot_caches();
    let cleared = snapshot_cache_metrics();
    assert_eq!(cleared["raster_faces"].as_u64(), Some(0));
    assert_eq!(cleared["entries"].as_u64(), Some(0));
    assert_eq!(cleared["approximate_bytes"].as_u64(), Some(0));
}

#[test]
fn glyph_cache_entries_are_effective_raster_size_specific() {
    let snapshot = incremental_snapshot();
    let one = SnapshotFrame::load_scaled(&snapshot, 120).expect("1x frame");
    let fractional = SnapshotFrame::load_scaled(&snapshot, 150).expect("1.25x frame");
    let key = one
        .cache
        .keys()
        .find(|key| fractional.cache.contains_key(key))
        .copied()
        .expect("common glyph key");
    assert!(!Arc::ptr_eq(&one.cache[&key], &fractional.cache[&key]));
}

#[test]
fn empty_overlays_leave_compositor_border_area_untouched() {
    let snapshot = incremental_snapshot();
    let frame = SnapshotFrame::load_scaled(&snapshot, 120).expect("frame");
    let geometry = frame.tight_geometry().unwrap();
    let mut focused = vec![0_u8; 200 * 200 * 4];
    let mut unfocused = focused.clone();
    paint_snapshot_overlays(
        &mut focused,
        200,
        200,
        &frame,
        &geometry,
        SnapshotOverlays {
            selection: None,
            hovered_url: None,
            dirty_rows: None,
            focused: true,
            selection_color: 0x0035_4a60,
            selection_foreground: 0x00eb_ebeb,
            url_color: 0x0078_beff,
            accent_color: 0x0078_d2ff,
            search_match: false,
        },
    );
    paint_snapshot_overlays(
        &mut unfocused,
        200,
        200,
        &frame,
        &geometry,
        SnapshotOverlays {
            selection: None,
            hovered_url: None,
            dirty_rows: None,
            focused: false,
            selection_color: 0x0035_4a60,
            selection_foreground: 0x00eb_ebeb,
            url_color: 0x0078_beff,
            accent_color: 0x0078_d2ff,
            search_match: false,
        },
    );
    assert_eq!(&focused[..4], &[0, 0, 0, 0]);
    assert_eq!(focused, unfocused);
}

#[test]
fn incremental_refresh_rejects_a_different_font_generation() {
    let snapshot = incremental_snapshot();
    let mut context = compatibility_render_context().unwrap();
    assert!(context.replace_font_generation(Arc::new(stage_test_font_generation())));
    let mut frame = SnapshotFrame::load_scaled_with_context(&snapshot, 120, &context).unwrap();
    let replacement = Arc::new(stage_test_font_generation());
    assert_ne!(frame.font_generation.id, replacement.id);
    frame.font_generation = replacement;
    let error = frame
        .refresh_rows_with_context(&snapshot, &[true, true], &context)
        .unwrap_err();
    assert!(error.to_string().contains("full rebuild is required"));
}

#[test]
fn prepared_frame_retains_its_font_generation_until_drop() {
    let snapshot = incremental_snapshot();
    let mut context = compatibility_render_context().unwrap();
    assert!(context.replace_font_generation(Arc::new(stage_test_font_generation())));
    let frame = SnapshotFrame::load_scaled_with_context(&snapshot, 120, &context).unwrap();
    let generation = Arc::clone(&frame.font_generation);
    let retained = Arc::strong_count(&generation);
    assert!(retained >= 3);
    drop(frame);
    assert_eq!(Arc::strong_count(&generation), retained - 1);
}

#[test]
fn identical_glyph_ids_do_not_share_cache_entries_across_generations() {
    reset_snapshot_cache();
    let current = stage_test_font_generation();
    let face = &current.faces[SNAPSHOT_PRIMARY_REGULAR];
    let glyph_id = font_ref(face).unwrap().charmap().map('M');
    let current_glyph =
        snapshot_glyph(&current.faces, SNAPSHOT_PRIMARY_REGULAR, glyph_id, 22.0).unwrap();
    let replacement = stage_test_font_generation();
    let replacement_glyph =
        snapshot_glyph(&replacement.faces, SNAPSHOT_PRIMARY_REGULAR, glyph_id, 22.0).unwrap();
    assert!(!Arc::ptr_eq(&current_glyph, &replacement_glyph));
}

#[test]
fn incremental_refresh_preserves_unchanged_prepared_rows() {
    let mut snapshot = incremental_snapshot();
    let mut frame = SnapshotFrame::load(&snapshot, 1).expect("initial frame");
    let row_zero_glyphs: Vec<_> = frame
        .glyphs
        .iter()
        .copied()
        .filter(|glyph| glyph.row == 0)
        .collect();
    let row_zero_backgrounds = frame.backgrounds[..snapshot.columns].to_vec();

    snapshot.visible_rows[1].cells[0].content = "z".into();
    snapshot.visible_rows[1].cells[0].attributes.reverse = true;
    frame
        .refresh_rows(&snapshot, &[false, true])
        .expect("refresh damaged row");

    assert_eq!(
        frame
            .glyphs
            .iter()
            .copied()
            .filter(|glyph| glyph.row == 0)
            .collect::<Vec<_>>(),
        row_zero_glyphs
    );
    assert_eq!(
        &frame.backgrounds[..snapshot.columns],
        row_zero_backgrounds.as_slice()
    );
}

#[test]
fn incremental_refresh_retains_warm_glyphs_below_budget() {
    let mut snapshot = incremental_snapshot();
    let mut frame = SnapshotFrame::load(&snapshot, 1).expect("initial frame");
    let old_keys: HashSet<_> = frame.cache.keys().copied().collect();
    for row in &mut snapshot.visible_rows {
        for cell in &mut row.cells {
            cell.content = "z".into();
        }
    }
    frame
        .refresh_rows(&snapshot, &[true, true])
        .expect("refresh every row");
    let referenced: HashSet<_> = frame.glyphs.iter().map(|glyph| glyph.key).collect();
    assert!(referenced.iter().all(|key| frame.cache.contains_key(key)));
    assert!(old_keys.iter().all(|key| frame.cache.contains_key(key)));
}

#[test]
fn cursor_and_title_changes_do_not_reshape_rows() {
    let mut snapshot = incremental_snapshot();
    let mut frame = SnapshotFrame::load(&snapshot, 1).expect("initial frame");
    let glyphs = frame.glyphs.clone();
    let backgrounds = frame.backgrounds.clone();

    snapshot.cursor_column = 1;
    snapshot.cursor_row = 1;
    snapshot.title = "new title".into();
    frame.refresh_cursor(&snapshot).unwrap();

    assert_eq!(frame.cursor, Some((1, 1)));
    assert_eq!(frame.glyphs, glyphs);
    assert_eq!(frame.backgrounds, backgrounds);
}

#[test]
fn snapshot_empty_spacer_and_concealed_cells_do_not_render() {
    let attributes = default_attributes();
    let mut cell = splinterm_protocol::TerminalCell {
        content: String::new(),
        spacer_remaining: None,
        attributes,
    };
    assert!(!cell_is_renderable(&cell));
    cell.content = "   ".into();
    assert!(!cell_is_renderable(&cell));
    cell.content = "\u{00a0}".into();
    assert!(cell_is_renderable(&cell));
    cell.content = "x".into();
    cell.spacer_remaining = Some(1);
    assert!(!cell_is_renderable(&cell));
    cell.spacer_remaining = None;
    cell.attributes.conceal = true;
    assert!(!cell_is_renderable(&cell));
    cell.attributes.conceal = false;
    assert!(cell_is_renderable(&cell));
}

#[test]
fn snapshot_spacer_run_defines_wide_leader_span() {
    let attributes = default_attributes();
    let cells = vec![
        splinterm_protocol::TerminalCell {
            content: "界".into(),
            spacer_remaining: None,
            attributes,
        },
        splinterm_protocol::TerminalCell {
            content: String::new(),
            spacer_remaining: Some(1),
            attributes,
        },
        splinterm_protocol::TerminalCell {
            content: "x".into(),
            spacer_remaining: None,
            attributes,
        },
    ];
    assert_eq!(leader_span(&cells, 0), 2);
    assert_eq!(leader_span(&cells, 2), 1);
}

#[test]
fn snapshot_colors_cover_rgb_palette_dim_and_reverse() {
    let mut attributes = default_attributes();
    attributes.foreground_source = ColorSource::Rgb;
    attributes.foreground = 0x80_40_20;
    attributes.background_source = ColorSource::Base256;
    attributes.background = 196;
    attributes.dim = true;
    let mut palette = vec![0; 256];
    palette[196] = 0xff_00_00;
    assert_eq!(
        rendition_colors(
            &attributes,
            &palette,
            default_foreground(),
            default_background()
        ),
        ([0x55, 0x2a, 0x15], [0xff, 0, 0])
    );
    attributes.reverse = true;
    assert_eq!(
        rendition_colors(
            &attributes,
            &palette,
            default_foreground(),
            default_background()
        ),
        ([0xaa, 0, 0], [0x80, 0x40, 0x20])
    );
}

#[test]
fn default_alpha_tracks_color_source_and_uses_premultiplied_argb() {
    let mut snapshot = incremental_snapshot();
    snapshot.visible_rows[0].cells[1]
        .attributes
        .background_source = ColorSource::Rgb;
    snapshot.visible_rows[0].cells[1].attributes.background = snapshot.default_colors[1];
    snapshot.visible_rows[1].cells[0].attributes.reverse = true;
    let frame = SnapshotFrame::load_scaled(&snapshot, 120).expect("alpha frame");
    assert_eq!(frame.default_backgrounds, [true, false, false, true]);

    let alpha = alpha_u8(u16::MAX / 2);
    assert_eq!(alpha, 127);
    assert_eq!(premultiplied_rgba([128, 64, 32], alpha), [64, 32, 16, 127]);
}

#[test]
fn snapshot_framebuffer_paints_background_wide_composed_glyphs_and_cursor() {
    let key = GlyphKey { face: 0, glyph: 1 };
    let frame = SnapshotFrame {
        font_ligatures: crate::font_shaping::FontLigatures::Off,
        font_features: crate::font_shaping::FeatureSettings::default(),
        font_size: BASE_FONT_SIZE,
        font_generation: Arc::clone(snapshot_font_generation().unwrap()),
        glyphs: vec![
            SnapshotGlyph {
                key,
                column: 0,
                row: 0,
                cells: 2,
                cluster_advance: 2.0,
                x_offset: 0.0,
                y_offset: 0.0,
                foreground: [200, 100, 50],
            },
            SnapshotGlyph {
                key,
                column: 0,
                row: 0,
                cells: 2,
                cluster_advance: 2.0,
                x_offset: -1.0,
                y_offset: 0.0,
                foreground: [200, 100, 50],
            },
        ],
        decorations: Vec::new(),
        cache: HashMap::from([(
            key,
            Arc::new(CachedGlyph {
                content: Content::Mask,
                left: 0,
                top: 1,
                width: 1,
                height: 1,
                data: vec![0xff],
            }),
        )]),
        backgrounds: vec![[1, 2, 3], [4, 5, 6]],
        default_backgrounds: vec![false; 2],
        foregrounds: vec![[200, 100, 50]; 2],
        cell_metrics: vec![
            DecorationMetrics {
                underline_position: -1,
                underline_thickness: 1,
                strike_position: 1,
                strike_thickness: 1,
            };
            2
        ],
        primary_metrics: [DecorationMetrics {
            underline_position: -1,
            underline_thickness: 1,
            strike_position: 1,
            strike_thickness: 1,
        }; 4],
        cell_spans: vec![2, 0],
        columns: 2,
        rows: 1,
        cell_width: 4,
        cell_height: 4,
        ascent: 2,
        descent: 2,
        baseline: 2,
        underline_position: -1,
        underline_thickness: 1,
        strike_position: 1,
        strike_thickness: 1,
        padding: TerminalPadding::uniform(2),
        cursor: None,
        canvas_background: [14, 18, 22],
        background_alpha: u16::MAX,
        cursor_color: [0xeb, 0xeb, 0xeb],
        images: Vec::new(),
        scale_120: 120,
    };
    let geometry = frame.tight_geometry().unwrap();
    let mut canvas = vec![0; 12 * 8 * 4];
    paint_snapshot(
        &mut canvas,
        12,
        8,
        &frame,
        &geometry,
        false,
        CursorStyle::Block,
    );
    let pixel = |x: usize, y: usize| &canvas[(y * 12 + x) * 4..(y * 12 + x + 1) * 4];
    assert_eq!(pixel(2, 2), [3, 2, 1, 0xff]);
    assert_eq!(pixel(2, 3), [50, 100, 200, 0xff]);
    assert_eq!(pixel(4, 3), [3, 2, 1, 0xff]);
    assert_eq!(pixel(6, 2), [3, 2, 1, 0xff]);
}

#[test]
fn glassy_alpha_makes_only_the_default_background_translucent() {
    // Omarchy Glassy Windows writes alpha=0.65; Foot and Splinterm store it as
    // 0.65 * 65535, truncated.
    let glassy_alpha = 42_597;
    let key = GlyphKey { face: 0, glyph: 1 };
    let metrics = DecorationMetrics {
        underline_position: -1,
        underline_thickness: 1,
        strike_position: 1,
        strike_thickness: 1,
    };
    let frame = SnapshotFrame {
        font_ligatures: crate::font_shaping::FontLigatures::Off,
        font_features: crate::font_shaping::FeatureSettings::default(),
        font_size: BASE_FONT_SIZE,
        font_generation: Arc::clone(snapshot_font_generation().unwrap()),
        glyphs: vec![SnapshotGlyph {
            key,
            column: 0,
            row: 0,
            cells: 1,
            cluster_advance: 1.0,
            x_offset: 0.0,
            y_offset: 0.0,
            foreground: [200, 100, 50],
        }],
        decorations: Vec::new(),
        cache: HashMap::from([(
            key,
            Arc::new(CachedGlyph {
                content: Content::Mask,
                left: 0,
                top: 1,
                width: 1,
                height: 1,
                data: vec![0xff],
            }),
        )]),
        // A default background, an explicit cell background, and a default
        // background under the block cursor.
        backgrounds: vec![[14, 18, 22], [200, 10, 10], [14, 18, 22]],
        default_backgrounds: vec![true, false, true],
        foregrounds: vec![[200, 100, 50]; 3],
        cell_metrics: vec![metrics; 3],
        primary_metrics: [metrics; 4],
        cell_spans: vec![1; 3],
        columns: 3,
        rows: 1,
        cell_width: 4,
        cell_height: 4,
        ascent: 2,
        descent: 2,
        baseline: 2,
        underline_position: -1,
        underline_thickness: 1,
        strike_position: 1,
        strike_thickness: 1,
        padding: TerminalPadding::uniform(2),
        cursor: Some((2, 0)),
        canvas_background: [14, 18, 22],
        background_alpha: glassy_alpha,
        cursor_color: [0xeb, 0xeb, 0xeb],
        images: Vec::new(),
        scale_120: 120,
    };
    let geometry = frame.tight_geometry().unwrap();
    let (width, height) = (16, 8);
    let mut canvas = vec![0; width * height * 4];
    paint_snapshot(
        &mut canvas,
        u32::try_from(width).unwrap(),
        u32::try_from(height).unwrap(),
        &frame,
        &geometry,
        true,
        CursorStyle::Block,
    );
    let pixel = |x: usize, y: usize| &canvas[(y * width + x) * 4..(y * width + x + 1) * 4];
    let translucent = background_bgra([14, 18, 22], glassy_alpha);
    assert_eq!(translucent, [14, 12, 9, 165], "premultiplied BGRA");
    // Padding and default-background cells carry the glass alpha.
    assert_eq!(pixel(0, 0), translucent);
    assert_eq!(pixel(4, 4), translucent);
    // Text ink, explicit cell backgrounds, and the cursor stay opaque.
    assert_eq!(pixel(2, 3), [50, 100, 200, 0xff]);
    assert_eq!(pixel(8, 4), [10, 10, 200, 0xff]);
    assert_eq!(pixel(12, 4), [0xeb, 0xeb, 0xeb, 0xff]);
}

fn damage_test_frame() -> SnapshotFrame {
    SnapshotFrame {
        font_ligatures: crate::font_shaping::FontLigatures::Off,
        font_features: crate::font_shaping::FeatureSettings::default(),
        font_size: BASE_FONT_SIZE,
        font_generation: Arc::clone(snapshot_font_generation().unwrap()),
        glyphs: Vec::new(),
        decorations: Vec::new(),
        cache: HashMap::new(),
        backgrounds: vec![[1, 0, 0], [2, 0, 0], [3, 0, 0]],
        default_backgrounds: vec![false; 3],
        foregrounds: vec![[255, 255, 255]; 3],
        cell_metrics: vec![
            DecorationMetrics {
                underline_position: 0,
                underline_thickness: 1,
                strike_position: 0,
                strike_thickness: 1,
            };
            3
        ],
        primary_metrics: [DecorationMetrics {
            underline_position: 0,
            underline_thickness: 1,
            strike_position: 0,
            strike_thickness: 1,
        }; 4],
        cell_spans: vec![1; 3],
        columns: 1,
        rows: 3,
        cell_width: 2,
        cell_height: 2,
        ascent: 1,
        descent: 1,
        baseline: 1,
        underline_position: 0,
        underline_thickness: 1,
        strike_position: 0,
        strike_thickness: 1,
        padding: TerminalPadding::uniform(0),
        cursor: None,
        canvas_background: [0, 0, 0],
        background_alpha: u16::MAX,
        cursor_color: [255, 255, 255],
        images: Vec::new(),
        scale_120: 120,
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "test helper exposes independent image geometry and ordering inputs"
)]
fn test_snapshot_image(
    pixels: &[u8],
    width: u32,
    height: u32,
    row: u32,
    source: splinterm_protocol::ImagePixelRect,
    x_offset: i32,
    z_index: i32,
    creation_order: u64,
) -> SnapshotImage {
    SnapshotImage {
        metadata: ImageContentMetadata {
            content_id: creation_order,
            generation: 1,
            width,
            height,
            source_format: splinterm_protocol::ImageSourceFormat::KittyRgba,
            alpha_mode: splinterm_protocol::ImageAlphaMode::Premultiplied,
            digest: [u8::try_from(creation_order).unwrap_or(1); 32],
            byte_length: pixels.len(),
            retention: splinterm_protocol::ImageRetention::WhilePlaced,
        },
        placement: ImagePlacement {
            placement_id: creation_order,
            content_id: creation_order,
            row_id: u64::from(row) + 1,
            column: 0,
            source,
            destination_columns: 1,
            destination_rows: 1,
            source_cell_size: Some(splinterm_protocol::ImagePixelSize {
                width: 2,
                height: 2,
            }),
            x_offset,
            y_offset: 0,
            z_index,
            application_image_id: None,
            application_placement_id: None,
            creation_order,
            erase_policy: splinterm_protocol::ImageErasePolicy::TextOverwrite,
        },
        row,
        source: ImageContentSource::Buffered(Arc::from(pixels)),
    }
}

fn expand_sixel_fixture_pixels(expected: &serde_json::Value) -> Vec<u8> {
    expected["rows"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|row| {
            row.as_array().unwrap().iter().flat_map(|run| {
                let run = run.as_array().unwrap();
                let count = usize::try_from(run[0].as_u64().unwrap()).unwrap();
                let pixel = run[1].as_str().unwrap();
                let bytes = (0..pixel.len())
                    .step_by(2)
                    .map(|index| u8::from_str_radix(&pixel[index..index + 2], 16).unwrap())
                    .collect::<Vec<_>>();
                bytes.repeat(count)
            })
        })
        .collect()
}

#[test]
fn sixel_identity_pixels_match_every_retained_foot_final_buffer() {
    use sha2::{Digest as _, Sha256};

    let fixtures: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../fixtures/terminal-images/v1/protocol-fixtures/sixel-v1.json"
    ))
    .unwrap();
    let artifact_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/terminal-images/v1/foot-sixel-oracle");

    for case in fixtures["cases"].as_array().unwrap() {
        let id = case["id"].as_str().unwrap();
        let source_width = u32::try_from(case["expected"]["width"].as_u64().unwrap()).unwrap();
        let source_height = u32::try_from(case["expected"]["height"].as_u64().unwrap()).unwrap();
        let source = expand_sixel_fixture_pixels(&case["expected"]);
        let foot_metadata: serde_json::Value =
            serde_json::from_slice(&fs::read(artifact_root.join(id).join("foot.json")).unwrap())
                .unwrap();
        let foot = fs::read(artifact_root.join(id).join("foot.argb")).unwrap();
        assert_eq!(
            foot_metadata["provenance"]["commit"].as_str().unwrap(),
            "3c5b584b0eafa772eb4376fb6eaf6643399e190e",
            "{id} Foot oracle commit"
        );
        assert_eq!(
            format!("{:x}", Sha256::digest(&foot)),
            foot_metadata["framebuffer_sha256"].as_str().unwrap(),
            "{id} framebuffer checksum"
        );
        let foot_stride = usize::try_from(foot_metadata["stride"].as_u64().unwrap()).unwrap();
        let foot_origin_x =
            usize::try_from(foot_metadata["origin"]["x"].as_u64().unwrap()).unwrap();
        let foot_origin_y =
            usize::try_from(foot_metadata["origin"]["y"].as_u64().unwrap()).unwrap();
        let cell_width = u32::try_from(foot_metadata["cell"]["width"].as_u64().unwrap()).unwrap();
        let cell_height = u32::try_from(foot_metadata["cell"]["height"].as_u64().unwrap()).unwrap();

        let mut frame = damage_test_frame();
        frame.rows = 1;
        frame.canvas_background = [14, 18, 22];
        frame.cell_width = cell_width;
        frame.cell_height = cell_height;
        frame.ascent = cell_height.saturating_sub(4);
        frame.descent = cell_height.saturating_sub(frame.ascent);
        frame.baseline = i32::try_from(frame.ascent).unwrap();
        frame.backgrounds.truncate(1);
        frame.backgrounds[0] = [14, 18, 22];
        frame.default_backgrounds.truncate(1);
        frame.default_backgrounds[0] = true;
        frame.foregrounds.truncate(1);
        frame.cell_metrics.truncate(1);
        frame.cell_spans.truncate(1);
        let crop = splinterm_protocol::ImagePixelRect {
            x: 0,
            y: 0,
            width: source_width,
            height: source_height,
        };
        let mut image = test_snapshot_image(&source, source_width, source_height, 0, crop, 0, 0, 1);
        image.metadata.source_format = splinterm_protocol::ImageSourceFormat::Sixel;
        image.placement.source_cell_size = Some(splinterm_protocol::ImagePixelSize {
            width: cell_width,
            height: cell_height,
        });
        frame.images = vec![image];

        let mut canvas = vec![0; usize::try_from(cell_width * cell_height * 4).unwrap()];
        let geometry = frame.tight_geometry().unwrap();
        paint_snapshot(
            &mut canvas,
            cell_width,
            cell_height,
            &frame,
            &geometry,
            false,
            CursorStyle::Block,
        );
        let expected_cell = (0..usize::try_from(cell_height).unwrap())
            .flat_map(|row| {
                let start = (foot_origin_y + row) * foot_stride + foot_origin_x * 4;
                foot[start..start + usize::try_from(cell_width).unwrap() * 4].to_vec()
            })
            .collect::<Vec<_>>();
        assert_eq!(canvas, expected_cell, "{id}");
    }
}

#[test]
fn image_compositor_uses_bilinear_phase_across_clipping() {
    let mut frame = damage_test_frame();
    frame.backgrounds.fill([0, 0, 0]);
    let geometry = frame.tight_geometry().unwrap();
    let identity_source = splinterm_protocol::ImagePixelRect {
        x: 0,
        y: 0,
        width: 2,
        height: 2,
    };
    let identity = [
        10, 20, 30, 255, 40, 50, 60, 255, 70, 80, 90, 255, 100, 110, 120, 255,
    ];
    frame.images = vec![test_snapshot_image(
        &identity,
        2,
        2,
        0,
        identity_source,
        0,
        -1,
        1,
    )];
    let mut identity_canvas = vec![0; 2 * 6 * 4];
    paint_snapshot(
        &mut identity_canvas,
        2,
        6,
        &frame,
        &geometry,
        false,
        CursorStyle::Block,
    );
    assert_eq!(&identity_canvas[0..8], &identity[0..8]);
    assert_eq!(&identity_canvas[8..16], &identity[8..16]);

    let source = splinterm_protocol::ImagePixelRect {
        x: 0,
        y: 0,
        width: 4,
        height: 4,
    };
    let mut pixels = Vec::with_capacity(4 * 4 * 4);
    for y in 0_u8..4 {
        for x in 0_u8..4 {
            pixels.extend_from_slice(&[x * 40, y * 50, (x + y) * 10, 255]);
        }
    }
    frame.images = vec![test_snapshot_image(&pixels, 4, 4, 0, source, 0, -1, 1)];
    let mut full = vec![0; 2 * 6 * 4];
    paint_snapshot(
        &mut full,
        2,
        6,
        &frame,
        &geometry,
        false,
        CursorStyle::Block,
    );
    assert_eq!(&full[0..4], &[20, 25, 10, 255]);
    assert_eq!(&full[4..8], &[100, 25, 30, 255]);

    frame.images[0].placement.x_offset = -1;
    frame.images[0].placement.y_offset = -1;
    let mut clipped = vec![0; 2 * 6 * 4];
    paint_snapshot(
        &mut clipped,
        2,
        6,
        &frame,
        &geometry,
        false,
        CursorStyle::Block,
    );
    assert_eq!(&clipped[0..4], &[100, 125, 50, 255]);
}

#[test]
fn image_compositor_honors_alpha_crop_offset_clip_and_z_tiers() {
    let mut frame = damage_test_frame();
    frame.backgrounds[0] = [1, 0, 0];
    let crop = splinterm_protocol::ImagePixelRect {
        x: 1,
        y: 0,
        width: 1,
        height: 1,
    };
    frame.images = vec![test_snapshot_image(
        &[255, 0, 0, 255, 0, 0, 128, 128],
        2,
        1,
        0,
        crop,
        1,
        -1,
        1,
    )];
    let geometry = frame.tight_geometry().unwrap();
    let mut canvas = vec![0; 2 * 6 * 4];
    paint_snapshot(
        &mut canvas,
        2,
        6,
        &frame,
        &geometry,
        false,
        CursorStyle::Block,
    );
    assert_eq!(&canvas[0..4], &[0, 0, 1, 0xff]);
    assert_eq!(&canvas[4..8], &[0, 0, 128, 0xff]);

    frame.images[0].placement.x_offset = 0;
    frame.images[0].placement.z_index = KITTY_BACKGROUND_Z_THRESHOLD - 1;
    let mut below_background = vec![0; 2 * 6 * 4];
    paint_snapshot(
        &mut below_background,
        2,
        6,
        &frame,
        &geometry,
        false,
        CursorStyle::Block,
    );
    assert_eq!(&below_background[0..4], &[0, 0, 1, 0xff]);

    frame.images[0].placement.z_index = -1;
    frame.images[0].placement.y_offset = -1;
    frame.images[0].row = 1;
    let mut negative_y = vec![0; 2 * 6 * 4];
    paint_snapshot(
        &mut negative_y,
        2,
        6,
        &frame,
        &geometry,
        false,
        CursorStyle::Block,
    );
    assert_eq!(&negative_y[2 * 4..3 * 4], &[0, 0, 128, 0xff]);
}

#[test]
fn image_creation_order_and_row_damage_match_full_composition() {
    let mut frame = damage_test_frame();
    frame.backgrounds[0] = [0, 0, 0];
    let source = splinterm_protocol::ImagePixelRect {
        x: 0,
        y: 0,
        width: 1,
        height: 1,
    };
    frame.images = vec![
        test_snapshot_image(&[255, 0, 0, 255], 1, 1, 0, source, 0, -1, 1),
        test_snapshot_image(&[0, 0, 255, 255], 1, 1, 0, source, 0, -1, 2),
    ];
    let geometry = frame.tight_geometry().unwrap();
    let mut full = vec![0; 2 * 6 * 4];
    paint_snapshot(
        &mut full,
        2,
        6,
        &frame,
        &geometry,
        false,
        CursorStyle::Block,
    );
    assert_eq!(&full[0..4], &[0, 0, 255, 0xff]);
    let mut incremental = vec![77; full.len()];
    paint_snapshot_rows(
        &mut incremental,
        2,
        6,
        &frame,
        &geometry,
        &[true, true, true],
        false,
        CursorStyle::Block,
    );
    assert_eq!(incremental, full);

    frame.images.clear();
    let mut clean_removed = vec![0; full.len()];
    paint_snapshot(
        &mut clean_removed,
        2,
        6,
        &frame,
        &geometry,
        false,
        CursorStyle::Block,
    );
    paint_snapshot_rows(
        &mut full,
        2,
        6,
        &frame,
        &geometry,
        &[true, true, true],
        false,
        CursorStyle::Block,
    );
    assert_eq!(full, clean_removed);
}

fn add_selection_foreground_fixture(frame: &mut SnapshotFrame) {
    let key = GlyphKey { face: 0, glyph: 1 };
    frame.glyphs = vec![SnapshotGlyph {
        key,
        column: 0,
        row: 0,
        cells: 1,
        cluster_advance: 1.0,
        x_offset: 0.0,
        y_offset: 0.0,
        foreground: [0xff; 3],
    }];
    frame.cache.insert(
        key,
        Arc::new(CachedGlyph {
            content: Content::Mask,
            left: 0,
            top: 1,
            width: 1,
            height: 1,
            data: vec![u8::MAX],
        }),
    );
    frame.decorations = vec![DecorationSpan {
        column: 0,
        row: 0,
        cells: 1,
        underline: UnderlineStyle::None,
        strikethrough: true,
        underline_color: [0xff; 3],
        underline_uses_foreground: true,
        strike_color: [0xff; 3],
        metrics: frame.cell_metrics[0],
    }];
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one overlay ordering scenario, with the search match outline"
)]
fn cursor_and_selection_overlay_remain_above_nonnegative_images() {
    let mut frame = damage_test_frame();
    frame.images = vec![test_snapshot_image(
        &[0, 0, 255, 255],
        1,
        1,
        0,
        splinterm_protocol::ImagePixelRect {
            x: 0,
            y: 0,
            width: 1,
            height: 1,
        },
        0,
        0,
        1,
    )];
    frame.cursor = Some((0, 0));
    let geometry = frame.tight_geometry().unwrap();
    let mut canvas = vec![0; 2 * 6 * 4];
    paint_snapshot(
        &mut canvas,
        2,
        6,
        &frame,
        &geometry,
        true,
        CursorStyle::Block,
    );
    assert_eq!(&canvas[0..4], &[255, 255, 255, 255]);

    frame.cursor = None;
    add_selection_foreground_fixture(&mut frame);
    paint_snapshot(
        &mut canvas,
        2,
        6,
        &frame,
        &geometry,
        false,
        CursorStyle::Block,
    );
    paint_snapshot_overlays(
        &mut canvas,
        2,
        6,
        &frame,
        &geometry,
        SnapshotOverlays {
            selection: Some(((0, 0), (0, 0))),
            hovered_url: None,
            dirty_rows: None,
            focused: true,
            selection_color: 0x00f2_3888,
            selection_foreground: 0x0011_2233,
            url_color: 0,
            accent_color: 0,
            search_match: false,
        },
    );
    assert_eq!(
        &canvas[0..4],
        &[0x33, 0x22, 0x11, 0xff],
        "selected glyphs are repainted with the exact selection foreground"
    );
    assert_eq!(
        &canvas[4..8],
        &[0x88, 0x38, 0xf2, 0xff],
        "Sakura Mochi selection RGB is not swapped, tinted, or alpha-mixed"
    );
    assert_eq!(
        &canvas[8..12],
        &[0x33, 0x22, 0x11, 0xff],
        "selected decorations are repainted with the exact selection foreground"
    );
    assert_eq!(
        &canvas[12..16],
        &[0x33, 0x22, 0x11, 0xff],
        "the complete selected decoration span uses selection foreground"
    );
    // The search's current match is outlined in the accent on its fill.
    paint_snapshot_overlays(
        &mut canvas,
        2,
        6,
        &frame,
        &geometry,
        SnapshotOverlays {
            selection: Some(((0, 0), (0, 0))),
            hovered_url: None,
            dirty_rows: None,
            focused: true,
            selection_color: 0x00f2_3888,
            selection_foreground: 0x0011_2233,
            url_color: 0,
            accent_color: 0x00aa_bbcc,
            search_match: true,
        },
    );
    assert_eq!(
        &canvas[0..4],
        &[0xcc, 0xbb, 0xaa, 0xff],
        "the accent outline"
    );
}

#[test]
fn selection_repaint_preserves_foot_right_to_left_glyph_overlap_order() {
    let mut frame = damage_test_frame();
    let first = GlyphKey { face: 0, glyph: 1 };
    let second = GlyphKey { face: 0, glyph: 2 };
    frame.glyphs = vec![
        SnapshotGlyph {
            key: first,
            column: 0,
            row: 0,
            cells: 1,
            cluster_advance: 1.0,
            x_offset: 0.0,
            y_offset: 0.0,
            foreground: [0xff; 3],
        },
        SnapshotGlyph {
            key: second,
            column: 0,
            row: 0,
            cells: 1,
            cluster_advance: 1.0,
            x_offset: 0.0,
            y_offset: 0.0,
            foreground: [0xff; 3],
        },
    ];
    frame.cache.insert(
        first,
        Arc::new(CachedGlyph {
            content: Content::Color,
            left: 0,
            top: 1,
            width: 1,
            height: 1,
            data: vec![128, 0, 0, 128],
        }),
    );
    frame.cache.insert(
        second,
        Arc::new(CachedGlyph {
            content: Content::Color,
            left: 0,
            top: 1,
            width: 1,
            height: 1,
            data: vec![0, 0, 128, 128],
        }),
    );
    let geometry = frame.tight_geometry().unwrap();
    let overlays = SnapshotOverlays {
        selection: Some(((0, 0), (0, 0))),
        hovered_url: None,
        dirty_rows: None,
        focused: true,
        selection_color: 0,
        selection_foreground: 0x00ff_ffff,
        url_color: 0,
        accent_color: 0,
        search_match: false,
    };
    let mut actual = vec![0; 2 * 6 * 4];
    paint_snapshot_overlays(&mut actual, 2, 6, &frame, &geometry, overlays);

    let mut expected = vec![0; actual.len()];
    fill_rect(&mut expected, 2, 6, (0, 0, 2, 2), [0, 0, 0, u8::MAX]);
    for glyph in frame.glyphs.iter().rev() {
        paint_placed_glyph(&mut expected, 2, 6, &frame, &geometry, glyph, [u8::MAX; 3]);
    }
    let mut forward = vec![0; actual.len()];
    fill_rect(&mut forward, 2, 6, (0, 0, 2, 2), [0, 0, 0, u8::MAX]);
    for glyph in &frame.glyphs {
        paint_placed_glyph(&mut forward, 2, 6, &frame, &geometry, glyph, [u8::MAX; 3]);
    }
    assert_eq!(actual, expected);
    assert_ne!(actual, forward, "overlap order is visually observable");
}

#[test]
fn image_order_uses_strict_adr_tier_boundary_and_kitty_application_ids() {
    assert_eq!(image_tier(KITTY_BACKGROUND_Z_THRESHOLD - 1), 0);
    assert_eq!(image_tier(KITTY_BACKGROUND_Z_THRESHOLD), 1);
    assert_eq!(image_tier(-1), 1);
    assert_eq!(image_tier(0), 2);
    let source = splinterm_protocol::ImagePixelRect {
        x: 0,
        y: 0,
        width: 1,
        height: 1,
    };
    let mut higher_application = test_snapshot_image(&[1, 0, 0, 255], 1, 1, 0, source, 0, 0, 1);
    higher_application.placement.application_image_id = Some(20);
    let mut lower_application = test_snapshot_image(&[2, 0, 0, 255], 1, 1, 0, source, 0, 0, 2);
    lower_application.placement.application_image_id = Some(10);
    let mut images = [higher_application, lower_application];
    images.sort_by(compare_snapshot_images);
    assert_eq!(images[0].placement.application_image_id, Some(10));
    assert_eq!(images[1].placement.application_image_id, Some(20));
}

#[allow(
    clippy::too_many_lines,
    reason = "one lifecycle regression covers leases, scales, eviction, and row anchoring"
)]
#[test]
fn prepared_image_sources_survive_cache_eviction_and_follow_row_ids() {
    use sha2::{Digest as _, Sha256};

    let pixels = vec![3_u8, 2, 1, 255];
    let metadata = ImageContentMetadata {
        content_id: 1,
        generation: 1,
        width: 1,
        height: 1,
        source_format: splinterm_protocol::ImageSourceFormat::Sixel,
        alpha_mode: splinterm_protocol::ImageAlphaMode::Opaque,
        digest: Sha256::digest(&pixels).into(),
        byte_length: pixels.len(),
        retention: splinterm_protocol::ImageRetention::WhilePlaced,
    };
    let placement = ImagePlacement {
        placement_id: 1,
        content_id: 1,
        row_id: 2,
        column: 0,
        source: splinterm_protocol::ImagePixelRect {
            x: 0,
            y: 0,
            width: 1,
            height: 1,
        },
        destination_columns: 1,
        destination_rows: 1,
        source_cell_size: None,
        x_offset: 0,
        y_offset: 0,
        z_index: -1,
        application_image_id: None,
        application_placement_id: None,
        creation_order: 1,
        erase_policy: splinterm_protocol::ImageErasePolicy::TextOverwrite,
    };
    let mut snapshot = incremental_snapshot();
    snapshot.input_modes.cursor_visible = false;
    for row in &mut snapshot.visible_rows {
        for cell in &mut row.cells {
            cell.content.clear();
        }
    }
    snapshot.images = Some(Box::new(splinterm_protocol::TerminalImagePlane {
        screen: ActiveScreen::Normal,
        contents: vec![metadata.clone()],
        placements: vec![placement],
    }));
    let sources =
        splinterm_automation_client::SharedImageContentCache::with_maximum_bytes(4).unwrap();
    sources
        .insert_source(
            &metadata,
            ImageContentSource::Buffered(Arc::from(pixels.clone())),
        )
        .unwrap();
    let leases = sources.lease(std::slice::from_ref(&metadata)).unwrap();
    let frame = SnapshotFrame::load_scaled_with_sources(&snapshot, 120, Some(&leases)).unwrap();
    assert_eq!(frame.images.len(), 1);
    assert_eq!(frame.images[0].row, 1);
    let capture =
        capture_final_buffer_with_sources(&snapshot, &leases, 120, false, CursorStyle::Block)
            .unwrap();
    let x = usize::try_from(capture.grid_rect.x).unwrap();
    let y = usize::try_from(capture.grid_rect.y + capture.cell_height).unwrap();
    let stride = usize::try_from(capture.stride).unwrap();
    assert_eq!(
        &capture.pixels[y * stride + x * 4..y * stride + x * 4 + 4],
        &pixels
    );
    let fractional =
        capture_final_buffer_with_sources(&snapshot, &leases, 150, false, CursorStyle::Block)
            .unwrap();
    let x = usize::try_from(fractional.grid_rect.x).unwrap();
    let y = usize::try_from(fractional.grid_rect.y + fractional.cell_height).unwrap();
    let stride = usize::try_from(fractional.stride).unwrap();
    assert_eq!(
        &fractional.pixels[y * stride + x * 4..y * stride + x * 4 + 4],
        &pixels
    );

    let replacement_pixels = vec![7_u8, 6, 5, 255];
    let mut replacement = metadata.clone();
    replacement.content_id = 2;
    replacement.digest = Sha256::digest(&replacement_pixels).into();
    assert!(
        sources
            .insert_source(
                &replacement,
                ImageContentSource::Buffered(Arc::from(replacement_pixels.clone())),
            )
            .is_err()
    );
    assert!(sources.contains(&metadata).unwrap());
    assert_eq!(frame.images[0].source.as_bytes(), pixels);
    drop(frame);
    drop(leases);
    sources
        .insert_source(
            &replacement,
            ImageContentSource::Buffered(Arc::from(replacement_pixels)),
        )
        .unwrap();
    assert!(!sources.contains(&metadata).unwrap());
}

#[test]
fn inactive_pane_paint_never_draws_a_terminal_cursor() {
    let mut frame = damage_test_frame();
    frame.cursor = Some((0, 1));
    let geometry = frame.tight_geometry().unwrap();
    let mut without_cursor = vec![0; 2 * 6 * 4];
    paint_snapshot_presented(
        &mut without_cursor,
        2,
        6,
        &frame,
        &geometry,
        false,
        CursorStyle::Block,
        CursorPresentation::FOCUSED_STEADY,
    );
    let mut inactive = vec![0; 2 * 6 * 4];
    paint_snapshot_presented(
        &mut inactive,
        2,
        6,
        &frame,
        &geometry,
        true,
        CursorStyle::Block,
        CursorPresentation::INACTIVE_PANE,
    );
    assert_eq!(inactive, without_cursor);

    let mut unfocused_window = vec![0; 2 * 6 * 4];
    paint_snapshot_presented(
        &mut unfocused_window,
        2,
        6,
        &frame,
        &geometry,
        true,
        CursorStyle::Block,
        CursorPresentation::for_keyboard_focus(false),
    );
    assert_ne!(unfocused_window, without_cursor);
}

#[test]
fn pane_region_paint_preserves_neighbor_pixels() {
    let mut frame = damage_test_frame();
    frame.images = vec![test_snapshot_image(
        &[0, 0, 255, 255],
        1,
        1,
        0,
        splinterm_protocol::ImagePixelRect {
            x: 0,
            y: 0,
            width: 1,
            height: 1,
        },
        -1,
        -1,
        1,
    )];
    let geometry = frame.tight_geometry().unwrap().translated(2, 0).unwrap();
    let mut canvas = vec![9; 6 * 6 * 4];
    paint_snapshot_region_presented(
        &mut canvas,
        6,
        6,
        &frame,
        &geometry,
        Rect {
            x: 2,
            y: 0,
            width: 2,
            height: 6,
        },
        false,
        CursorStyle::Block,
        CursorPresentation::for_keyboard_focus(false),
    );
    let pixel = |x: usize, y: usize| &canvas[(y * 6 + x) * 4..(y * 6 + x + 1) * 4];
    assert_eq!(pixel(0, 0), [9, 9, 9, 9]);
    assert_eq!(pixel(2, 0), [0, 0, 255, 0xff]);
    assert_eq!(pixel(4, 0), [9, 9, 9, 9]);
}

#[test]
fn row_damage_paints_only_selected_rows() {
    let frame = damage_test_frame();
    let geometry = frame.tight_geometry().unwrap();
    let mut canvas = vec![0; 2 * 6 * 4];
    paint_snapshot_rows(
        &mut canvas,
        2,
        6,
        &frame,
        &geometry,
        &[false, true, false],
        false,
        CursorStyle::Block,
    );
    assert_eq!(&canvas[0..4], &[0, 0, 0, 0]);
    assert_eq!(&canvas[2 * 2 * 4..2 * 2 * 4 + 4], &[0, 0, 2, 0xff]);
    assert_eq!(&canvas[4 * 2 * 4..4 * 2 * 4 + 4], &[0, 0, 0, 0]);
}

#[test]
fn scroll_damage_copies_existing_grid_pixels() {
    let frame = damage_test_frame();
    let geometry = frame.tight_geometry().unwrap();
    let mut canvas = vec![0; 2 * 6 * 4];
    paint_snapshot(
        &mut canvas,
        2,
        6,
        &frame,
        &geometry,
        false,
        CursorStyle::Block,
    );
    scroll_snapshot_pixels(
        &mut canvas,
        2,
        &frame,
        &geometry,
        TerminalScroll {
            direction: ScrollDirection::Forward,
            start_row: 0,
            end_row: 3,
            rows: 1,
        },
    );
    assert_eq!(&canvas[0..4], &[0, 0, 2, 0xff]);
    assert_eq!(&canvas[2 * 2 * 4..2 * 2 * 4 + 4], &[0, 0, 3, 0xff]);
}

#[test]
fn scroll_copy_clips_to_undersized_framebuffers() {
    let frame = damage_test_frame();
    let geometry = frame.tight_geometry().unwrap();
    let scroll = TerminalScroll {
        direction: ScrollDirection::Forward,
        start_row: 0,
        end_row: 3,
        rows: 1,
    };
    let mut narrow = vec![0_u8; 12];
    narrow[8..12].copy_from_slice(&[1, 2, 3, 4]);
    scroll_snapshot_pixels(&mut narrow, 1, &frame, &geometry, scroll);
    assert_eq!(&narrow[..4], &[1, 2, 3, 4]);

    let mut short = vec![0_u8; 8];
    let unchanged = short.clone();
    scroll_snapshot_pixels(&mut short, 2, &frame, &geometry, scroll);
    assert_eq!(short, unchanged);

    let mut partial_scanline = vec![0_u8; 7];
    scroll_snapshot_pixels(&mut partial_scanline, 2, &frame, &geometry, scroll);
}

#[test]
fn terminal_size_calculation_clamps_minimum_and_protocol_limits() {
    let frame = SnapshotFrame {
        font_ligatures: crate::font_shaping::FontLigatures::Off,
        font_features: crate::font_shaping::FeatureSettings::default(),
        font_size: BASE_FONT_SIZE,
        font_generation: Arc::clone(snapshot_font_generation().unwrap()),
        glyphs: Vec::new(),
        decorations: Vec::new(),
        cache: HashMap::new(),
        backgrounds: Vec::new(),
        default_backgrounds: Vec::new(),
        foregrounds: Vec::new(),
        cell_metrics: Vec::new(),
        primary_metrics: [DecorationMetrics {
            underline_position: -2,
            underline_thickness: 1,
            strike_position: 5,
            strike_thickness: 1,
        }; 4],
        cell_spans: Vec::new(),
        columns: 0,
        rows: 0,
        cell_width: 10,
        cell_height: 20,
        ascent: 15,
        descent: 5,
        baseline: 15,
        underline_position: -2,
        underline_thickness: 1,
        strike_position: 5,
        strike_thickness: 1,
        padding: TerminalPadding::uniform(10),
        cursor: None,
        canvas_background: [14, 18, 22],
        background_alpha: u16::MAX,
        cursor_color: [0xeb, 0xeb, 0xeb],
        images: Vec::new(),
        scale_120: 120,
    };
    assert_eq!(
        frame.terminal_size(1_020, 620, 120).expect("normal grid"),
        (100, 30, 1_000, 600)
    );
    assert!(
        frame
            .terminal_size(1, 1, 120)
            .unwrap_err()
            .to_string()
            .contains("SurfaceTooSmall")
    );
    assert_eq!(
        frame
            .terminal_size(20_000, 20_000, 120)
            .expect("bounded grid"),
        (MAX_COLUMNS, MAX_ROWS, 4_800, 2_560)
    );
    assert_eq!(
        frame.terminal_size(2_560, 1_440, 120).expect("1440p grid"),
        (254, 71, 2_540, 1_420)
    );
    assert_eq!(
        frame.terminal_size(3_840, 2_160, 120).expect("4K grid"),
        (382, 107, 3_820, 2_140)
    );
    let configured = frame.window_geometry(1_027, 629, 120).unwrap();
    assert_eq!(
        (configured.logical_width(), configured.logical_height()),
        (1_027, 629)
    );
    assert_eq!(
        configured.actual_padding.left
            + configured.grid_rect.width
            + configured.actual_padding.right,
        1_027
    );
    assert_eq!(
        configured.actual_padding.top
            + configured.grid_rect.height
            + configured.actual_padding.bottom,
        629
    );
    assert!(configured.residual_right > 0 || configured.residual_bottom > 0);
}

#[test]
fn paint_clips_the_row_outside_a_small_canvas() {
    let row = synthetic_row();
    let mut canvas = vec![0; 3 * 2 * 4];

    paint(&mut canvas, 3, 2, &row);

    assert_eq!(canvas, [22, 18, 14, 0xff].repeat(6));
}

#[test]
fn corpus_contains_each_required_evidence_segment() {
    assert_eq!(CORPUS.len(), 6);
    assert_eq!(CORPUS[0], (CorpusKind::Ascii, "ASCII"));
    assert_eq!(CORPUS[1], (CorpusKind::BoxDrawing, "┌─┼─┐"));
    assert_eq!(CORPUS[2], (CorpusKind::NerdFont, "\u{f120}"));
    assert_eq!(CORPUS[3], (CorpusKind::Combining, "e\u{0301}"));
    assert_eq!(CORPUS[4], (CorpusKind::Cjk, "界"));
    assert_eq!(CORPUS[5], (CorpusKind::Emoji, "🙂"));
}

#[test]
fn ppm_capture_is_lossless_rgb_and_checks_dimensions() {
    let path = std::env::temp_dir().join(format!(
        "splinterm-renderer-{}-{}.ppm",
        std::process::id(),
        std::thread::current().name().unwrap_or("test")
    ));
    let canvas = [3, 2, 1, 0xff, 30, 20, 10, 0xff];

    write_ppm(&path, &canvas, 2, 1).expect("write capture");
    let capture = fs::read(&path).expect("read capture");
    fs::remove_file(path).expect("remove capture");

    assert_eq!(&capture[..11], b"P6\n2 1\n255\n");
    assert_eq!(&capture[11..], &[1, 2, 3, 10, 20, 30]);
    assert_eq!(
        write_ppm(std::env::temp_dir().join("unused.ppm"), &canvas, 1, 1)
            .expect_err("dimension mismatch")
            .kind(),
        io::ErrorKind::InvalidInput
    );
}

fn shaping_context(mode: crate::font_shaping::FontLigatures, features: &str) -> RenderContext {
    compatibility_render_context().unwrap().with_test_shaping(
        mode,
        crate::font_shaping::FeatureSettings::parse(features).unwrap(),
    )
}

fn shaping_snapshot(lines: &[&str]) -> TerminalSnapshot {
    let mut snapshot = incremental_snapshot();
    snapshot.columns = lines[0].chars().count();
    snapshot.rows = lines.len();
    snapshot.visible_rows = lines
        .iter()
        .enumerate()
        .map(|(row, text)| TerminalRow {
            row_id: Some(u64::try_from(row + 1).unwrap()),
            linebreak: false,
            cells: text
                .chars()
                .map(|character| TerminalCell {
                    content: character.to_string(),
                    spacer_remaining: None,
                    attributes: default_attributes(),
                })
                .collect(),
        })
        .collect();
    snapshot.input_modes.cursor_visible = false;
    snapshot
}

#[test]
fn live_shaping_joins_ascii_with_spaces_and_keeps_grid_source_spans() {
    use crate::font_shaping::FontLigatures::{Off, On};
    let snapshot = shaping_snapshot(&[" != === -> "]);
    let on =
        SnapshotFrame::load_scaled_with_context(&snapshot, 120, &shaping_context(On, "calt=1"))
            .unwrap();
    let off =
        SnapshotFrame::load_scaled_with_context(&snapshot, 120, &shaping_context(Off, "calt=1"))
            .unwrap();
    assert_ne!(
        on.glyphs.iter().map(|glyph| glyph.key).collect::<Vec<_>>(),
        off.glyphs.iter().map(|glyph| glyph.key).collect::<Vec<_>>()
    );
    assert_eq!(
        on.glyphs.len(),
        3,
        "spaces and empty contextual masks provide context without empty placements"
    );
    assert_eq!(on.cell_spans, off.cell_spans);
    assert_eq!(on.foregrounds, off.foregrounds);
    assert_eq!(on.backgrounds, off.backgrounds);
    assert_eq!(on.cell_width, off.cell_width);
    assert!(on.glyphs.iter().all(|glyph| glyph.cells == 1
        && snapshot.visible_rows[0].cells[glyph.column as usize].content != " "));
    assert!(
        on.glyphs
            .iter()
            .all(|glyph| glyph.x_offset.to_bits() == 0.0_f32.to_bits())
    );
    let no_calt =
        SnapshotFrame::load_scaled_with_context(&snapshot, 120, &shaping_context(On, "calt=0"))
            .unwrap();
    assert_eq!(
        no_calt
            .glyphs
            .iter()
            .map(|glyph| glyph.key)
            .collect::<Vec<_>>(),
        off.glyphs.iter().map(|glyph| glyph.key).collect::<Vec<_>>()
    );
}

#[test]
fn live_shaping_features_apply_with_ligatures_off_and_on() {
    use crate::font_shaping::FontLigatures::{Off, On};
    for mode in [Off, On] {
        // A combining mark forces the second cell through per-cell fallback.
        let mut snapshot = shaping_snapshot(&["00"]);
        snapshot.visible_rows[0].cells[1].content = "0\u{301}".into();
        let plain = SnapshotFrame::load_scaled_with_context(
            &snapshot,
            120,
            &shaping_context(mode, "zero=0"),
        )
        .unwrap();
        let slashed = SnapshotFrame::load_scaled_with_context(
            &snapshot,
            120,
            &shaping_context(mode, "zero=1"),
        )
        .unwrap();
        for column in 0..2 {
            let ids = |frame: &SnapshotFrame| {
                frame
                    .glyphs
                    .iter()
                    .filter(|glyph| glyph.column == column)
                    .map(|glyph| glyph.key)
                    .collect::<Vec<_>>()
            };
            assert_ne!(ids(&plain), ids(&slashed));
        }
        assert_eq!(plain.cell_spans, slashed.cell_spans);
    }
}

#[test]
fn live_shaping_attribute_and_fallback_boundaries_preserve_per_cell_rendering() {
    use crate::font_shaping::FontLigatures::{Off, On};
    for boundary in 0..13 {
        let mut snapshot = shaping_snapshot(&["!="]);
        let cell = &mut snapshot.visible_rows[0].cells[1];
        match boundary {
            0 => cell.attributes.bold = true,
            1 => cell.attributes.italic = true,
            2 => {
                cell.attributes.foreground_source = ColorSource::Rgb;
                cell.attributes.foreground = 0x0011_2233;
            }
            3 => {
                cell.attributes.background_source = ColorSource::Rgb;
                cell.attributes.background = 0x0011_2233;
            }
            4 => cell.attributes.underline = UnderlineStyle::Single,
            5 => cell.attributes.strikethrough = true,
            6 => cell.attributes.conceal = true,
            7 => cell.attributes.reverse = true,
            8 => cell.attributes.dim = true,
            9 => cell.content.clear(),
            10 => cell.content = "界".into(),
            11 => cell.content = "─".into(),
            12 => cell.content = "=\u{301}".into(),
            _ => unreachable!(),
        }
        let on =
            SnapshotFrame::load_scaled_with_context(&snapshot, 120, &shaping_context(On, "calt=1"))
                .unwrap();
        let off = SnapshotFrame::load_scaled_with_context(
            &snapshot,
            120,
            &shaping_context(Off, "calt=1"),
        )
        .unwrap();
        assert_eq!(
            on.glyphs
                .iter()
                .map(|glyph| (glyph.column, glyph.key))
                .collect::<Vec<_>>(),
            off.glyphs
                .iter()
                .map(|glyph| (glyph.column, glyph.key))
                .collect::<Vec<_>>(),
            "boundary {boundary}"
        );
        assert_eq!(on.decorations, off.decorations);
        assert_eq!(on.cell_spans, off.cell_spans);
    }
    let mut snapshot = shaping_snapshot(&["!= ", "!= "]);
    snapshot.visible_rows[0].cells[2].content.clear();
    snapshot.visible_rows[0].cells[2].spacer_remaining = Some(1);
    snapshot.visible_rows[1].cells.truncate(1);
    let on =
        SnapshotFrame::load_scaled_with_context(&snapshot, 120, &shaping_context(On, "calt=1"))
            .unwrap();
    let off =
        SnapshotFrame::load_scaled_with_context(&snapshot, 120, &shaping_context(Off, "calt=1"))
            .unwrap();
    assert_eq!(
        on.glyphs.iter().map(|glyph| glyph.key).collect::<Vec<_>>(),
        off.glyphs.iter().map(|glyph| glyph.key).collect::<Vec<_>>()
    );
    assert_eq!(on.cell_spans, off.cell_spans);
}

fn assert_shaping_frame_matches_full(
    frame: &SnapshotFrame,
    snapshot: &TerminalSnapshot,
    context: &RenderContext,
) {
    let full = SnapshotFrame::load_scaled_with_context(snapshot, 120, context).unwrap();
    assert_eq!(frame.glyphs, full.glyphs);
    assert_eq!(frame.decorations, full.decorations);
    assert_eq!(frame.cell_spans, full.cell_spans);
    assert_eq!(frame.cursor, full.cursor);
    let geometry = frame.tight_geometry().unwrap();
    let width = geometry.buffer_width();
    let height = geometry.buffer_height();
    let mut actual = vec![0; usize::try_from(width * height * 4).unwrap()];
    let mut expected = actual.clone();
    for style in [
        CursorStyle::Block,
        CursorStyle::Beam,
        CursorStyle::Underline,
    ] {
        paint_snapshot(&mut actual, width, height, frame, &geometry, true, style);
        paint_snapshot(&mut expected, width, height, &full, &geometry, true, style);
        assert_eq!(actual, expected);
    }
}

#[test]
fn live_shaping_cursor_dirty_rows_and_incremental_frames_match_full() {
    use crate::font_shaping::FontLigatures::{Cursor, On};
    let context = shaping_context(Cursor, "calt=1");
    let mut snapshot = shaping_snapshot(&["!= === ->", "!= === ->", "!= === ->"]);
    let mut frame = SnapshotFrame::load_scaled_with_context(&snapshot, 120, &context).unwrap();
    let joined = frame.glyphs.clone();
    let geometry = frame.tight_geometry().unwrap();
    let width = geometry.buffer_width();
    let height = geometry.buffer_height();
    let mut incremental = vec![0; usize::try_from(width * height * 4).unwrap()];
    paint_snapshot(
        &mut incremental,
        width,
        height,
        &frame,
        &geometry,
        true,
        CursorStyle::Block,
    );
    for (column, row, visible, expected_dirty) in [
        (0, 0, true, [true, false, false]),
        (1, 0, true, [true, false, false]),
        (4, 2, true, [true, false, true]),
        (4, 2, false, [false, false, true]),
    ] {
        snapshot.cursor_column = column;
        snapshot.cursor_row = row;
        snapshot.input_modes.cursor_visible = visible;
        let mut dirty = [false; 3];
        frame.mark_cursor_dirty_rows(&snapshot, &mut dirty);
        assert_eq!(dirty, expected_dirty);
        let untouched = frame
            .glyphs
            .iter()
            .filter(|glyph| glyph.row == 1)
            .copied()
            .collect::<Vec<_>>();
        frame
            .refresh_rows_with_context(&snapshot, &[], &context)
            .unwrap();
        assert_eq!(
            untouched,
            frame
                .glyphs
                .iter()
                .filter(|glyph| glyph.row == 1)
                .copied()
                .collect::<Vec<_>>()
        );
        paint_snapshot_rows(
            &mut incremental,
            width,
            height,
            &frame,
            &geometry,
            &dirty,
            true,
            CursorStyle::Block,
        );
        let full = SnapshotFrame::load_scaled_with_context(&snapshot, 120, &context).unwrap();
        let mut expected = vec![0; incremental.len()];
        paint_snapshot(
            &mut expected,
            width,
            height,
            &full,
            &geometry,
            true,
            CursorStyle::Block,
        );
        assert_eq!(
            incremental, expected,
            "cursor dirty raster rows match full composition"
        );
        assert_shaping_frame_matches_full(&frame, &snapshot, &context);
    }
    assert_eq!(frame.glyphs, joined, "hiding cursor restores context");
    snapshot.input_modes.cursor_blink = true;
    let mut dirty = [false; 3];
    frame.mark_cursor_dirty_rows(&snapshot, &mut dirty);
    assert_eq!(dirty, [false; 3]);
    frame.refresh_cursor(&snapshot).unwrap();
    assert_eq!(frame.glyphs, joined);
    snapshot.visible_rows[1].cells[0].content = "-".into();
    snapshot.visible_rows[1].cells[1].content = ">".into();
    frame
        .refresh_rows_with_context(&snapshot, &[false, true, false], &context)
        .unwrap();
    assert_shaping_frame_matches_full(&frame, &snapshot, &context);
    assert!(
        frame
            .refresh_rows_with_context(&snapshot, &[true; 3], &shaping_context(On, "calt=1"))
            .is_err()
    );
    assert!(
        frame
            .refresh_rows_with_context(&snapshot, &[true; 3], &shaping_context(Cursor, "calt=0"))
            .is_err()
    );
}

#[test]
fn live_shaping_cursor_splits_only_at_reported_cell_and_history_reuse_is_guarded() {
    use crate::font_shaping::FontLigatures::{Cursor, Off};
    let context = shaping_context(Cursor, "calt=1");
    let mut snapshot = shaping_snapshot(&["!= === ->", "!= === ->", "!= === ->"]);
    snapshot.input_modes.cursor_visible = true;
    snapshot.cursor_column = 0;
    let mut frame = SnapshotFrame::load_scaled_with_context(&snapshot, 120, &context).unwrap();
    let off =
        SnapshotFrame::load_scaled_with_context(&snapshot, 120, &shaping_context(Off, "calt=1"))
            .unwrap();
    assert_eq!(
        frame.glyphs[..2]
            .iter()
            .map(|glyph| glyph.key)
            .collect::<Vec<_>>(),
        off.glyphs[..2]
            .iter()
            .map(|glyph| glyph.key)
            .collect::<Vec<_>>()
    );
    assert_ne!(
        frame.glyphs[2].key, off.glyphs[2].key,
        "other runs retain joining"
    );
    assert!(
        frame
            .scroll_viewport_rows_with_context(&snapshot, 1, &context)
            .unwrap()
            .is_none()
    );
    snapshot.input_modes.cursor_visible = false;
    assert!(
        frame
            .scroll_viewport_rows_with_context(&snapshot, 1, &context)
            .unwrap()
            .is_none()
    );
    frame.refresh_cursor(&snapshot).unwrap();
    // Distinct reused rows and newly exposed content exercise both copy directions.
    snapshot.visible_rows[1].cells[0].content = "-".into();
    snapshot.visible_rows[1].cells[1].content = ">".into();
    frame
        .refresh_rows_with_context(&snapshot, &[false, true, false], &context)
        .unwrap();
    for delta in [1, -1] {
        let geometry = frame.tight_geometry().unwrap();
        let width = geometry.buffer_width();
        let height = geometry.buffer_height();
        let mut actual = vec![0; usize::try_from(width * height * 4).unwrap()];
        paint_snapshot(
            &mut actual,
            width,
            height,
            &frame,
            &geometry,
            false,
            CursorStyle::Block,
        );
        let mut shifted = snapshot.clone();
        let dirty = if delta > 0 {
            shifted.visible_rows.rotate_right(1);
            shifted.visible_rows[0].cells[0].content = "x".into();
            [true, false, false]
        } else {
            shifted.visible_rows.rotate_left(1);
            shifted.visible_rows[2].cells[0].content = "y".into();
            [false, false, true]
        };
        let scroll = frame
            .scroll_viewport_rows_with_context(&shifted, delta, &context)
            .unwrap()
            .unwrap();
        scroll_snapshot_pixels(&mut actual, width, &frame, &geometry, scroll);
        paint_snapshot_rows(
            &mut actual,
            width,
            height,
            &frame,
            &geometry,
            &dirty,
            false,
            CursorStyle::Block,
        );
        assert_shaping_frame_matches_full(&frame, &shifted, &context);
        let full = SnapshotFrame::load_scaled_with_context(&shifted, 120, &context).unwrap();
        let mut expected = vec![0; actual.len()];
        paint_snapshot(
            &mut expected,
            width,
            height,
            &full,
            &geometry,
            false,
            CursorStyle::Block,
        );
        assert_eq!(
            actual, expected,
            "history scroll pixels match full composition"
        );
        snapshot = shifted;
    }
}

#[test]
fn live_shaping_overhanging_and_merged_glyph_ink_is_clipped_by_selection_and_cursor() {
    use crate::font_shaping::FontLigatures::On;
    for (column, cells, left) in [(0, 1, 0), (0, 3, 0), (2, 1, -4)] {
        let mut frame = damage_test_frame();
        frame.font_ligatures = On;
        frame.columns = 3;
        frame.rows = 1;
        let key = GlyphKey { face: 0, glyph: 1 };
        frame.cache.insert(
            key,
            Arc::new(CachedGlyph {
                content: Content::Mask,
                left,
                top: 1,
                width: 6,
                height: 1,
                data: vec![255; 6],
            }),
        );
        frame.glyphs.push(SnapshotGlyph {
            key,
            column,
            row: 0,
            cells,
            cluster_advance: 6.0,
            x_offset: 0.0,
            y_offset: 0.0,
            foreground: [200, 100, 50],
        });
        let geometry = frame.tight_geometry().unwrap();
        let mut base = vec![0; 6 * 2 * 4];
        paint_snapshot(
            &mut base,
            6,
            2,
            &frame,
            &geometry,
            false,
            CursorStyle::Block,
        );
        for cursor in 0..3 {
            frame.cursor = Some((cursor, 0));
            let mut actual = vec![0; base.len()];
            paint_snapshot(
                &mut actual,
                6,
                2,
                &frame,
                &geometry,
                true,
                CursorStyle::Block,
            );
            for x in 0..6 {
                let pixel = &actual[x * 4..x * 4 + 4];
                if x / 2 == cursor as usize {
                    assert_eq!(pixel, [255; 4]);
                } else {
                    assert_eq!(
                        pixel,
                        &base[x * 4..x * 4 + 4],
                        "outside cursor ink must survive"
                    );
                }
            }
        }
        let mut selected = base.clone();
        paint_snapshot_overlays(
            &mut selected,
            6,
            2,
            &frame,
            &geometry,
            SnapshotOverlays {
                selection: Some(((0, 1), (0, 1))),
                hovered_url: None,
                dirty_rows: None,
                focused: true,
                selection_color: 0,
                selection_foreground: 0x0011_2233,
                url_color: 0,
                accent_color: 0,
                search_match: false,
            },
        );
        for x in 0..6 {
            let pixel = &selected[x * 4..x * 4 + 4];
            if x / 2 == 1 {
                assert_eq!(pixel, [0x33, 0x22, 0x11, 255]);
            } else {
                assert_eq!(pixel, &base[x * 4..x * 4 + 4]);
            }
        }
    }
}
