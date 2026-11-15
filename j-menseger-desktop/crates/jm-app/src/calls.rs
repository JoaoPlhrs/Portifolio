//! Chamadas de voz: amarra a sinalização do motor com a conexão direta e o
//! áudio do computador.
//!
//! O caminho de uma chamada é este:
//!
//! 1. Quem liga monta a conexão e manda a proposta por envelope efêmero.
//! 2. Quem recebe vê a tela de chamada e, ao atender, devolve a resposta.
//! 3. A partir daí o áudio vai **direto** entre os dois computadores.
//!
//! O microfone e o alto-falante são abertos só enquanto a chamada dura.

use jm_core::engine::Engine;
use jm_core::messages::{CallAction, CallSignal};
use jm_media::audio::{Capture, Playback};
use jm_media::codec::{AudioQuality, Decoder, Encoder, VoiceDetector};
use jm_media::peer::{CallConfig, ConnectionState, MediaKind, Peer, PeerEvent};
use jm_media::screen::{self, Capturer, Fps, Quality, Source};
use jm_media::video::{DecodedFrame, VideoDecoder, VideoEncoder};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

/// Quanto tempo o telefone toca antes de desistir.
const RING_TIMEOUT: Duration = Duration::from_secs(45);

/// Em que ponto a chamada está.
#[derive(Debug, Clone, PartialEq)]
pub enum CallState {
    /// Chamando, esperando a outra pessoa atender.
    Calling,
    /// Alguém está ligando para você.
    Ringing { video: bool },
    /// Atendeu, montando a conexão.
    Connecting,
    /// Falando.
    Active { since: std::time::Instant },
    /// Caiu e está tentando voltar.
    Reconnecting,
    /// Acabou.
    Ended { reason: EndReason },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndReason {
    /// Alguém desligou.
    HungUp,
    /// A outra pessoa recusou.
    Declined,
    /// Ninguém atendeu.
    NoAnswer,
    /// A outra pessoa já estava em outra chamada.
    Busy,
    /// A conexão não fechou.
    Failed,
}

impl EndReason {
    pub fn label(&self) -> &'static str {
        match self {
            EndReason::HungUp => "Chamada encerrada",
            EndReason::Declined => "Chamada recusada",
            EndReason::NoAnswer => "Não atendeu",
            EndReason::Busy => "A pessoa está em outra chamada",
            EndReason::Failed => "Não foi possível conectar. Em redes muito fechadas pode ser preciso um servidor TURN.",
        }
    }
}

/// O que a interface precisa saber sobre a chamada.
#[derive(Debug, Clone)]
pub struct CallInfo {
    /// Chave pública de quem está do outro lado.
    pub peer: String,
    pub state: CallState,
    pub muted: bool,
    /// Volume do microfone agora, de 0 a 1.
    pub mic_level: f32,
    /// A outra pessoa está falando.
    pub remote_speaking: bool,
    /// Entrou na chamada sem microfone.
    pub sem_microfone: bool,
    /// O que você está transmitindo agora, se estiver.
    pub sharing: Option<String>,
    /// Está chegando vídeo do outro lado.
    pub receiving_video: bool,
    /// Número do último quadro recebido; muda quando há imagem nova.
    pub frame_version: u64,
}

/// Uma chamada em andamento.
struct Active {
    peer_pk: String,
    cid: String,
    /// Quem começou a chamada.
    outgoing: bool,
    video: bool,
    conn: Arc<Peer>,
    capture: Option<Capture>,
    playback: Option<Playback>,
    muted: Arc<AtomicBool>,
    state: CallState,
    /// Proposta recebida, guardada até a pessoa atender.
    pending_offer: Option<String>,
    remote_speaking: Arc<AtomicBool>,
    receiving_video: Arc<AtomicBool>,
    /// Este computador não tem microfone utilizável: a pessoa ouve, mas não
    /// é ouvida. Precisa aparecer na tela — num PC de mesa isso é comum.
    sem_microfone: bool,
    /// Última imagem recebida, com um número que muda a cada quadro novo.
    /// A interface só copia a imagem quando o número muda, para não arrastar
    /// megabytes a cada redesenho.
    incoming_frame: Arc<Mutex<Option<(u64, DecodedFrame)>>>,
    /// Nome do que está sendo transmitido, e o sinal para parar.
    sharing: Option<(String, Arc<AtomicBool>)>,
}

/// Cuida da chamada atual. Só existe uma por vez.
pub struct CallManager {
    engine: Arc<Engine>,
    config: CallConfig,
    active: Arc<Mutex<Option<Active>>>,
    /// Como a última chamada terminou, e quando.
    ///
    /// Os recursos (microfone, alto-falante, conexão) são soltos na hora em
    /// que a chamada acaba, mas o painel some junto e a pessoa fica sem saber
    /// se recusaram, se ninguém atendeu ou se a conexão falhou. Isto guarda
    /// só o aviso, pelo tempo de ser lido.
    ending: Arc<Mutex<Option<(String, EndReason, std::time::Instant)>>>,
}

/// Quanto tempo o aviso de chamada encerrada fica na tela.
const AVISO_DE_FIM: Duration = Duration::from_secs(5);

impl CallManager {
    pub fn new(engine: Arc<Engine>, config: CallConfig) -> Arc<Self> {
        Arc::new(Self {
            engine,
            config,
            active: Arc::new(Mutex::new(None)),
            ending: Arc::new(Mutex::new(None)),
        })
    }

    /// Resumo da chamada atual, para desenhar a tela.
    pub async fn info(&self) -> Option<CallInfo> {
        if self.active.lock().await.is_none() {
            let mut fim = self.ending.lock().await;
            let (peer, reason, quando) = fim.as_ref()?.clone();
            if quando.elapsed() > AVISO_DE_FIM {
                *fim = None;
                return None;
            }
            return Some(CallInfo {
                peer,
                state: CallState::Ended { reason },
                muted: false,
                mic_level: 0.0,
                remote_speaking: false,
                sem_microfone: false,
                sharing: None,
                receiving_video: false,
                frame_version: 0,
            });
        }

        // A imagem fica atrás do próprio travamento; pegamos o número dela
        // antes de montar o resumo, para não segurar dois ao mesmo tempo.
        let quadro = {
            let guard = self.active.lock().await;
            Arc::clone(&guard.as_ref()?.incoming_frame)
        };
        let versao = quadro.lock().await.as_ref().map(|(v, _)| *v).unwrap_or(0);

        let guard = self.active.lock().await;
        let a = guard.as_ref()?;
        Some(CallInfo {
            peer: a.peer_pk.clone(),
            state: a.state.clone(),
            muted: a.muted.load(Ordering::Relaxed),
            mic_level: a.capture.as_ref().map(|c| c.level()).unwrap_or(0.0),
            remote_speaking: a.remote_speaking.load(Ordering::Relaxed),
            sem_microfone: a.sem_microfone,
            sharing: a.sharing.as_ref().map(|(n, _)| n.clone()),
            receiving_video: a.receiving_video.load(Ordering::Relaxed),
            frame_version: versao,
        })
    }

    /// Última imagem recebida, se for mais nova que a `desde`.
    pub async fn take_frame(&self, desde: u64) -> Option<(u64, DecodedFrame)> {
        let destino = {
            let g = self.active.lock().await;
            Arc::clone(&g.as_ref()?.incoming_frame)
        };
        let slot = destino.lock().await;
        match slot.as_ref() {
            Some((v, img)) if *v > desde => Some((*v, img.clone())),
            _ => None,
        }
    }

    pub async fn is_busy(&self) -> bool {
        self.active.lock().await.is_some()
    }

    /// Liga para alguém.
    pub async fn start(self: &Arc<Self>, peer_pk: &str, video: bool) -> Result<(), EndReason> {
        *self.ending.lock().await = None;
        if self.is_busy().await {
            return Err(EndReason::Busy);
        }
        let cid = format!("{:x}", jm_core::engine::now_ms());
        let (conn, eventos) = Peer::new(self.config.clone())
            .await
            .map_err(|_| EndReason::Failed)?;
        let conn = Arc::new(conn);

        let oferta = conn.create_offer().await.map_err(|_| EndReason::Failed)?;

        let mudo = Arc::new(AtomicBool::new(false));
        let falando = Arc::new(AtomicBool::new(false));
        {
            let mut guard = self.active.lock().await;
            *guard = Some(Active {
                peer_pk: peer_pk.to_string(),
                cid: cid.clone(),
                outgoing: true,
                video,
                conn: Arc::clone(&conn),
                capture: None,
                playback: None,
                muted: Arc::clone(&mudo),
                state: CallState::Calling,
                pending_offer: None,
                remote_speaking: Arc::clone(&falando),
                receiving_video: Arc::new(AtomicBool::new(false)),
                sem_microfone: false,
                incoming_frame: Arc::new(Mutex::new(None)),
                sharing: None,
            });
        }

        self.pump(eventos);
        let _ = self
            .engine
            .send_call_signal(
                peer_pk,
                CallSignal {
                    cid: cid.clone(),
                    a: CallAction::Offer,
                    sdp: Some(oferta),
                    v: Some(video as u8),
                },
            )
            .await;

        // Se ninguém atender, a chamada se encerra sozinha e vira "perdida".
        let eu = Arc::clone(self);
        let alvo = peer_pk.to_string();
        tokio::spawn(async move {
            tokio::time::sleep(RING_TIMEOUT).await;
            let ainda_chamando = {
                let g = eu.active.lock().await;
                matches!(g.as_ref().map(|a| &a.state), Some(CallState::Calling))
            };
            if ainda_chamando {
                let _ = eu.engine.report_missed_call(&alvo, video).await;
                eu.end(EndReason::NoAnswer).await;
            }
        });
        Ok(())
    }

    /// Trata a sinalização que chega de alguém.
    pub async fn on_signal(self: &Arc<Self>, from: &str, sig: CallSignal) {
        match sig.a {
            CallAction::Offer => self.on_offer(from, sig).await,
            CallAction::Answer => {
                let guard = self.active.lock().await;
                if let Some(a) = guard.as_ref() {
                    if a.peer_pk == from && a.cid == sig.cid {
                        if let Some(sdp) = &sig.sdp {
                            let _ = a.conn.accept_answer(sdp).await;
                        }
                    }
                }
            }
            CallAction::Decline => {
                if self.peer_matches(from).await {
                    self.end(EndReason::Declined).await;
                }
            }
            CallAction::Busy => {
                if self.peer_matches(from).await {
                    self.end(EndReason::Busy).await;
                }
            }
            CallAction::End => {
                if self.peer_matches(from).await {
                    self.end(EndReason::HungUp).await;
                }
            }
            CallAction::Missed => {}
        }
    }

    async fn on_offer(self: &Arc<Self>, from: &str, sig: CallSignal) {
        let Some(sdp) = sig.sdp.clone() else { return };

        // Já em outra chamada: avisa quem está ligando em vez de ignorar.
        {
            let guard = self.active.lock().await;
            if let Some(a) = guard.as_ref() {
                // A mesma proposta reenviada não é uma chamada nova.
                if a.peer_pk == from && a.cid == sig.cid {
                    return;
                }
                drop(guard);
                let _ = self
                    .engine
                    .send_call_signal(
                        from,
                        CallSignal { cid: sig.cid, a: CallAction::Busy, sdp: None, v: None },
                    )
                    .await;
                return;
            }
        }

        let video = sig.v.unwrap_or(0) == 1;
        let Ok((conn, eventos)) = Peer::new(self.config.clone()).await else {
            return;
        };
        let conn = Arc::new(conn);
        {
            let mut guard = self.active.lock().await;
            *guard = Some(Active {
                peer_pk: from.to_string(),
                cid: sig.cid,
                outgoing: false,
                video,
                conn,
                capture: None,
                playback: None,
                muted: Arc::new(AtomicBool::new(false)),
                state: CallState::Ringing { video },
                pending_offer: Some(sdp),
                remote_speaking: Arc::new(AtomicBool::new(false)),
                receiving_video: Arc::new(AtomicBool::new(false)),
                sem_microfone: false,
                incoming_frame: Arc::new(Mutex::new(None)),
                sharing: None,
            });
        }
        self.pump(eventos);
    }

    /// Atende a chamada que está tocando.
    pub async fn accept(self: &Arc<Self>) -> Result<(), EndReason> {
        let (conn, sdp, peer_pk, cid) = {
            let mut guard = self.active.lock().await;
            let a = guard.as_mut().ok_or(EndReason::HungUp)?;
            let sdp = a.pending_offer.take().ok_or(EndReason::Failed)?;
            a.state = CallState::Connecting;
            (
                Arc::clone(&a.conn),
                sdp,
                a.peer_pk.clone(),
                a.cid.clone(),
            )
        };

        let resposta = conn.accept_offer(&sdp).await.map_err(|_| EndReason::Failed)?;
        let _ = self
            .engine
            .send_call_signal(
                &peer_pk,
                CallSignal { cid, a: CallAction::Answer, sdp: Some(resposta), v: None },
            )
            .await;
        Ok(())
    }

    /// Recusa a chamada que está tocando.
    pub async fn decline(self: &Arc<Self>) {
        let alvo = {
            let g = self.active.lock().await;
            g.as_ref().map(|a| (a.peer_pk.clone(), a.cid.clone()))
        };
        if let Some((pk, cid)) = alvo {
            let _ = self
                .engine
                .send_call_signal(
                    &pk,
                    CallSignal { cid, a: CallAction::Decline, sdp: None, v: None },
                )
                .await;
        }
        self.end(EndReason::Declined).await;
    }

    /// Desliga.
    pub async fn hang_up(self: &Arc<Self>) {
        let alvo = {
            let g = self.active.lock().await;
            g.as_ref().map(|a| (a.peer_pk.clone(), a.cid.clone(), a.outgoing, a.state.clone(), a.video))
        };
        if let Some((pk, cid, saindo, estado, video)) = alvo {
            let _ = self
                .engine
                .send_call_signal(
                    &pk,
                    CallSignal { cid, a: CallAction::End, sdp: None, v: None },
                )
                .await;
            // Desligar antes de a pessoa atender é uma chamada perdida para ela.
            if saindo && estado == CallState::Calling {
                let _ = self.engine.report_missed_call(&pk, video).await;
            }
        }
        self.end(EndReason::HungUp).await;
    }

    /// Liga e desliga o microfone.
    pub async fn set_muted(&self, mudo: bool) {
        let g = self.active.lock().await;
        if let Some(a) = g.as_ref() {
            a.muted.store(mudo, Ordering::Relaxed);
            if let Some(c) = &a.capture {
                c.set_muted(mudo);
            }
        }
    }

    /// O que pode ser transmitido neste computador.
    pub fn share_sources(&self) -> Vec<Source> {
        screen::sources().unwrap_or_default()
    }

    /// Começa a transmitir uma tela ou janela para quem está na chamada.
    pub async fn start_sharing(
        self: &Arc<Self>,
        source: Source,
        quality: Quality,
        fps: Fps,
    ) -> Result<(), String> {
        let conn = {
            let g = self.active.lock().await;
            let a = g.as_ref().ok_or("não há chamada em andamento")?;
            if a.sharing.is_some() {
                return Err("você já está transmitindo".into());
            }
            Arc::clone(&a.conn)
        };
        let rotulo = source.label();

        let parar = Arc::new(AtomicBool::new(false));
        let intervalo = fps.interval();
        let altura = quality.height();

        // Capturar e comprimir é trabalho pesado, constante e bloqueante: fica
        // numa thread do sistema, não no executor assíncrono, para não disputar
        // espaço com a interface nem com a rede. A thread também é o único
        // lugar onde o capturador pode existir: no Windows ele guarda um
        // ponteiro do sistema e não pode atravessar threads.
        let (avisa_inicio, inicio_pronto) = tokio::sync::oneshot::channel();
        // Fila curta de propósito: se a rede não dá conta, a captura espera em
        // vez de acumular quadros velhos na memória.
        let (manda, mut recebe) = tokio::sync::mpsc::channel::<Vec<u8>>(8);
        let p = Arc::clone(&parar);
        std::thread::Builder::new()
            .name("captura-de-tela".into())
            .spawn(move || {
                let preparar = || -> Result<(Capturer, VideoEncoder), String> {
                    let cap = Capturer::open(&source)
                        .map_err(|e| format!("não deu para abrir a captura: {e}"))?;
                    let primeiro = cap
                        .grab()
                        .map_err(|e| format!("não deu para capturar a tela: {e}"))?;
                    let ajustado = primeiro.scale_to_height(altura);
                    let enc = VideoEncoder::new(ajustado.width, ajustado.height, quality, fps)
                        .map_err(|e| format!("não deu para preparar o vídeo: {e}"))?;
                    Ok((cap, enc))
                };
                let (capturador, mut enc) = match preparar() {
                    Ok(v) => {
                        let _ = avisa_inicio.send(Ok(()));
                        v
                    }
                    Err(e) => {
                        let _ = avisa_inicio.send(Err(e));
                        return;
                    }
                };

                let mut n: u64 = 0;
                while !p.load(Ordering::Relaxed) {
                    let inicio = std::time::Instant::now();
                    let Ok(quadro) = capturador.grab() else { break };
                    let pronto = quadro.scale_to_height(altura);
                    // Um quadro inteiro no começo, para quem entra no meio da
                    // transmissão não ficar olhando para o nada.
                    match enc.encode(&pronto.rgba, n == 0) {
                        Ok(pacotes) => {
                            for pkt in pacotes {
                                if manda.blocking_send(pkt.data).is_err() {
                                    return;
                                }
                            }
                        }
                        Err(e) => {
                            tracing::warn!("falha ao comprimir a tela: {e}");
                            break;
                        }
                    }
                    n += 1;
                    // Mantém a cadência sem acumular atraso quando um quadro
                    // demora mais que o normal.
                    let gasto = inicio.elapsed();
                    if gasto < intervalo {
                        std::thread::sleep(intervalo - gasto);
                    }
                }
            })
            .map_err(|e| format!("não deu para iniciar a captura: {e}"))?;

        // Só responde ao usuário depois de saber que a captura abriu mesmo.
        match inicio_pronto.await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => return Err(e),
            Err(_) => return Err("a captura de tela encerrou antes de começar".into()),
        }

        {
            let mut g = self.active.lock().await;
            if let Some(a) = g.as_mut() {
                a.sharing = Some((rotulo, Arc::clone(&parar)));
            }
        }

        // Tarefa curta: só leva para a rede o que a thread já comprimiu.
        tokio::task::spawn(async move {
            while let Some(dados) = recebe.recv().await {
                if conn.send_video(&dados, intervalo).await.is_err() {
                    break;
                }
            }
        });
        Ok(())
    }

    /// Para a transmissão de tela.
    pub async fn stop_sharing(self: &Arc<Self>) {
        let mut g = self.active.lock().await;
        if let Some(a) = g.as_mut() {
            if let Some((_, parar)) = a.sharing.take() {
                parar.store(true, Ordering::Relaxed);
            }
        }
    }

    async fn peer_matches(&self, from: &str) -> bool {
        self.active
            .lock()
            .await
            .as_ref()
            .map(|a| a.peer_pk == from)
            .unwrap_or(false)
    }

    /// Encerra tudo e solta o microfone e o alto-falante.
    pub async fn end(self: &Arc<Self>, reason: EndReason) {
        let anterior = self.active.lock().await.take();
        if let Some(a) = anterior {
            *self.ending.lock().await = Some((
                a.peer_pk.clone(),
                reason,
                std::time::Instant::now(),
            ));
            if let Some((_, parar)) = &a.sharing {
                parar.store(true, Ordering::Relaxed);
            }
            a.conn.close().await;
            // Soltar a captura fecha o microfone: a luz do aparelho apaga.
            drop(a.capture);
            drop(a.playback);
            tracing::info!("chamada encerrada: {}", reason.label());
        }
    }

    /// Escuta a conexão e cuida do áudio enquanto a chamada dura.
    fn pump(self: &Arc<Self>, mut eventos: tokio::sync::mpsc::UnboundedReceiver<PeerEvent>) {
        let eu = Arc::clone(self);
        tokio::spawn(async move {
            let mut dec = match Decoder::new() {
                Ok(d) => d,
                Err(_) => return,
            };
            let mut vad = VoiceDetector::default();
            // Só é criado quando chega vídeo de verdade: numa chamada de voz
            // não faz sentido carregar o decodificador.
            let mut dec_video: Option<VideoDecoder> = None;

            while let Some(ev) = eventos.recv().await {
                match ev {
                    PeerEvent::State(ConnectionState::Connected) => {
                        eu.on_connected().await;
                    }
                    PeerEvent::State(ConnectionState::Reconnecting) => {
                        let mut g = eu.active.lock().await;
                        if let Some(a) = g.as_mut() {
                            a.state = CallState::Reconnecting;
                        }
                    }
                    PeerEvent::State(ConnectionState::Failed) => {
                        eu.end(EndReason::Failed).await;
                        break;
                    }
                    PeerEvent::State(ConnectionState::Closed) => break,
                    PeerEvent::TrackOpened { kind: MediaKind::Video } => {
                        let g = eu.active.lock().await;
                        if let Some(a) = g.as_ref() {
                            a.receiving_video.store(true, Ordering::Relaxed);
                        }
                    }
                    PeerEvent::Media { kind: MediaKind::Video, data } => {
                        let destino = {
                            let g = eu.active.lock().await;
                            match g.as_ref() {
                                Some(a) => {
                                    a.receiving_video.store(true, Ordering::Relaxed);
                                    Arc::clone(&a.incoming_frame)
                                }
                                None => break,
                            }
                        };
                        if dec_video.is_none() {
                            dec_video = VideoDecoder::new().ok();
                        }
                        if let Some(d) = dec_video.as_mut() {
                            match d.decode(&data) {
                                Ok(Some(img)) => {
                                    let mut slot = destino.lock().await;
                                    let n = slot.as_ref().map(|(v, _)| v + 1).unwrap_or(1);
                                    *slot = Some((n, img));
                                }
                                Ok(None) => {}
                                Err(e) => tracing::debug!("quadro de vídeo descartado: {e}"),
                            }
                        }
                    }
                    PeerEvent::Media { kind: MediaKind::Audio, data } => {
                        // Um pacote vazio significa pacote perdido: o codec
                        // inventa algo parecido em vez de deixar um buraco.
                        let pcm = if data.is_empty() {
                            dec.decode_lost()
                        } else {
                            dec.decode(&data)
                        };
                        if let Ok(pcm) = pcm {
                            let g = eu.active.lock().await;
                            if let Some(a) = g.as_ref() {
                                a.remote_speaking.store(vad.push(&pcm), Ordering::Relaxed);
                                if let Some(p) = &a.playback {
                                    p.push(&pcm);
                                }
                            } else {
                                break;
                            }
                        }
                    }
                    _ => {}
                }
            }
        });
    }

    /// A conexão fechou: abre o microfone e o alto-falante e começa a falar.
    async fn on_connected(self: &Arc<Self>) {
        let ja_ativa = {
            let g = self.active.lock().await;
            matches!(g.as_ref().map(|a| &a.state), Some(CallState::Active { .. }))
        };
        if ja_ativa {
            return;
        }

        let saida = Playback::start(None).ok();
        let entrada = Capture::start(None);

        let (conn, mudo) = {
            let mut g = self.active.lock().await;
            let Some(a) = g.as_mut() else { return };
            a.state = CallState::Active { since: std::time::Instant::now() };
            a.playback = saida;
            (Arc::clone(&a.conn), Arc::clone(&a.muted))
        };

        match entrada {
            Ok((cap, mut quadros)) => {
                cap.set_muted(mudo.load(Ordering::Relaxed));
                {
                    let mut g = self.active.lock().await;
                    if let Some(a) = g.as_mut() {
                        a.capture = Some(cap);
                    }
                }
                // Microfone → Opus → conexão.
                tokio::spawn(async move {
                    let mut enc = match Encoder::new(AudioQuality::Normal) {
                        Ok(e) => e,
                        Err(_) => return,
                    };
                    while let Some(pcm) = quadros.recv().await {
                        let Ok(pacote) = enc.encode(&pcm) else { continue };
                        if conn
                            .send_audio(&pacote, Duration::from_millis(20))
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                });
            }
            Err(e) => {
                // Sem microfone a chamada ainda serve para ouvir, então ela
                // continua; mas a pessoa precisa ver que ninguém a escuta.
                tracing::warn!("sem microfone nesta chamada: {e}");
                let mut g = self.active.lock().await;
                if let Some(a) = g.as_mut() {
                    a.sem_microfone = true;
                }
            }
        }
    }
}

/// Quanto tempo a chamada já dura, formatado para a tela.
pub fn duration_label(since: std::time::Instant) -> String {
    let s = since.elapsed().as_secs();
    if s >= 3600 {
        format!("{:02}:{:02}:{:02}", s / 3600, (s % 3600) / 60, s % 60)
    } else {
        format!("{:02}:{:02}", s / 60, s % 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn motivos_de_encerramento_tem_texto() {
        for r in [
            EndReason::HungUp,
            EndReason::Declined,
            EndReason::NoAnswer,
            EndReason::Busy,
            EndReason::Failed,
        ] {
            assert!(!r.label().is_empty());
        }
        assert!(EndReason::Failed.label().contains("TURN"), "deve explicar o que fazer");
    }

    #[test]
    fn duracao_e_formatada_para_a_tela() {
        let agora = std::time::Instant::now();
        let texto = duration_label(agora);
        assert_eq!(texto, "00:00");
        assert_eq!(texto.len(), 5, "formato curto enquanto é menos de uma hora");
    }

    #[test]
    fn estados_da_chamada_sao_distintos() {
        assert_ne!(CallState::Calling, CallState::Connecting);
        assert_ne!(
            CallState::Ringing { video: true },
            CallState::Ringing { video: false }
        );
        assert_eq!(
            CallState::Ended { reason: EndReason::HungUp },
            CallState::Ended { reason: EndReason::HungUp }
        );
    }
}
