# Telinha (app de PC)

App de compartilhar tela em Tauri v2 + Rust. Entra nas mesmas salas do site
`https://telinha-xp0y.onrender.com`, então quem não tem o app assiste pelo navegador.

- Vídeo: motor do Edge (WebView2) com aceleração de GPU.
- Áudio: nativo em Rust (WASAPI loopback por processo):
  - **Todo o som do PC**: exclui o próprio Telinha, então não volta eco.
  - **Só um app**: captura só o jogo/Spotify/etc.

## Requisitos

- Windows 10 2004+ ou Windows 11 (necessário pro áudio por app)
- Rust com toolchain MSVC (`rustup default stable-msvc`)
- Visual Studio Build Tools com "Desenvolvimento para desktop com C++"
- WebView2 (já vem no Windows 10/11 atualizado)

## Rodar em modo desenvolvimento

```powershell
cd src-tauri
cargo run
```

A primeira compilação demora alguns minutos.

## Pros amigos instalarem

Mande o link **https://telinha-xp0y.onrender.com/download**. Ele baixa sempre o instalador da versão mais nova.

1. Abrir o `Telinha_x.y.z_x64-setup.exe`.
2. Se aparecer "O Windows protegeu o computador", clicar em **Mais informações → Executar assim mesmo**
   (aparece porque o app não tem certificado pago de assinatura).
3. Next, next, pronto. Não pede administrador e cria atalho no Menu Iniciar.

Quando sair versão nova, aparece um botão verde **Atualizar** no topo do app. Ninguém precisa baixar de novo.

## Lançar versão nova

1. Subir o `version` em `src-tauri/tauri.conf.json` (ex.: `0.1.0` → `0.1.1`).
2. Commit e tag:

```powershell
git commit -am "v0.1.1"
git tag v0.1.1
git push; git push origin v0.1.1
```

O GitHub Actions (`.github/workflows/release.yml`) compila no Windows, assina a atualização e publica o
release com o instalador + `latest.json`. Demora uns 10 min na primeira vez e uns 4 nas próximas.

A chave que assina as atualizações fica no secret `TAURI_SIGNING_PRIVATE_KEY` do repositório.
**Guarde uma cópia do `telinha-updater.key`**: se perder, os apps instalados param de aceitar atualização.

## Gerar o instalador localmente

```powershell
npm install
$env:TAURI_SIGNING_PRIVATE_KEY = Get-Content ..\telinha-updater.key -Raw
$env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD = ""
npm run tauri build
```

Sai em `src-tauri\target\release\bundle\nsis\`.

## Estrutura

```
ui/                    interface (HTML/CSS/JS, sem build)
  app.js               salas, WebRTC, tela, painel de compartilhar
  native.js            ponte com o Rust (comandos + Channel de áudio)
  audio-worklet.js     toca o PCM nativo dentro do WebRTC
  reactions.js         reações + figurinhas
src-tauri/
  src/audio.rs         captura WASAPI, lista apps tocando som
  src/lib.rs           registro dos comandos
  tauri.conf.json      janela sem bordas, transparente, efeito acrílico
```
