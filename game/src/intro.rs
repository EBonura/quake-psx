//! Boot intro: the Bonnie Studios mark and the "Built with PSoXide" line every
//! PSoXide game opens with (the Celeste collection cadence, as VoXide and
//! Half-Life carry it). Fade in, hold, fade out; a fresh face-button or Start
//! press past a short grace skips it.
//!
//! Immediate-mode drawing on the display frame buffer, before any Quake asset
//! reaches VRAM: the logo, its CLUT and the SDK font borrow texture columns
//! that the level graphics overwrite afterwards.

use psx_font::{fonts::BASIC_8X16, FontAtlas};
use psx_gpu::framebuf::FrameBuffer;
use psx_pad::{button, poll_port1, ButtonState};
use psx_vram::{Clut, TexDepth, Tpage};

const LOGO_TPAGE: Tpage = Tpage::new(896, 0, TexDepth::Bit4);
const LOGO_CLUT: Clut = Clut::new(768, 256);
const FONT_TPAGE: Tpage = Tpage::new(320, 0, TexDepth::Bit4);
const FONT_CLUT: Clut = Clut::new(320, 256);

const FADE_IN: i32 = 32;
const HOLD: i32 = 74;
const TOTAL: i32 = 150;
const FADE_OUT: i32 = TOTAL - FADE_IN - HOLD;
const SKIP_GRACE: i32 = 8;
const TAG: &str = "Built with PSoXide";

#[optimize(size)]
pub fn show(fb: &mut FrameBuffer) {
    psx_vram::upload_16bpp(
        psx_vram::VramRect::new(LOGO_TPAGE.x(), LOGO_TPAGE.y(), 32, 128),
        &crate::bonnie::COVER_BONNIE,
    );
    let mut clut = crate::bonnie::BONNIE_CLUT;
    // Entry 0 opaque near-black: the backdrop is black and 0x0000 is
    // transparent on the PS1.
    clut[0] = 0x0421;
    psx_vram::upload_16bpp(
        psx_vram::VramRect::new(LOGO_CLUT.x(), LOGO_CLUT.y(), 16, 1),
        &clut,
    );
    let font = FontAtlas::upload(&BASIC_8X16, FONT_TPAGE, FONT_CLUT);

    let any = |buttons: ButtonState| {
        buttons.is_held(button::CROSS)
            || buttons.is_held(button::CIRCLE)
            || buttons.is_held(button::START)
    };
    let mut previous = poll_port1().buttons;
    let mut frame = 0i32;
    while frame < TOTAL {
        let buttons = poll_port1().buttons;
        if frame > SKIP_GRACE && any(buttons) && !any(previous) {
            break;
        }
        previous = buttons;
        let level = if frame < FADE_IN {
            frame * 0x80 / FADE_IN
        } else if frame < FADE_IN + HOLD {
            0x80
        } else {
            (TOTAL - frame) * 0x80 / FADE_OUT
        }
        .clamp(0, 0x80) as u8;

        fb.clear(0, 0, 0);
        // The 128x128 source drawn as a 96x96 mark, centred above the line.
        psx_gpu::draw_quad_textured(
            [(112, 34), (208, 34), (112, 130), (208, 130)],
            [(0, 0), (128, 0), (0, 128), (128, 128)],
            LOGO_CLUT.uv_clut_word(),
            LOGO_TPAGE.uv_tpage_word(0),
            (level, level, level),
        );
        // Gradient text with the sweeping sheen.
        let mut x = 160 - (font.text_width(TAG) / 2) as i16;
        let span = TAG.chars().count() as i32 + 18;
        let head = (frame / 2).rem_euclid(span);
        let mix = |colour: (u8, u8, u8), amount: i32| -> (u8, u8, u8) {
            let channel = |value: u8| {
                let base = i32::from(value) * i32::from(level) / 0x80;
                (base + (i32::from(level) - base) * amount / 18) as u8
            };
            (channel(colour.0), channel(colour.1), channel(colour.2))
        };
        for (index, ch) in TAG.char_indices() {
            let glyph = &TAG[index..index + ch.len_utf8()];
            for (dx, dy) in [(-1, -1), (1, -1), (-1, 1), (1, 1)] {
                font.draw_text(x + dx, 150 + dy, glyph, (0, 0, 0));
            }
            let amount = (18 - (index as i32 - head).abs() * 6).max(0);
            font.draw_text_gradient(
                x,
                150,
                glyph,
                mix((0x68, 0x80, 0x80), amount),
                mix((0x38, 0x58, 0x80), amount),
            );
            x += font.text_width(glyph) as i16;
        }
        psx_gpu::draw_sync();
        psx_rt::interrupts::wait_vblank();
        fb.swap();
        frame += 1;
    }
    // Leave the display black rather than the last intro frame while the
    // graphics load.
    fb.clear(0, 0, 0);
    psx_gpu::draw_sync();
    psx_rt::interrupts::wait_vblank();
    fb.swap();
}
