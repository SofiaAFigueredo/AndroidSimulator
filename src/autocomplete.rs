//! Autocomplete do editor: um servidor de linguagem por linguagem (iniciado só quando há um arquivo dela
//! aberto) e a lógica do popup de sugestões (prefixo, filtro e inserção), separada da interface para
//! poder ser testada.

use crate::highlight::Lang;
use crate::lsp::{self, Client, Item};
use crate::servers;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};

enum Slot {
    Off,
    Starting(Receiver<Result<Client, String>>),
    Ready(Client),
    Failed(String),
}

/// Os servidores de linguagem em uso. Cada um é um processo Java (o jdtls reserva até 1 GB), então só
/// sobe quando existe um arquivo da linguagem aberto e cai junto com o projeto.
pub struct Manager {
    java: Slot,
    kotlin: Slot,
    project: Option<PathBuf>,
}

impl Default for Manager {
    fn default() -> Self {
        Self { java: Slot::Off, kotlin: Slot::Off, project: None }
    }
}

impl Manager {
    fn slot(&self, lang: Lang) -> Option<&Slot> {
        match lang {
            Lang::Java => Some(&self.java),
            Lang::Kotlin => Some(&self.kotlin),
            _ => None,
        }
    }

    fn slot_mut(&mut self, lang: Lang) -> Option<&mut Slot> {
        match lang {
            Lang::Java => Some(&mut self.java),
            Lang::Kotlin => Some(&mut self.kotlin),
            _ => None,
        }
    }

    pub fn client(&self, lang: Lang) -> Option<&Client> {
        match self.slot(lang)? {
            Slot::Ready(c) => Some(c),
            _ => None,
        }
    }

    /// Texto curto para a barra de status; `None` se a linguagem não tem autocomplete.
    pub fn status(&self, root: &Path, lang: Lang) -> Option<String> {
        let name = match lang {
            Lang::Java => "Java",
            Lang::Kotlin => "Kotlin",
            _ => return None,
        };
        Some(match self.slot(lang)? {
            Slot::Ready(_) => format!("{name}: autocomplete pronto"),
            Slot::Starting(_) => format!("{name}: iniciando o servidor…"),
            Slot::Failed(e) => format!("{name}: {}", e.lines().next().unwrap_or("erro")),
            Slot::Off if servers::installed(root, lang) => format!("{name}: autocomplete desligado"),
            Slot::Off => format!("{name}: sem autocomplete (baixe o perfil Completa)"),
        })
    }

    /// Se a pasta do projeto mudou, derruba os servidores (eles ficam presos a uma pasta). Devolve
    /// `true` quando mudou, para quem abriu documentos nos servidores antigos abri-los de novo.
    pub fn set_project(&mut self, project: &Path) -> bool {
        let changed = self.project.as_deref() != Some(project);
        if changed {
            self.java = Slot::Off;
            self.kotlin = Slot::Off;
            self.project = Some(project.to_owned());
        }
        changed
    }

    /// Inicia o servidor da linguagem, se estiver instalado e ainda não iniciado.
    pub fn ensure(&mut self, root: &Path, lang: Lang, repaint: impl Fn() + Send + 'static) {
        let Some(project) = self.project.clone() else { return };
        let installed = servers::installed(root, lang);
        let Some(slot) = self.slot_mut(lang) else { return };
        if !matches!(slot, Slot::Off) || !installed {
            return;
        }
        let (tx, rx) = mpsc::channel();
        let root = root.to_owned();
        std::thread::spawn(move || {
            let _ = tx.send(servers::start(&root, lang, &project));
            repaint();
        });
        *slot = Slot::Starting(rx);
    }

    /// Recolhe os servidores que terminaram de iniciar. Devolve as linguagens que ficaram prontas agora
    /// (para abrir os documentos já abertos nelas).
    pub fn poll(&mut self) -> Vec<Lang> {
        let mut ready = Vec::new();
        for lang in [Lang::Java, Lang::Kotlin] {
            let Some(slot) = self.slot_mut(lang) else { continue };
            let Slot::Starting(rx) = slot else { continue };
            match rx.try_recv() {
                Ok(Ok(client)) => {
                    *slot = Slot::Ready(client);
                    ready.push(lang);
                }
                Ok(Err(e)) => *slot = Slot::Failed(e),
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => *slot = Slot::Failed("o servidor não iniciou".into()),
            }
        }
        ready
    }

    pub fn shutdown(&mut self) {
        for lang in [Lang::Java, Lang::Kotlin] {
            if let Some(slot) = self.slot_mut(lang)
                && let Slot::Ready(c) = std::mem::replace(slot, Slot::Off)
            {
                c.shutdown();
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Popup: prefixo, filtro e inserção.

pub fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

/// Índice (em caracteres) onde começa a palavra que termina em `cursor`.
pub fn prefix_start(text: &str, cursor: usize) -> usize {
    let chars: Vec<char> = text.chars().take(cursor).collect();
    let mut i = chars.len();
    while i > 0 && is_ident(chars[i - 1]) {
        i -= 1;
    }
    i
}

/// O texto entre `start` e `cursor`.
pub fn prefix_text(text: &str, start: usize, cursor: usize) -> String {
    text.chars().skip(start).take(cursor.saturating_sub(start)).collect()
}

/// Itens que combinam com o prefixo digitado: primeiro os que começam com ele, depois os que o contêm
/// (só com 2+ letras, para um `m` não trazer a lista inteira). Sem prefixo, todos.
pub fn matching<'a>(items: &'a [Item], prefix: &str) -> Vec<&'a Item> {
    if prefix.is_empty() {
        return items.iter().collect();
    }
    let p = prefix.to_lowercase();
    let starts = |i: &Item| i.filter.to_lowercase().starts_with(&p) || i.label.to_lowercase().starts_with(&p);
    let mut out: Vec<&Item> = items.iter().filter(|i| starts(i)).collect();
    if p.chars().count() >= 2 {
        out.extend(items.iter().filter(|i| !starts(i) && (i.filter.to_lowercase().contains(&p) || i.label.to_lowercase().contains(&p))));
    }
    out
}

/// Aplica a sugestão: devolve o texto novo e o trecho `(início, fim)` que fica selecionado: o espaço
/// reservado do snippet ou, sem ele, só o cursor depois do que foi inserido. Usa o trecho que o servidor mandou
/// substituir quando houver (do começo dele até o cursor, já que o usuário pode ter digitado mais depois
/// do pedido); senão, o prefixo digitado.
pub fn apply(text: &str, cursor: usize, prefix_start: usize, item: &Item) -> (String, (usize, usize)) {
    let chars: Vec<char> = text.chars().collect();
    let to = cursor.min(chars.len());
    let mut from = prefix_start;
    if let Some(((line, col), _)) = item.range {
        let idx = lsp::char_index_of(text, line, col);
        if idx <= to {
            from = idx;
        }
    }
    let from = from.min(to);
    let (insert, stop) = if item.snippet { lsp::snippet_to_plain(&item.insert) } else { (item.insert.clone(), None) };
    let inserted = insert.chars().count();
    let mut out: String = chars[..from].iter().collect();
    out.push_str(&insert);
    out.extend(&chars[to..]);
    let selection = stop.map_or((from + inserted, from + inserted), |(a, b)| (from + a, from + b));
    (out, selection)
}

/// A sugestão aberta no editor.
pub struct Popup {
    pub items: Vec<Item>,
    /// O servidor cortou a lista: refazer o pedido conforme o usuário digita.
    pub incomplete: bool,
    pub selected: usize,
    /// Índice (em caracteres) onde começava a palavra quando o pedido foi feito.
    pub origin: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(label: &str, insert: &str) -> Item {
        Item { label: label.into(), kind: None, detail: None, sort: label.into(), filter: label.into(), insert: insert.into(), snippet: false, range: None }
    }

    #[test]
    fn prefix_is_the_identifier_before_the_cursor() {
        let t = "System.out.pri";
        assert_eq!(prefix_start(t, t.chars().count()), 11);
        assert_eq!(prefix_text(t, 11, 14), "pri");
        assert_eq!(prefix_start("a.", 2), 2, "logo depois do ponto não há prefixo");
        assert_eq!(prefix_start("ação_x", 6), 0, "acentos e _ fazem parte da palavra");
        assert_eq!(prefix_start("", 0), 0);
    }

    #[test]
    fn matching_puts_prefix_matches_first_and_needs_two_letters_for_contains() {
        let items = vec![item("toString", "toString"), item("println", "println"), item("print", "print"), item("sprint", "sprint")];
        let m: Vec<&str> = matching(&items, "pr").into_iter().map(|i| i.label.as_str()).collect();
        assert_eq!(m, ["println", "print", "sprint"]);
        let one: Vec<&str> = matching(&items, "p").into_iter().map(|i| i.label.as_str()).collect();
        assert_eq!(one, ["println", "print"], "com uma letra só entram os que começam com ela");
        assert_eq!(matching(&items, "").len(), 4);
        assert!(matching(&items, "zzz").is_empty());
        assert_eq!(matching(&items, "PRINT").len(), 3, "sem diferenciar maiúsculas");
    }

    #[test]
    fn apply_replaces_the_typed_prefix() {
        let text = "val x = lista.ma\n";
        let cursor = "val x = lista.ma".chars().count();
        let start = prefix_start(text, cursor);
        let (out, sel) = apply(text, cursor, start, &item("map", "map"));
        assert_eq!(out, "val x = lista.map\n");
        let end = "val x = lista.map".chars().count();
        assert_eq!(sel, (end, end), "sem snippet o cursor fica depois do que foi inserido");
    }

    #[test]
    fn apply_honours_the_server_range_and_keeps_the_text_after_the_cursor() {
        let text = "a.pr(b)";
        let it = Item { range: Some(((0, 2), (0, 4))), ..item("println", "println") };
        let (out, sel) = apply(text, 4, 2, &it);
        assert_eq!(out, "a.println(b)");
        let end = "a.println".chars().count();
        assert_eq!(sel, (end, end));
    }

    #[test]
    fn apply_turns_snippets_into_text_and_places_the_cursor_on_the_first_tabstop() {
        let it = Item { snippet: true, ..item("println(x)", "println(${1:x})$0") };
        let (out, sel) = apply("pr", 2, 0, &it);
        assert_eq!(out, "println(x)");
        let open = "println(".chars().count();
        assert_eq!(sel, (open, open + 1), "o x fica selecionado para a próxima digitação trocá-lo");
        let call = Item { snippet: true, ..item("run()", "run($0)") };
        let (out, sel) = apply("ru", 2, 0, &call);
        assert_eq!((out.as_str(), sel), ("run()", (4, 4)));
    }

    #[test]
    fn apply_works_with_accents_and_emoji_before_the_cursor() {
        let text = "// 🌍 ação\nlis";
        let cursor = text.chars().count();
        let start = prefix_start(text, cursor);
        let (out, sel) = apply(text, cursor, start, &item("lista", "lista"));
        assert_eq!(out, "// 🌍 ação\nlista");
        assert_eq!(sel, (out.chars().count(), out.chars().count()));
    }

    #[test]
    fn manager_without_servers_reports_it_and_does_not_start_anything() {
        let root = std::env::temp_dir().join(format!("andsim-ac-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let mut m = Manager::default();
        m.set_project(&root);
        m.ensure(&root, Lang::Java, || {});
        m.ensure(&root, Lang::Kotlin, || {});
        assert!(m.poll().is_empty());
        assert!(m.client(Lang::Java).is_none());
        assert!(m.status(&root, Lang::Java).unwrap().contains("sem autocomplete"));
        assert_eq!(m.status(&root, Lang::Xml), None);
        let _ = std::fs::remove_dir_all(root);
    }
}
