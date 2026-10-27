//! Busca no texto de um arquivo (com substituição) e nos arquivos do projeto.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

/// Para comparar sem diferenciar maiúsculas: um caractere por caractere, para os índices continuarem
/// alinhados (o `to_lowercase` do texto inteiro pode mudar o tamanho, como em `İ`).
fn fold(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

/// Todas as ocorrências de `query` em `text`, como `(início, fim)` em bytes, sem se sobrepor.
pub fn find_all(text: &str, query: &str, case_sensitive: bool) -> Vec<(usize, usize)> {
    if query.is_empty() {
        return Vec::new();
    }
    let norm = |c: char| if case_sensitive { c } else { fold(c) };
    let q: Vec<char> = query.chars().map(norm).collect();
    let chars: Vec<(usize, char)> = text.char_indices().map(|(b, c)| (b, norm(c))).collect();
    let (mut out, mut i) = (Vec::new(), 0);
    while i + q.len() <= chars.len() {
        if chars[i..i + q.len()].iter().zip(&q).all(|((_, a), b)| a == b) {
            let end = chars.get(i + q.len()).map_or(text.len(), |(b, _)| *b);
            out.push((chars[i].0, end));
            i += q.len();
        } else {
            i += 1;
        }
    }
    out
}

/// A ocorrência a partir da qual "próxima" começa: a primeira que não termina antes de `byte_pos`;
/// passando do fim, volta para a primeira.
pub fn pick_from(matches: &[(usize, usize)], byte_pos: usize) -> Option<usize> {
    if matches.is_empty() {
        return None;
    }
    Some(matches.iter().position(|(start, _)| *start >= byte_pos).unwrap_or(0))
}

/// A ocorrência anterior a `byte_pos`: a última que começa antes; antes da primeira volta para a última.
pub fn pick_before(matches: &[(usize, usize)], byte_pos: usize) -> usize {
    matches.iter().rposition(|(start, _)| *start < byte_pos).unwrap_or(matches.len().saturating_sub(1))
}

/// Anda para a próxima ou anterior ocorrência, dando a volta.
pub fn step(current: usize, len: usize, forward: bool) -> usize {
    if len == 0 {
        return 0;
    }
    if forward { (current + 1) % len } else { (current + len - 1) % len }
}

pub fn replace_range(text: &str, range: (usize, usize), with: &str) -> String {
    let (a, b) = (range.0.min(text.len()), range.1.min(text.len()));
    format!("{}{with}{}", &text[..a], &text[b..])
}

/// Troca todas as ocorrências. Devolve o texto novo e quantas foram trocadas.
pub fn replace_all(text: &str, query: &str, with: &str, case_sensitive: bool) -> (String, usize) {
    let found = find_all(text, query, case_sensitive);
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for (a, b) in &found {
        out.push_str(&text[last..*a]);
        out.push_str(with);
        last = *b;
    }
    out.push_str(&text[last..]);
    (out, found.len())
}

/// Índice de caractere (no arquivo todo) de uma posição `(linha a partir de 1, coluna em caracteres)`.
pub fn char_offset(text: &str, line1: usize, col: usize) -> usize {
    let mut idx = 0;
    for (n, line) in text.split('\n').enumerate() {
        let len = line.chars().count();
        if n + 1 == line1 {
            return idx + col.min(len);
        }
        idx += len + 1;
    }
    idx.min(text.chars().count())
}

// ---------------------------------------------------------------------------------------------
// Busca nos arquivos do projeto.

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hit {
    pub path: PathBuf,
    /// Linha a partir de 1.
    pub line: usize,
    /// Coluna e tamanho da ocorrência, em caracteres.
    pub col: usize,
    pub len: usize,
    /// A linha, sem os espaços do começo e cortada, para a lista.
    pub preview: String,
}

#[derive(Default, Debug, PartialEq, Eq)]
pub struct Stats {
    pub files: usize,
    pub hits: usize,
    /// Parou no limite de resultados.
    pub truncated: bool,
}

/// Pastas que não valem a pena varrer: controle de versão e saídas de build.
const SKIP_DIRS: &[&str] = &[".git", ".gradle", ".idea", ".kls", ".cxx", "build", "target", "node_modules", ".metadata"];
const MAX_FILE_BYTES: u64 = 1 << 20;
pub const MAX_HITS: usize = 2000;
const PREVIEW_CHARS: usize = 160;

/// Procura `query` nos arquivos de texto abaixo de `root`. Chama `on_file` com os resultados de cada
/// arquivo (para a lista ir enchendo) e para quando `cancel` é ligado ou o limite de resultados chega.
/// Arquivos binários, grandes demais ou que não são UTF-8 são ignorados.
pub fn search_dir(root: &Path, query: &str, case_sensitive: bool, cancel: &AtomicBool, mut on_file: impl FnMut(Vec<Hit>)) -> Stats {
    let mut stats = Stats::default();
    if query.is_empty() {
        return stats;
    }
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = fs::read_dir(&dir) else { continue };
        let mut entries: Vec<_> = rd.filter_map(Result::ok).collect();
        entries.sort_by_key(|e| e.file_name());
        let mut subdirs = Vec::new();
        for e in entries {
            if cancel.load(Ordering::Relaxed) {
                return stats;
            }
            let path = e.path();
            let name = e.file_name().to_string_lossy().into_owned();
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_dir() {
                if !SKIP_DIRS.contains(&name.as_str()) {
                    subdirs.push(path);
                }
                continue;
            }
            if !ft.is_file() || e.metadata().map_or(true, |m| m.len() > MAX_FILE_BYTES) {
                continue;
            }
            let Ok(bytes) = fs::read(&path) else { continue };
            if bytes.contains(&0) {
                continue;
            }
            let Ok(text) = String::from_utf8(bytes) else { continue };
            stats.files += 1;
            let mut hits = Vec::new();
            for (n, line) in text.lines().enumerate() {
                for (a, b) in find_all(line, query, case_sensitive) {
                    hits.push(Hit {
                        path: path.clone(),
                        line: n + 1,
                        col: line[..a].chars().count(),
                        len: line[a..b].chars().count(),
                        preview: line.trim().chars().take(PREVIEW_CHARS).collect(),
                    });
                    if stats.hits + hits.len() >= MAX_HITS {
                        stats.truncated = true;
                        break;
                    }
                }
                if stats.truncated {
                    break;
                }
            }
            stats.hits += hits.len();
            if !hits.is_empty() {
                on_file(hits);
            }
            if stats.truncated {
                return stats;
            }
        }
        // As pastas entram na pilha ao contrário, para sair em ordem alfabética.
        stack.extend(subdirs.into_iter().rev());
    }
    stats
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_every_occurrence_without_overlap() {
        assert_eq!(find_all("aaaa", "aa", true), vec![(0, 2), (2, 4)]);
        assert_eq!(find_all("abcabc", "bc", true), vec![(1, 3), (4, 6)]);
        assert!(find_all("abc", "", true).is_empty());
        assert!(find_all("abc", "zz", true).is_empty());
        assert!(find_all("", "a", true).is_empty());
    }

    #[test]
    fn case_sensitivity_and_unicode() {
        assert_eq!(find_all("Foo foo FOO", "foo", false).len(), 3);
        assert_eq!(find_all("Foo foo FOO", "foo", true), vec![(4, 7)]);
        let text = "AÇÃO ação";
        let m = find_all(text, "ação", false);
        assert_eq!(m.len(), 2);
        assert_eq!(&text[m[0].0..m[0].1], "AÇÃO", "os bytes de cada ocorrência respeitam os acentos");
        assert_eq!(&text[m[1].0..m[1].1], "ação");
        assert_eq!(find_all("😀 a 😀", "😀", true).len(), 2);
    }

    #[test]
    fn next_and_previous_wrap_around() {
        let m = [(2, 4), (10, 12), (20, 22)];
        assert_eq!(pick_from(&m, 0), Some(0));
        assert_eq!(pick_from(&m, 5), Some(1));
        assert_eq!(pick_from(&m, 20), Some(2));
        assert_eq!(pick_from(&m, 21), Some(0), "depois da última volta para a primeira");
        assert_eq!(pick_from(&[], 0), None);
        assert_eq!((step(2, 3, true), step(0, 3, false), step(1, 3, true), step(0, 0, true)), (0, 2, 2, 0));
        assert_eq!((pick_before(&m, 21), pick_before(&m, 10), pick_before(&m, 2), pick_before(&m, 0)), (2, 0, 2, 2), "antes da primeira volta para a última");
    }

    #[test]
    fn replaces_one_or_all() {
        assert_eq!(replace_range("a foo b", (2, 5), "barra"), "a barra b");
        assert_eq!(replace_all("foo Foo", "foo", "x", false), ("x x".to_string(), 2));
        assert_eq!(replace_all("foo Foo", "foo", "x", true), ("x Foo".to_string(), 1));
        assert_eq!(replace_all("sem nada", "foo", "x", true), ("sem nada".to_string(), 0));
        assert_eq!(replace_all("aaa", "a", "aa", true), ("aaaaaa".to_string(), 3), "a troca não é buscada de novo");
        assert_eq!(replace_all("ação", "ç", "c", true).0, "acão");
        assert_eq!(replace_all("x", "x", "", true), ("".to_string(), 1), "dá para apagar");
    }

    #[test]
    fn char_offset_counts_characters_across_lines() {
        let t = "ab\ncção\n\nfim";
        assert_eq!(char_offset(t, 1, 1), 1);
        assert_eq!(char_offset(t, 2, 0), 3);
        assert_eq!(char_offset(t, 2, 3), 6, "ç e ã contam como um caractere cada");
        assert_eq!(char_offset(t, 4, 2), t.chars().count() - 1);
        assert_eq!(char_offset(t, 2, 99), 7, "coluna além do fim da linha vai para o fim dela");
        assert_eq!(char_offset(t, 99, 0), t.chars().count());
    }

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("andsim-search-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn searches_text_files_and_skips_build_dirs_binaries_and_big_files() {
        let root = tmp("dir");
        fs::create_dir_all(root.join("app/src")).unwrap();
        fs::create_dir_all(root.join("app/build")).unwrap();
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::write(root.join("app/src/Main.kt"), "fun main() {\n    println(\"ação\")\n    // ACAO tambem\n}\n").unwrap();
        fs::write(root.join("app/src/Outro.kt"), "val ação = 1\n").unwrap();
        fs::write(root.join("app/build/Gerado.kt"), "val ação = 2\n").unwrap();
        fs::write(root.join(".git/config"), "ação\n").unwrap();
        fs::write(root.join("img.bin"), b"a\0\xE7\xC3\xA3o").unwrap();
        fs::write(root.join("latin1.txt"), b"a\xE7\xE3o").unwrap();
        fs::write(root.join("grande.txt"), "ação ".repeat(400_000)).unwrap();

        let mut got = Vec::new();
        let stats = search_dir(&root, "ação", false, &AtomicBool::new(false), |h| got.extend(h));
        let names: Vec<String> = got.iter().map(|h| h.path.file_name().unwrap().to_string_lossy().into_owned()).collect();
        assert_eq!(names, ["Main.kt", "Outro.kt"], "só os arquivos de texto fora das pastas ignoradas: {names:?}");
        assert_eq!(stats.hits, 2);
        assert!(!stats.truncated);
        let h = &got[0];
        assert_eq!((h.line, h.col, h.len), (2, 13, 4));
        assert_eq!(h.preview, "println(\"ação\")");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn stops_at_the_result_limit_and_when_cancelled() {
        let root = tmp("limit");
        fs::write(root.join("a.txt"), "x\n".repeat(MAX_HITS + 500)).unwrap();
        let stats = search_dir(&root, "x", true, &AtomicBool::new(false), |_| {});
        assert_eq!((stats.hits, stats.truncated), (MAX_HITS, true));
        let mut called = false;
        let stats = search_dir(&root, "x", true, &AtomicBool::new(true), |_| called = true);
        assert_eq!((stats.hits, called), (0, false), "cancelado antes de começar não devolve nada");
        assert_eq!(search_dir(&root, "", true, &AtomicBool::new(false), |_| {}), Stats::default(), "busca vazia não varre nada");
        let _ = fs::remove_dir_all(root);
    }
}
