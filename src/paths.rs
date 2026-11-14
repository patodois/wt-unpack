//! File names for output, made from names that come from game data.
//!
//! A sample, a texture or a model is named by the file it was read from, and that name ends up in the name of
//! a file this program writes. Every such name goes through this module, so that it names one file in the
//! output folder and nothing else: not another folder (`../x`, `a\b`), not a drive or an alternate data stream
//! (`c:boom`, `a:b`) and, on Windows, not a device (`NUL`).
//!
//! Ordinary names (letters, digits, spaces, dots, dashes, underscores, in any script) come out exactly as
//! they went in.

/// Whether `c` cannot stay in a file name: a path separator of either kind, the colon of drives and
/// alternate data streams, or a control character. The same on every system, so an export has the same file
/// names wherever it is made.
fn replaced(c: char) -> bool {
    matches!(c, '/' | '\\' | ':') || c.is_control()
}

/// Whether Windows opens a device instead of a file for this name. The device names count with any
/// extension and with spaces before it (`NUL`, `nul.txt`, `COM1 .wav`), in any case.
fn is_device(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or("").trim_end_matches(' ');
    let upper = stem.to_ascii_uppercase();
    if matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$") {
        return true;
    }
    let mut chars = upper.chars();
    let port: String = chars.by_ref().take(3).collect();
    matches!(port.as_str(), "COM" | "LPT") && matches!((chars.next(), chars.next()), (Some('0'..='9' | '\u{b9}' | '\u{b2}' | '\u{b3}'), None))
}

/// [`stem`] for a program that runs on Windows (`windows`) or elsewhere, so both can be tested anywhere
fn stem_for(name: &str, windows: bool) -> String {
    let mut out: String = name.chars().map(|c| if replaced(c) { '_' } else { c }).collect();
    // Elsewhere `aux` or `con.x` is an ordinary name and is kept as it is
    if windows && is_device(&out) {
        out.insert(0, '_');
    }
    out
}

/// The part of a file name before its extension, from a name read from game data: path separators, colons
/// and control characters become `_`, and on Windows a device name such as `NUL` gets a `_` in front (it
/// would be written to the device, not to a file). Every other name comes back unchanged.
///
/// It is not a file name yet: it can be empty or all dots. Add an extension ([`file_name`] does), and it
/// names a file in whatever folder it is joined to. Different names can give the same stem (`a/b` and
/// `a_b`); a caller that writes several files has to keep them apart.
pub fn stem(name: &str) -> String {
    stem_for(name, cfg!(windows))
}

/// `<stem>.<extension>` for a name read from game data (see [`stem`]). With its extension the result is
/// never `.` or `..` and has no separator or colon, so `folder.join(..)` is a file in that folder.
pub fn file_name(name: &str, extension: &str) -> String {
    format!("{}.{extension}", stem(name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Component, Path};

    /// Names made only of what an ordinary name has: letters of any script, digits, spaces, dots, dashes and
    /// underscores. Some are also Windows device names, end in a dot or a space, or are nothing but dots.
    const ORDINARY: &[&str] = &[
        "dshk_close_01",
        "t_34_85_c__nocamo__noao",
        "m4a1_76w_sherman",
        "Ünïcode ok",
        "танк",
        "戦車 1",
        ".hidden",
        "a.b.c",
        "two  spaces",
        " leading",
        "trailing ",
        "trailing.",
        "-dash-",
        "_",
        "console",
        "com10",
        "lpt",
        "nulls",
        "COM",
        "config.blk",
        "..",
        ".",
        "...",
        ". .",
        " ",
        // device names on Windows, ordinary names elsewhere
        "con",
        "CON",
        "aux",
        "nul.txt",
        "NUL.tar.gz",
        "prn",
        "COM1",
        "com9.ogg",
        "LPT1",
        "lpt0",
        "Aux .wav",
    ];

    const HOSTILE: &[(&str, &str)] = &[
        ("c:boom", "c_boom"),
        ("C:\\Windows\\System32\\x", "C__Windows_System32_x"),
        ("c:/boom", "c__boom"),
        ("../../x", ".._.._x"),
        ("..\\..\\x", ".._.._x"),
        ("/etc/passwd", "_etc_passwd"),
        ("\\\\server\\share\\x", "__server_share_x"),
        ("\\\\?\\C:\\x", "__?_C__x"),
        ("name:stream", "name_stream"),
        ("a/b", "a_b"),
        ("a\\b", "a_b"),
        ("line\nbreak", "line_break"),
        ("nul\0byte", "nul_byte"),
        ("tab\there", "tab_here"),
        ("", ""),
    ];

    #[test]
    fn ordinary_names_are_written_exactly_as_before() {
        for name in ORDINARY {
            // Off Windows nothing changes: the file is `<name>.<extension>`, as it was before this module
            assert_eq!(stem_for(name, false), *name);
            // On Windows only the device names do, which were never written to a file there
            let on_windows = stem_for(name, true);
            if is_device(name) {
                assert_eq!(on_windows, format!("_{name}"));
            } else {
                assert_eq!(on_windows, *name);
            }
        }
        assert_eq!(file_name("dshk_close_01", "wav"), "dshk_close_01.wav");
        assert_eq!(file_name("a.b.", "png"), "a.b..png");
        assert_eq!(ORDINARY.iter().filter(|n| is_device(n)).count(), 11);
    }

    #[test]
    fn hostile_names_become_one_file_in_the_folder() {
        for windows in [false, true] {
            for (name, expected) in HOSTILE {
                assert_eq!(stem_for(name, windows), *expected, "{name:?}");
            }
            for name in HOSTILE.iter().map(|(name, _)| *name).chain(ORDINARY.iter().copied()) {
                let file = format!("{}.png", stem_for(name, windows));
                assert!(!file.contains(['/', '\\', ':']) && !file.chars().any(char::is_control), "{name:?} became {file:?}");
                // With its extension it is neither a directory reference nor, on Windows, a device or a name
                // Windows would shorten (one that ends in a dot or a space)
                assert!(file != "." && file != ".." && !file.ends_with(['.', ' ']), "{name:?} became {file:?}");
                assert!(!windows || !is_device(&file), "{name:?} became {file:?}");
                // and on the system running this test it is exactly one ordinary path component
                let joined = Path::new("out").join(&file);
                let parts: Vec<_> = joined.components().collect();
                assert!(matches!(parts[..], [Component::Normal(_), Component::Normal(_)]), "{name:?} became {file:?}: {parts:?}");
            }
        }
    }
}
