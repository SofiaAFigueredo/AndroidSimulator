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
app, mostrando a saída no painel inferior. Verificado de ponta a ponta: o projeto novo compila em ~1 min
(depois de baixar o plugin e as dependências) e gera um APK de ~855 KB.
`cargo test builds_a_new_project -- --ignored --nocapture`

O app grava o `package.xml` de cada pacote instalado, como o sdkmanager faz; sem ele o AGP não reconhece
a plataforma como instalada.

## Imagem mínima testada

`system-images;android-10;default;x86` (72 MB) inicia no emulador 37.2 com WHPX e chega a
`sys.boot_completed=1` em cerca de 8 s (Windows 11, sem janela). Teste:
`ANDSIM_IMAGES="system-images;android-10;default;x86" cargo test boots_images -- --ignored --nocapture`.
O API 10 é antigo; apps modernos exigem uma imagem mais nova.

`system-images;android-30;google_atd;x86_64` (614 MB, ATD: sem tela inicial nem apps do sistema)
também inicia e reporta `ro.build.version.sdk=30`, mas o primeiro boot a frio, sem janela e sem
snapshot, levou cerca de 175 s. A menor imagem API 30+ do catálogo é a `android-30;google_atd;x86`
(476 MB), ainda não testada.

## Estado atual

- [x] Casca estilo VS Code: barra de atividades, barra lateral, abas, painel de saída, barra de status
- [x] Explorador de arquivos e editor de texto com abas (Ctrl+S salva)
- [x] Gerenciador de download do emulador com escolha da imagem
- [x] Syntax highlight (Kotlin, Java, XML) e números de linha
- [x] Criar, iniciar e parar AVDs (instalar APK ainda não)
- [ ] Logcat e terminal
- [x] Build via Gradle (saída no painel; erros clicáveis ainda não)
- [ ] Instalar o APK no emulador e abrir o app
- [ ] Autocomplete via LSP

## Rodar

```bash
cargo run --release
cargo test
cargo test -- --ignored   # baixa o platform-tools de verdade
```
