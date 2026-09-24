use std::process::Command;

/// A `Command` for helper tools (`route`, `reg`, `powershell`, ...).
///
/// The release app is a GUI-subsystem binary on Windows, so every console
/// program it spawns gets a fresh console window of its own — without
/// `CREATE_NO_WINDOW` a PowerShell window would flash up on every call.
pub fn command(program: &str) -> Command {
    #[allow(unused_mut)]
    let mut cmd = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// Quotes `s` as a single POSIX shell word.
#[cfg_attr(windows, allow(dead_code))]
pub fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Quotes `s` as a PowerShell single-quoted string literal. PowerShell
/// also treats the typographic quotes ‘ ’ ‚ ‛ as single quotes, so a user
/// folder like `O’Brien` needs them doubled just the same.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn ps_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        if matches!(c, '\'' | '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}') {
            out.push(c);
        }
        out.push(c);
    }
    out.push('\'');
    out
}

/// `-EncodedCommand` payload: base64 of the script's UTF-16LE bytes. Passing
/// a script this way sidesteps every layer of command-line quoting.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn ps_encode(script: &str) -> String {
    use base64::Engine as _;
    let bytes: Vec<u8> = script
        .encode_utf16()
        .flat_map(|u| u.to_le_bytes())
        .collect();
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// Substitutes `@@NAME@@` placeholders — script templates are full of
/// shell/PowerShell braces, which `format!` would force us to double.
pub fn render(template: &str, vars: &[(&str, String)]) -> String {
    let mut out = template.to_string();
    for (name, value) in vars {
        out = out.replace(&format!("@@{name}@@"), value);
    }
    debug_assert!(
        !out.contains("@@"),
        "unfilled placeholder in script template"
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sh_quote_survives_single_quotes() {
        assert_eq!(sh_quote("it's"), r"'it'\''s'");
    }

    #[test]
    fn ps_quote_doubles_single_quotes() {
        assert_eq!(ps_quote(r"C:\Users\O'Brien"), r"'C:\Users\O''Brien'");
        assert_eq!(
            ps_quote("C:\\Users\\O\u{2019}Brien"),
            "'C:\\Users\\O\u{2019}\u{2019}Brien'"
        );
    }

    #[test]
    fn ps_encode_is_utf16le_base64() {
        // "hi" -> 68 00 69 00
        assert_eq!(ps_encode("hi"), "aABpAA==");
    }
}
