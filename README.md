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
| Completa | a Compacta mais plataforma, build-tools, cmdline-tools, fontes do Android, NDK e CMake | 1,5 GB |

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
- [ ] Logcat e terminal
- [x] Build via Gradle, com erros do Kotlin, javac e aapt2 clicáveis no painel de saída
- [x] Executar: instala o APK no emulador e abre o app; Captura salva a tela do emulador
- [ ] Autocomplete via LSP

## Rodar

```bash
cargo run --release
cargo test
cargo test -- --ignored   # testes que usam a rede, o Gradle e o emulador (veja os comentários de cada um)
```
