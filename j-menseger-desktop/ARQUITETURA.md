# J-Menseger para computador — decisões técnicas

Este documento registra as escolhas de arquitetura do porte do aplicativo mobile
para uma aplicação desktop nativa, e o porquê de cada uma.

O produto é o mesmo: o celular e o computador são dois clientes do **mesmo**
aplicativo, com a mesma conta, o mesmo protocolo e os mesmos servidores.

---

## 1. Interface: Iced 0.14

| Candidato | Avaliação |
|---|---|
| **Iced 0.14** | **Escolhido.** Rust puro, renderização por `wgpu` (Vulkan/Metal/DX12) com retorno automático para software quando não há GPU. Publicado no crates.io com versões estáveis. Widgets customizáveis em Rust, sem linguagem de marcação intermediária. |
| GPUI (Zed) | Descartado. Não é publicado no crates.io como versão estável; só por dependência de git, com API volátil e suporte imaturo a Linux e Windows. Arriscado como base de um produto. |
| Slint | Viável, bom suporte a Windows. Descartado porque a interface é descrita em uma linguagem de marcação própria (`.slint`), o que divide a lógica entre Rust e um arquivo externo. Em uma tela de conversa com listas dinâmicas e widgets sob medida, manter tudo em Rust é mais direto. |
| egui | Descartado. Modo imediato é ótimo para ferramentas internas, mas entrega um acabamento aquém do exigido para um aplicativo de comunicação. |

**Validação feita, não presumida:** um protótipo com o layout de quatro colunas
(barra de navegação, barra lateral, área de mensagens, painel de membros) foi
compilado e executado de verdade neste contêiner, sob `Xvfb` com renderização por
software, e a janela foi capturada. Não há HTML, CSS, WebView, Electron ou
Chromium em nenhuma camada.

## 2. Protocolo: biblioteca `nostr` 0.45 (Rust)

O aplicativo mobile fala o protocolo aberto Nostr, usando as NIPs 17, 44 e 59
através da biblioteca JavaScript `nostr-tools`.

A versão desktop usa a biblioteca Rust `nostr` (projeto rust-nostr), que
implementa as **mesmas NIPs**. Isso não é uma dependência de conveniência: é o
que garante compatibilidade de fio entre os dois clientes. Reimplementar
NIP-44 v2 à mão seria reintroduzir risco criptográfico sem ganho nenhum.

- `nostr` — eventos, assinaturas Schnorr (BIP-340), NIP-06 (12 palavras),
  NIP-44 v2 (ChaCha20 + HMAC-SHA256), NIP-59 (gift wrap), NIP-19 (npub/nsec).
- `nostr-sdk` — conjunto de relays com reconexão, filtros e NIP-42 (AUTH).

A camada específica do aplicativo (os tipos de mensagem `t:` dentro do rumor de
kind 1400, o formato das chaves de conversa, o fatiamento de mídia) é portada
diretamente do motor em JavaScript e descrita em `PROTOCOLO.md`.

## 3. Armazenamento: SQLite

O celular usa IndexedDB. No computador, SQLite (`rusqlite`, compilado junto) com
o mesmo modelo de dados: `contacts`, `groups`, `chats`, `msgs`, `media`,
`chunks`, `calls`, `outbox`, `sounds`, `kv`.

Vantagens sobre um arquivo solto: consultas indexadas por conversa e data,
transações, e histórico grande sem carregar tudo na memória.

A chave privada **não** fica no banco: vai no cofre de credenciais do sistema
operacional (Credential Manager no Windows, Secret Service no Linux, Keychain no
macOS), espelhando o uso do Android Keystore no celular.

## 4. Separação em módulos

```
j-menseger-desktop/
  crates/
    jm-core/    protocolo, relays, identidade, armazenamento, estado
    jm-app/     interface nativa (Iced), navegação, telas
    jm-media/   áudio, vídeo e captura de tela (a seguir)
```

`jm-core` não conhece a interface e não depende do Iced: roda sem janela, o que
permite testá-lo com clientes reais em linha de comando, exatamente como o motor
em JavaScript é testado hoje contra um relay local.

## 5. Concorrência

- Uma thread de interface, que nunca faz trabalho pesado.
- Um runtime Tokio para rede, relays e banco.
- Threads dedicadas para captura de áudio, captura de tela e codificação.
- A comunicação entre o núcleo e a interface é por canal de eventos, não por
  bloqueio: a interface recebe atualizações e redesenha.

Uma transmissão de tela a 60 FPS não pode travar a digitação de uma mensagem.

## 6. Voz, vídeo e transmissão de tela

| Camada | Escolha | Observação |
|---|---|---|
| Transporte | `webrtc` (webrtc-rs) | Rust puro, DTLS-SRTP, canais de dados. É o mesmo transporte que o celular usa, o que mantém a chamada compatível entre PC e celular. |
| Áudio de entrada/saída | `cpal` | Enumeração e captura de dispositivos em Windows (WASAPI), Linux (ALSA) e macOS (CoreAudio). |
| Codec de áudio | `opus` | Mesmo codec do celular. |
| Captura de tela | `xcap` + APIs nativas | Monitor e janela; Windows Graphics Capture no Windows, PipeWire/X11 no Linux. |
| Codec de vídeo | H.264 por hardware quando houver, software como alternativa | NVENC, AMF e Quick Sync quando disponíveis. |

**Limitação deste ambiente, declarada desde já:** este contêiner não tem GPU,
placa de som, câmera nem tela física. Posso escrever e compilar essas camadas e
testar a lógica, mas **não posso validar aqui** qualidade de imagem, FPS reais,
latência de áudio ou codificação por hardware. Isso exige rodar em máquinas de
verdade, e será declarado como não testado até que isso aconteça.

## 7. O que não é aceitável neste projeto

- Botão que não faz o que diz.
- Dados de exemplo fingindo ser conversa real.
- Recurso declarado pronto sem ter sido executado.
- Um segundo sistema de contas, separado do celular.
