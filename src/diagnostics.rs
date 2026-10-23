//! Lê as linhas de erro e aviso do Gradle (Kotlin, javac e aapt2) para que o painel de saída
//! possa abrir o arquivo na linha certa.

use std::path::PathBuf;

#[derive(Debug, PartialEq, Eq, Clone)]
pub struct Diagnostic {
    pub path: PathBuf,
    pub line: u32,
    pub col: u32,
    pub message: String,
    pub error: bool,
}

const EXTENSIONS: [&str; 4] = [".kt", ".kts", ".java", ".xml"];

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() && let Some(v) = s.get(i + 1..i + 3).and_then(|h| u8::from_str_radix(h, 16).ok()) {
            out.push(v);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn digits(s: &str) -> Option<(u32, &str)> {
    let end = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    (end > 0).then(|| s[..end].parse().ok().map(|n| (n, &s[end..]))).flatten()
}

/// Interpreta uma linha de saída do Gradle; `None` se não for um diagnóstico com arquivo e linha.
pub fn parse_line(raw: &str) -> Option<Diagnostic> {
    let line = raw.trim();
    let (kotlin_prefix, rest) = if let Some(r) = line.strip_prefix("e: ") {
        (Some(true), r)
    } else if let Some(r) = line.strip_prefix("w: ") {
        (Some(false), r)
    } else {
        (None, line)
    };
    let rest = rest.strip_prefix("file:///").map(percent_decode).unwrap_or_else(|| rest.to_owned());
    let rest = rest.as_str();

    // Procura `<arquivo com extensão conhecida>:<linha>` e, opcionalmente, `:<coluna>`.
    for (i, _) in rest.match_indices(':') {
        let path = &rest[..i];
        if !EXTENSIONS.iter().any(|e| path.to_ascii_lowercase().ends_with(e)) {
            continue;
        }
        let Some((ln, after)) = digits(&rest[i + 1..]) else { continue };
        let (col, after) = match after.strip_prefix(':').and_then(digits) {
            Some((c, a)) => (c, a),
            None => (1, after),
        };
        // aapt2 escreve `5:5-10`: descarta o intervalo de colunas.
        let after = after.strip_prefix('-').and_then(digits).map_or(after, |(_, a)| a);
        let after = after.trim_start_matches([':', ' ']);
        let lower = after.to_ascii_lowercase();
        let error = match kotlin_prefix {
            Some(e) => e,
            None => !(lower.starts_with("warning") || lower.starts_with("aapt: warning")),
        };
        if kotlin_prefix.is_none() && !(lower.contains("error") || lower.contains("warning")) {
            continue;
        }
        let mut message = after.trim();
        for prefix in ["AAPT: error:", "AAPT: warning:", "error:", "warning:"] {
            if let Some(rest) = message.strip_prefix(prefix) {
                message = rest.trim();
                break;
            }
        }
        return Some(Diagnostic { path: PathBuf::from(path), line: ln, col, message: message.to_owned(), error });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kotlin_error_with_file_uri() {
        let d = parse_line("e: file:///C:/Users/Sofia/Meu%20App/app/src/main/java/com/a/MainActivity.kt:7:5 Unresolved reference 'foo'.").unwrap();
        assert_eq!(d.path, PathBuf::from("C:/Users/Sofia/Meu App/app/src/main/java/com/a/MainActivity.kt"));
        assert_eq!((d.line, d.col, d.error), (7, 5, true));
        assert_eq!(d.message, "Unresolved reference 'foo'.");
    }

    #[test]
    fn kotlin_warning() {
        let d = parse_line("w: file:///tmp/p/A.kt:3:1 Variable 'x' is never used").unwrap();
        assert!(!d.error);
        assert_eq!(d.path, PathBuf::from("tmp/p/A.kt"));
    }

    #[test]
    fn javac_error_without_column() {
        let d = parse_line(r"C:\proj\app\src\main\java\Foo.java:12: error: cannot find symbol").unwrap();
        assert_eq!(d.path, PathBuf::from(r"C:\proj\app\src\main\java\Foo.java"));
        assert_eq!((d.line, d.col, d.error), (12, 1, true));
        assert_eq!(d.message, "cannot find symbol");
    }

    #[test]
    fn aapt_error_with_column_range() {
        let d = parse_line(r"C:\p\app\src\main\AndroidManifest.xml:5:5-10: AAPT: error: attribute android:foo not found.").unwrap();
        assert_eq!((d.line, d.col, d.error), (5, 5, true));
        assert!(d.message.contains("attribute android:foo not found"));
    }

    #[test]
    fn ordinary_lines_are_not_diagnostics() {
        for l in [
            "> Task :app:compileDebugKotlin",
            "BUILD FAILED in 4s",
            "Using Android SDK: C:\\Users\\Sofia\\sdk",
            "e: sem arquivo nem linha",
            "see https://example.com/x.kt:12 for details",
            "",
        ] {
            assert_eq!(parse_line(l), None, "{l:?}");
        }
    }
}
