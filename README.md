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

## Perfis de instalação

| Perfil | O que baixa | Tamanho (catálogo de out/2026) |
|---|---|---|
| Compacta | emulador, adb e a imagem escolhida: o necessário para **rodar** apps | 514 MB com a imagem de 72 MB |
| Completa | a Compacta mais plataforma, build-tools, cmdline-tools, fontes do Android, NDK, CMake, JDK, Gradle e os servidores de autocomplete | cerca de 2,0 GB |

Sempre a versão estável mais recente de cada ferramenta. A Completa também baixa o JDK (Eclipse
Temurin 21, da Adoptium) e o Gradle 9.8.2, com o SHA-256 conferido, para compilar apps.

## Criar e compilar um projeto

No Explorador, **Novo projeto** gera um app Kotlin (plugin do Android para Gradle 9.4.1, Kotlin embutido,
sem AndroidX) e **Compilar** roda `gradle assembleDebug` com o JDK, o SDK e o cache do Gradle do próprio
app, mostrando a saída no painel inferior. **Executar** instala o APK no primeiro emulador em
execução e abre o app. Verificado de ponta a ponta: o projeto novo compila em ~1 min (depois de baixar
o plugin e as dependências), gera um APK de ~855 KB e aparece na tela do emulador.
`cargo test builds_a_new_project -- --ignored --nocapture`

O app grava o `package.xml` de cada pacote instalado, como o sdkmanager faz; sem ele o AGP não reconhece
a plataforma como instalada.

## Qual imagem escolher

Medido neste computador (Windows 11, WHPX), emulador 37.2:

| Imagem | Download | Boot | Mostra a tela? |
|---|---|---|---|
| `android-10;default;x86` | 72 MB | ~8 s | sim, mas o app do modelo exige API 24+ |
| `android-24;default;x86` | 299 MB | ~14 s | **sim; é a recomendada** (menor que roda o app do modelo) |
| `android-30;google_atd;x86_64` | 614 MB | 30 a 175 s | **não**: as imagens ATD rodam o app, mas não desenham a interface (a captura sai preta) |

O app marca como "recomendada" a menor imagem com API 24 ou mais que não seja ATD.

## Espaço em disco

As imagens vêm compactadas: baixar 614 MB vira 3 GB, e a API 10 gera um `userdata.img` de 2 GB quase
todo de zeros. O app extrai arquivos grandes como **esparsos** (os blocos de zeros não ocupam disco) e o
botão **Compactar imagens** faz o mesmo nas já instaladas. Medido nas três imagens acima:
`system-images` foi de **6,4 GB para 2,2 GB** em disco, e elas continuam iniciando.

## Estado atual

- [x] Casca estilo VS Code: barra de atividades, barra lateral, abas, painel de saída, barra de status
- [x] Explorador de arquivos e editor de texto com abas (Ctrl+S salva)
- [x] Gerenciador de download do emulador com escolha da imagem
- [x] Syntax highlight (Kotlin, Java, XML) e números de linha
- [x] Criar, iniciar, parar e excluir AVDs
- [x] Logcat (filtro por nível e texto) e terminal integrado, em abas no painel inferior
- [x] Build via Gradle, com erros do Kotlin, javac e aapt2 clicáveis no painel de saída
- [x] Executar: instala o APK no emulador e abre o app; Captura salva a tela do emulador
- [x] Simulador: tela ao vivo no painel, mouse como dedo, teclado e botões de hardware
- [x] Autocomplete via LSP para Java (jdtls) e Kotlin (kotlin-language-server), com classes do Android

## Logcat e terminal

O painel inferior tem três abas: **Saída** (downloads, Gradle, emulador), **Logcat** e **Terminal**.

- **Logcat:** lê `adb logcat -v threadtime` do primeiro emulador em execução, com filtro por nível e por
  texto (tag, pacote, mensagem) e até 20 mil linhas em memória. Verificado no emulador: milhares de linhas
  interpretadas e uma linha escrita pelo teste chegou com a tag e o nível certos.
- **Terminal:** cada linha digitada roda no `cmd` (ou `sh`) na pasta do projeto, com `adb`, `gradle` e
  `java` já no PATH e `JAVA_HOME` apontando para o JDK do app. Tem `cd`, `clear`, histórico (↑/↓) e o botão
  Parar. Não é um PTY: programas interativos (vim, REPLs) não funcionam. Não usa nenhuma biblioteca de
  terminal, para o binário continuar pequeno.

## Simulador (a tela do app dentro do app)

O emulador roda **sem janela** e a tela dele aparece num painel à direita do app (ícone do celular na barra
de atividades, ou automaticamente ao iniciar um emulador e ao usar **Executar**). Dá para usar o app como num
celular:

- **Mouse** é o dedo: clicar toca, arrastar arrasta (em tempo real) e a roda do mouse rola a tela.
- **Teclado**: depois de clicar no painel, o texto digitado e Enter, Backspace, Delete, Tab, setas e Esc
  (Voltar) vão para o app.
- **Botões**: Voltar, Início, Recentes, Energia, Vol + e Vol -.
- Marque **Emulador em janela separada** na aba Dispositivos para usar a janela do próprio emulador.

Como funciona (sem biblioteca de vídeo nem gRPC, para o binário continuar pequeno): um único processo
`screencap` no dispositivo transmite os quadros crus (cerca de **8 a 9 quadros por segundo** medidos num
emulador 480x800 sem placa de vídeo), e o toque é gravado direto no dispositivo de entrada multitoque do
emulador com `sendevent`, o que permite arrastar de verdade, e não só tocar.

Verificado de ponta a ponta, num app compilado pelo próprio app: um clique de mouse no painel contou 1 no
botão do app, arrastar para fora do botão não contou, e o texto digitado chegou ao campo e foi apagado.
O app de exemplo gerado tem um botão contador e um campo de texto justamente para isso.

Limitações: a tela não gira (o toque é gravado nas coordenadas do painel em pé); não há multitoque
(pinça) nem sensores; a taxa de quadros depende do emulador (sem placa de vídeo é a do `screencap`);
as imagens ATD não desenham a interface e o painel mostra preto.

## Autocomplete (LSP)

O editor sugere código em Java e Kotlin falando LSP com servidores que o app baixa na Completa
(só quando há um arquivo da linguagem aberto): **Ctrl+Espaço** ou digitar `.` abre a lista; ↑/↓ escolhem,
**Enter/Tab** aceitam e **Esc** fecha. Se a sugestão for um snippet (`map { transform }`), o espaço
reservado fica selecionado para a próxima digitação substituí-lo.

| Linguagem | Servidor | Download | Observação |
|---|---|---|---|
| Java | Eclipse JDT LS 1.61.0 (EPL-2.0) | 49 MB | a 1.62+ exige Java 25 e o JDK do app é o 21; SHA-256 conferido |
| Kotlin | kotlin-language-server 1.3.13 (MIT) | 83 MB | o GitHub não publica hash; o SHA-256 fica fixado no código |

Verificado com os servidores reais, incluindo o editor dirigido por eventos de teclado simulados:
`System.` → `out`, `lista.ma` → `map`, e `import android.os.Bu` → `Bundle` nas duas linguagens.

Limitações: o servidor Kotlin devolve no máximo 75 itens (o app pede de novo conforme se digita); logo
depois de abrir um arquivo a primeira resposta pode vir vazia, enquanto o servidor analisa (o app avisa e
basta repetir o atalho); o import do Gradle é desligado, então o classpath é a `android.jar` mais a
biblioteca padrão e não as dependências do `build.gradle`; cada servidor é um processo Java (o jdtls
reserva até 1 GB de memória). Não há diagnósticos (sublinhado de erros) nem ir-para-definição.

## Rodar

```bash
cargo run --release
cargo test
cargo test -- --ignored   # testes que usam a rede, o Gradle e o emulador (veja os comentários de cada um)
```
