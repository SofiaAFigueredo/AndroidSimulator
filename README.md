# Android Simulator

Alternativa compacta ao Android Studio, escrita em Rust, com interface inspirada no VS Code.
O binário de release tem cerca de 6 MB.

## O que o app faz

O app não traz o emulador dentro dele nem usa um SDK já instalado. Ele lê o catálogo oficial
do Google (`dl.google.com/android/repository`), mostra as imagens de sistema com versão da API,
tipo, ABI e tamanho, e baixa só o que o usuário escolher para uma pasta própria
(`%LOCALAPPDATA%\AndroidSimulator\sdk` no Windows).

- Os termos de licença do Android SDK são mostrados e precisam ser aceitos antes do primeiro download.
- Cada arquivo baixado é conferido pelo SHA-1 do catálogo antes de ser extraído.
- O emulador oficial (~435 MB) e o `platform-tools` (~8 MB) são baixados uma vez; a imagem é a que o usuário escolhe.
- O emulador precisa de aceleração por hardware (WHPX/Hyper-V no Windows), que é do sistema.

## Estado atual (etapa 1)

- [x] Casca estilo VS Code: barra de atividades, barra lateral, abas, painel de saída, barra de status
- [x] Explorador de arquivos e editor de texto com abas (Ctrl+S salva)
- [x] Gerenciador de download do emulador com escolha da imagem
- [ ] Syntax highlight (Kotlin, Java, XML)
- [ ] Criar e iniciar AVDs, instalar APK
- [ ] Logcat e terminal
- [ ] Build via Gradle, com erros clicáveis
- [ ] Autocomplete via LSP

## Rodar

```bash
cargo run --release
cargo test
cargo test -- --ignored   # baixa o platform-tools de verdade
```
