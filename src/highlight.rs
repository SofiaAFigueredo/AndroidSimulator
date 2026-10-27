//! Realce de sintaxe leve, escrito à mão (sem tree-sitter) para manter o binário pequeno.
//! Cobre o que um projeto Android usa: Kotlin, Java e XML.

use eframe::egui::{Color32, FontId, Stroke, text::{LayoutJob, TextFormat}};
use std::path::Path;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lang {
    Kotlin,
    Java,
    Xml,
    Plain,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tok {
    Normal,
    Keyword,
    Type,
    String,
    Comment,
    Number,
    Annotation,
    Tag,
    Attr,
}

pub fn lang_for(path: &Path) -> Lang {
    match path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).as_deref() {
        Some("kt" | "kts" | "gradle") => Lang::Kotlin,
        Some("java") => Lang::Java,
        Some("xml") => Lang::Xml,
        _ => Lang::Plain,
    }
}

const KOTLIN: &[&str] = &[
    "package", "import", "class", "interface", "object", "fun", "val", "var", "if", "else", "when", "for", "while",
    "do", "return", "break", "continue", "is", "in", "as", "try", "catch", "finally", "throw", "override", "open",
    "abstract", "sealed", "data", "enum", "companion", "private", "public", "protected", "internal", "lateinit",
    "by", "init", "constructor", "suspend", "inline", "null", "true", "false", "this", "super", "typealias",
    "where", "out", "vararg", "const", "annotation",
];

const JAVA: &[&str] = &[
    "abstract", "assert", "boolean", "break", "byte", "case", "catch", "char", "class", "const", "continue",
    "default", "do", "double", "else", "enum", "extends", "final", "finally", "float", "for", "goto", "if",
    "implements", "import", "instanceof", "int", "interface", "long", "native", "new", "package", "private",
    "protected", "public", "return", "short", "static", "strictfp", "super", "switch", "synchronized", "this",
    "throw", "throws", "transient", "try", "void", "volatile", "while", "var", "record", "null", "true", "false",
];

/// Faixas contíguas `(início, fim, tipo)` que cobrem o texto inteiro, sempre em limites de caractere.
pub fn tokens(text: &str, lang: Lang) -> Vec<(usize, usize, Tok)> {
    let mut out: Vec<(usize, usize, Tok)> = Vec::new();
    match lang {
        Lang::Plain => push(&mut out, 0, text.len(), Tok::Normal),
        Lang::Xml => xml(text, &mut out),
        Lang::Kotlin => clike(text, KOTLIN, true, &mut out),
        Lang::Java => clike(text, JAVA, false, &mut out),
    }
    out
}

fn push(out: &mut Vec<(usize, usize, Tok)>, a: usize, b: usize, t: Tok) {
    if a >= b {
        return;
    }
    match out.last_mut() {
        Some(last) if last.2 == t && last.1 == a => last.1 = b,
        _ => out.push((a, b, t)),
    }
}

fn find(text: &str, from: usize, pat: &str) -> usize {
    text[from..].find(pat).map_or(text.len(), |i| from + i + pat.len())
}

fn is_ident(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

fn clike(text: &str, keywords: &[&str], raw_strings: bool, out: &mut Vec<(usize, usize, Tok)>) {
    let b = text.as_bytes();
    let n = b.len();
    let mut i = 0;
    while i < n {
        let rest = &text[i..];
        let (end, tok) = if rest.starts_with("//") {
            (rest.find('\n').map_or(n, |k| i + k), Tok::Comment)
        } else if rest.starts_with("/*") {
            (find(text, i + 2, "*/"), Tok::Comment)
        } else if raw_strings && rest.starts_with("\"\"\"") {
            (find(text, i + 3, "\"\"\""), Tok::String)
        } else if b[i] == b'"' || b[i] == b'\'' {
            let q = b[i];
            let mut j = i + 1;
            while j < n && b[j] != q && b[j] != b'\n' {
                j += if b[j] == b'\\' && j + 1 < n { 2 } else { 1 };
            }
            ((j + 1).min(n), Tok::String)
        } else if b[i] == b'@' && i + 1 < n && is_ident(b[i + 1]) {
            let mut j = i + 1;
            while j < n && is_ident(b[j]) {
                j += 1;
            }
            (j, Tok::Annotation)
        } else if b[i].is_ascii_digit() {
            let mut j = i;
            while j < n && (is_ident(b[j]) || b[j] == b'.') {
                j += 1;
            }
            (j, Tok::Number)
        } else if b[i].is_ascii_alphabetic() || b[i] == b'_' {
            let mut j = i;
            while j < n && is_ident(b[j]) {
                j += 1;
            }
            let word = &text[i..j];
            let tok = if keywords.contains(&word) {
                Tok::Keyword
            } else if b[i].is_ascii_uppercase() {
                Tok::Type
            } else {
                Tok::Normal
            };
            (j, tok)
        } else {
            // Um caractere qualquer (pode ter vários bytes): avança até o próximo limite.
            let mut j = i + 1;
            while j < n && !text.is_char_boundary(j) {
                j += 1;
            }
            (j, Tok::Normal)
        };
        push(out, i, end, tok);
        i = end;
    }
}

fn xml(text: &str, out: &mut Vec<(usize, usize, Tok)>) {
    let b = text.as_bytes();
    let n = b.len();
    let mut i = 0;
    while i < n {
        if text[i..].starts_with("<!--") {
            let end = find(text, i + 4, "-->");
            push(out, i, end, Tok::Comment);
            i = end;
        } else if b[i] == b'<' {
            // Marca de abertura e nome da tag.
            let mut j = i + 1;
            while j < n && matches!(b[j], b'/' | b'?' | b'!') {
                j += 1;
            }
            while j < n && (is_ident(b[j]) || matches!(b[j], b':' | b'-' | b'.')) {
                j += 1;
            }
            push(out, i, j, Tok::Tag);
            // Atributos até o fim da tag.
            while j < n && b[j] != b'>' {
                let start = j;
                if b[j] == b'"' || b[j] == b'\'' {
                    let q = b[j];
                    j += 1;
                    while j < n && b[j] != q {
                        j += 1;
                    }
                    j = (j + 1).min(n);
                    push(out, start, j, Tok::String);
                } else if is_ident(b[j]) || matches!(b[j], b':' | b'-' | b'.') {
                    while j < n && (is_ident(b[j]) || matches!(b[j], b':' | b'-' | b'.')) {
                        j += 1;
                    }
                    push(out, start, j, Tok::Attr);
                } else {
                    j += 1;
                    while j < n && !text.is_char_boundary(j) {
                        j += 1;
                    }
                    push(out, start, j, Tok::Normal);
                }
            }
            let end = (j + 1).min(n);
            // O último `/` de `/>` e o `>` fazem parte da tag.
            push(out, j, end, Tok::Tag);
            i = end;
        } else {
            let end = text[i..].find('<').map_or(n, |k| i + k);
            push(out, i, end, Tok::Normal);
            i = end;
        }
    }
}

fn color(t: Tok) -> Color32 {
    match t {
        Tok::Normal => Color32::from_rgb(0xd4, 0xd4, 0xd4),
        Tok::Keyword | Tok::Tag => Color32::from_rgb(0x56, 0x9c, 0xd6),
        Tok::Type => Color32::from_rgb(0x4e, 0xc9, 0xb0),
        Tok::String => Color32::from_rgb(0xce, 0x91, 0x78),
        Tok::Comment => Color32::from_rgb(0x6a, 0x99, 0x55),
        Tok::Number => Color32::from_rgb(0xb5, 0xce, 0xa8),
        Tok::Annotation => Color32::from_rgb(0xdc, 0xdc, 0xaa),
        Tok::Attr => Color32::from_rgb(0x9c, 0xdc, 0xfe),
    }
}

/// Um trecho destacado, em bytes do texto: sublinhado (erro, aviso…) ou com fundo colorido (achado da busca).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mark {
    pub start: usize,
    pub end: usize,
    pub color: Color32,
    /// `true`: pinta o fundo; `false`: sublinha.
    pub fill: bool,
}

impl Mark {
    pub fn underline(start: usize, end: usize, color: Color32) -> Self {
        Self { start, end, color, fill: false }
    }

    pub fn fill(start: usize, end: usize, color: Color32) -> Self {
        Self { start, end, color, fill: true }
    }
}

/// Um pedaço do texto com o estilo final: `(início, fim, tipo, cor do sublinhado, cor do fundo)`.
pub type Segment = (usize, usize, Tok, Option<Color32>, Option<Color32>);

/// Os tokens, cortados nas bordas das marcas. Cada pedaço diz se fica sublinhado (e de que cor); com mais
/// de uma marca no mesmo lugar vale a primeira da lista. Marcas fora do texto ou que cortariam um caractere
/// no meio são ignoradas (o texto pode ter mudado desde que elas foram calculadas).
pub fn segments(text: &str, lang: Lang, marks: &[Mark]) -> Vec<Segment> {
    let valid: Vec<&Mark> =
        marks.iter().filter(|m| m.start < m.end && m.end <= text.len() && text.is_char_boundary(m.start) && text.is_char_boundary(m.end)).collect();
    let mut out = Vec::new();
    for (a, b, t) in tokens(text, lang) {
        let mut cuts = vec![a, b];
        for m in &valid {
            cuts.extend([m.start, m.end].into_iter().filter(|c| *c > a && *c < b));
        }
        cuts.sort_unstable();
        cuts.dedup();
        for w in cuts.windows(2) {
            let covering = |fill: bool| valid.iter().find(|m| m.fill == fill && m.start <= w[0] && w[1] <= m.end).map(|m| m.color);
            out.push((w[0], w[1], t, covering(false), covering(true)));
        }
    }
    out
}

/// Texto colorido, sem quebra de linha (assim o número de linha da lateral sempre alinha), com os trechos
/// de `marks` sublinhados.
pub fn layout_job(text: &str, lang: Lang, font: FontId, marks: &[Mark]) -> LayoutJob {
    let mut job = LayoutJob::default();
    job.wrap.max_width = f32::INFINITY;
    for (a, b, t, under, fill) in segments(text, lang, marks) {
        let mut format = TextFormat::simple(font.clone(), color(t));
        if let Some(c) = under {
            format.underline = Stroke::new(1.5, c);
        }
        if let Some(c) = fill {
            format.background = c;
        }
        job.append(&text[a..b], 0.0, format);
    }
    job
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(text: &str, lang: Lang) -> Vec<(&str, Tok)> {
        tokens(text, lang).into_iter().map(|(a, b, t)| (&text[a..b], t)).collect()
    }

    #[test]
    fn segments_split_tokens_at_the_mark_borders_and_underline_only_the_mark() {
        let text = "val x: Int = \"texto\" // ok";
        let start = text.find("\"texto\"").unwrap();
        let red = Color32::RED;
        let seg = segments(text, Lang::Kotlin, &[Mark::underline(start + 1, start + 6, red)]);
        let under: String = seg.iter().filter(|s| s.3 == Some(red)).map(|s| &text[s.0..s.1]).collect();
        assert_eq!(under, "texto", "só o trecho marcado, que está no meio de um token de texto");
        let all: String = seg.iter().map(|s| &text[s.0..s.1]).collect();
        assert_eq!(all, text, "cortar nas marcas não pode perder nem repetir texto");
    }

    #[test]
    fn invalid_marks_are_ignored_instead_of_panicking() {
        let text = "ação";
        let marks = [
            Mark::underline(2, 3, Color32::RED), // no meio do "ç"
            Mark::underline(0, 99, Color32::RED), // além do fim
            Mark::underline(3, 3, Color32::RED),  // vazia
        ];
        let seg = segments(text, Lang::Plain, &marks);
        assert!(seg.iter().all(|s| s.3.is_none()));
        let all: String = seg.iter().map(|s| &text[s.0..s.1]).collect();
        assert_eq!(all, text);
        let _ = layout_job(text, Lang::Plain, FontId::monospace(14.0), &marks);
    }

    #[test]
    fn fill_and_underline_marks_combine_independently() {
        let text = "abcdef";
        let (red, yellow) = (Color32::RED, Color32::YELLOW);
        let seg = segments(text, Lang::Plain, &[Mark::underline(1, 4, red), Mark::fill(2, 5, yellow)]);
        let at = |i: usize| seg.iter().find(|s| s.0 <= i && i < s.1).map(|s| (s.3, s.4)).unwrap();
        assert_eq!((at(0), at(1), at(2), at(4), at(5)), ((None, None), (Some(red), None), (Some(red), Some(yellow)), (None, Some(yellow)), (None, None)));
        let job = layout_job(text, Lang::Plain, FontId::monospace(14.0), &[Mark::fill(1, 3, yellow)]);
        assert!(job.sections.iter().any(|s| s.format.background == yellow), "o fundo chega ao texto desenhado");
    }

    #[test]
    fn the_first_mark_wins_where_they_overlap() {
        let text = "abcdef";
        let (red, yellow) = (Color32::RED, Color32::YELLOW);
        let seg = segments(text, Lang::Plain, &[Mark::underline(1, 3, red), Mark::underline(2, 5, yellow)]);
        let color_of = |i: usize| seg.iter().find(|s| s.0 <= i && i < s.1).and_then(|s| s.3);
        assert_eq!((color_of(0), color_of(1), color_of(2), color_of(3), color_of(5)), (None, Some(red), Some(red), Some(yellow), None));
    }

    #[test]
    fn language_by_extension() {
        assert_eq!(lang_for(Path::new("A.kt")), Lang::Kotlin);
        assert_eq!(lang_for(Path::new("build.gradle")), Lang::Kotlin);
        assert_eq!(lang_for(Path::new("Main.JAVA")), Lang::Java);
        assert_eq!(lang_for(Path::new("AndroidManifest.xml")), Lang::Xml);
        assert_eq!(lang_for(Path::new("notas.txt")), Lang::Plain);
    }

    #[test]
    fn kotlin_tokens() {
        let k = kinds("fun main() { val s = \"oi\" // fim\n}", Lang::Kotlin);
        assert!(k.contains(&("fun", Tok::Keyword)));
        assert!(k.contains(&("val", Tok::Keyword)));
        assert!(k.contains(&("\"oi\"", Tok::String)));
        assert!(k.contains(&("// fim", Tok::Comment)));
        let t = kinds("@Composable class Tela", Lang::Kotlin);
        assert!(t.contains(&("@Composable", Tok::Annotation)));
        assert!(t.contains(&("Tela", Tok::Type)));
    }

    #[test]
    fn kotlin_raw_string_and_java_has_no_raw_string() {
        assert_eq!(kinds("\"\"\"a\nb\"\"\"", Lang::Kotlin), vec![("\"\"\"a\nb\"\"\"", Tok::String)]);
        assert!(kinds("int x = 0x1F;", Lang::Java).contains(&("0x1F", Tok::Number)));
    }

    #[test]
    fn xml_tags_attributes_and_comments() {
        let k = kinds("<!-- c --><Button android:id=\"@+id/ok\" />", Lang::Xml);
        assert!(k.contains(&("<!-- c -->", Tok::Comment)));
        assert!(k.contains(&("<Button", Tok::Tag)));
        assert!(k.contains(&("android:id", Tok::Attr)));
        assert!(k.contains(&("\"@+id/ok\"", Tok::String)));
    }

    #[test]
    fn ranges_cover_the_text_and_never_split_a_character() {
        let samples = [
            "val ação = \"olá, 🌍\" // ünï\n/* não fecha",
            "<a b=\"é\">çã</a><!-- sem fim",
            "\"sem aspas finais é",
            "",
        ];
        for lang in [Lang::Kotlin, Lang::Java, Lang::Xml, Lang::Plain] {
            for s in samples {
                let toks = tokens(s, lang);
                let mut pos = 0;
                for (a, b, _) in &toks {
                    assert_eq!(*a, pos, "lacuna em {s:?} ({lang:?})");
                    assert!(s.is_char_boundary(*a) && s.is_char_boundary(*b), "{s:?} ({lang:?})");
                    pos = *b;
                }
                assert_eq!(pos, s.len(), "não cobre {s:?} ({lang:?})");
            }
        }
    }
}
