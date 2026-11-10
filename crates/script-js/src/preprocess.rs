//! Source preprocessing: `#`-directives at the start of a line.
//!
//! `#target`, `#targetengine`, `#strict` and `#script` lines are blanked (kept as empty lines so
//! error line numbers still match the file). `#include` is reported as unsupported rather than
//! silently dropped. The whole file is also checked for a leading byte-order mark.

/// The source to evaluate.
///
/// # Errors
/// A message with the 1-based line of an unsupported directive.
pub fn preprocess(source: &str) -> Result<String, String> {
    let source = source.strip_prefix('\u{feff}').unwrap_or(source);
    let mut out = String::with_capacity(source.len());
    for (n, line) in source.split_inclusive('\n').enumerate() {
        let trimmed = line.trim_start();
        let directive = trimmed.strip_prefix('#').map(|d| d.split(|c: char| c.is_whitespace() || c == ';').next().unwrap_or(""));
        match directive {
            Some("target" | "targetengine" | "strict" | "script") => {
                out.push_str(if line.ends_with("\r\n") {
                    "\r\n"
                } else if line.ends_with('\n') {
                    "\n"
                } else {
                    ""
                });
            }
            Some(d @ ("include" | "includepath")) => return Err(format!("line {}: #{d} is not supported yet", n + 1)),
            _ => out.push_str(line),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::preprocess;

    #[test]
    fn blanks_known_directives_and_keeps_line_numbers() {
        let out = preprocess("\u{feff}#target photocraft\r\n#targetengine main\nvar a = 1;\n").unwrap();
        assert_eq!(out, "\r\n\nvar a = 1;\n");
    }

    #[test]
    fn include_is_reported_with_its_line() {
        assert_eq!(preprocess("var a;\n#include \"x.jsx\"\n").unwrap_err(), "line 2: #include is not supported yet");
    }

    #[test]
    fn private_fields_are_not_directives() {
        assert_eq!(preprocess("  #x = 1;\n").unwrap(), "  #x = 1;\n");
    }
}
