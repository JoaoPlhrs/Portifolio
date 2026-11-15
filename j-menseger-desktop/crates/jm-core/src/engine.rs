//! O motor: amarra identidade, servidores e banco, e avisa a interface.
//!
//! A interface nunca fala com a rede nem com o banco diretamente. Ela chama
//! métodos daqui e escuta [`EngineEvent`]. Assim a janela nunca trava
//! esperando um servidor responder.

use crate::identity;
use crate::messages::{AppMessage, ChatKey, Delivery, ReceiptKind};
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
    Deleted,
}

impl MessageBody {
    fn kind_name(&self) -> &'static str {
        match self {
            MessageBody::Text(_) => "text",
            MessageBody::System(_) => "sys",
            MessageBody::Sound { .. } => "sound",
            MessageBody::Deleted => "deleted",
        }
    }
    fn to_json(&self) -> String {
        match self {
            MessageBody::Text(t) => serde_json::json!({ "text": t }).to_string(),
            MessageBody::System(t) => serde_json::json!({ "text": t }).to_string(),
            MessageBody::Sound { id } => serde_json::json!({ "id": id }).to_string(),
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
            _ => MessageBody::Deleted,
        }
    }

    /// Resumo curto para a lista de conversas.
    pub fn preview(&self) -> String {
        match self {
            MessageBody::Text(t) => t.clone(),
            MessageBody::System(t) => t.clone(),
            MessageBody::Sound { .. } => "Som do soundboard".into(),
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

        let (tx, rx) = mpsc::unbounded_channel();
        let engine = Arc::new(Self {
            pool: Arc::new(RelayPool::new(keys.clone())),
            keys,
            me,
            settings,
            store: Arc::new(Mutex::new(store)),
            state: Arc::new(Mutex::new(State {
                contacts,
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
                &[self.keys.public_key()],
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

        let rumor = protocol::unwrap_event(event, &self.keys)
            .map_err(|e| EngineError::Protocol(e.to_string()))?;

        let author = rumor.pubkey.to_hex();
        let from_me = rumor.pubkey == self.keys.public_key();

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
        let ChatKey::Direct { peer } = &key else {
            return Err(EngineError::Protocol(
                "envio para canal de grupo ainda não ligado nesta versão".into(),
            ));
        };
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
                title: peer.as_deref().map(|p| self.name_of(p)).unwrap_or_else(|| "Grupo".into()),
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
