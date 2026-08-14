//! Web-app icons, synthesised per server.
//!
//! A dock app captures its icon at install time, so the icon has to say which
//! server it belongs to: the glyph gives the role (`>τ` for the language
//! server, `τ` for a plain document server, `τ` with an underline for an
//! annotating one) and the colour comes from the port, which is itself derived
//! from the canonical path being served. Two projects therefore get two
//! recognisably different icons without anyone choosing colours.
//!
//! The glyphs are baked alpha masks (512×512, one byte per pixel, zlib
//! deflated) rather than a font: rendering a τ at runtime would mean shipping a
//! rasteriser for one character. `src/static/icons/gen_icons.html` regenerates them.

use std::io::Read;

/// The role a server plays, which the glyph announces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum IconRole {
    /// The editor-driven preview: `>τ`.
    #[value(name = "preview", alias = "lsp")]
    Lsp,
    /// A plain document server: `τ`.
    #[value(name = "serve")]
    Serve,
    /// A document server with annotation enabled: `τ` underlined.
    #[value(name = "annotate")]
    Annotate,
}

impl IconRole {
    /// The baked alpha mask for this role.
    fn mask(self) -> &'static [u8] {
        match self {
            IconRole::Lsp => include_bytes!("../../static/icons/glyph-lsp.z"),
            IconRole::Serve => include_bytes!("../../static/icons/glyph-serve.z"),
            IconRole::Annotate => include_bytes!("../../static/icons/glyph-anno.z"),
        }
    }

    /// The name of the role as it appears in a web app's title.
    pub fn title(self) -> &'static str {
        match self {
            IconRole::Lsp => "Typst LSP",
            IconRole::Serve => "Typst Server",
            IconRole::Annotate => "Typst Annotator",
        }
    }
}

/// The side of the baked masks.
const MASK_SIZE: usize = 512;

/// Blends two colours, `t` of the way from `a` to `b`.
fn mix(a: [u8; 3], b: [u8; 3], t: f32) -> [u8; 3] {
    let f = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round().clamp(0.0, 255.0) as u8;
    [f(a[0], b[0]), f(a[1], b[1]), f(a[2], b[2])]
}

/// The hue for a port, in degrees.
///
/// Continuous rather than a palette: a fixed list collides (sixteen servers
/// drawing from fourteen colours repeat about half the time) and its entries
/// disagree about lightness, so navy reads as black next to lime. A hue angle
/// with lightness and chroma held constant gives every server a distinct
/// colour of the same weight.
pub fn hue_for_port(port: u16) -> f32 {
    // Scrambled so that sibling directories, whose ports differ by very
    // little, do not land on neighbouring hues.
    let mut x = (port as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15);
    x ^= x >> 30;
    x = x.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x ^= x >> 27;
    x = x.wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^= x >> 31;
    (x % 3600) as f32 / 10.0
}

/// Parses `#rrggbb`, `rrggbb`, `#rgb` or `rgb`.
pub fn parse_hex(text: &str) -> Option<[u8; 3]> {
    let hex = text.trim().trim_start_matches('#');
    let digits: Vec<u8> = hex
        .chars()
        .map(|c| c.to_digit(16).map(|d| d as u8))
        .collect::<Option<_>>()?;
    match digits.len() {
        3 => Some([
            digits[0] * 17,
            digits[1] * 17,
            digits[2] * 17,
        ]),
        6 => Some([
            digits[0] * 16 + digits[1],
            digits[2] * 16 + digits[3],
            digits[4] * 16 + digits[5],
        ]),
        _ => None,
    }
}

/// The tile and glyph colours for a server: the port's hue by default, or a
/// colour the operator chose.
///
/// A chosen colour is honoured as given — someone naming a hex wants that tile,
/// not an approximation of it — and only the glyph is derived, flipping dark
/// when the tile is light so the icon stays legible either way.
pub fn colors_for(port: u16, chosen: Option<[u8; 3]>) -> ([u8; 3], [u8; 3]) {
    let Some(bg) = chosen else {
        return colors_for_port(port);
    };
    let (l, c, h) = srgb_to_oklch(bg);
    let glyph = if l > 0.62 {
        oklch_to_srgb(0.22, c.min(0.09), h)
    } else {
        oklch_to_srgb(0.90, c.min(0.075), h)
    };
    (bg, glyph)
}

fn srgb_to_oklch(rgb: [u8; 3]) -> (f32, f32, f32) {
    let lin = |v: u8| {
        let s = v as f32 / 255.0;
        if s <= 0.04045 {
            s / 12.92
        } else {
            ((s + 0.055) / 1.055).powf(2.4)
        }
    };
    let (r, g, b) = (lin(rgb[0]), lin(rgb[1]), lin(rgb[2]));
    let l = (0.412_221_46 * r + 0.536_332_55 * g + 0.051_445_995 * b).cbrt();
    let m = (0.211_903_5 * r + 0.680_699_5 * g + 0.107_396_96 * b).cbrt();
    let s = (0.088_302_46 * r + 0.281_718_84 * g + 0.629_978_5 * b).cbrt();
    let lightness = 0.210_454_26 * l + 0.793_617_8 * m - 0.004_072_047 * s;
    let a = 1.977_998_5 * l - 2.428_592_2 * m + 0.450_593_7 * s;
    let bb = 0.025_904_037 * l + 0.782_771_77 * m - 0.808_675_77 * s;
    (lightness, (a * a + bb * bb).sqrt(), bb.atan2(a).to_degrees().rem_euclid(360.0))
}

/// The tile and glyph colours for a port.
///
/// Both are the same hue at fixed Oklch lightness and chroma — a perceptual
/// space, so "equally dark" means equally dark to the eye rather than equal in
/// sRGB numbers.
pub fn colors_for_port(port: u16) -> ([u8; 3], [u8; 3]) {
    let hue = hue_for_port(port);
    (oklch_to_srgb(0.34, 0.085, hue), oklch_to_srgb(0.90, 0.075, hue))
}

/// Converts Oklch to sRGB, pulling chroma in until the colour fits in the
/// gamut: at these lightnesses a few hues (saturated blues especially) would
/// otherwise clip a channel and shift hue as a result.
fn oklch_to_srgb(l: f32, chroma: f32, hue_deg: f32) -> [u8; 3] {
    let mut chroma = chroma;
    loop {
        let (r, g, b) = oklab_to_linear(l, chroma, hue_deg);
        let inside = [r, g, b].iter().all(|c| (-0.001..=1.001).contains(c));
        if inside || chroma <= 0.002 {
            return [encode_srgb(r), encode_srgb(g), encode_srgb(b)];
        }
        chroma -= 0.002;
    }
}

fn oklab_to_linear(l: f32, chroma: f32, hue_deg: f32) -> (f32, f32, f32) {
    let h = hue_deg.to_radians();
    let (a, b) = (chroma * h.cos(), chroma * h.sin());
    let l_ = l + 0.396_337_78 * a + 0.215_803_76 * b;
    let m_ = l - 0.105_561_346 * a - 0.063_854_17 * b;
    let s_ = l - 0.089_484_18 * a - 1.291_485_5 * b;
    let (l3, m3, s3) = (l_ * l_ * l_, m_ * m_ * m_, s_ * s_ * s_);
    (
        4.076_741_7 * l3 - 3.307_711_6 * m3 + 0.230_969_94 * s3,
        -1.268_438 * l3 + 2.609_757_4 * m3 - 0.341_319_38 * s3,
        -0.004_196_086 * l3 - 0.703_418_6 * m3 + 1.707_614_7 * s3,
    )
}

fn encode_srgb(linear: f32) -> u8 {
    let c = linear.clamp(0.0, 1.0);
    let srgb = if c <= 0.003_130_8 {
        12.92 * c
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    };
    (srgb * 255.0).round().clamp(0.0, 255.0) as u8
}

/// Renders the icon for a role and port at the given size, as PNG.
pub fn icon_png(
    role: IconRole,
    port: u16,
    size: usize,
    color: Option<[u8; 3]>,
) -> Result<Vec<u8>, String> {
    let mut mask = Vec::with_capacity(MASK_SIZE * MASK_SIZE);
    flate2::read::ZlibDecoder::new(role.mask())
        .read_to_end(&mut mask)
        .map_err(|err| format!("cannot inflate the {role:?} glyph: {err}"))?;
    if mask.len() != MASK_SIZE * MASK_SIZE {
        return Err(format!("the {role:?} glyph is {} bytes", mask.len()));
    }

    let (bg, fg) = colors_for(port, color);
    // The corner radius macOS itself uses for app icons; Safari masks maskable
    // icons to its own squircle, and a rounded tile survives that unharmed
    // while still looking right anywhere the icon is used unmasked.
    let radius = size as f32 * 0.225;
    let scale = MASK_SIZE as f32 / size as f32;

    let mut rgba = Vec::with_capacity(size * size * 4);
    for y in 0..size {
        for x in 0..size {
            // Area-average the mask over the pixel's footprint.
            let (x0, x1) = ((x as f32 * scale) as usize, ((x + 1) as f32 * scale) as usize);
            let (y0, y1) = ((y as f32 * scale) as usize, ((y + 1) as f32 * scale) as usize);
            let mut total = 0u32;
            let mut count = 0u32;
            for my in y0..y1.max(y0 + 1).min(MASK_SIZE) {
                for mx in x0..x1.max(x0 + 1).min(MASK_SIZE) {
                    total += mask[my * MASK_SIZE + mx] as u32;
                    count += 1;
                }
            }
            let ink = if count == 0 { 0.0 } else { total as f32 / count as f32 / 255.0 };

            let color = mix(bg, fg, ink);
            let alpha = corner_alpha(x, y, size, radius);
            rgba.extend_from_slice(&[color[0], color[1], color[2], alpha]);
        }
    }
    Ok(encode_png(size, &rgba))
}

/// How much of a pixel falls inside the rounded tile, as an alpha byte.
fn corner_alpha(x: usize, y: usize, size: usize, radius: f32) -> u8 {
    let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
    let cx = px.clamp(radius, size as f32 - radius);
    let cy = py.clamp(radius, size as f32 - radius);
    let (dx, dy) = (px - cx, py - cy);
    let dist = (dx * dx + dy * dy).sqrt();
    // One pixel of feathering at the curve, so the corners are not stepped.
    (((radius - dist) + 0.5).clamp(0.0, 1.0) * 255.0).round() as u8
}

/// Writes 8-bit RGBA pixels as a PNG.
fn encode_png(size: usize, rgba: &[u8]) -> Vec<u8> {
    use std::io::Write;

    let mut raw = Vec::with_capacity(rgba.len() + size);
    for y in 0..size {
        raw.push(0); // filter: none
        raw.extend_from_slice(&rgba[y * size * 4..(y + 1) * size * 4]);
    }
    let mut deflated = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
    let _ = deflated.write_all(&raw);
    let idat = deflated.finish().unwrap_or_default();

    let chunk = |tag: &[u8; 4], data: &[u8]| {
        let mut out = Vec::with_capacity(data.len() + 12);
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        out.extend_from_slice(tag);
        out.extend_from_slice(data);
        let mut crc = flate2::Crc::new();
        crc.update(tag);
        crc.update(data);
        out.extend_from_slice(&crc.sum().to_be_bytes());
        out
    };

    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&(size as u32).to_be_bytes());
    ihdr.extend_from_slice(&(size as u32).to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]); // 8-bit, RGBA, no interlace

    let mut png = Vec::with_capacity(idat.len() + 64);
    png.extend_from_slice(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]);
    png.extend_from_slice(&chunk(b"IHDR", &ihdr));
    png.extend_from_slice(&chunk(b"IDAT", &idat));
    png.extend_from_slice(&chunk(b"IEND", &[]));
    png
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_valid_pngs() {
        for role in [IconRole::Lsp, IconRole::Serve, IconRole::Annotate] {
            for size in [192, 512] {
                let png = icon_png(role, 24123, size, None).expect("icon renders");
                assert_eq!(&png[..8], &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]);
                let width = u32::from_be_bytes(png[16..20].try_into().unwrap());
                assert_eq!(width as usize, size);
            }
        }
    }

    #[test]
    fn hues_spread_and_stay_equally_dark() {
        // Neighbouring ports must not look alike: sibling directories land next
        // to each other. With a continuous hue "distinct" means far apart on
        // the circle, not merely unequal.
        let close = (23700u16..24700)
            .filter(|p| {
                let (a, b) = (hue_for_port(*p), hue_for_port(p + 1));
                let d = (a - b).abs();
                d.min(360.0 - d) < 12.0
            })
            .count();
        // Independent hues fall within 12° of each other 24/360 of the time, so
        // ~67 of 1000 pairs is the target, not zero. A scramble that leaked the
        // port's low bits would push this far higher.
        assert!(close < 110, "{close} of 1000 adjacent ports landed within 12°");

        // Every tile carries the same visual weight, whatever hue it drew: the
        // failure of a named palette was navy reading as black beside lime.
        let lum = |c: [u8; 3]| {
            let f = |v: u8| {
                let s = v as f32 / 255.0;
                if s <= 0.04045 { s / 12.92 } else { ((s + 0.055) / 1.055).powf(2.4) }
            };
            0.2126 * f(c[0]) + 0.7152 * f(c[1]) + 0.0722 * f(c[2])
        };
        let bgs: Vec<_> = (0..360).map(|d| lum(oklch_to_srgb(0.34, 0.085, d as f32))).collect();
        let (lo, hi) = bgs.iter().fold((f32::MAX, 0f32), |(lo, hi), v| (lo.min(*v), hi.max(*v)));
        assert!(hi / lo < 2.0, "tile luminance varies {lo:.4}..{hi:.4} across the hue circle");
    }

    #[test]
    fn chosen_colours_are_honoured_and_stay_legible() {
        assert_eq!(parse_hex("#4b2e83"), Some([0x4b, 0x2e, 0x83]));
        assert_eq!(parse_hex("4b2e83"), Some([0x4b, 0x2e, 0x83]));
        assert_eq!(parse_hex("#abc"), Some([0xaa, 0xbb, 0xcc]));
        assert_eq!(parse_hex("nope"), None);
        assert_eq!(parse_hex("#12345"), None);

        // The tile is exactly what was asked for.
        let (bg, glyph) = colors_for(24123, Some([0x4b, 0x2e, 0x83]));
        assert_eq!(bg, [0x4b, 0x2e, 0x83]);

        // And the glyph goes the other way from the tile, whichever way that is.
        let lum = |c: [u8; 3]| {
            let f = |v: u8| {
                let s = v as f32 / 255.0;
                if s <= 0.04045 { s / 12.92 } else { ((s + 0.055) / 1.055).powf(2.4) }
            };
            0.2126 * f(c[0]) + 0.7152 * f(c[1]) + 0.0722 * f(c[2])
        };
        assert!(lum(glyph) > lum(bg), "dark tile should take a light glyph");
        let (pale, ink) = colors_for(24123, Some([0xff, 0xe8, 0xa0]));
        assert!(lum(ink) < lum(pale), "light tile should take a dark glyph");
    }

    /// Writes a gallery of samples to the temp directory, for eyeballing.
    #[test]
    fn dump_gallery() {
        let dir = std::env::temp_dir().join("talimist-icons");
        let _ = std::fs::create_dir_all(&dir);
        for port in [23707, 23761, 23824, 23890, 23955, 24012, 24088, 24123, 24190, 24244, 24301, 24377, 24418, 24476, 24533, 24612] {
            for role in [IconRole::Lsp, IconRole::Serve, IconRole::Annotate] {
                let png = icon_png(role, port, 256, None).expect("icon renders");
                let name = format!("{port}-{}.png", format!("{role:?}").to_lowercase());
                let _ = std::fs::write(dir.join(name), png);
            }
        }
    }
}
