//! Problemas (erros, avisos…) de duas fontes, num só formato: o LSP (os servidores publicam enquanto se
//! digita) e o Gradle (o que a última compilação reclamou). Daqui saem o sublinhado do editor, o tooltip,
//! a aba Problemas e a contagem da barra de status.

use crate::autocomplete::is_ident;
use crate::diagnostics::Diagnostic;
use crate::highlight::Mark;
use crate::lsp::{self, Diag};
use eframe::egui::Color32;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Severity {
    Error,
    Warning,
    Info,
    Hint,
}

impl Severity {
    /// Pelo número do LSP (1 erro, 2 aviso, 3 informação, 4 dica).
    pub fn from_lsp(n: u8) -> Severity {
        match n {
            2 => Severity::Warning,
            3 => Severity::Info,
            4 => Severity::Hint,
            _ => Severity::Error,
        }
    }

    pub fn color(self) -> Color32 {
        match self {
            Severity::Error => Color32::from_rgb(0xf1, 0x4c, 0x4c),
            Severity::Warning => Color32::from_rgb(0xcc, 0xa7, 0x00),
            Severity::Info => Color32::from_rgb(0x37, 0x94, 0xff),
            Severity::Hint => Color32::from_rgb(0x85, 0x85, 0x85),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Severity::Error => "Erro",
            Severity::Warning => "Aviso",
            Severity::Info => "Info",
            Severity::Hint => "Dica",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Problem {
    pub severity: Severity,
    /// Linha e coluna (em UTF-16), a partir de 0.
    pub line: u32,
    pub col: u32,
    /// Fim do trecho, quando a fonte informa (o LSP informa; o Gradle só dá o começo).
    pub end: Option<(u32, u32)>,
    pub message: String,
    /// Quem achou: `Java`, `kotlin`, `Gradle`…
    pub source: String,
}

pub fn from_lsp(d: &Diag, default_source: &str) -> Problem {
    Problem {
        severity: Severity::from_lsp(d.severity),
        line: d.start.0,
        col: d.start.1,
        end: Some(d.end),
        message: d.message.clone(),
        source: d.source.clone().unwrap_or_else(|| default_source.to_owned()),
    }
}

/// Os diagnósticos do Gradle começam em 1.
pub fn from_gradle(d: &Diagnostic) -> Problem {
    Problem {
        severity: if d.error { Severity::Error } else { Severity::Warning },
        line: d.line.saturating_sub(1),
        col: d.col.saturating_sub(1),
        end: None,
        message: d.message.clone(),
        source: "Gradle".into(),
    }
}

/// O trecho `[início, fim)`, em caracteres, que o problema ocupa no texto. Sem fim informado (ou com fim
/// vazio), pega a palavra que começa ali; se não houver, um caractere; no fim da linha, o último dela.
pub fn span(text: &str, p: &Problem) -> (usize, usize) {
    let total = text.chars().count();
    let start = lsp::char_index_of(text, p.line, p.col).min(total);
    if let Some(end) = p.end.map(|(l, c)| lsp::char_index_of(text, l, c).min(total)).filter(|e| *e > start) {
        return (start, end);
    }
    let ahead: Vec<char> = text.chars().skip(start).take(256).collect();
    let word = ahead.iter().take_while(|c| is_ident(**c)).count();
    if word > 0 {
        return (start, start + word);
    }
    match ahead.first() {
        Some(c) if *c != '\n' && *c != '\r' => (start, start + 1),
        _ if start > 0 && text.chars().nth(start - 1).is_some_and(|c| c != '\n' && c != '\r') => (start - 1, start),
        _ => (start, start),
    }
}

/// Os trechos a sublinhar, em bytes (é o que o realce usa), com os erros na frente dos avisos.
pub fn marks(text: &str, problems: &[Problem]) -> Vec<Mark> {
    if problems.is_empty() {
        return Vec::new();
    }
    let offsets: Vec<usize> = text.char_indices().map(|(b, _)| b).chain(std::iter::once(text.len())).collect();
    let mut sorted: Vec<&Problem> = problems.iter().collect();
    sorted.sort_by_key(|p| p.severity);
    sorted
        .into_iter()
        .filter_map(|p| {
            let (a, b) = span(text, p);
            (a < b).then(|| Mark { start: offsets[a], end: offsets[b], color: p.severity.color() })
        })
        .collect()
}

/// A mensagem do problema que cobre o caractere `idx`, para o tooltip.
pub fn message_at(text: &str, problems: &[Problem], idx: usize) -> Option<String> {
    let mut hits: Vec<&Problem> = problems
        .iter()
        .filter(|p| {
            let (a, b) = span(text, p);
            a <= idx && idx < b
        })
        .collect();
    hits.sort_by_key(|p| p.severity);
    (!hits.is_empty()).then(|| hits.iter().map(|p| format!("{} ({}): {}", p.severity.label(), p.source, p.message)).collect::<Vec<_>>().join("\n"))
}

/// Contagem para a barra de status: `(erros, avisos)`.
pub fn counts<'a>(problems: impl IntoIterator<Item = &'a Problem>) -> (usize, usize) {
    problems.into_iter().fold((0, 0), |(e, w), p| match p.severity {
        Severity::Error => (e + 1, w),
        Severity::Warning => (e, w + 1),
        _ => (e, w),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn problem(line: u32, col: u32, end: Option<(u32, u32)>, severity: Severity) -> Problem {
        Problem { severity, line, col, end, message: "m".into(), source: "t".into() }
    }

    #[test]
    fn converts_from_both_sources() {
        let lsp = Diag { start: (2, 4), end: (2, 9), severity: 2, message: "x".into(), source: None };
        let p = from_lsp(&lsp, "Java");
        assert_eq!((p.severity, p.line, p.col, p.end, p.source.as_str()), (Severity::Warning, 2, 4, Some((2, 9)), "Java"));
        let gradle = Diagnostic { path: PathBuf::from("A.kt"), line: 12, col: 26, message: "ruim".into(), error: true };
        let p = from_gradle(&gradle);
        assert_eq!((p.severity, p.line, p.col, p.end, p.source.as_str()), (Severity::Error, 11, 25, None, "Gradle"));
        assert_eq!(Severity::from_lsp(1), Severity::Error);
        assert_eq!(Severity::from_lsp(99), Severity::Error, "desconhecido vale erro");
    }

    #[test]
    fn span_uses_the_explicit_end_or_the_word_that_starts_there() {
        let text = "val x: Int = naoExiste + 1\n";
        let at = text.find("naoExiste").unwrap() as u32;
        assert_eq!(span(text, &problem(0, at, Some((0, at + 9)), Severity::Error)), (at as usize, at as usize + 9));
        assert_eq!(span(text, &problem(0, at, None, Severity::Error)), (at as usize, at as usize + 9), "sem fim: a palavra inteira");
        assert_eq!(span(text, &problem(0, at, Some((0, at)), Severity::Error)), (at as usize, at as usize + 9), "fim vazio: também a palavra");
        let plus = text.find('+').unwrap() as u32;
        assert_eq!(span(text, &problem(0, plus, None, Severity::Error)), (plus as usize, plus as usize + 1), "fora de palavra: um caractere");
    }

    #[test]
    fn span_at_the_end_of_a_line_underlines_the_last_character() {
        let text = "foo(\nbar\n";
        assert_eq!(span(text, &problem(0, 4, None, Severity::Error)), (3, 4));
        assert_eq!(span(text, &problem(0, 99, None, Severity::Error)), (3, 4), "coluna além do fim da linha");
        assert_eq!(span("", &problem(0, 0, None, Severity::Error)), (0, 0), "texto vazio não tem o que sublinhar");
        assert_eq!(span("\nx", &problem(0, 0, None, Severity::Error)), (0, 0), "linha vazia");
    }

    #[test]
    fn columns_are_utf16_so_accents_and_emoji_line_up() {
        let text = "// 🌍 ação\nval n = erro\n";
        // Na linha 1 a coluna é simples; na 0, o emoji vale 2 unidades UTF-16.
        let at = "val n = ".len() as u32;
        let (a, b) = span(text, &problem(1, at, Some((1, at + 4)), Severity::Error));
        assert_eq!(text.chars().skip(a).take(b - a).collect::<String>(), "erro");
        let emoji_line = problem(0, 3, Some((0, 5)), Severity::Error); // 🌍 = 2 unidades
        let (a, b) = span(text, &emoji_line);
        assert_eq!(text.chars().skip(a).take(b - a).collect::<String>(), "🌍");
    }

    #[test]
    fn marks_are_in_bytes_and_errors_come_first() {
        let text = "ação erro aviso";
        let e = problem(0, 5, Some((0, 9)), Severity::Error);
        let w = problem(0, 10, Some((0, 15)), Severity::Warning);
        let m = marks(text, &[w, e]);
        assert_eq!(m.len(), 2);
        assert_eq!(&text[m[0].start..m[0].end], "erro", "o erro vem primeiro e os bytes contam o acento");
        assert_eq!(&text[m[1].start..m[1].end], "aviso");
        assert_eq!(m[0].color, Severity::Error.color());
        assert!(marks(text, &[]).is_empty());
        assert!(marks("", &[problem(0, 0, None, Severity::Error)]).is_empty(), "problema vazio não vira marca");
    }

    #[test]
    fn tooltip_message_lists_the_problems_under_the_pointer() {
        let text = "int x = \"a\";\n";
        let mut e = problem(0, 8, Some((0, 11)), Severity::Error);
        e.message = "Type mismatch".into();
        e.source = "Java".into();
        assert_eq!(message_at(text, &[e.clone()], 9), Some("Erro (Java): Type mismatch".into()));
        assert_eq!(message_at(text, &[e.clone()], 8), Some("Erro (Java): Type mismatch".into()), "o primeiro caractere conta");
        assert_eq!(message_at(text, &[e.clone()], 11), None, "o fim é exclusivo");
        assert_eq!(message_at(text, &[e], 0), None);
    }

    #[test]
    fn counts_errors_and_warnings_only() {
        let ps = [
            problem(0, 0, None, Severity::Error),
            problem(1, 0, None, Severity::Error),
            problem(2, 0, None, Severity::Warning),
            problem(3, 0, None, Severity::Hint),
            problem(4, 0, None, Severity::Info),
        ];
        assert_eq!(counts(&ps), (2, 1));
        assert_eq!(counts(&[] as &[Problem]), (0, 0));
    }
}
