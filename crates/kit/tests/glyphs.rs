//! Every symbol the UI writes has a glyph in the bundled fonts: a missing one
//! draws as an empty box (it happened with ✎).
use std::{collections::BTreeMap, path::Path};

/// Characters inside string literals, with where they are.
fn ui_chars(dir: &Path, out: &mut BTreeMap<char, String>) {
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        if path.is_dir() {
            if name != "target" && name != "tests" {
                ui_chars(&path, out);
            }
            continue;
        }
        if path.extension().is_none_or(|e| e != "rs") {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap();
        for (c, line) in literal_chars(&text) {
            out.entry(c)
                .or_insert_with(|| format!("{}:{line}", path.display()));
        }
    }
}

/// Non-ASCII characters inside string literals, with their line: escapes,
/// char literals and `//` comments are skipped; strings may span lines.
/// Raw strings and block comments are read like plain code: none holds a
/// symbol today.
fn literal_chars(text: &str) -> Vec<(char, usize)> {
    let (mut out, mut line) = (Vec::new(), 1);
    let (mut quoted, mut comment) = (false, false);
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\n' => {
                line += 1;
                comment = false;
            }
            _ if comment => {}
            '\\' if quoted => {
                chars.next();
            }
            '"' => quoted = !quoted,
            '/' if !quoted && chars.peek() == Some(&'/') => comment = true,
            // A char literal (`'"'`, `'\''`, `'→'`), not a lifetime.
            '\'' if !quoted => {
                let mut ahead = chars.clone();
                let inner = match ahead.next() {
                    Some('\\') => ahead.next().map(|_| None),
                    other => other.map(Some),
                };
                if let Some(inner) = inner
                    && ahead.next() == Some('\'')
                {
                    chars = ahead;
                    if let Some(c) = inner.filter(|c| !c.is_ascii()) {
                        out.push((c, line));
                    }
                }
            }
            c if quoted && !c.is_ascii() => out.push((c, line)),
            _ => {}
        }
    }
    out
}

#[test]
fn literals_are_told_from_code() {
    let found = literal_chars(
        "let a = \"\\\"…\\\"\"; // “x”\nlet q = ('\"', '\\'', '\\\"'); fn f<'a>() {} let b = \"→\n·\"; let c = '✓';",
    );
    assert_eq!(found, [('…', 1), ('→', 2), ('·', 3), ('✓', 3)]);
}

#[test]
fn ui_symbols_have_glyphs() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut chars = BTreeMap::new();
    for dir in ["crates", "plugins", "src"] {
        ui_chars(&root.join(dir), &mut chars);
    }
    let ctx = egui::Context::default();
    rproxy_kit::theme::install_fonts(&ctx);
    let mut out = ctx.run_ui(Default::default(), |_| {});
    out.textures_delta.clear();
    let missing: Vec<String> = ctx.fonts_mut(|f| {
        chars
            .iter()
            .filter(|(c, _)| {
                let fonts = [
                    egui::FontId::proportional(14.),
                    egui::FontId::monospace(14.),
                ];
                !fonts.iter().all(|font| f.has_glyph(font, **c))
            })
            .map(|(c, at)| format!("{c} (U+{:04X}) at {at}", *c as u32))
            .collect()
    });
    assert!(missing.is_empty(), "no glyph for:\n{}", missing.join("\n"));
}

/// Each emoji in a name is one color glyph: flags, ZWJ and skin-tone
/// sequences, and emoji the symbol font also has (❤, 🔒).
#[test]
fn emoji_are_color_glyphs() {
    let ctx = egui::Context::default();
    rproxy_kit::theme::install_fonts(&ctx);
    ctx.run_ui(Default::default(), |_| {})
        .textures_delta
        .clear();
    let samples = ["🇫🇮", "🇺🇸", "👩🏽‍💻", "👨‍👩‍👧", "❤️", "❤", "🔒", "⚡️", "☁️", "🤖"];
    let font = egui::FontId::proportional(14.);
    for sample in samples {
        let galley = ctx.fonts_mut(|f| {
            f.layout_no_wrap(format!("A{sample}Z"), font.clone(), egui::Color32::WHITE)
        });
        let glyphs = &galley.rows[0].glyphs;
        let color = glyphs.iter().filter(|g| g.is_color).count();
        let visible = glyphs.iter().filter(|g| g.advance_width > 0.).count();
        assert_eq!((color, visible), (1, 3), "{sample}: {glyphs:#?}");
    }
    // Digits and © stay text. A keycap takes the digit's font (Ubuntu), so
    // 1️⃣ reads as a plain 1, without a missing-glyph box.
    let plain = ctx.fonts_mut(|f| {
        f.layout_no_wrap(
            "Server 1 #2 © 1️⃣".into(),
            font.clone(),
            egui::Color32::WHITE,
        )
    });
    let glyphs = &plain.rows[0].glyphs;
    assert!(glyphs.iter().all(|g| !g.is_color));
    let visible = glyphs.iter().filter(|g| g.advance_width > 0.).count();
    assert_eq!(visible, "Server 1 #2 © 1".chars().count());
}
