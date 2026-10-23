//! Cliente LSP mínimo (JSON-RPC sobre stdio) só com o que o autocomplete precisa: iniciar o servidor,
//! abrir e atualizar documentos e pedir `textDocument/completion`. Todas as chamadas são assíncronas
//! (devolvem um canal), então a interface nunca trava esperando o servidor.

use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

// ---------------------------------------------------------------------------------------------
// Texto: o LSP conta colunas em UTF-16, o editor conta caracteres.

/// `(linha, coluna em UTF-16)` do caractere de índice `char_idx`.
pub fn position_of(text: &str, char_idx: usize) -> (u32, u32) {
    let (mut line, mut col) = (0u32, 0u32);
    for c in text.chars().take(char_idx) {
        if c == '\n' {
            line += 1;
            col = 0;
        } else {
            col += c.len_utf16() as u32;
        }
    }
    (line, col)
}

/// Índice de caractere de `(linha, coluna UTF-16)`; posições além do fim da linha vão para o fim dela.
pub fn char_index_of(text: &str, line: u32, utf16: u32) -> usize {
    let (mut cur_line, mut col, mut idx) = (0u32, 0u32, 0usize);
    for c in text.chars() {
        if cur_line == line && (col >= utf16 || c == '\n') {
            return idx;
        }
        if cur_line > line {
            break;
        }
        if c == '\n' {
            cur_line += 1;
            col = 0;
        } else {
            col += c.len_utf16() as u32;
        }
        idx += 1;
    }
    idx
}

/// `file:///C:/Users/Fulana/Meu%20App/A.kt`: o que os servidores esperam como identificador de arquivo.
pub fn path_to_uri(p: &Path) -> String {
    let s = p.to_string_lossy().replace('\\', "/");
    let mut out = String::from(if s.starts_with('/') { "file://" } else { "file:///" });
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~/:".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Remove a sintaxe de snippet do LSP (`$1`, `${1:nome}`, `$0`, placeholders aninhados). Devolve o
/// texto puro e, se houver tabstop, o trecho `(início, fim)` que deve ficar selecionado: o do tabstop de
/// menor número (`$0` por último). Num `${1:nome}` ele cobre o texto padrão, para a próxima digitação
/// substituí-lo; num `$1` ou `$0` o início e o fim são iguais (só o cursor).
pub fn snippet_to_plain(s: &str) -> (String, Option<(usize, usize)>) {
    struct State {
        chars: Vec<char>,
        out: String,
        stop_range: Option<(usize, usize)>,
        rank: u32,
    }

    impl State {
        /// Registra um tabstop; devolve `true` se ele passou a ser o escolhido.
        fn stop(&mut self, n: u32) -> bool {
            let rank = if n == 0 { u32::MAX - 1 } else { n };
            if rank >= self.rank {
                return false;
            }
            self.rank = rank;
            let at = self.out.chars().count();
            self.stop_range = Some((at, at));
            true
        }

        fn number(&mut self, mut i: usize) -> (u32, usize) {
            let start = i;
            while i < self.chars.len() && self.chars[i].is_ascii_digit() {
                i += 1;
            }
            (self.chars[start..i].iter().collect::<String>().parse().unwrap_or(0), i)
        }

        /// Consome até o fim do texto ou, em `nested`, até a `}` que fecha o placeholder.
        fn parse(&mut self, mut i: usize, nested: bool) -> usize {
            while i < self.chars.len() {
                match self.chars[i] {
                    '}' if nested => return i + 1,
                    '\\' if i + 1 < self.chars.len() && matches!(self.chars[i + 1], '$' | '}' | '\\') => {
                        self.out.push(self.chars[i + 1]);
                        i += 2;
                    }
                    '$' if i + 1 < self.chars.len() && self.chars[i + 1].is_ascii_digit() => {
                        let (n, j) = self.number(i + 1);
                        self.stop(n);
                        i = j;
                    }
                    '$' if i + 1 < self.chars.len() && self.chars[i + 1] == '{' => {
                        let (n, j) = self.number(i + 2);
                        let best = self.stop(n);
                        i = if j < self.chars.len() && self.chars[j] == ':' {
                            let next = self.parse(j + 1, true);
                            if best && let Some((start, _)) = self.stop_range {
                                self.stop_range = Some((start, self.out.chars().count()));
                            }
                            next
                        } else {
                            j + 1
                        };
                    }
                    c => {
                        self.out.push(c);
                        i += 1;
                    }
                }
            }
            i
        }
    }

    let mut st = State { chars: s.chars().collect(), out: String::new(), stop_range: None, rank: u32::MAX };
    st.parse(0, false);
    (st.out, st.stop_range)
}

// ---------------------------------------------------------------------------------------------
// Itens de completion.

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    pub label: String,
    pub kind: Option<u32>,
    pub detail: Option<String>,
    pub sort: String,
    pub filter: String,
    pub insert: String,
    pub snippet: bool,
    /// Trecho que o servidor quer substituir: `((linha, col), (linha, col))` em UTF-16.
    pub range: Option<((u32, u32), (u32, u32))>,
}

impl Item {
    /// Letra curta do tipo, para a lista.
    pub fn kind_letter(&self) -> char {
        match self.kind {
            Some(2..=4) => 'm',
            Some(5 | 10) => 'f',
            Some(6) => 'v',
            Some(7 | 22) => 'C',
            Some(8) => 'I',
            Some(9) => 'p',
            Some(13 | 20) => 'E',
            Some(14) => 'k',
            Some(15) => 's',
            Some(21) => 'c',
            _ => '·',
        }
    }
}

fn position(v: &Value) -> Option<(u32, u32)> {
    Some((v.get("line")?.as_u64()? as u32, v.get("character")?.as_u64()? as u32))
}

/// O servidor cortou a lista (o kotlin-language-server devolve no máximo 75 itens): é preciso pedir de
/// novo conforme o usuário digita, para o que ficou de fora aparecer.
pub fn is_incomplete(v: &Value) -> bool {
    v.get("isIncomplete").and_then(Value::as_bool).unwrap_or(false)
}

/// Interpreta a resposta de `textDocument/completion` (lista simples ou `CompletionList`).
pub fn parse_completion(v: &Value) -> Vec<Item> {
    let items = v.get("items").and_then(Value::as_array).or_else(|| v.as_array());
    let mut out: Vec<Item> = items
        .into_iter()
        .flatten()
        .filter_map(|it| {
            let label = it.get("label")?.as_str()?.to_owned();
            let edit = it.get("textEdit");
            let insert = edit
                .and_then(|e| e.get("newText"))
                .or_else(|| it.get("insertText"))
                .and_then(Value::as_str)
                .unwrap_or(&label)
                .to_owned();
            // `insert`/`replace` (InsertReplaceEdit) usam `insert`; edit simples usa `range`.
            let range = edit.and_then(|e| e.get("range").or_else(|| e.get("insert"))).and_then(|r| Some((position(r.get("start")?)?, position(r.get("end")?)?)));
            Some(Item {
                kind: it.get("kind").and_then(Value::as_u64).map(|k| k as u32),
                detail: it.get("detail").and_then(Value::as_str).map(str::to_owned),
                sort: it.get("sortText").and_then(Value::as_str).unwrap_or(&label).to_owned(),
                filter: it.get("filterText").and_then(Value::as_str).unwrap_or(&label).to_owned(),
                snippet: it.get("insertTextFormat").and_then(Value::as_u64) == Some(2),
                insert,
                range,
                label,
            })
        })
        .collect();
    out.sort_by(|a, b| a.sort.cmp(&b.sort).then_with(|| a.label.cmp(&b.label)));
    out
}

// ---------------------------------------------------------------------------------------------
// Transporte.

/// Mensagem pronta para escrever: cabeçalho `Content-Length` + corpo JSON.
pub fn encode(msg: &Value) -> Vec<u8> {
    let body = msg.to_string();
    format!("Content-Length: {}\r\n\r\n{body}", body.len()).into_bytes()
}

/// Lê uma mensagem; `None` quando o servidor fechou a saída.
pub fn read_message<R: BufRead>(r: &mut R) -> std::io::Result<Option<Value>> {
    let mut len = None;
    loop {
        let mut line = String::new();
        if r.read_line(&mut line)? == 0 {
            return Ok(None);
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some(v) = line.strip_prefix("Content-Length:") {
            len = v.trim().parse::<usize>().ok();
        }
    }
    let Some(len) = len else { return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "sem Content-Length")) };
    let mut body = vec![0u8; len];
    r.read_exact(&mut body)?;
    serde_json::from_slice(&body).map(Some).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

type Pending = Mutex<HashMap<i64, Sender<Result<Value, String>>>>;

struct Inner {
    stdin: Mutex<ChildStdin>,
    pending: Pending,
    next_id: AtomicI64,
    child: Mutex<Child>,
    /// Últimas linhas que o servidor escreveu no stderr, para diagnosticar falhas.
    stderr: Mutex<VecDeque<String>>,
}

impl Inner {
    fn send(&self, msg: &Value) -> Result<(), String> {
        let mut w = self.stdin.lock().map_err(|_| "servidor travado")?;
        w.write_all(&encode(msg)).and_then(|_| w.flush()).map_err(|e| format!("o servidor LSP fechou a conexão: {e}"))
    }
}

#[derive(Clone)]
pub struct Client {
    inner: Arc<Inner>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        if let Ok(mut c) = self.child.lock() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

impl Client {
    /// Inicia o servidor e faz o `initialize`. Bloqueante (jdtls leva dezenas de segundos): chame de
    /// uma thread.
    pub fn start(cmd: &mut Command, root: &Path, init_options: Value, timeout: Duration) -> Result<Client, String> {
        cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = cmd.spawn().map_err(|e| format!("não consegui iniciar o servidor: {e}"))?;
        let stdin = child.stdin.take().ok_or("sem stdin")?;
        let stdout = child.stdout.take().ok_or("sem stdout")?;
        let stderr = child.stderr.take().ok_or("sem stderr")?;
        let inner = Arc::new(Inner {
            stdin: Mutex::new(stdin),
            pending: Mutex::new(HashMap::new()),
            next_id: AtomicI64::new(1),
            child: Mutex::new(child),
            stderr: Mutex::new(VecDeque::new()),
        });

        let weak = Arc::downgrade(&inner);
        std::thread::spawn(move || {
            let mut r = BufReader::new(stderr);
            let mut line = String::new();
            while r.read_line(&mut line).unwrap_or(0) > 0 {
                let Some(inner) = weak.upgrade() else { break };
                if let Ok(mut q) = inner.stderr.lock() {
                    if q.len() >= 200 {
                        q.pop_front();
                    }
                    q.push_back(line.trim_end().to_owned());
                }
                line.clear();
            }
        });

        let weak = Arc::downgrade(&inner);
        std::thread::spawn(move || {
            let mut r = BufReader::new(stdout);
            while let Ok(Some(msg)) = read_message(&mut r) {
                let Some(inner) = weak.upgrade() else { break };
                let id = msg.get("id").cloned();
                // Os servidores contam o que estão fazendo por `window/logMessage`; guardamos junto
                // com o stderr, que é onde se olha quando algo não funciona.
                if msg.get("method").and_then(Value::as_str) == Some("window/logMessage")
                    && let Some(text) = msg.pointer("/params/message").and_then(Value::as_str)
                    && let Ok(mut q) = inner.stderr.lock()
                {
                    if q.len() >= 200 {
                        q.pop_front();
                    }
                    q.push_back(text.lines().next().unwrap_or_default().to_owned());
                }
                if msg.get("method").is_some() {
                    // Pedido do servidor para nós (registerCapability, workDoneProgress/create…):
                    // responder vazio basta e evita que ele fique esperando.
                    if let Some(id) = id {
                        let _ = inner.send(&json!({"jsonrpc": "2.0", "id": id, "result": Value::Null}));
                    }
                } else if let Some(id) = id.and_then(|i| i.as_i64()) {
                    let result = match msg.get("error") {
                        Some(e) => Err(e.get("message").and_then(Value::as_str).unwrap_or("erro do servidor").to_owned()),
                        None => Ok(msg.get("result").cloned().unwrap_or(Value::Null)),
                    };
                    if let Some(tx) = inner.pending.lock().ok().and_then(|mut p| p.remove(&id)) {
                        let _ = tx.send(result);
                    }
                }
            }
            // O servidor fechou: quem esperava resposta recebe um erro em vez de ficar pendurado.
            if let Some(inner) = weak.upgrade()
                && let Ok(mut p) = inner.pending.lock()
            {
                for (_, tx) in p.drain() {
                    let _ = tx.send(Err("o servidor LSP encerrou".into()));
                }
            }
        });

        let client = Client { inner };
        let uri = path_to_uri(root);
        let name = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let rx = client.request(
            "initialize",
            json!({
                "processId": std::process::id(),
                "rootUri": uri,
                "workspaceFolders": [{"uri": uri, "name": name}],
                "initializationOptions": init_options,
                "capabilities": {
                    "textDocument": {
                        "synchronization": {"didSave": false, "dynamicRegistration": false},
                        "completion": {"completionItem": {"snippetSupport": true, "documentationFormat": ["plaintext"]}}
                    },
                    "workspace": {"workspaceFolders": true, "configuration": false}
                }
            }),
        );
        match rx.recv_timeout(timeout) {
            Ok(Ok(_)) => {}
            Ok(Err(e)) => return Err(format!("initialize falhou: {e}{}", client.stderr_tail())),
            Err(_) => return Err(format!("o servidor não respondeu ao initialize em {}s{}", timeout.as_secs(), client.stderr_tail())),
        }
        client.notify("initialized", json!({}))?;
        Ok(client)
    }

    /// As últimas linhas de log do servidor (stderr e `window/logMessage`).
    #[cfg(test)]
    pub fn log_lines(&self) -> Vec<String> {
        self.inner.stderr.lock().map(|q| q.iter().cloned().collect()).unwrap_or_default()
    }

    fn stderr_tail(&self) -> String {
        let q = self.inner.stderr.lock().map(|q| q.iter().rev().take(5).rev().cloned().collect::<Vec<_>>()).unwrap_or_default();
        if q.is_empty() { String::new() } else { format!("\n{}", q.join("\n")) }
    }

    pub fn request(&self, method: &str, params: Value) -> Receiver<Result<Value, String>> {
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel();
        if let Ok(mut p) = self.inner.pending.lock() {
            p.insert(id, tx.clone());
        }
        if let Err(e) = self.inner.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})) {
            let _ = tx.send(Err(e));
        }
        rx
    }

    pub fn notify(&self, method: &str, params: Value) -> Result<(), String> {
        self.inner.send(&json!({"jsonrpc": "2.0", "method": method, "params": params}))
    }

    pub fn did_open(&self, uri: &str, language: &str, version: i32, text: &str) -> Result<(), String> {
        self.notify("textDocument/didOpen", json!({"textDocument": {"uri": uri, "languageId": language, "version": version, "text": text}}))
    }

    /// Envia o texto inteiro: todo servidor aceita, e dispensa calcular diferenças.
    pub fn did_change(&self, uri: &str, version: i32, text: &str) -> Result<(), String> {
        self.notify("textDocument/didChange", json!({"textDocument": {"uri": uri, "version": version}, "contentChanges": [{"text": text}]}))
    }

    pub fn did_close(&self, uri: &str) -> Result<(), String> {
        self.notify("textDocument/didClose", json!({"textDocument": {"uri": uri}}))
    }

    pub fn completion(&self, uri: &str, line: u32, character: u32) -> Receiver<Result<Value, String>> {
        self.request("textDocument/completion", json!({"textDocument": {"uri": uri}, "position": {"line": line, "character": character}}))
    }

    /// Desliga com educação (shutdown + exit); o `Drop` mata o processo se ele não sair.
    pub fn shutdown(&self) {
        let rx = self.request("shutdown", Value::Null);
        let _ = rx.recv_timeout(Duration::from_secs(3));
        let _ = self.notify("exit", Value::Null);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn positions_count_utf16_units_and_round_trip() {
        let t = "olá\nfoo 🌍 bar\n";
        // "🌍" vale 2 unidades UTF-16 mas é 1 caractere.
        let idx = t.chars().position(|c| c == 'b').unwrap();
        assert_eq!(position_of(t, idx), (1, 7));
        assert_eq!(char_index_of(t, 1, 7), idx);
        assert_eq!(position_of(t, 2), (0, 2));
        assert_eq!(char_index_of(t, 0, 99), 3, "além do fim da linha vai para o fim dela");
        for i in 0..=t.chars().count() {
            let (l, c) = position_of(t, i);
            assert_eq!(char_index_of(t, l, c), i, "índice {i}");
        }
    }

    #[test]
    fn uri_escapes_spaces_and_accents_and_backslashes() {
        assert_eq!(path_to_uri(Path::new(r"C:\Users\Sofia\Meu App\Ação.kt")), "file:///C:/Users/Sofia/Meu%20App/A%C3%A7%C3%A3o.kt");
        assert_eq!(path_to_uri(Path::new("/home/x/a.java")), "file:///home/x/a.java");
    }

    #[test]
    fn snippets_become_plain_text_with_a_cursor() {
        assert_eq!(snippet_to_plain("println($0)"), ("println()".into(), Some((8, 8))));
        assert_eq!(snippet_to_plain("for (${1:item} in ${2:list}) {\n\t$0\n}"), ("for (item in list) {\n\t\n}".into(), Some((5, 9))));
        assert_eq!(snippet_to_plain("custo \\$5"), ("custo $5".into(), None));
        assert_eq!(snippet_to_plain("sem nada"), ("sem nada".into(), None));
        assert_eq!(snippet_to_plain("a${1:b${2:c}}d"), ("abcd".into(), Some((1, 3))), "o tabstop externo cobre o aninhado");
        assert_eq!(snippet_to_plain("map { ${1:transform} }"), ("map { transform }".into(), Some((6, 15))));
        assert_eq!(snippet_to_plain("f($1, ${2:b})"), ("f(, b)".into(), Some((2, 2))), "o tabstop 1 vem antes do 2");
    }

    #[test]
    fn framing_round_trip_with_multibyte_body() {
        let msg = json!({"jsonrpc": "2.0", "method": "x", "params": {"t": "olá 🌍"}});
        let bytes = encode(&msg);
        let mut two = bytes.clone();
        two.extend(&bytes);
        let mut r = Cursor::new(two);
        assert_eq!(read_message(&mut r).unwrap(), Some(msg.clone()));
        assert_eq!(read_message(&mut r).unwrap(), Some(msg));
        assert_eq!(read_message(&mut r).unwrap(), None);
        assert!(read_message(&mut Cursor::new(b"Foo: 1\r\n\r\n{}".to_vec())).is_err());
    }

    #[test]
    fn parses_a_completion_list_with_text_edits_and_snippets() {
        let v = json!({"isIncomplete": false, "items": [
            {"label": "println(Object)", "kind": 2, "detail": "void", "sortText": "000", "filterText": "println",
             "insertTextFormat": 2, "textEdit": {"newText": "println(${1:x})$0", "range": {"start": {"line": 3, "character": 11}, "end": {"line": 3, "character": 12}}}},
            {"label": "out", "kind": 5, "sortText": "001"},
            {"label": "naoSemLabel"}
        ]});
        let items = parse_completion(&v);
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].label, "println(Object)");
        assert!(items[0].snippet);
        assert_eq!(items[0].range, Some(((3, 11), (3, 12))));
        assert_eq!(items[0].filter, "println");
        assert_eq!((items[1].insert.as_str(), items[1].kind_letter()), ("out", 'f'));
        assert_eq!(parse_completion(&json!([{"label": "a"}])).len(), 1);
        assert!(parse_completion(&Value::Null).is_empty());
        assert!(!is_incomplete(&v) && is_incomplete(&json!({"isIncomplete": true, "items": []})) && !is_incomplete(&json!([])));
    }

    #[test]
    fn items_come_sorted_by_sort_text_then_label() {
        let v = json!([{"label": "b", "sortText": "2"}, {"label": "z", "sortText": "1"}, {"label": "a", "sortText": "2"}]);
        let labels: Vec<_> = parse_completion(&v).into_iter().map(|i| i.label).collect();
        assert_eq!(labels, ["z", "a", "b"]);
    }

    /// Um servidor falso (um script que responde `initialize`) confere o ciclo inteiro do cliente:
    /// iniciar, pedir completion, receber a resposta e ver o erro quando o servidor morre.
    #[test]
    fn talks_to_a_fake_server_over_stdio() {
        if !cfg!(windows) {
            return; // o servidor falso usa PowerShell
        }
        let script = r#"
function Send($o) { $b = [System.Text.Encoding]::UTF8.GetBytes(($o | ConvertTo-Json -Depth 10 -Compress)); $h = [System.Text.Encoding]::ASCII.GetBytes("Content-Length: $($b.Length)`r`n`r`n"); $out = [Console]::OpenStandardOutput(); $out.Write($h,0,$h.Length); $out.Write($b,0,$b.Length); $out.Flush() }
$in = [Console]::OpenStandardInput()
function ReadMsg { $len = 0; while ($true) { $l = ""; while (($c = $in.ReadByte()) -ne 10) { if ($c -lt 0) { return $null }; if ($c -ne 13) { $l += [char]$c } }; if ($l -eq "") { break }; if ($l -like "Content-Length:*") { $len = [int]($l.Split(":")[1].Trim()) } }; $buf = New-Object byte[] $len; $r = 0; while ($r -lt $len) { $r += $in.Read($buf, $r, $len - $r) }; return ([System.Text.Encoding]::UTF8.GetString($buf) | ConvertFrom-Json) }
while ($m = ReadMsg) {
  if ($m.method -eq "initialize") { Send @{ jsonrpc = "2.0"; id = $m.id; result = @{ capabilities = @{} } } }
  elseif ($m.method -eq "textDocument/completion") { Send @{ jsonrpc = "2.0"; id = $m.id; result = @(@{ label = "falso"; kind = 2 }) } }
  elseif ($m.method -eq "shutdown") { Send @{ jsonrpc = "2.0"; id = $m.id; result = $null } }
  elseif ($m.method -eq "exit") { break }
}
"#;
        let path = std::env::temp_dir().join(format!("andsim-fake-lsp-{}.ps1", std::process::id()));
        std::fs::write(&path, script).unwrap();
        let mut cmd = Command::new("powershell");
        cmd.args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File"]).arg(&path);
        let client = Client::start(&mut cmd, &std::env::temp_dir(), json!({}), Duration::from_secs(30)).expect("initialize");
        client.did_open("file:///x.kt", "kotlin", 1, "val a = 1").unwrap();
        let items = parse_completion(&client.completion("file:///x.kt", 0, 3).recv_timeout(Duration::from_secs(10)).unwrap().unwrap());
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].label, "falso");
        client.shutdown();
        let after = client.completion("file:///x.kt", 0, 3).recv_timeout(Duration::from_secs(10)).unwrap();
        assert!(after.is_err(), "depois do exit a requisição precisa falhar, não ficar pendurada");
        let _ = std::fs::remove_file(path);
    }
}
