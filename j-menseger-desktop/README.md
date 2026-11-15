# J-Menseger para computador

Porte do aplicativo J-Menseger de celular para uma aplicação **nativa de
computador**, escrita em Rust. O celular e o PC são dois clientes do mesmo
aplicativo: mesma conta, mesmo protocolo, mesmos servidores.

Não há HTML, CSS, WebView, Electron, Tauri nem Chromium em nenhuma camada. A
interface é desenhada pela GPU (ou por software, quando não há GPU).

- **Decisões técnicas e porquês:** [`ARQUITETURA.md`](ARQUITETURA.md)
- **Protocolo e compatibilidade com o celular:** testes em
  `crates/jm-core/tests/compatibilidade_com_o_mobile.rs`

## Situação de cada parte

| Área | Situação | Como foi verificado |
|---|---|---|
| Conta e 12 palavras | **Pronto** | Testes comparam o Rust contra vetores das mesmas bibliotecas JavaScript do celular: mesma chave, mesmo `npub`, mesmo `nsec` |
| Mensagens | **Pronto** | Dois aplicativos conversando por servidor real, com recibo de entrega e leitura |
| Pedidos de conversa | **Pronto** | Aceitar e bloquear; só contato aceito pode ligar |
| Grupos e canais | **Pronto** | Convite com a chave do grupo, canais de texto, membros, entrada e saída |
| Chamadas de voz | **Pronto** (sem hardware de áudio aqui) | Conexão WebRTC entre dois aplicativos; áudio Opus atravessou a conexão nos testes |
| Transmissão de tela | **Pronto** | A tela de um computador apareceu dentro do outro |
| Executável do Windows | **Compilado, não executado** | Gerado aqui por compilação cruzada; conferidos o formato, o ícone embutido e as DLLs exigidas. Falta rodar num Windows de verdade |
| Instalador do Windows | **Gerado, não executado** | `makensis` produziu o instalador; instalar e desinstalar só se testa no Windows |
| Câmera | **Não implementado** | Não havia câmera para testar |
| Codificação por hardware | **Não implementado** | Só software (VP8 por libvpx). NVENC, AMF e Quick Sync não existem no código |
| Mídia em conversas (fotos, áudios, arquivos) | **Não implementado** | O protocolo já prevê; falta a interface |
| Salas de voz de grupo | **Não implementado** | Aparecem marcadas como "em breve", sem botão falso |
| Notificações do sistema e ícone na bandeja | **Não implementado** | — |
| Tela de configurações | **Não implementado** | Escolha de microfone e alto-falante ainda usa o padrão do sistema |

Nada nesta tabela é marcado como pronto sem ter sido executado.

## Como está organizado

```
crates/
  jm-core/     protocolo cifrado, servidores, identidade, banco, motor
  jm-media/    áudio (Opus), vídeo (VP8), conexão WebRTC, captura de tela
  jm-app/      interface nativa em Iced e gerenciador de chamadas
tools/
  testrelay.mjs   servidor Nostr local para testes
```

`jm-core` e `jm-media` não conhecem a interface e rodam sem janela, o que
permite testá-los de verdade em linha de comando.

## Compilar e rodar no Linux

Dependências do sistema (Debian/Ubuntu):

```bash
sudo apt install build-essential pkg-config \
  libasound2-dev libopus-dev libvpx-dev \
  libxkbcommon-dev libxkbcommon-x11-dev libwayland-dev libvulkan-dev \
  libgl1-mesa-dev libgbm-dev libegl1-mesa-dev libxcb1-dev \
  libpipewire-0.3-dev libdbus-1-dev libfontconfig1-dev libclang-dev
```

Depois:

```bash
cargo run --release -p jm-app
```

Os dados ficam em `~/.local/share/J-Menseger/jmenseger.db`.

### Testar dois clientes na mesma máquina

```bash
cd tools && npm install && node testrelay.mjs 7781 &

J_MENSEGER_DATA=/tmp/ana/jm.db  J_MENSEGER_RELAYS=ws://127.0.0.1:7781 cargo run -p jm-app &
J_MENSEGER_DATA=/tmp/beto/jm.db J_MENSEGER_RELAYS=ws://127.0.0.1:7781 cargo run -p jm-app &
```

| Variável | Para quê |
|---|---|
| `J_MENSEGER_DATA` | Caminho do banco local |
| `J_MENSEGER_RELAYS` | Servidores, separados por vírgula |
| `RUST_LOG` | Nível dos registros, ex.: `RUST_LOG=jm_core=debug` |

## Gerar o programa para Windows

O aplicativo é Rust com duas dependências em C: **libvpx** (vídeo) e **libopus**
(voz). Tudo o mais usa bibliotecas que já vêm no Windows, então o executável
final **não acompanha nenhuma DLL**.

### A partir do Linux (é como os arquivos entregues foram gerados)

```bash
sudo apt install gcc-mingw-w64-x86-64 binutils-mingw-w64-x86-64 nasm nsis
rustup target add x86_64-pc-windows-gnu

# Compila libvpx e libopus na versão para Windows (uma vez só).
packaging/windows/bibliotecas-c.sh

export PKG_CONFIG_ALLOW_CROSS=1
export PKG_CONFIG_LIBDIR="$PWD/target/win-c/lib/pkgconfig"
cargo build --release -p jm-app --target x86_64-pc-windows-gnu

# Instalador
makensis packaging/windows/j-menseger.nsi
```

Saem dois arquivos:

| Arquivo | O que é |
|---|---|
| `target/x86_64-pc-windows-gnu/release/j-menseger.exe` | O programa, sozinho. Roda sem instalar |
| `target/J-Menseger-0.1.0-instalador.exe` | Instalador, com atalho, ícone e desinstalador |

**O que foi conferido aqui:** o `.exe` é um PE64 de interface gráfica (não abre
console), tem o ícone e os dados do programa embutidos, e só depende de DLLs do
próprio Windows (`d3d11`, `mmdevapi`, `combase`, `ws2_32`…). **O que não foi:**
rodar. Não há Windows neste ambiente, então a janela, o som e a captura de tela
ainda não foram vistos funcionando lá.

### No próprio Windows

```powershell
# Rust + Visual Studio Build Tools ("Desenvolvimento para desktop com C++")
vcpkg install opus:x64-windows-static-md libvpx:x64-windows-static-md
$env:VCPKG_ROOT = "C:\vcpkg"
cargo build --release -p jm-app
```

No Windows a captura de tela usa Windows Graphics Capture e o áudio usa WASAPI,
pelas mesmas bibliotecas usadas no Linux — não há código diferente na aplicação.

### Onde ficam os dados

`%APPDATA%\J-Menseger\jmenseger.db`. O desinstalador **não apaga** essa pasta:
sem as 12 palavras, uma conta apagada não volta. Na versão final do Windows não
há console, então os registros vão para `jmenseger.log`, ao lado do banco.

## Instalar no Linux

```bash
cargo build --release -p jm-app
packaging/linux/instalar.sh      # instala em ~/.local
```

## Testes

```bash
cargo test --workspace
```

São 108 testes. Os que dependem de rede sobem o servidor local de
`tools/` automaticamente (rode `npm install` lá antes). Os de tela e de
chamada são pulados com aviso quando não há tela ou rede utilizável, em vez de
falharem por algo que não é defeito.

O que os testes cobrem, resumido:

- **Compatibilidade com o celular:** as mesmas 12 palavras geram a mesma conta;
  um envelope cifrado montado pelo celular abre no computador.
- **Protocolo:** envelope de três camadas, horários embaralhados, recusa de
  tentativa de falsificar autoria.
- **Mensagens:** conversa completa entre dois clientes com servidor real,
  contato bloqueado, conta restaurada trazendo o histórico de volta.
- **Grupos:** convite, entrada, conversa no canal, saída, e que quem não tem a
  chave não lê nada.
- **Áudio:** compressão e descompressão, silêncio custando pouco, pacote
  perdido reconstruído, detecção de fala.
- **Chamada:** dois pares conectam e o áudio atravessa a conexão cifrada.
- **Tela:** 60 capturas seguidas sem esgotar o sistema, compressão real, e os
  quadros recebidos virando imagem do tamanho certo.

### Além dos testes automáticos

Há coisas que teste nenhum pega, porque dependem de ver a tela. A transmissão
foi conferida com dois aplicativos de verdade rodando lado a lado, um
transmitindo e o outro recebendo. Medindo a área da imagem recebida a 83
amostras por segundo, ela mudava **cerca de 14 vezes por segundo** — e isto
com os dois aplicativos, o codificador e o decodificador dividindo a mesma
máquina, sem GPU. Num computador comum, com placa de vídeo, a conta é outra;
o número honesto aqui é o do pior caso.

## Privacidade

- As conversas são cifradas de ponta a ponta (NIP-17, NIP-44 v2, NIP-59). Os
  servidores só veem um envelope fechado endereçado a uma chave pública: não
  sabem quem enviou, o que diz, nem o horário real.
- O áudio, o vídeo e a tela transmitida vão **direto entre os computadores**,
  cifrados com DTLS-SRTP. Os servidores não participam disso.
- A chave da conta é criada no aparelho. Não há telefone, e-mail nem cadastro.
- "Digitando…", sinalização de chamada e presença usam envelopes efêmeros, que
  os servidores repassam e não guardam.

## Licença

MIT.
