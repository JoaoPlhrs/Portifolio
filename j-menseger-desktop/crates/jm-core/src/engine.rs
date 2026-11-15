//! O motor: amarra identidade, servidores e banco, e avisa a interface.
//!
//! A interface nunca fala com a rede nem com o banco diretamente. Ela chama
//! métodos daqui e escuta [`EngineEvent`]. Assim a janela nunca trava
//! esperando um servidor responder.

use crate::groups::{Group, GroupMeta, Template};
use crate::identity;
use crate::messages::{AppMessage, CallAction, CallSignal, ChatKey, Delivery, ReceiptKind};
use crate::protocol::{self, WrapOptions, KIND_APP, KIND_REACTION, KIND_TEXT};
use crate::relays::{RelayPool, RelayStatus};
use crate::store::{StoredMsg, Store};
use nostr::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error(transparent)]
    Store(#[from] crate::store::StoreError),
    #[error(transparent)]
    Identity(#[from] identity::IdentityError),
    #[error("protocolo: {0}")]
    Protocol(String),
    #[error("servidores: {0}")]
    Relay(String),
    #[error("ainda não há uma conta neste aparelho")]
    NoIdentity,
    #[error("conversa desconhecida: {0}")]
    UnknownChat(String),
}

type Result<T> = std::result::Result<T, EngineError>;

/// Seu próprio perfil.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Profile {
    pub name: String,
    #[serde(default)]
    pub note: String,
    #[serde(default)]
    pub tone: u8,
    #[serde(default)]
    pub pic: Option<String>,
    #[serde(default)]
    pub created: i64,
}

/// Situação de um contato.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ContactStatus {
    /// Mandou mensagem mas ainda não foi aceito.
    Request,
    /// Contato aceito: pode mandar mensagem e ligar.
    Ok,
    /// Bloqueado: nada do que ele manda chega.
    Blocked,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Contact {
    pub pk: String,
    #[serde(default)]
    pub name: String,
    /// Apelido que você deu. Tem prioridade sobre o nome que a pessoa escolheu.
    #[serde(default)]
    pub nick: String,
    #[serde(default)]
    pub note: String,
    #[serde(default)]
    pub pic: Option<String>,
    #[serde(default)]
    pub tone: u8,
    pub status: ContactStatus,
    #[serde(default)]
    pub verified: bool,
    #[serde(default)]
    pub relays: Vec<String>,
    #[serde(default)]
    pub prof_sent: bool,
    #[serde(default)]
    pub added_at: i64,
}

impl Contact {
    /// Como o nome aparece na tela.
    pub fn display_name(&self) -> String {
        if !self.nick.is_empty() {
            self.nick.clone()
        } else if !self.name.is_empty() {
            self.name.clone()
        } else {
            PublicKey::from_hex(&self.pk)
                .map(|p| identity::short_id(&p))
                .unwrap_or_else(|_| self.pk[..8.min(self.pk.len())].to_string())
        }
    }
}

/// Uma conversa na lista.
#[derive(Debug, Clone)]
pub struct ChatSummary {
    pub key: String,
    pub title: String,
    pub last_text: String,
    pub last_ts: i64,
    pub last_status: Option<Delivery>,
    pub last_from_me: bool,
    pub unread: u32,
    pub pinned: bool,
    pub muted: bool,
    pub is_group: bool,
    pub peer: Option<String>,
}

/// Uma mensagem como a interface precisa dela.
#[derive(Debug, Clone)]
pub struct Message {
    pub id: String,
    pub chat: String,
    pub author: String,
    pub from_me: bool,
    pub ts: i64,
    pub body: MessageBody,
    pub status: Option<Delivery>,
    pub reply: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum MessageBody {
    Text(String),
    /// Aviso do sistema ("entrou no grupo", "chamada perdida"…).
    System(String),
    Sound { id: String },
    /// Convite para um grupo, com botão de entrar.
    Invite { lid: String, name: String },
    Deleted,
}

impl MessageBody {
    fn kind_name(&self) -> &'static str {
        match self {
            MessageBody::Text(_) => "text",
            MessageBody::System(_) => "sys",
            MessageBody::Sound { .. } => "sound",
            MessageBody::Invite { .. } => "invite",
            MessageBody::Deleted => "deleted",
        }
    }
    fn to_json(&self) -> String {
        match self {
            MessageBody::Text(t) => serde_json::json!({ "text": t }).to_string(),
            MessageBody::System(t) => serde_json::json!({ "text": t }).to_string(),
            MessageBody::Sound { id } => serde_json::json!({ "id": id }).to_string(),
            MessageBody::Invite { lid, name } => serde_json::json!({ "lid": lid, "text": name }).to_string(),
            MessageBody::Deleted => "{}".into(),
        }
    }
    fn from_stored(kind: &str, body: &str) -> MessageBody {
        let v: serde_json::Value = serde_json::from_str(body).unwrap_or_default();
        let text = v["text"].as_str().unwrap_or_default().to_string();
        match kind {
            "text" => MessageBody::Text(text),
            "sys" => MessageBody::System(text),
            "sound" => MessageBody::Sound {
                id: v["id"].as_str().unwrap_or_default().to_string(),
            },
            "invite" => MessageBody::Invite {
                lid: v["lid"].as_str().unwrap_or_default().to_string(),
                name: text.clone(),
            },
            _ => MessageBody::Deleted,
        }
    }

    /// Resumo curto para a lista de conversas.
    pub fn preview(&self) -> String {
        match self {
            MessageBody::Text(t) => t.clone(),
            MessageBody::System(t) => t.clone(),
            MessageBody::Sound { .. } => "Som do soundboard".into(),
            MessageBody::Invite { name, .. } => format!("Convite para {name}"),
            MessageBody::Deleted => "Mensagem apagada".into(),
        }
    }
}

/// O que o motor avisa para a interface.
#[derive(Debug, Clone)]
pub enum EngineEvent {
    /// Chegou ou saiu uma mensagem.
    Message { chat: String, message: Box<Message> },
    /// Mudou o estado de entrega de uma mensagem já na tela.
    MessageUpdated { chat: String, id: String, status: Delivery },
    /// Uma mensagem foi apagada para todos.
    MessageDeleted { chat: String, id: String },
    /// A lista de conversas mudou.
    ChatsChanged,
    /// Um contato mudou (nome, foto, situação).
    ContactChanged { pk: String },
    /// Alguém está digitando nesta conversa.
    Typing { chat: String, who: String },
    /// Mudou a conexão com os servidores.
    Relays(Vec<RelayStatus>),
    /// A lista de grupos ou os dados de um grupo mudaram.
    GroupsChanged,
    /// Chegou sinalização de chamada de alguém.
    CallSignal { from: String, signal: CallSignal },
    /// Uma chamada que você não atendeu.
    MissedCall { from: String, video: bool, ts: i64 },
}

/// Ajustes do aplicativo.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    pub relays: Vec<String>,
    pub read_receipts: bool,
    pub typing: bool,
    pub ttl_default: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            relays: protocol::DEFAULT_RELAYS.iter().map(|s| s.to_string()).collect(),
            read_receipts: true,
            typing: true,
            ttl_default: 0,
        }
    }
}

struct State {
    contacts: HashMap<String, Contact>,
    groups: HashMap<String, Group>,
    /// Quando pedimos o perfil de alguém pela última vez, para não insistir.
    prof_req_at: HashMap<String, i64>,
    /// Conversa aberta na tela, para marcar como lida e não notificar.
    active: Option<String>,
}

pub struct Engine {
    keys: Keys,
    me: Profile,
    settings: Settings,
    store: Arc<Mutex<Store>>,
    pool: Arc<RelayPool>,
    state: Arc<Mutex<State>>,
    events: mpsc::UnboundedSender<EngineEvent>,
}

impl Engine {
    /// Abre o motor com uma conta já existente.
    pub fn open(
        keys: Keys,
        store: Store,
    ) -> Result<(Arc<Self>, mpsc::UnboundedReceiver<EngineEvent>)> {
        let me: Profile = store.get_kv("me")?.unwrap_or_default();
        let settings: Settings = store.get_kv("settings")?.unwrap_or_default();
        let contacts = load_contacts(&store)?;
        let groups = load_groups(&store)?;

        let (tx, rx) = mpsc::unbounded_channel();
        let engine = Arc::new(Self {
            pool: Arc::new(RelayPool::new(keys.clone())),
            keys,
            me,
            settings,
            store: Arc::new(Mutex::new(store)),
            state: Arc::new(Mutex::new(State {
                contacts,
                groups,
                prof_req_at: HashMap::new(),
                active: None,
            })),
            events: tx,
        });
        Ok((engine, rx))
    }

    /// Cria uma conta nova a partir de 12 palavras.
    pub fn create_identity(store: &Store, words: &str, name: &str) -> Result<Keys> {
        let keys = identity::keys_from_words(words)?;
        let me = Profile {
            name: name.trim().to_string(),
            note: String::new(),
            tone: (keys.public_key().to_hex().as_bytes()[0] % 6) as u8,
            pic: None,
            created: now_ms(),
        };
        store.set_kv("me", &me)?;
        store.set_kv("last_sync", &now_secs())?;
        Ok(keys)
    }

    /// Entra em uma conta que já existe, em outro aparelho.
    ///
    /// Busca o último mês guardado nos servidores, que é o que eles costumam
    /// manter: o histórico mais antigo fica só no aparelho de origem.
    pub fn restore_identity(store: &Store, input: &str, name: &str) -> Result<Keys> {
        let keys = identity::keys_from_any(input)?;
        let me = Profile {
            name: name.trim().to_string(),
            tone: (keys.public_key().to_hex().as_bytes()[0] % 6) as u8,
            created: now_ms(),
            ..Default::default()
        };
        store.set_kv("me", &me)?;
        store.set_kv("last_sync", &(now_secs() - 30 * 86_400))?;
        Ok(keys)
    }

    pub fn public_key(&self) -> PublicKey {
        self.keys.public_key()
    }

    pub fn npub(&self) -> String {
        identity::npub(&self.keys.public_key())
    }

    pub fn me(&self) -> &Profile {
        &self.me
    }

    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    /// Conecta aos servidores e começa a receber.
    pub async fn start(self: &Arc<Self>) -> Result<()> {
        self.pool
            .set_relays(&self.settings.relays)
            .await
            .map_err(|e| EngineError::Relay(e.to_string()))?;

        let since: i64 = self
            .store
            .lock()
            .unwrap()
            .get_kv("last_sync")?
            .unwrap_or_else(|| now_secs() - 86_400);

        self.pool
            .subscribe_inbox(
                &self.inbox_keys(),
                Timestamp::from_secs(since.max(0) as u64),
            )
            .await
            .map_err(|e| EngineError::Relay(e.to_string()))?;

        let mut inbox = self.pool.listen();
        let me = Arc::clone(self);
        tokio::spawn(async move {
            while let Some(incoming) = inbox.recv().await {
                if let Err(e) = me.handle_envelope(&incoming.event) {
                    tracing::debug!("envelope descartado: {e}");
                }
            }
        });

        // Avisa a interface sobre o estado das conexões, de tempos em tempos.
        let me = Arc::clone(self);
        tokio::spawn(async move {
            let mut anterior: Vec<RelayStatus> = Vec::new();
            loop {
                let agora = me.pool.status().await;
                if agora != anterior {
                    let _ = me.events.send(EngineEvent::Relays(agora.clone()));
                    anterior = agora;
                }
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
        });

        Ok(())
    }

    /* ---------------- recebimento ---------------- */

    /// Abre um envelope que chegou e decide o que fazer com ele.
    fn handle_envelope(self: &Arc<Self>, event: &Event) -> Result<()> {
        let id = event.id.to_hex();
        // A mesma mensagem chega por vários servidores: só a primeira conta.
        if !self.store.lock().unwrap().mark_seen(&id, now_secs())? {
            return Ok(());
        }

        // O envelope diz para quem foi endereçado. Pode ser para mim ou para a
        // chave de um grupo que eu participo.
        let destino = protocol::tag_value_event(event, "p").unwrap_or_default();
        let grupo = self.group_for_pubkey(&destino);

        let abridor = match &grupo {
            Some((_, keys)) => keys.clone(),
            None => {
                if destino != self.keys.public_key().to_hex() {
                    return Ok(());
                }
                self.keys.clone()
            }
        };

        let rumor = protocol::unwrap_event(event, &abridor)
            .map_err(|e| EngineError::Protocol(e.to_string()))?;

        let author = rumor.pubkey.to_hex();
        let from_me = rumor.pubkey == self.keys.public_key();

        if let Some((lid, _)) = grupo {
            return self.handle_group_rumor(&rumor, &lid, from_me);
        }

        // Quem está bloqueado não entrega nada.
        if !from_me {
            let st = self.state.lock().unwrap();
            if matches!(
                st.contacts.get(&author).map(|c| c.status),
                Some(ContactStatus::Blocked)
            ) {
                return Ok(());
            }
        }

        // Em conversa de duas pessoas, a chave é sempre a do outro lado: se eu
        // mesmo enviei, o destinatário está na marca `p`.
        let peer = if from_me {
            protocol::tag_value(&rumor, "p").map(|s| s.to_string())
        } else {
            Some(author.clone())
        };
        let Some(peer) = peer else { return Ok(()) };
        if peer == self.keys.public_key().to_hex() {
            return Ok(());
        }
        let chat = ChatKey::Direct { peer: peer.clone() }.as_string();

        let ts_ms = rumor.created_at.as_secs() as i64 * 1000;
        let exp = protocol::expiration_of(&rumor).map(|t| t.as_secs() as i64 * 1000);

        match rumor.kind.as_u16() {
            KIND_TEXT => {
                let rid = rumor.id.map(|i| i.to_hex()).unwrap_or_default();
                self.ensure_contact(&peer, if from_me { ContactStatus::Ok } else { ContactStatus::Request })?;
                let msg = Message {
                    id: rid,
                    chat: chat.clone(),
                    author: author.clone(),
                    from_me,
                    ts: ts_ms,
                    body: MessageBody::Text(rumor.content.chars().take(6000).collect()),
                    status: from_me.then_some(Delivery::Sent),
                    reply: protocol::tag_value(&rumor, "e").map(|s| s.to_string()),
                };
                self.store_and_emit(msg, exp, !from_me)?;
                if !from_me {
                    self.maybe_request_profile(&peer);
                }
            }
            KIND_REACTION => { /* reações entram junto com o restante da interface */ }
            KIND_APP => {
                let Some(app) = AppMessage::parse(&rumor.content) else {
                    return Ok(());
                };
                self.handle_app_message(app, &peer, &chat, from_me, ts_ms)?;
            }
            _ => {}
        }
        Ok(())
    }

    fn handle_app_message(
        self: &Arc<Self>,
        app: AppMessage,
        peer: &str,
        chat: &str,
        from_me: bool,
        ts_ms: i64,
    ) -> Result<()> {
        match app {
            AppMessage::Profile { n, s, c, p, r } if !from_me => {
                let mut st = self.state.lock().unwrap();
                let contact = st.contacts.entry(peer.to_string()).or_insert_with(|| {
                    new_contact(peer, ContactStatus::Request)
                });
                contact.name = n.chars().take(40).collect();
                contact.note = s.chars().take(80).collect();
                if c < 6 {
                    contact.tone = c;
                }
                if let Some(pic) = p {
                    contact.pic = (!pic.is_empty()).then_some(pic);
                }
                contact.relays = r.into_iter().take(5).collect();
                let snapshot = contact.clone();
                drop(st);
                self.save_contact(&snapshot)?;
                let _ = self.events.send(EngineEvent::ContactChanged { pk: peer.into() });
                let _ = self.events.send(EngineEvent::ChatsChanged);
            }
            AppMessage::ProfileRequest if !from_me => {
                let me = Arc::clone(self);
                let p = peer.to_string();
                tokio::spawn(async move {
                    let _ = me.send_profile(&p).await;
                });
            }
            AppMessage::Call(sig) if !from_me => {
                if sig.a == CallAction::Missed {
                    let video = sig.v.unwrap_or(0) == 1;
                    let msg = Message {
                        id: format!("missed-{ts_ms}"),
                        chat: chat.into(),
                        author: peer.into(),
                        from_me: false,
                        ts: ts_ms,
                        body: MessageBody::System(if video {
                            "Chamada de vídeo perdida".into()
                        } else {
                            "Chamada de voz perdida".into()
                        }),
                        status: None,
                        reply: None,
                    };
                    self.store_and_emit(msg, None, true)?;
                    let _ = self.events.send(EngineEvent::MissedCall {
                        from: peer.into(),
                        video,
                        ts: ts_ms,
                    });
                } else {
                    // Quem não é contato aceito não pode ligar.
                    let aceito = matches!(
                        self.state.lock().unwrap().contacts.get(peer).map(|c| c.status),
                        Some(ContactStatus::Ok)
                    );
                    if aceito {
                        let _ = self.events.send(EngineEvent::CallSignal {
                            from: peer.into(),
                            signal: sig,
                        });
                    }
                }
            }
            AppMessage::GroupInvite { g } if !from_me => {
                self.store_invite(g, peer, chat, ts_ms)?;
            }
            AppMessage::Receipt { s, ids } if !from_me => {
                let status = match s {
                    ReceiptKind::Delivered => Delivery::Delivered,
                    ReceiptKind::Read => Delivery::Read,
                };
                for id in ids.into_iter().take(200) {
                    self.apply_receipt(chat, &id, status)?;
                }
            }
            AppMessage::Typing if !from_me => {
                let _ = self.events.send(EngineEvent::Typing {
                    chat: chat.into(),
                    who: peer.into(),
                });
            }
            AppMessage::Delete { ids } => {
                for id in ids.into_iter().take(100) {
                    let store = self.store.lock().unwrap();
                    if let Some(m) = store.get_msg(&id)? {
                        // Só quem escreveu pode apagar para todos.
                        if m.chat == chat && m.author == *peer || (from_me && m.chat == chat) {
                            store.delete_msg(&id)?;
                            drop(store);
                            let _ = self.events.send(EngineEvent::MessageDeleted {
                                chat: chat.into(),
                                id,
                            });
                        }
                    }
                }
                let _ = self.events.send(EngineEvent::ChatsChanged);
            }
            AppMessage::Ttl { v } => {
                let texto = if v == 0 {
                    "Mensagens temporárias desativadas".to_string()
                } else {
                    format!("Mensagens temporárias: {}", ttl_label(v))
                };
                let msg = Message {
                    id: format!("sys-ttl-{ts_ms}"),
                    chat: chat.into(),
                    author: peer.into(),
                    from_me,
                    ts: ts_ms,
                    body: MessageBody::System(texto),
                    status: None,
                    reply: None,
                };
                self.store_and_emit(msg, None, false)?;
            }
            AppMessage::Sound { id } if !from_me => {
                self.ensure_contact(peer, ContactStatus::Request)?;
                let msg = Message {
                    id: format!("snd-{ts_ms}-{}", &id),
                    chat: chat.into(),
                    author: peer.into(),
                    from_me,
                    ts: ts_ms,
                    body: MessageBody::Sound { id },
                    status: None,
                    reply: None,
                };
                self.store_and_emit(msg, None, true)?;
            }
            _ => {}
        }
        Ok(())
    }

    fn apply_receipt(&self, chat: &str, id: &str, status: Delivery) -> Result<()> {
        let store = self.store.lock().unwrap();
        let Some(m) = store.get_msg(id)? else { return Ok(()) };
        // Só vale para mensagens minhas, nesta conversa.
        if m.chat != chat || m.author != self.keys.public_key().to_hex() {
            return Ok(());
        }
        let atual = m
            .status
            .as_deref()
            .and_then(parse_delivery)
            .unwrap_or(Delivery::Sent);
        let novo = atual.advance(status);
        if novo == atual {
            return Ok(());
        }
        store.set_msg_status(id, delivery_name(novo))?;
        drop(store);
        let _ = self.events.send(EngineEvent::MessageUpdated {
            chat: chat.into(),
            id: id.into(),
            status: novo,
        });
        let _ = self.events.send(EngineEvent::ChatsChanged);
        Ok(())
    }

    fn store_and_emit(&self, msg: Message, exp: Option<i64>, incoming: bool) -> Result<()> {
        let stored = StoredMsg {
            id: msg.id.clone(),
            chat: msg.chat.clone(),
            author: msg.author.clone(),
            kind: msg.body.kind_name().into(),
            ts: msg.ts,
            body: msg.body.to_json(),
            status: msg.status.map(|s| delivery_name(s).to_string()),
            exp,
            reply: msg.reply.clone(),
        };
        let store = self.store.lock().unwrap();
        if !store.put_msg(&stored)? {
            return Ok(()); // já estava guardada
        }
        let visivel = self.state.lock().unwrap().active.as_deref() == Some(msg.chat.as_str());
        upsert_chat(&store, &msg, incoming && !visivel)?;
        drop(store);

        let chat = msg.chat.clone();
        let _ = self.events.send(EngineEvent::Message {
            chat,
            message: Box::new(msg),
        });
        let _ = self.events.send(EngineEvent::ChatsChanged);
        Ok(())
    }

    /* ---------------- envio ---------------- */

    /// Manda uma mensagem de texto.
    pub async fn send_text(self: &Arc<Self>, chat_key: &str, text: &str) -> Result<Message> {
        let text: String = text.trim().chars().take(6000).collect();
        if text.is_empty() {
            return Err(EngineError::Protocol("mensagem vazia".into()));
        }
        let key = ChatKey::parse(chat_key)
            .ok_or_else(|| EngineError::UnknownChat(chat_key.to_string()))?;
        if let ChatKey::Channel { lid, channel } = &key {
            return self.send_text_to_channel(lid, channel, &text).await;
        }
        let ChatKey::Direct { peer } = &key else { unreachable!() };
        let peer_pk = PublicKey::from_hex(peer)
            .map_err(|_| EngineError::UnknownChat(chat_key.to_string()))?;

        let contato = self.ensure_contact(peer, ContactStatus::Ok)?;
        // Na primeira mensagem para alguém, vai o seu perfil junto: é assim que
        // a outra pessoa passa a ver o seu nome em vez do seu código.
        if !contato.prof_sent {
            let _ = self.send_profile(peer).await;
            self.mark_profile_sent(peer)?;
        }

        let rumor = protocol::make_rumor(
            &self.keys,
            KIND_TEXT,
            &text,
            vec![Tag::public_key(peer_pk)],
        );
        let id = rumor.id.map(|i| i.to_hex()).unwrap_or_default();

        let msg = Message {
            id: id.clone(),
            chat: chat_key.to_string(),
            author: self.keys.public_key().to_hex(),
            from_me: true,
            ts: now_ms(),
            body: MessageBody::Text(text),
            status: Some(Delivery::Pending),
            reply: None,
        };
        self.store_and_emit(msg.clone(), None, false)?;

        // Duas cópias: uma para quem recebe e outra para mim, para a conversa
        // reaparecer se eu entrar na conta em outro aparelho.
        let para_ele =
            protocol::wrap_rumor(&rumor, &self.keys, &peer_pk, WrapOptions::default())
                .map_err(|e| EngineError::Protocol(e.to_string()))?;
        let para_mim = protocol::wrap_rumor(
            &rumor,
            &self.keys,
            &self.keys.public_key(),
            WrapOptions::default(),
        )
        .map_err(|e| EngineError::Protocol(e.to_string()))?;

        match self.pool.publish(&para_ele).await {
            Ok(_) => {
                let store = self.store.lock().unwrap();
                store.set_msg_status(&id, delivery_name(Delivery::Sent))?;
                drop(store);
                let _ = self.events.send(EngineEvent::MessageUpdated {
                    chat: chat_key.to_string(),
                    id: id.clone(),
                    status: Delivery::Sent,
                });
            }
            Err(e) => {
                // Guarda para reenviar quando a internet voltar.
                let store = self.store.lock().unwrap();
                let wraps = serde_json::to_string(&vec![para_ele.as_json(), para_mim.as_json()])
                    .unwrap_or_default();
                store.queue_outbox(&id, &wraps, "[]", now_secs())?;
                tracing::warn!("mensagem na fila para reenvio: {e}");
            }
        }
        let _ = self.pool.publish(&para_mim).await;
        let _ = self.events.send(EngineEvent::ChatsChanged);

        Ok(msg)
    }

    /// Manda texto em um canal de grupo.
    async fn send_text_to_channel(
        self: &Arc<Self>,
        lid: &str,
        channel: &str,
        text: &str,
    ) -> Result<Message> {
        let g = self.group(lid).ok_or_else(|| EngineError::UnknownChat(lid.into()))?;
        if g.joined_at == 0 {
            return Err(EngineError::Protocol("aceite o convite antes de escrever".into()));
        }
        let pk = PublicKey::from_hex(&g.pk)
            .map_err(|_| EngineError::Protocol("chave do grupo inválida".into()))?;
        let chat_key = ChatKey::Channel { lid: lid.into(), channel: channel.into() }.as_string();

        let rumor = protocol::make_rumor(
            &self.keys,
            KIND_TEXT,
            text,
            vec![
                Tag::public_key(pk),
                Tag::custom("ch", [channel.to_string()]),
            ],
        );
        let id = rumor.id.map(|i| i.to_hex()).unwrap_or_default();

        let msg = Message {
            id: id.clone(),
            chat: chat_key.clone(),
            author: self.keys.public_key().to_hex(),
            from_me: true,
            ts: now_ms(),
            body: MessageBody::Text(text.to_string()),
            status: Some(Delivery::Pending),
            reply: None,
        };
        self.store_and_emit(msg.clone(), None, false)?;

        // Uma cópia só: todos no grupo abrem com a mesma chave.
        let env = protocol::wrap_rumor(&rumor, &self.keys, &pk, WrapOptions::default())
            .map_err(|e| EngineError::Protocol(e.to_string()))?;
        match self.pool.publish(&env).await {
            Ok(_) => {
                self.store.lock().unwrap().set_msg_status(&id, delivery_name(Delivery::Sent))?;
                let _ = self.events.send(EngineEvent::MessageUpdated {
                    chat: chat_key,
                    id,
                    status: Delivery::Sent,
                });
            }
            Err(e) => tracing::warn!("mensagem de grupo não saiu: {e}"),
        }
        let _ = self.events.send(EngineEvent::ChatsChanged);
        Ok(msg)
    }

    /// Envia uma mensagem interna do aplicativo.
    pub async fn send_app(self: &Arc<Self>, peer: &str, app: &AppMessage) -> Result<()> {
        let peer_pk = PublicKey::from_hex(peer)
            .map_err(|_| EngineError::UnknownChat(peer.to_string()))?;
        let rumor = protocol::make_rumor(
            &self.keys,
            KIND_APP,
            app.to_json(),
            vec![Tag::public_key(peer_pk)],
        );
        let env = protocol::wrap_rumor(
            &rumor,
            &self.keys,
            &peer_pk,
            WrapOptions {
                ephemeral: app.is_ephemeral(),
                expiration: None,
            },
        )
        .map_err(|e| EngineError::Protocol(e.to_string()))?;
        self.pool
            .publish(&env)
            .await
            .map_err(|e| EngineError::Relay(e.to_string()))?;
        Ok(())
    }

    /// Manda sinalização de chamada para alguém.
    ///
    /// Vai como envelope efêmero: o servidor repassa na hora e não guarda.
    pub async fn send_call_signal(self: &Arc<Self>, peer: &str, signal: CallSignal) -> Result<()> {
        self.send_app(peer, &AppMessage::Call(signal)).await
    }

    /// Registra no histórico que você não atendeu, e avisa o outro lado.
    pub async fn report_missed_call(self: &Arc<Self>, peer: &str, video: bool) -> Result<()> {
        self.send_call_signal(
            peer,
            CallSignal {
                cid: String::new(),
                a: CallAction::Missed,
                sdp: None,
                v: Some(video as u8),
            },
        )
        .await
    }

    /// Manda o próprio perfil para alguém.
    pub async fn send_profile(self: &Arc<Self>, peer: &str) -> Result<()> {
        let app = AppMessage::Profile {
            n: self.me.name.clone(),
            s: self.me.note.clone(),
            c: self.me.tone,
            p: self.me.pic.clone(),
            r: self.settings.relays.iter().take(5).cloned().collect(),
        };
        self.send_app(peer, &app).await
    }

    /// Avisa que você está digitando. Não vale a pena insistir: vai como
    /// efêmero e, se falhar, o pior que acontece é o aviso não aparecer.
    pub async fn send_typing(self: &Arc<Self>, peer: &str) {
        if self.settings.typing {
            let _ = self.send_app(peer, &AppMessage::Typing).await;
        }
    }

    /* ---------------- contatos e conversas ---------------- */

    /// Adiciona alguém pelo código e já manda o seu perfil.
    pub async fn add_contact(self: &Arc<Self>, input: &str) -> Result<Contact> {
        let pk = identity::parse_pubkey(input)
            .ok_or_else(|| EngineError::Protocol("código inválido".into()))?;
        if pk == self.keys.public_key() {
            return Err(EngineError::Protocol("esse é o seu próprio código".into()));
        }
        let hex = pk.to_hex();
        self.ensure_contact(&hex, ContactStatus::Ok)?;
        // A conversa precisa existir na lista antes da primeira mensagem, senão
        // a pessoa adiciona alguém e não tem onde escrever.
        self.ensure_chat(&ChatKey::Direct { peer: hex.clone() }.as_string())?;
        let c = self.contact(&hex).ok_or(EngineError::NoIdentity)?;
        let _ = self.send_profile(&hex).await;
        let _ = self.events.send(EngineEvent::ChatsChanged);
        Ok(c)
    }

    fn mark_profile_sent(&self, pk: &str) -> Result<()> {
        let mut st = self.state.lock().unwrap();
        if let Some(c) = st.contacts.get_mut(pk) {
            c.prof_sent = true;
            let snap = c.clone();
            drop(st);
            self.save_contact(&snap)?;
        }
        Ok(())
    }

    /// Pede o perfil de quem ainda não tem nome, no máximo uma vez a cada 6 h.
    fn maybe_request_profile(self: &Arc<Self>, pk: &str) {
        {
            let st = self.state.lock().unwrap();
            match st.contacts.get(pk) {
                Some(c) if c.name.is_empty() => {}
                _ => return,
            }
        }
        let agora = now_ms();
        {
            let mut st = self.state.lock().unwrap();
            let ultimo = st.prof_req_at.get(pk).copied().unwrap_or(0);
            if agora - ultimo < 6 * 3600 * 1000 {
                return;
            }
            st.prof_req_at.insert(pk.to_string(), agora);
        }
        let me = Arc::clone(self);
        let pk = pk.to_string();
        tokio::spawn(async move {
            let _ = me.send_app(&pk, &AppMessage::ProfileRequest).await;
            let _ = me.send_profile(&pk).await;
        });
    }

    /* ---------------- grupos ---------------- */

    /// Todas as chaves que este aparelho precisa escutar: a minha e a de cada
    /// grupo, incluindo as chaves antigas dos grupos que trocaram de chave.
    fn inbox_keys(&self) -> Vec<PublicKey> {
        let mut v = vec![self.keys.public_key()];
        let st = self.state.lock().unwrap();
        for g in st.groups.values() {
            for (pk, _) in g.all_keys() {
                if let Ok(p) = PublicKey::from_hex(&pk) {
                    v.push(p);
                }
            }
        }
        v
    }

    /// Descobre de qual grupo é uma chave de destino, e com que chave abrir.
    fn group_for_pubkey(&self, pk_hex: &str) -> Option<(String, Keys)> {
        let st = self.state.lock().unwrap();
        for g in st.groups.values() {
            for (pk, keys) in g.all_keys() {
                if pk == pk_hex {
                    return Some((g.lid.clone(), keys));
                }
            }
        }
        None
    }

    pub fn group(&self, lid: &str) -> Option<Group> {
        self.state.lock().unwrap().groups.get(lid).cloned()
    }

    pub fn groups(&self) -> Vec<Group> {
        let mut v: Vec<Group> = self.state.lock().unwrap().groups.values().cloned().collect();
        v.sort_by_key(|g| g.meta.name.to_lowercase());
        v
    }

    /// Cria um grupo e já abre os canais dele na lista de conversas.
    pub async fn create_group(self: &Arc<Self>, name: &str, template: Template) -> Result<Group> {
        let g = Group::create(name, &self.keys.public_key(), &self.me.name, template);
        self.save_group(&g)?;
        {
            let mut st = self.state.lock().unwrap();
            st.groups.insert(g.lid.clone(), g.clone());
        }
        for ch in g.meta.text_channels() {
            self.ensure_chat(&ChatKey::Channel { lid: g.lid.clone(), channel: ch.id.clone() }.as_string())?;
        }
        self.resubscribe().await;
        let _ = self.events.send(EngineEvent::GroupsChanged);
        Ok(g)
    }

    /// Convida alguém: manda a chave do grupo por mensagem privada.
    pub async fn invite_to_group(self: &Arc<Self>, lid: &str, peer: &str) -> Result<()> {
        let g = self.group(lid).ok_or_else(|| EngineError::UnknownChat(lid.into()))?;
        self.ensure_contact(peer, ContactStatus::Ok)?;
        let convite = crate::messages::GroupInvite {
            sk: g.sk.clone(),
            meta: serde_json::to_value(&g.meta).unwrap_or_default(),
            ver: g.ver,
            pic: g.pic.clone().unwrap_or_default(),
            pic_v: g.pic_v,
        };
        self.send_app(peer, &AppMessage::GroupInvite { g: convite }).await
    }

    /// Entra em um grupo a partir de um convite recebido.
    pub async fn accept_invite(self: &Arc<Self>, lid: &str) -> Result<Group> {
        let mut g = {
            let st = self.state.lock().unwrap();
            st.groups.get(lid).cloned()
        }
        .ok_or_else(|| EngineError::UnknownChat(lid.into()))?;

        let eu = self.keys.public_key().to_hex();
        if !g.meta.members.contains(&eu) {
            g.meta.members.push(eu.clone());
        }
        g.meta.names.insert(eu, self.me.name.clone());
        g.joined_at = now_ms();
        self.save_group(&g)?;
        {
            let mut st = self.state.lock().unwrap();
            st.groups.insert(g.lid.clone(), g.clone());
        }
        for ch in g.meta.text_channels() {
            self.ensure_chat(&ChatKey::Channel { lid: g.lid.clone(), channel: ch.id.clone() }.as_string())?;
        }
        self.resubscribe().await;
        // Avisa o grupo que você entrou, para os outros verem o seu nome.
        let _ = self
            .send_to_group(&g, &AppMessage::GroupJoin {
                n: self.me.name.clone(),
                p: self.me.pic.clone(),
            })
            .await;
        let _ = self.events.send(EngineEvent::GroupsChanged);
        Ok(g)
    }

    /// Sai do grupo e apaga as conversas dele deste aparelho.
    pub async fn leave_group(self: &Arc<Self>, lid: &str) -> Result<()> {
        let g = self.group(lid).ok_or_else(|| EngineError::UnknownChat(lid.into()))?;
        let _ = self.send_to_group(&g, &AppMessage::GroupLeave).await;
        {
            let store = self.store.lock().unwrap();
            for ch in g.meta.channels.iter() {
                let key = ChatKey::Channel { lid: lid.into(), channel: ch.id.clone() }.as_string();
                store.delete_chat_msgs(&key)?;
                store
                    .raw()
                    .execute("DELETE FROM chats WHERE key = ?1", rusqlite::params![key])
                    .map_err(crate::store::StoreError::from)?;
            }
            store
                .raw()
                .execute("DELETE FROM groups WHERE lid = ?1", rusqlite::params![lid])
                .map_err(crate::store::StoreError::from)?;
        }
        self.state.lock().unwrap().groups.remove(lid);
        self.resubscribe().await;
        let _ = self.events.send(EngineEvent::GroupsChanged);
        let _ = self.events.send(EngineEvent::ChatsChanged);
        Ok(())
    }

    /// Publica uma mensagem interna para todo o grupo.
    async fn send_to_group(self: &Arc<Self>, g: &Group, app: &AppMessage) -> Result<()> {
        let canal = g
            .main_channel()
            .map(|c| c.id.clone())
            .ok_or_else(|| EngineError::Protocol("grupo sem canal de texto".into()))?;
        let pk = PublicKey::from_hex(&g.pk).map_err(|_| EngineError::Protocol("chave do grupo inválida".into()))?;
        let rumor = protocol::make_rumor(
            &self.keys,
            KIND_APP,
            app.to_json(),
            vec![Tag::public_key(pk), Tag::custom("ch", [canal])],
        );
        let env = protocol::wrap_rumor(
            &rumor,
            &self.keys,
            &pk,
            WrapOptions { ephemeral: app.is_ephemeral(), expiration: None },
        )
        .map_err(|e| EngineError::Protocol(e.to_string()))?;
        self.pool.publish(&env).await.map_err(|e| EngineError::Relay(e.to_string()))?;
        Ok(())
    }

    /// Abre um rumor que veio endereçado à chave de um grupo.
    fn handle_group_rumor(self: &Arc<Self>, rumor: &UnsignedEvent, lid: &str, from_me: bool) -> Result<()> {
        let Some(g) = self.group(lid) else { return Ok(()) };
        let author = rumor.pubkey.to_hex();

        // O canal vem na marca `ch`. Se ele não existe mais, a mensagem é
        // descartada em vez de cair no canal errado.
        let canal = protocol::tag_value(rumor, "ch").map(|s| s.to_string());
        let canal = match canal {
            Some(c) if g.meta.channels.iter().any(|x| x.id == c) => c,
            Some(_) => return Ok(()),
            None => match g.main_channel() {
                Some(c) => c.id.clone(),
                None => return Ok(()),
            },
        };
        let chat = ChatKey::Channel { lid: lid.into(), channel: canal }.as_string();
        let ts_ms = rumor.created_at.as_secs() as i64 * 1000;

        match rumor.kind.as_u16() {
            KIND_TEXT => {
                let msg = Message {
                    id: rumor.id.map(|i| i.to_hex()).unwrap_or_default(),
                    chat,
                    author,
                    from_me,
                    ts: ts_ms,
                    body: MessageBody::Text(rumor.content.chars().take(6000).collect()),
                    status: from_me.then_some(Delivery::Sent),
                    reply: protocol::tag_value(rumor, "e").map(|s| s.to_string()),
                };
                let exp = protocol::expiration_of(rumor).map(|t| t.as_secs() as i64 * 1000);
                self.store_and_emit(msg, exp, !from_me)?;
            }
            KIND_APP => {
                let Some(app) = AppMessage::parse(&rumor.content) else { return Ok(()) };
                self.handle_group_app(app, g, &author, &chat, from_me, ts_ms)?;
            }
            _ => {}
        }
        Ok(())
    }

    fn handle_group_app(
        self: &Arc<Self>,
        app: AppMessage,
        mut g: Group,
        author: &str,
        chat: &str,
        from_me: bool,
        ts_ms: i64,
    ) -> Result<()> {
        match app {
            AppMessage::GroupJoin { n, p } if !from_me => {
                let nome = n.chars().take(40).collect::<String>();
                let novo = !g.meta.members.contains(&author.to_string());
                if !nome.is_empty() {
                    g.meta.names.insert(author.to_string(), nome.clone());
                }
                if let Some(pic) = p {
                    if !pic.is_empty() {
                        // Foto de perfil mostrada dentro do grupo.
                        let mut st = self.state.lock().unwrap();
                        if let Some(c) = st.contacts.get_mut(author) {
                            c.pic = Some(pic);
                        }
                    }
                }
                // Só admin muda a lista de membros, mas quem entra por convite
                // precisa aparecer para os outros.
                if novo && g.meta.members.len() < 64 {
                    g.meta.members.push(author.to_string());
                }
                self.save_group(&g)?;
                self.state.lock().unwrap().groups.insert(g.lid.clone(), g.clone());
                if novo {
                    let msg = Message {
                        id: format!("sys-join-{ts_ms}-{}", &author[..8.min(author.len())]),
                        chat: chat.into(),
                        author: author.into(),
                        from_me: false,
                        ts: ts_ms,
                        body: MessageBody::System(format!("{nome} entrou no grupo")),
                        status: None,
                        reply: None,
                    };
                    self.store_and_emit(msg, None, false)?;
                }
                let _ = self.events.send(EngineEvent::GroupsChanged);
            }
            AppMessage::GroupLeave if !from_me => {
                let nome = g.meta.name_of(author).unwrap_or("Alguém").to_string();
                g.meta.members.retain(|m| m != author);
                g.meta.admins.retain(|m| m != author);
                self.save_group(&g)?;
                self.state.lock().unwrap().groups.insert(g.lid.clone(), g.clone());
                let msg = Message {
                    id: format!("sys-leave-{ts_ms}-{}", &author[..8.min(author.len())]),
                    chat: chat.into(),
                    author: author.into(),
                    from_me: false,
                    ts: ts_ms,
                    body: MessageBody::System(format!("{nome} saiu do grupo")),
                    status: None,
                    reply: None,
                };
                self.store_and_emit(msg, None, false)?;
                let _ = self.events.send(EngineEvent::GroupsChanged);
            }
            AppMessage::GroupMeta { meta, ver } if !from_me => {
                // Só quem manda no grupo pode mudar os dados dele, e a versão
                // tem que ser mais nova: senão um membro comum reescreveria o
                // grupo inteiro, ou uma mensagem antiga desfaria uma mudança.
                if !g.meta.is_admin(author) || ver <= g.ver {
                    return Ok(());
                }
                let Ok(nova): std::result::Result<GroupMeta, _> = serde_json::from_value(meta) else {
                    return Ok(());
                };
                let Some(nova) = nova.sanitize(&g.meta.owner) else { return Ok(()) };
                if nova.owner != g.meta.owner && author != g.meta.owner {
                    return Ok(());
                }
                // Canais de texto que sumiram levam as mensagens junto.
                let removidos: Vec<String> = g
                    .meta
                    .text_channels()
                    .filter(|c| !nova.channels.iter().any(|x| x.id == c.id))
                    .map(|c| c.id.clone())
                    .collect();
                g.meta = nova;
                g.ver = ver;
                self.save_group(&g)?;
                self.state.lock().unwrap().groups.insert(g.lid.clone(), g.clone());

                for ch in removidos {
                    let key = ChatKey::Channel { lid: g.lid.clone(), channel: ch }.as_string();
                    let store = self.store.lock().unwrap();
                    store.delete_chat_msgs(&key)?;
                    store
                        .raw()
                        .execute("DELETE FROM chats WHERE key = ?1", rusqlite::params![key])
                        .map_err(crate::store::StoreError::from)?;
                }
                for ch in g.meta.text_channels() {
                    self.ensure_chat(&ChatKey::Channel { lid: g.lid.clone(), channel: ch.id.clone() }.as_string())?;
                }
                let _ = self.events.send(EngineEvent::GroupsChanged);
                let _ = self.events.send(EngineEvent::ChatsChanged);
            }
            AppMessage::GroupPic { p, v } if !from_me => {
                if !g.meta.is_admin(author) || v <= g.pic_v {
                    return Ok(());
                }
                g.pic = (!p.is_empty()).then_some(p);
                g.pic_v = v;
                self.save_group(&g)?;
                self.state.lock().unwrap().groups.insert(g.lid.clone(), g.clone());
                let _ = self.events.send(EngineEvent::GroupsChanged);
            }
            _ => {}
        }
        Ok(())
    }

    /// Guarda um convite recebido. A pessoa só entra quando aceitar.
    fn store_invite(
        &self,
        convite: crate::messages::GroupInvite,
        de: &str,
        chat: &str,
        ts_ms: i64,
    ) -> Result<()> {
        let Ok(meta): std::result::Result<GroupMeta, _> = serde_json::from_value(convite.meta.clone())
        else {
            return Ok(());
        };
        let Some(meta) = meta.sanitize("") else { return Ok(()) };
        let Ok(keys) = Keys::parse(&convite.sk) else { return Ok(()) };
        let pk = keys.public_key().to_hex();
        let lid = meta.lid.clone().unwrap_or_else(|| pk.clone());

        // Já participo: não há o que aceitar de novo.
        if self.state.lock().unwrap().groups.contains_key(&lid) {
            return Ok(());
        }

        let g = Group {
            lid: lid.clone(),
            pk,
            sk: convite.sk,
            old_keys: Vec::new(),
            meta,
            ver: convite.ver,
            pic: (!convite.pic.is_empty()).then_some(convite.pic),
            pic_v: convite.pic_v,
            muted: false,
            joined_at: 0, // 0 marca convite ainda não aceito
        };
        self.save_group(&g)?;
        self.state.lock().unwrap().groups.insert(lid.clone(), g.clone());

        let msg = Message {
            id: format!("inv-{lid}"),
            chat: chat.into(),
            author: de.into(),
            from_me: false,
            ts: ts_ms,
            body: MessageBody::Invite { lid, name: g.meta.name.clone() },
            status: None,
            reply: None,
        };
        self.store_and_emit(msg, None, true)?;
        let _ = self.events.send(EngineEvent::GroupsChanged);
        Ok(())
    }

    fn save_group(&self, g: &Group) -> Result<()> {
        let store = self.store.lock().unwrap();
        store
            .raw()
            .execute(
                "INSERT INTO groups (lid, pk, sk, old_keys, meta, ver, pic, pic_v, pics, gone, muted, joined_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,'{}','[]',?9,?10)
                 ON CONFLICT(lid) DO UPDATE SET pk=?2, sk=?3, old_keys=?4, meta=?5, ver=?6,
                                                pic=?7, pic_v=?8, muted=?9",
                rusqlite::params![
                    g.lid, g.pk, g.sk,
                    serde_json::to_string(&g.old_keys).unwrap_or_else(|_| "[]".into()),
                    serde_json::to_string(&g.meta).unwrap_or_default(),
                    g.ver as i64, g.pic, g.pic_v as i64, g.muted as i64, g.joined_at
                ],
            )
            .map_err(crate::store::StoreError::from)?;
        Ok(())
    }

    /// Reassina a caixa de entrada: usado quando a lista de grupos muda.
    async fn resubscribe(self: &Arc<Self>) {
        let since: i64 = self
            .store
            .lock()
            .unwrap()
            .get_kv("last_sync")
            .ok()
            .flatten()
            .unwrap_or_else(|| now_secs() - 86_400);
        let _ = self
            .pool
            .subscribe_inbox(&self.inbox_keys(), Timestamp::from_secs(since.max(0) as u64))
            .await;
    }

    /// Garante que a conversa aparece na lista, mesmo sem nenhuma mensagem.
    pub fn ensure_chat(&self, chat_key: &str) -> Result<()> {
        let key = ChatKey::parse(chat_key)
            .ok_or_else(|| EngineError::UnknownChat(chat_key.to_string()))?;
        let (kind, peer) = match &key {
            ChatKey::Direct { peer } => ("dm", Some(peer.clone())),
            ChatKey::Channel { .. } => ("group", None),
        };
        let store = self.store.lock().unwrap();
        store
            .raw()
            .execute(
                "INSERT INTO chats (key, kind, peer, last_ts, last_text)
                 VALUES (?1, ?2, ?3, ?4, '')
                 ON CONFLICT(key) DO UPDATE SET deleted = 0",
                rusqlite::params![chat_key, kind, peer, now_ms()],
            )
            .map_err(crate::store::StoreError::from)?;
        drop(store);
        let _ = self.events.send(EngineEvent::ChatsChanged);
        Ok(())
    }

    fn ensure_contact(&self, pk: &str, status: ContactStatus) -> Result<Contact> {
        let mut st = self.state.lock().unwrap();
        let entry = st
            .contacts
            .entry(pk.to_string())
            .or_insert_with(|| new_contact(pk, status));
        // Um contato já aceito não volta a ser pedido.
        if status == ContactStatus::Ok && entry.status == ContactStatus::Request {
            entry.status = ContactStatus::Ok;
        }
        let snapshot = entry.clone();
        drop(st);
        self.save_contact(&snapshot)?;
        Ok(snapshot)
    }

    fn save_contact(&self, c: &Contact) -> Result<()> {
        let store = self.store.lock().unwrap();
        store.raw().execute(
            "INSERT INTO contacts (pk, name, nick, note, pic, tone, status, verified, relays, prof_sent, added_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)
             ON CONFLICT(pk) DO UPDATE SET name=?2, nick=?3, note=?4, pic=?5, tone=?6, status=?7,
                                           verified=?8, relays=?9, prof_sent=?10",
            rusqlite::params![
                c.pk, c.name, c.nick, c.note, c.pic, c.tone,
                status_name(c.status), c.verified as i64,
                serde_json::to_string(&c.relays).unwrap_or_else(|_| "[]".into()),
                c.prof_sent as i64, c.added_at
            ],
        ).map_err(crate::store::StoreError::from)?;
        Ok(())
    }

    pub fn contact(&self, pk: &str) -> Option<Contact> {
        self.state.lock().unwrap().contacts.get(pk).cloned()
    }

    pub fn contacts(&self) -> Vec<Contact> {
        let mut v: Vec<Contact> = self.state.lock().unwrap().contacts.values().cloned().collect();
        v.sort_by_key(|c| c.display_name().to_lowercase());
        v
    }

    /// Nome de alguém, seja contato ou não.
    pub fn name_of(&self, pk: &str) -> String {
        if pk == self.keys.public_key().to_hex() {
            return if self.me.name.is_empty() { "Você".into() } else { self.me.name.clone() };
        }
        self.contact(pk).map(|c| c.display_name()).unwrap_or_else(|| {
            PublicKey::from_hex(pk)
                .map(|p| identity::short_id(&p))
                .unwrap_or_else(|_| pk[..8.min(pk.len())].to_string())
        })
    }

    pub fn set_contact_status(&self, pk: &str, status: ContactStatus) -> Result<()> {
        let mut st = self.state.lock().unwrap();
        if let Some(c) = st.contacts.get_mut(pk) {
            c.status = status;
            let snap = c.clone();
            drop(st);
            self.save_contact(&snap)?;
            let _ = self.events.send(EngineEvent::ContactChanged { pk: pk.into() });
            let _ = self.events.send(EngineEvent::ChatsChanged);
        }
        Ok(())
    }

    /// Conversas, as fixadas primeiro e depois por data.
    pub fn chat_list(&self) -> Result<Vec<ChatSummary>> {
        let store = self.store.lock().unwrap();
        let mut stmt = store.raw().prepare(
            "SELECT key, kind, peer, last_ts, last_text, last_status, last_from, unread, pinned, muted
             FROM chats WHERE deleted = 0 ORDER BY pinned DESC, last_ts DESC",
        ).map_err(crate::store::StoreError::from)?;
        let rows: Vec<(String, String, Option<String>, i64, String, Option<String>, Option<String>, u32, bool, bool)> =
            stmt.query_map([], |r| Ok((
                r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?,
                r.get::<_, i64>(7)? as u32, r.get::<_, i64>(8)? != 0, r.get::<_, i64>(9)? != 0,
            ))).map_err(crate::store::StoreError::from)?
            .collect::<std::result::Result<_, _>>()
            .map_err(crate::store::StoreError::from)?;
        drop(stmt);
        drop(store);

        let eu = self.keys.public_key().to_hex();
        Ok(rows
            .into_iter()
            .map(|(key, kind, peer, ts, text, status, from, unread, pinned, muted)| ChatSummary {
                title: match (&peer, ChatKey::parse(&key)) {
                    (Some(p), _) => self.name_of(p),
                    (None, Some(ChatKey::Channel { lid, channel })) => {
                        match self.group(&lid) {
                            Some(g) => {
                                let nome_canal = g
                                    .meta
                                    .channels
                                    .iter()
                                    .find(|c| c.id == channel)
                                    .map(|c| c.name.clone())
                                    .unwrap_or(channel);
                                format!("{} · #{}", g.meta.name, nome_canal)
                            }
                            None => "Grupo".into(),
                        }
                    }
                    _ => "Grupo".into(),
                },
                is_group: kind == "group",
                last_from_me: from.as_deref() == Some(eu.as_str()),
                key,
                last_text: text,
                last_ts: ts,
                last_status: status.as_deref().and_then(parse_delivery),
                unread,
                pinned,
                muted,
                peer,
            })
            .collect())
    }

    /// Página de mensagens de uma conversa.
    pub fn messages(&self, chat: &str, before_ts: i64, limit: usize) -> Result<Vec<Message>> {
        let eu = self.keys.public_key().to_hex();
        let store = self.store.lock().unwrap();
        Ok(store
            .msgs_by_chat(chat, before_ts, limit)?
            .into_iter()
            .map(|m| Message {
                from_me: m.author == eu,
                body: MessageBody::from_stored(&m.kind, &m.body),
                status: m.status.as_deref().and_then(parse_delivery),
                id: m.id,
                chat: m.chat,
                author: m.author,
                ts: m.ts,
                reply: m.reply,
            })
            .collect())
    }

    /// Marca a conversa como aberta: zera o não lido e confirma a leitura.
    pub async fn set_active(self: &Arc<Self>, chat: Option<&str>) -> Result<()> {
        {
            let mut st = self.state.lock().unwrap();
            st.active = chat.map(|s| s.to_string());
        }
        let Some(chat) = chat else { return Ok(()) };

        let pendentes: Vec<String> = {
            let store = self.store.lock().unwrap();
            store.raw().execute(
                "UPDATE chats SET unread = 0 WHERE key = ?1",
                rusqlite::params![chat],
            ).map_err(crate::store::StoreError::from)?;
            let eu = self.keys.public_key().to_hex();
            let mut stmt = store.raw().prepare(
                "SELECT id FROM msgs WHERE chat = ?1 AND author != ?2 ORDER BY ts DESC LIMIT 50",
            ).map_err(crate::store::StoreError::from)?;
            let v: Vec<String> = stmt
                .query_map(rusqlite::params![chat, eu], |r| r.get(0))
                .map_err(crate::store::StoreError::from)?
                .collect::<std::result::Result<_, _>>()
                .map_err(crate::store::StoreError::from)?;
            v
        };
        let _ = self.events.send(EngineEvent::ChatsChanged);

        if self.settings.read_receipts && !pendentes.is_empty() {
            if let Some(ChatKey::Direct { peer }) = ChatKey::parse(chat) {
                let _ = self
                    .send_app(
                        &peer,
                        &AppMessage::Receipt {
                            s: ReceiptKind::Read,
                            ids: pendentes,
                        },
                    )
                    .await;
            }
        }
        Ok(())
    }

    pub async fn relay_status(&self) -> Vec<RelayStatus> {
        self.pool.status().await
    }

    pub async fn shutdown(&self) {
        self.pool.shutdown().await;
    }
}

/* ---------------- apoio ---------------- */

fn new_contact(pk: &str, status: ContactStatus) -> Contact {
    let tone = u8::from_str_radix(&pk[..2.min(pk.len())], 16).unwrap_or(0) % 6;
    Contact {
        pk: pk.to_string(),
        name: String::new(),
        nick: String::new(),
        note: String::new(),
        pic: None,
        tone,
        status,
        verified: false,
        relays: Vec::new(),
        prof_sent: false,
        added_at: now_ms(),
    }
}

fn load_contacts(store: &Store) -> Result<HashMap<String, Contact>> {
    let mut stmt = store
        .raw()
        .prepare("SELECT pk, name, nick, note, pic, tone, status, verified, relays, prof_sent, added_at FROM contacts")
        .map_err(crate::store::StoreError::from)?;
    let rows: Vec<Contact> = stmt
        .query_map([], |r| {
            Ok(Contact {
                pk: r.get(0)?,
                name: r.get(1)?,
                nick: r.get(2)?,
                note: r.get(3)?,
                pic: r.get(4)?,
                tone: r.get::<_, i64>(5)? as u8,
                status: parse_status(&r.get::<_, String>(6)?),
                verified: r.get::<_, i64>(7)? != 0,
                relays: serde_json::from_str(&r.get::<_, String>(8)?).unwrap_or_default(),
                prof_sent: r.get::<_, i64>(9)? != 0,
                added_at: r.get(10)?,
            })
        })
        .map_err(crate::store::StoreError::from)?
        .collect::<std::result::Result<_, _>>()
        .map_err(crate::store::StoreError::from)?;
    Ok(rows.into_iter().map(|c| (c.pk.clone(), c)).collect())
}

fn load_groups(store: &Store) -> Result<HashMap<String, Group>> {
    let mut stmt = store
        .raw()
        .prepare("SELECT lid, pk, sk, old_keys, meta, ver, pic, pic_v, muted, joined_at FROM groups")
        .map_err(crate::store::StoreError::from)?;
    let linhas: Vec<Option<Group>> = stmt
        .query_map([], |r| {
            let meta_json: String = r.get(4)?;
            let old_json: String = r.get(3)?;
            let meta: Option<GroupMeta> = serde_json::from_str(&meta_json).ok();
            Ok(meta.map(|meta| Group {
                lid: r.get(0).unwrap_or_default(),
                pk: r.get(1).unwrap_or_default(),
                sk: r.get(2).unwrap_or_default(),
                old_keys: serde_json::from_str(&old_json).unwrap_or_default(),
                meta,
                ver: r.get::<_, i64>(5).unwrap_or(1) as u64,
                pic: r.get(6).unwrap_or(None),
                pic_v: r.get::<_, i64>(7).unwrap_or(0) as u64,
                muted: r.get::<_, i64>(8).unwrap_or(0) != 0,
                joined_at: r.get(9).unwrap_or(0),
            }))
        })
        .map_err(crate::store::StoreError::from)?
        .collect::<std::result::Result<_, _>>()
        .map_err(crate::store::StoreError::from)?;
    Ok(linhas
        .into_iter()
        .flatten()
        .map(|g| (g.lid.clone(), g))
        .collect())
}

fn upsert_chat(store: &Store, msg: &Message, count_unread: bool) -> Result<()> {
    let key = ChatKey::parse(&msg.chat);
    let (kind, peer) = match &key {
        Some(ChatKey::Direct { peer }) => ("dm", Some(peer.clone())),
        Some(ChatKey::Channel { .. }) => ("group", None),
        None => ("dm", None),
    };
    store
        .raw()
        .execute(
            "INSERT INTO chats (key, kind, peer, last_ts, last_id, last_from, last_kind, last_text, last_status, unread)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)
             ON CONFLICT(key) DO UPDATE SET
               last_ts = MAX(last_ts, ?4),
               last_id = CASE WHEN ?4 >= last_ts THEN ?5 ELSE last_id END,
               last_from = CASE WHEN ?4 >= last_ts THEN ?6 ELSE last_from END,
               last_kind = CASE WHEN ?4 >= last_ts THEN ?7 ELSE last_kind END,
               last_text = CASE WHEN ?4 >= last_ts THEN ?8 ELSE last_text END,
               last_status = CASE WHEN ?4 >= last_ts THEN ?9 ELSE last_status END,
               unread = unread + ?10,
               deleted = 0",
            rusqlite::params![
                msg.chat, kind, peer, msg.ts, msg.id, msg.author,
                msg.body.kind_name(), msg.body.preview(),
                msg.status.map(|s| delivery_name(s).to_string()),
                count_unread as i64
            ],
        )
        .map_err(crate::store::StoreError::from)?;
    Ok(())
}

fn status_name(s: ContactStatus) -> &'static str {
    match s {
        ContactStatus::Request => "request",
        ContactStatus::Ok => "ok",
        ContactStatus::Blocked => "blocked",
    }
}

fn parse_status(s: &str) -> ContactStatus {
    match s {
        "ok" => ContactStatus::Ok,
        "blocked" => ContactStatus::Blocked,
        _ => ContactStatus::Request,
    }
}

fn delivery_name(d: Delivery) -> &'static str {
    match d {
        Delivery::Pending => "pending",
        Delivery::Sent => "sent",
        Delivery::Delivered => "delivered",
        Delivery::Read => "read",
    }
}

fn parse_delivery(s: &str) -> Option<Delivery> {
    match s {
        "pending" => Some(Delivery::Pending),
        "sent" => Some(Delivery::Sent),
        "delivered" => Some(Delivery::Delivered),
        "read" => Some(Delivery::Read),
        _ => None,
    }
}

fn ttl_label(secs: u32) -> String {
    match secs {
        0 => "desligadas".into(),
        s if s < 3600 => format!("{} min", s / 60),
        s if s < 86400 => format!("{} h", s / 3600),
        s => format!("{} dias", s / 86400),
    }
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

pub fn now_secs() -> i64 {
    now_ms() / 1000
}
