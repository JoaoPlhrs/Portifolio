//! A aplicação: estado, mensagens da interface e desenho das telas.
//!
//! A janela nunca fala com a rede nem com o banco: ela chama o motor e escuta
//! os avisos dele. Toda operação demorada vira uma `Task`, que roda fora da
//! thread da interface.

use crate::theme as t;
use iced::widget::{
    button, column, container, row, scrollable, space, text, text_input, Space,
};
use iced::{Alignment, Element, Length, Subscription, Task};
use jm_core::engine::{
    ChatSummary, ContactStatus, Engine, EngineEvent, Message as CoreMessage, MessageBody, Settings,
};
use crate::calls::{CallInfo, CallManager, CallState};
use jm_core::groups::{Group, Template};
use jm_media::peer::CallConfig;
use jm_core::identity;
use jm_core::relays::RelayStatus;
use jm_core::store::Store;
use std::collections::HashMap;
use std::sync::Arc;

/// Em qual tela estamos.
pub enum Screen {
    /// Primeira vez: criar conta ou entrar com as 12 palavras.
    Welcome,
    /// Criando conta: escolhendo o nome.
    NewAccount { words: String },
    /// Entrando em uma conta existente.
    Restore,
    /// Tela principal.
    Main(Box<MainState>),
}

/// Qual seção está aberta na barra de navegação.
#[derive(Debug, Clone, PartialEq)]
pub enum Section {
    /// Conversas de duas pessoas.
    Direct,
    /// Um grupo, com seus canais.
    Group(String),
}

pub struct MainState {
    engine: Arc<Engine>,
    calls: Arc<CallManager>,
    call: Option<CallInfo>,
    /// Continua consultando por alguns instantes depois que a chamada some,
    /// para a tela não ficar presa em um estado antigo.
    calls_maybe_ringing: bool,
    section: Section,
    groups: Vec<Group>,
    new_group: Option<String>,
    chats: Vec<ChatSummary>,
    open_chat: Option<String>,
    messages: Vec<CoreMessage>,
    draft: String,
    relays: Vec<RelayStatus>,
    /// Quem está digitando, por conversa, até quando mostrar.
    typing: HashMap<String, i64>,
    add_contact: String,
    show_add: bool,
    show_invite: bool,
    error: Option<String>,
    copied: bool,
}

pub struct App {
    screen: Screen,
    name: String,
    input: String,
    error: Option<String>,
    busy: bool,
    db_path: std::path::PathBuf,
    relay_override: Option<Vec<String>>,
}

#[derive(Clone)]
pub enum Msg {
    // onboarding
    GoNewAccount,
    GoRestore,
    GoWelcome,
    NameChanged(String),
    InputChanged(String),
    ConfirmNewAccount,
    ConfirmRestore,
    Started(Result<StartedOk, String>),
    // principal
    FromEngine(Box<EngineEvent>),
    SelectChat(String),
    DraftChanged(String),
    SendDraft,
    Sent(Result<(), String>),
    ChatsReloaded(Vec<ChatSummary>),
    MessagesLoaded(String, Vec<CoreMessage>),
    ToggleAddContact,
    AddContactChanged(String),
    ConfirmAddContact,
    CopyMyCode,
    SelectSection(Section),
    ToggleNewGroup,
    NewGroupChanged(String),
    ConfirmNewGroup(Template),
    GroupsReloaded(Vec<Group>),
    AcceptInvite(String),
    StartCall(String),
    AcceptCall,
    DeclineCall,
    HangUp,
    ToggleMute,
    AcceptContact(String),
    BlockContact(String),
    CallUpdated(Option<CallInfo>),
    ToggleInvite,
    InviteContact(String, String),
    LeaveGroup(String),
    Tick,
    Noop,
}

/// O que volta quando o motor sobe.
#[derive(Clone)]
pub struct StartedOk {
    engine: Arc<Engine>,
    chats: Vec<ChatSummary>,
}

impl App {
    pub fn new(db_path: std::path::PathBuf, relay_override: Option<Vec<String>>) -> (Self, Task<Msg>) {
        // Se já existe uma conta neste computador, entra direto.
        let existing = Store::open(&db_path)
            .ok()
            .and_then(|s| s.get_kv::<String>("account_sk").ok().flatten());

        let app = Self {
            screen: Screen::Welcome,
            name: String::new(),
            input: String::new(),
            error: None,
            busy: existing.is_some(),
            db_path: db_path.clone(),
            relay_override: relay_override.clone(),
        };

        let task = match existing {
            Some(sk) => Task::perform(
                start_engine(db_path, sk, relay_override),
                Msg::Started,
            ),
            None => Task::none(),
        };
        (app, task)
    }

    pub fn title(&self) -> String {
        match &self.screen {
            Screen::Main(m) => match m.open_chat.as_ref().and_then(|k| m.chats.iter().find(|c| &c.key == k)) {
                Some(c) => format!("{} · J-Menseger", c.title),
                None => "J-Menseger".into(),
            },
            _ => "J-Menseger".into(),
        }
    }

    pub fn theme(&self) -> iced::Theme {
        iced::Theme::Dark
    }

    pub fn subscription(&self) -> Subscription<Msg> {
        // Um pulso lento mantém "digitando…" e os horários em dia sem gastar CPU.
        iced::time::every(std::time::Duration::from_secs(2)).map(|_| Msg::Tick)
    }

    pub fn update(&mut self, msg: Msg) -> Task<Msg> {
        match msg {
            Msg::GoNewAccount => {
                match identity::new_words() {
                    Ok(words) => {
                        self.error = None;
                        self.screen = Screen::NewAccount { words };
                    }
                    Err(e) => self.error = Some(e.to_string()),
                }
                Task::none()
            }
            Msg::GoRestore => {
                self.error = None;
                self.input.clear();
                self.screen = Screen::Restore;
                Task::none()
            }
            Msg::GoWelcome => {
                self.error = None;
                self.screen = Screen::Welcome;
                Task::none()
            }
            Msg::NameChanged(v) => {
                self.name = v;
                Task::none()
            }
            Msg::InputChanged(v) => {
                self.input = v;
                Task::none()
            }
            Msg::ConfirmNewAccount => {
                let Screen::NewAccount { words } = &self.screen else {
                    return Task::none();
                };
                if self.name.trim().len() < 2 {
                    self.error = Some("Escolha um nome com pelo menos 2 letras.".into());
                    return Task::none();
                }
                let (words, name, db) = (words.clone(), self.name.clone(), self.db_path.clone());
                let relays = self.relay_override.clone();
                self.busy = true;
                self.error = None;
                Task::perform(
                    async move {
                        let store = Store::open(&db).map_err(|e| e.to_string())?;
                        let keys =
                            Engine::create_identity(&store, &words, &name).map_err(|e| e.to_string())?;
                        let sk = keys.secret_key().to_secret_hex();
                        store.set_kv("account_sk", &sk).map_err(|e| e.to_string())?;
                        drop(store);
                        start_engine(db, sk, relays).await
                    },
                    Msg::Started,
                )
            }
            Msg::ConfirmRestore => {
                let entrada = self.input.trim().to_string();
                if entrada.is_empty() {
                    self.error = Some("Digite as 12 palavras ou a chave que começa com nsec1.".into());
                    return Task::none();
                }
                let nome = if self.name.trim().is_empty() {
                    "Eu".to_string()
                } else {
                    self.name.clone()
                };
                let db = self.db_path.clone();
                let relays = self.relay_override.clone();
                self.busy = true;
                self.error = None;
                Task::perform(
                    async move {
                        let store = Store::open(&db).map_err(|e| e.to_string())?;
                        let keys = Engine::restore_identity(&store, &entrada, &nome)
                            .map_err(|e| e.to_string())?;
                        let sk = keys.secret_key().to_secret_hex();
                        store.set_kv("account_sk", &sk).map_err(|e| e.to_string())?;
                        drop(store);
                        start_engine(db, sk, relays).await
                    },
                    Msg::Started,
                )
            }
            Msg::Started(Ok(ok)) => {
                self.busy = false;
                let engine = Arc::clone(&ok.engine);
                let groups = ok.engine.groups();
                let calls = CallManager::new(Arc::clone(&ok.engine), CallConfig::default());
                self.screen = Screen::Main(Box::new(MainState {
                    engine,
                    calls,
                    call: None,
                    calls_maybe_ringing: true,
                    section: Section::Direct,
                    groups,
                    new_group: None,
                    chats: ok.chats,
                    open_chat: None,
                    messages: Vec::new(),
                    draft: String::new(),
                    relays: Vec::new(),
                    typing: HashMap::new(),
                    add_contact: String::new(),
                    show_add: false,
                    show_invite: false,
                    error: None,
                    copied: false,
                }));
                Task::none()
            }
            Msg::Started(Err(e)) => {
                self.busy = false;
                self.error = Some(e);
                self.screen = Screen::Welcome;
                Task::none()
            }
            outras => self.update_main(outras),
        }
    }

    fn update_main(&mut self, msg: Msg) -> Task<Msg> {
        let Screen::Main(m) = &mut self.screen else {
            return Task::none();
        };
        match msg {
            Msg::FromEngine(ev) => match *ev {
                EngineEvent::ChatsChanged => {
                    let e = Arc::clone(&m.engine);
                    Task::perform(async move { e.chat_list().unwrap_or_default() }, Msg::ChatsReloaded)
                }
                EngineEvent::Message { chat, message } => {
                    if m.open_chat.as_deref() == Some(chat.as_str()) {
                        if !m.messages.iter().any(|x| x.id == message.id) {
                            m.messages.push(*message);
                            m.messages.sort_by_key(|x| x.ts);
                        }
                    }
                    Task::none()
                }
                EngineEvent::MessageUpdated { chat, id, status } => {
                    if m.open_chat.as_deref() == Some(chat.as_str()) {
                        if let Some(x) = m.messages.iter_mut().find(|x| x.id == id) {
                            x.status = Some(status);
                        }
                    }
                    Task::none()
                }
                EngineEvent::MessageDeleted { chat, id } => {
                    if m.open_chat.as_deref() == Some(chat.as_str()) {
                        m.messages.retain(|x| x.id != id);
                    }
                    Task::none()
                }
                EngineEvent::Typing { chat, .. } => {
                    m.typing.insert(chat, jm_core::engine::now_ms() + 6000);
                    Task::none()
                }
                EngineEvent::Relays(st) => {
                    m.relays = st;
                    Task::none()
                }
                EngineEvent::ContactChanged { .. } => {
                    let e = Arc::clone(&m.engine);
                    Task::perform(async move { e.chat_list().unwrap_or_default() }, Msg::ChatsReloaded)
                }
                EngineEvent::CallSignal { from, signal } => {
                    let c = Arc::clone(&m.calls);
                    Task::perform(
                        async move {
                            c.on_signal(&from, signal).await;
                            c.info().await
                        },
                        Msg::CallUpdated,
                    )
                }
                EngineEvent::MissedCall { .. } => {
                    let e = Arc::clone(&m.engine);
                    Task::perform(async move { e.chat_list().unwrap_or_default() }, Msg::ChatsReloaded)
                }
                EngineEvent::GroupsChanged => {
                    let e = Arc::clone(&m.engine);
                    let e2 = Arc::clone(&m.engine);
                    Task::batch([
                        Task::perform(async move { e.groups() }, Msg::GroupsReloaded),
                        Task::perform(async move { e2.chat_list().unwrap_or_default() }, Msg::ChatsReloaded),
                    ])
                }
            },
            Msg::ChatsReloaded(c) => {
                m.chats = c;
                Task::none()
            }
            Msg::SelectChat(key) => {
                m.open_chat = Some(key.clone());
                m.messages.clear();
                m.error = None;
                let e = Arc::clone(&m.engine);
                let k = key.clone();
                let carregar = Task::perform(
                    async move {
                        let msgs = e.messages(&k, i64::MAX, 100).unwrap_or_default();
                        (k, msgs)
                    },
                    |(k, v)| Msg::MessagesLoaded(k, v),
                );
                let e2 = Arc::clone(&m.engine);
                let marcar = Task::perform(
                    async move {
                        let _ = e2.set_active(Some(&key)).await;
                    },
                    |_| Msg::Noop,
                );
                Task::batch([carregar, marcar])
            }
            Msg::MessagesLoaded(key, msgs) => {
                if m.open_chat.as_deref() == Some(key.as_str()) {
                    m.messages = msgs;
                }
                Task::none()
            }
            Msg::DraftChanged(v) => {
                m.draft = v;
                Task::none()
            }
            Msg::SendDraft => {
                let texto = m.draft.trim().to_string();
                let Some(chat) = m.open_chat.clone() else {
                    return Task::none();
                };
                if texto.is_empty() {
                    return Task::none();
                }
                m.draft.clear();
                let e = Arc::clone(&m.engine);
                Task::perform(
                    async move { e.send_text(&chat, &texto).await.map(|_| ()).map_err(|e| e.to_string()) },
                    Msg::Sent,
                )
            }
            Msg::Sent(Err(e)) => {
                m.error = Some(e);
                Task::none()
            }
            Msg::Sent(Ok(())) => Task::none(),
            Msg::ToggleAddContact => {
                m.show_add = !m.show_add;
                m.error = None;
                m.copied = false;
                Task::none()
            }
            Msg::AddContactChanged(v) => {
                m.add_contact = v;
                Task::none()
            }
            Msg::ConfirmAddContact => {
                let codigo = m.add_contact.trim().to_string();
                if codigo.is_empty() {
                    return Task::none();
                }
                let e = Arc::clone(&m.engine);
                m.add_contact.clear();
                m.show_add = false;
                Task::perform(
                    async move { e.add_contact(&codigo).await.map(|_| ()).map_err(|e| e.to_string()) },
                    Msg::Sent,
                )
            }
            Msg::CopyMyCode => {
                m.copied = true;
                iced::clipboard::write(m.engine.npub())
            }
            Msg::SelectSection(sec) => {
                m.section = sec;
                m.open_chat = None;
                m.messages.clear();
                // Ao abrir um grupo, já entra no primeiro canal de texto.
                if let Section::Group(lid) = &m.section {
                    if let Some(g) = m.groups.iter().find(|g| &g.lid == lid) {
                        if let Some(ch) = g.meta.text_channels().next() {
                            let key = format!("g:{}:{}", g.lid, ch.id);
                            return Task::done(Msg::SelectChat(key));
                        }
                    }
                }
                Task::none()
            }
            Msg::ToggleNewGroup => {
                m.new_group = if m.new_group.is_some() { None } else { Some(String::new()) };
                Task::none()
            }
            Msg::NewGroupChanged(v) => {
                if let Some(n) = &mut m.new_group {
                    *n = v;
                }
                Task::none()
            }
            Msg::ConfirmNewGroup(tpl) => {
                let Some(nome) = m.new_group.clone() else { return Task::none() };
                let nome = nome.trim().to_string();
                if nome.len() < 2 {
                    m.error = Some("Dê um nome ao grupo.".into());
                    return Task::none();
                }
                m.new_group = None;
                let e = Arc::clone(&m.engine);
                Task::perform(
                    async move { e.create_group(&nome, tpl).await.map(|_| ()).map_err(|e| e.to_string()) },
                    Msg::Sent,
                )
            }
            Msg::ToggleInvite => {
                m.show_invite = !m.show_invite;
                Task::none()
            }
            Msg::InviteContact(lid, pk) => {
                m.show_invite = false;
                let e = Arc::clone(&m.engine);
                Task::perform(
                    async move {
                        e.invite_to_group(&lid, &pk).await.map(|_| ()).map_err(|e| e.to_string())
                    },
                    Msg::Sent,
                )
            }
            Msg::LeaveGroup(lid) => {
                m.section = Section::Direct;
                m.open_chat = None;
                m.messages.clear();
                let e = Arc::clone(&m.engine);
                Task::perform(
                    async move { e.leave_group(&lid).await.map(|_| ()).map_err(|e| e.to_string()) },
                    Msg::Sent,
                )
            }
            Msg::GroupsReloaded(g) => {
                m.groups = g;
                Task::none()
            }
            Msg::AcceptInvite(lid) => {
                let e = Arc::clone(&m.engine);
                Task::perform(
                    async move { e.accept_invite(&lid).await.map(|_| ()).map_err(|e| e.to_string()) },
                    Msg::Sent,
                )
            }
            Msg::AcceptContact(pk) => {
                let e = Arc::clone(&m.engine);
                Task::perform(
                    async move {
                        e.set_contact_status(&pk, ContactStatus::Ok)
                            .map_err(|e| e.to_string())?;
                        // Aceitar é também se apresentar: o outro lado passa a
                        // ver o seu nome em vez do código.
                        let _ = e.send_profile(&pk).await;
                        Ok(())
                    },
                    Msg::Sent,
                )
            }
            Msg::BlockContact(pk) => {
                m.open_chat = None;
                m.messages.clear();
                let e = Arc::clone(&m.engine);
                Task::perform(
                    async move {
                        e.set_contact_status(&pk, ContactStatus::Blocked)
                            .map_err(|e| e.to_string())
                    },
                    Msg::Sent,
                )
            }
            Msg::StartCall(pk) => {
                let c = Arc::clone(&m.calls);
                Task::perform(
                    async move {
                        if let Err(e) = c.start(&pk, false).await {
                            tracing::warn!("não deu para ligar: {}", e.label());
                        }
                        c.info().await
                    },
                    Msg::CallUpdated,
                )
            }
            Msg::AcceptCall => {
                let c = Arc::clone(&m.calls);
                Task::perform(
                    async move {
                        let _ = c.accept().await;
                        c.info().await
                    },
                    Msg::CallUpdated,
                )
            }
            Msg::DeclineCall => {
                let c = Arc::clone(&m.calls);
                Task::perform(async move { c.decline().await; c.info().await }, Msg::CallUpdated)
            }
            Msg::HangUp => {
                let c = Arc::clone(&m.calls);
                Task::perform(async move { c.hang_up().await; c.info().await }, Msg::CallUpdated)
            }
            Msg::ToggleMute => {
                let c = Arc::clone(&m.calls);
                let agora = m.call.as_ref().map(|c| c.muted).unwrap_or(false);
                Task::perform(
                    async move { c.set_muted(!agora).await; c.info().await },
                    Msg::CallUpdated,
                )
            }
            Msg::CallUpdated(info) => {
                m.call = info;
                Task::none()
            }
            Msg::Tick => {
                let agora = jm_core::engine::now_ms();
                m.typing.retain(|_, ate| *ate > agora);
                // A tela da chamada mostra duração e nível do microfone, que
                // mudam o tempo todo: só vale consultar enquanto há chamada.
                if m.call.is_some() || m.calls_maybe_ringing {
                    let c = Arc::clone(&m.calls);
                    return Task::perform(async move { c.info().await }, Msg::CallUpdated);
                }
                Task::none()
            }
            _ => Task::none(),
        }
    }

    pub fn view(&self) -> Element<'_, Msg> {
        match &self.screen {
            Screen::Welcome => self.view_welcome(),
            Screen::NewAccount { words } => self.view_new_account(words),
            Screen::Restore => self.view_restore(),
            Screen::Main(m) => view_main(m),
        }
    }

    /* ---------------- telas de entrada ---------------- */

    fn centered<'a>(&self, conteudo: Element<'a, Msg>) -> Element<'a, Msg> {
        container(
            container(conteudo)
                .max_width(520)
                .padding(32)
                .style(t::panel),
        )
        .width(Length::Fill)
        .height(Length::Fill)
        .center_x(Length::Fill)
        .center_y(Length::Fill)
        .style(t::fill(t::MAIN))
        .into()
    }

    fn erro(&self) -> Element<'_, Msg> {
        match &self.error {
            Some(e) => text(e.clone()).size(14).color(t::DANGER).into(),
            None => Space::new().into(),
        }
    }

    fn view_welcome(&self) -> Element<'_, Msg> {
        let corpo = column![
            text("J-Menseger").size(34).color(t::ACCENT),
            text("Mensagens cifradas de ponta a ponta, no seu computador.")
                .size(15)
                .color(t::FG_DIM),
            Space::new().height(22),
            if self.busy {
                Element::from(text("Abrindo sua conta…").size(15).color(t::FG_DIM))
            } else {
                Element::from(
                    column![
                        button(text("Criar uma conta nova").center())
                            .on_press(Msg::GoNewAccount)
                            .width(Length::Fill)
                            .padding(13)
                            .style(t::primary),
                        button(text("Já tenho conta: entrar com as 12 palavras").center())
                            .on_press(Msg::GoRestore)
                            .width(Length::Fill)
                            .padding(13)
                            .style(t::ghost),
                    ]
                    .spacing(10),
                )
            },
            Space::new().height(6),
            self.erro(),
            Space::new().height(10),
            text("Sua conta é um par de chaves criado neste computador. Não pedimos telefone nem e-mail.")
                .size(12.5)
                .color(t::FG_FAINT),
        ]
        .spacing(8);
        self.centered(corpo.into())
    }

    fn view_new_account(&self, words: &str) -> Element<'_, Msg> {
        let grade = words
            .split_whitespace()
            .enumerate()
            .fold(column![].spacing(6), |col, (i, w)| {
                col.push(
                    row![
                        text(format!("{:>2}.", i + 1)).size(13).color(t::FG_FAINT),
                        text(w.to_string()).size(14.5).color(t::FG),
                    ]
                    .spacing(8),
                )
            });

        let corpo = column![
            text("Sua chave de recuperação").size(24).color(t::FG),
            text("Anote estas 12 palavras no papel, na ordem. Elas são a sua conta: com elas você entra em outro aparelho, e sem elas ninguém recupera o acesso — nem nós.")
                .size(13.5)
                .color(t::FG_DIM),
            Space::new().height(12),
            container(grade).padding(14).style(t::panel),
            Space::new().height(14),
            text("Como você quer ser chamado").size(13).color(t::FG_DIM),
            text_input("Seu nome", &self.name)
                .on_input(Msg::NameChanged)
                .on_submit(Msg::ConfirmNewAccount)
                .padding(11)
                .style(t::input),
            Space::new().height(6),
            self.erro(),
            Space::new().height(8),
            row![
                button(text("Voltar").center())
                    .on_press(Msg::GoWelcome)
                    .padding(12)
                    .style(t::ghost),
                button(
                    text(if self.busy { "Criando…" } else { "Anotei, criar conta" }).center()
                )
                .on_press_maybe((!self.busy).then_some(Msg::ConfirmNewAccount))
                .width(Length::Fill)
                .padding(12)
                .style(t::primary),
            ]
            .spacing(10),
        ]
        .spacing(6);
        self.centered(scrollable(corpo).style(t::scroll).into())
    }

    fn view_restore(&self) -> Element<'_, Msg> {
        let corpo = column![
            text("Entrar na sua conta").size(24).color(t::FG),
            text("Digite as 12 palavras de recuperação, separadas por espaço. Também aceita uma chave que começa com nsec1.")
                .size(13.5)
                .color(t::FG_DIM),
            Space::new().height(14),
            text_input("palavra1 palavra2 palavra3 …", &self.input)
                .on_input(Msg::InputChanged)
                .padding(12)
                .style(t::input),
            Space::new().height(10),
            text("Seu nome (opcional)").size(13).color(t::FG_DIM),
            text_input("Como você quer ser chamado", &self.name)
                .on_input(Msg::NameChanged)
                .on_submit(Msg::ConfirmRestore)
                .padding(11)
                .style(t::input),
            Space::new().height(8),
            self.erro(),
            Space::new().height(8),
            row![
                button(text("Voltar").center())
                    .on_press(Msg::GoWelcome)
                    .padding(12)
                    .style(t::ghost),
                button(text(if self.busy { "Entrando…" } else { "Entrar" }).center())
                    .on_press_maybe((!self.busy).then_some(Msg::ConfirmRestore))
                    .width(Length::Fill)
                    .padding(12)
                    .style(t::primary),
            ]
            .spacing(10),
        ]
        .spacing(6);
        self.centered(corpo.into())
    }
}

/* ---------------- tela principal ---------------- */

fn view_main(m: &MainState) -> Element<'_, Msg> {
    row![rail(m), sidebar(m), conversation(m)]
        .height(Length::Fill)
        .into()
}

/// Coluna da esquerda: conversas diretas, grupos e o botão de criar grupo.
fn rail(m: &MainState) -> Element<'_, Msg> {
    let conectados = m.relays.iter().filter(|r| r.connected).count();
    let cor = if conectados > 0 { t::ACCENT } else { t::WARN };

    let mut itens = column![
        container(text("J").size(22).color(t::ACCENT_INK))
            .width(44)
            .height(44)
            .center_x(44)
            .center_y(44)
            .style(t::fill(t::ACCENT)),
        Space::new().height(8),
        button(text("💬").size(18).center())
            .width(44)
            .height(44)
            .style(t::rail_item(m.section == Section::Direct))
            .on_press(Msg::SelectSection(Section::Direct)),
    ]
    .align_x(Alignment::Center)
    .spacing(6);

    // Um ícone por grupo em que você já entrou.
    for g in m.groups.iter().filter(|g| g.joined_at > 0) {
        let aberto = m.section == Section::Group(g.lid.clone());
        itens = itens.push(
            button(text(t::initials(&g.meta.name)).size(14).center())
                .width(44)
                .height(44)
                .style(t::rail_item(aberto))
                .on_press(Msg::SelectSection(Section::Group(g.lid.clone()))),
        );
    }

    itens = itens.push(
        button(text("＋").size(18).center())
            .width(44)
            .height(44)
            .style(t::rail_item(false))
            .on_press(Msg::ToggleNewGroup),
    );
    itens = itens.push(Space::new().height(Length::Fill));
    itens = itens.push(container(text("●").size(13).color(cor)).width(44).center_x(44));
    itens = itens.push(Space::new().height(6));

    container(scrollable(itens).height(Length::Fill).style(t::scroll))
        .width(72)
        .height(Length::Fill)
        .padding(10)
        .style(t::fill(t::RAIL))
        .into()
}

/// Coluna do meio: conversas diretas ou os canais do grupo aberto.
fn sidebar(m: &MainState) -> Element<'_, Msg> {
    let (titulo, mut lista) = match &m.section {
        Section::Direct => ("Conversas".to_string(), direct_list(m)),
        Section::Group(lid) => match m.groups.iter().find(|g| &g.lid == lid) {
            Some(g) => (g.meta.name.clone(), channel_list(m, g)),
            None => ("Grupo".to_string(), column![]),
        },
    };

    if let Some(nome) = &m.new_group {
        let mut modelos = row![].spacing(5);
        for tpl in Template::all() {
            modelos = modelos.push(
                button(text(tpl.label()).size(11.5).center())
                    .on_press(Msg::ConfirmNewGroup(tpl))
                    .padding(6)
                    .style(t::ghost),
            );
        }
        lista = column![
            container(
                column![
                    text("Novo grupo").size(13).color(t::FG),
                    text_input("Nome do grupo", nome)
                        .on_input(Msg::NewGroupChanged)
                        .padding(9)
                        .size(13)
                        .style(t::input),
                    text("Escolha um modelo de canais:").size(11.5).color(t::FG_FAINT),
                    modelos,
                ]
                .spacing(7),
            )
            .padding(10)
            .style(t::panel),
            Space::new().height(8),
        ]
        .push(lista);
    }

    let conectados = m.relays.iter().filter(|r| r.connected).count();
    let total = m.relays.len();
    let estado = if total == 0 {
        "conectando…".to_string()
    } else if conectados == 0 {
        "sem conexão".to_string()
    } else {
        format!("{conectados} de {total} servidores")
    };

    container(
        column![
            container(text(titulo).size(15).color(t::FG))
                .padding(iced::Padding::from([14, 14]))
                .width(Length::Fill),
            scrollable(lista.padding(iced::Padding::from([0, 8])))
                .height(Length::Fill)
                .style(t::scroll),
            container(
                row![
                    container(text(t::initials(&m.engine.me().name)).size(13))
                        .width(34)
                        .height(34)
                        .center_x(34)
                        .center_y(34)
                        .style(t::avatar(m.engine.me().tone)),
                    column![
                        text(m.engine.me().name.clone()).size(13.5).color(t::FG),
                        text(estado).size(11.5).color(t::FG_FAINT),
                    ]
                    .spacing(1)
                    .width(Length::Fill),
                ]
                .spacing(9)
                .align_y(Alignment::Center),
            )
            .padding(10)
            .style(t::fill(t::RAIL)),
        ]
        .height(Length::Fill),
    )
    .width(260)
    .height(Length::Fill)
    .style(t::fill(t::SIDEBAR))
    .into()
}

/// Lista de conversas de duas pessoas.
fn direct_list(m: &MainState) -> iced::widget::Column<'_, Msg> {
    let mut lista = column![].spacing(2);

    if m.show_add {
        lista = lista.push(
            container(
                column![
                    text("Adicionar contato").size(13).color(t::FG),
                    text_input("Cole o código npub1…", &m.add_contact)
                        .on_input(Msg::AddContactChanged)
                        .on_submit(Msg::ConfirmAddContact)
                        .padding(9)
                        .size(13)
                        .style(t::input),
                    row![
                        button(text("Meu código").size(12).center())
                            .on_press(Msg::CopyMyCode)
                            .padding(7)
                            .style(t::ghost),
                        button(text("Adicionar").size(12).center())
                            .on_press(Msg::ConfirmAddContact)
                            .padding(7)
                            .style(t::primary),
                    ]
                    .spacing(6),
                    if m.copied {
                        Element::from(text("Código copiado.").size(11.5).color(t::ACCENT))
                    } else {
                        Element::from(Space::new())
                    },
                ]
                .spacing(7),
            )
            .padding(10)
            .style(t::panel),
        );
        lista = lista.push(Space::new().height(8));
    } else {
        lista = lista.push(
            button(text("＋  Adicionar contato").size(12.5))
                .on_press(Msg::ToggleAddContact)
                .width(Length::Fill)
                .padding(8)
                .style(t::ghost),
        );
        lista = lista.push(Space::new().height(4));
    }

    let diretas: Vec<&ChatSummary> = m.chats.iter().filter(|c| !c.is_group).collect();
    if diretas.is_empty() {
        lista = lista.push(
            container(
                text("Nenhuma conversa ainda. Adicione alguém pelo código.")
                    .size(12.5)
                    .color(t::FG_FAINT),
            )
            .padding(12),
        );
    }
    for c in diretas {
        lista = lista.push(chat_row(m, c, &c.title));
    }
    lista
}

/// Canais do grupo aberto, separados por tipo.
fn channel_list<'a>(m: &'a MainState, g: &'a Group) -> iced::widget::Column<'a, Msg> {
    let mut lista = column![].spacing(2);

    let admin = g.meta.is_admin(&m.engine.public_key().to_hex());
    lista = lista.push(
        row![
            button(text(if m.show_invite { "Fechar" } else { "＋ Convidar" }).size(12).center())
                .on_press(Msg::ToggleInvite)
                .padding(7)
                .style(t::ghost),
            button(text("Sair").size(12).center())
                .on_press(Msg::LeaveGroup(g.lid.clone()))
                .padding(7)
                .style(t::ghost),
        ]
        .spacing(6),
    );

    if m.show_invite {
        let mut picks = column![text("Quem você quer convidar?").size(12).color(t::FG_DIM)].spacing(4);
        let candidatos: Vec<_> = m
            .engine
            .contacts()
            .into_iter()
            .filter(|c| c.status == ContactStatus::Ok && !g.meta.members.contains(&c.pk))
            .collect();
        if candidatos.is_empty() {
            picks = picks.push(
                text("Nenhum contato disponível. Adicione alguém pelo código primeiro.")
                    .size(11.5)
                    .color(t::FG_FAINT),
            );
        }
        for c in candidatos {
            picks = picks.push(
                button(text(c.display_name()).size(13))
                    .on_press(Msg::InviteContact(g.lid.clone(), c.pk.clone()))
                    .width(Length::Fill)
                    .padding(7)
                    .style(t::row_item(false)),
            );
        }
        lista = lista.push(Space::new().height(6));
        lista = lista.push(container(picks).padding(10).style(t::panel));
    }

    if admin {
        lista = lista.push(Space::new().height(4));
    }

    lista = lista.push(
        container(text("CANAIS DE TEXTO").size(11).color(t::FG_FAINT))
            .padding(iced::Padding::from([8, 6])),
    );
    for ch in g.meta.text_channels() {
        let key = format!("g:{}:{}", g.lid, ch.id);
        match m.chats.iter().find(|c| c.key == key) {
            Some(c) => lista = lista.push(chat_row(m, c, &format!("# {}", ch.name))),
            None => {
                lista = lista.push(
                    button(text(format!("# {}", ch.name)).size(14))
                        .on_press(Msg::SelectChat(key))
                        .width(Length::Fill)
                        .padding(8)
                        .style(t::row_item(false)),
                )
            }
        }
    }

    let vozes: Vec<_> = g.meta.voice_channels().collect();
    if !vozes.is_empty() {
        lista = lista.push(
            container(text("CANAIS DE VOZ").size(11).color(t::FG_FAINT))
                .padding(iced::Padding { top: 12.0, right: 6.0, bottom: 4.0, left: 6.0 }),
        );
        for ch in vozes {
            // A sala de voz ainda não está ligada: o botão não finge funcionar.
            lista = lista.push(
                container(
                    row![
                        text("🔊").size(13),
                        text(ch.name.clone()).size(14).color(t::FG_FAINT),
                        space().width(Length::Fill),
                        text("em breve").size(10.5).color(t::FG_FAINT),
                    ]
                    .spacing(8)
                    .align_y(Alignment::Center),
                )
                .padding(8)
                .width(Length::Fill),
            );
        }
    }

    let n = g.meta.members.len();
    lista = lista.push(Space::new().height(12));
    lista = lista.push(
        container(
            text(format!("{n} {}", if n == 1 { "membro" } else { "membros" }))
                .size(11.5)
                .color(t::FG_FAINT),
        )
        .padding(iced::Padding::from([4, 6])),
    );
    for pk in g.meta.members.iter().take(30) {
        let nome = g
            .meta
            .name_of(pk)
            .map(|s| s.to_string())
            .unwrap_or_else(|| m.engine.name_of(pk));
        let papel = if g.meta.owner == *pk {
            " · dono"
        } else if g.meta.is_admin(pk) {
            " · admin"
        } else {
            ""
        };
        lista = lista.push(
            container(
                row![
                    container(text(t::initials(&nome)).size(11))
                        .width(26)
                        .height(26)
                        .center_x(26)
                        .center_y(26)
                        .style(t::avatar(0)),
                    text(format!("{nome}{papel}")).size(13).color(t::FG_DIM),
                ]
                .spacing(8)
                .align_y(Alignment::Center),
            )
            .padding(iced::Padding::from([4, 6])),
        );
    }
    lista
}

/// Uma linha da lista, usada tanto para conversas quanto para canais.
fn chat_row<'a>(m: &'a MainState, c: &'a ChatSummary, rotulo: &str) -> Element<'a, Msg> {
    let selecionado = m.open_chat.as_deref() == Some(c.key.as_str());
    let digitando = m
        .typing
        .get(&c.key)
        .is_some_and(|ate| *ate > jm_core::engine::now_ms());

    let previa = if digitando {
        text("digitando…").size(12.5).color(t::ACCENT)
    } else if c.last_text.is_empty() {
        text("sem mensagens").size(12.5).color(t::FG_FAINT)
    } else {
        let p = if c.last_from_me {
            format!("Você: {}", c.last_text)
        } else {
            c.last_text.clone()
        };
        text(ellipsis(&p, 32)).size(12.5).color(t::FG_FAINT)
    };

    let mut linha = row![].spacing(10).align_y(Alignment::Center);
    if !c.is_group {
        linha = linha.push(
            container(text(t::initials(rotulo)).size(13))
                .width(36)
                .height(36)
                .center_x(36)
                .center_y(36)
                .style(t::avatar(0)),
        );
    }
    linha = linha.push(
        column![text(ellipsis(rotulo, 24)).size(14.5).color(t::FG), previa]
            .spacing(2)
            .width(Length::Fill),
    );
    if c.unread > 0 {
        linha = linha.push(
            container(text(c.unread.to_string()).size(11))
                .padding(iced::Padding::from([2, 7]))
                .style(t::badge),
        );
    }

    button(linha)
        .on_press(Msg::SelectChat(c.key.clone()))
        .width(Length::Fill)
        .padding(8)
        .style(t::row_item(selecionado))
        .into()
}

/// Coluna principal: cabeçalho, mensagens e campo de escrita.
fn conversation(m: &MainState) -> Element<'_, Msg> {
    let Some(chave) = m.open_chat.as_deref() else {
        return container(
            column![
                text("J-Menseger").size(26).color(t::ACCENT),
                text("Escolha uma conversa à esquerda, ou adicione alguém pelo código.")
                    .size(14)
                    .color(t::FG_DIM),
            ]
            .spacing(8)
            .align_x(Alignment::Center),
        )
        .width(Length::Fill)
        .height(Length::Fill)
        .center_x(Length::Fill)
        .center_y(Length::Fill)
        .style(t::fill(t::MAIN))
        .into();
    };

    let atual = m.chats.iter().find(|c| c.key == chave);
    let titulo = atual.map(|c| c.title.clone()).unwrap_or_else(|| "Conversa".into());

    let mut acoes = row![].spacing(8).align_y(Alignment::Center);
    // Ligar só faz sentido em conversa de duas pessoas, e só com contato aceito.
    if let Some(jm_core::messages::ChatKey::Direct { peer }) =
        jm_core::messages::ChatKey::parse(chave)
    {
        let aceito = m
            .engine
            .contact(&peer)
            .map(|c| c.status == ContactStatus::Ok)
            .unwrap_or(false);
        if aceito && m.call.is_none() {
            acoes = acoes.push(
                button(text("📞 Ligar").size(12.5).center())
                    .on_press(Msg::StartCall(peer))
                    .padding(8)
                    .style(t::ghost),
            );
        }
    }
    acoes = acoes.push(text("cifrada de ponta a ponta").size(12).color(t::FG_FAINT));

    let cabecalho = container(
        row![
            text(titulo).size(16).color(t::FG),
            space().width(Length::Fill),
            acoes,
        ]
        .align_y(Alignment::Center),
    )
    .width(Length::Fill)
    .padding(iced::Padding::from([13, 18]))
    .style(t::header);

    let mut lista = column![].spacing(3).padding(iced::Padding::from([14, 18]));
    let mut autor_anterior: Option<&str> = None;

    for msg in &m.messages {
        match &msg.body {
            MessageBody::System(texto) => {
                autor_anterior = None;
                lista = lista.push(
                    container(
                        container(text(texto.clone()).size(12.5))
                            .padding(iced::Padding::from([5, 12]))
                            .style(t::system_note),
                    )
                    .width(Length::Fill)
                    .center_x(Length::Fill),
                );
            }
            corpo => {
                let primeira = autor_anterior != Some(msg.author.as_str());
                autor_anterior = Some(&msg.author);

                if let MessageBody::Invite { lid, name } = corpo {
                    let ja_entrou = m.groups.iter().any(|g| &g.lid == lid && g.joined_at > 0);
                    lista = lista.push(
                        container(
                            column![
                                text(format!("Convite para o grupo {name}")).size(14).color(t::FG),
                                if ja_entrou {
                                    Element::from(text("Você já está neste grupo.").size(12).color(t::FG_FAINT))
                                } else {
                                    Element::from(
                                        button(text("Entrar no grupo").size(13).center())
                                            .on_press(Msg::AcceptInvite(lid.clone()))
                                            .padding(9)
                                            .style(t::primary),
                                    )
                                },
                            ]
                            .spacing(8),
                        )
                        .padding(12)
                        .max_width(380)
                        .style(t::panel),
                    );
                    continue;
                }
                let conteudo = match corpo {
                    MessageBody::Text(txt) => txt.clone(),
                    MessageBody::Sound { id } => format!("🔊 Som do soundboard: {id}"),
                    MessageBody::Deleted => "Mensagem apagada".into(),
                    MessageBody::Invite { .. } | MessageBody::System(_) => unreachable!(),
                };

                let marca = match msg.status {
                    Some(jm_core::messages::Delivery::Pending) => " ⏳",
                    Some(jm_core::messages::Delivery::Sent) => " ✓",
                    Some(jm_core::messages::Delivery::Delivered) => " ✓✓",
                    Some(jm_core::messages::Delivery::Read) => " ✓✓",
                    None => "",
                };
                let lida = msg.status == Some(jm_core::messages::Delivery::Read);

                let balao = container(
                    column![
                        text(conteudo).size(14.5).color(t::FG),
                        row![
                            text(t::hhmm(msg.ts)).size(10.5).color(t::FG_FAINT),
                            text(marca)
                                .size(10.5)
                                .color(if lida { t::ACCENT } else { t::FG_FAINT }),
                        ]
                        .spacing(2),
                    ]
                    .spacing(2),
                )
                .padding(iced::Padding::from([7, 11]))
                .max_width(560)
                .style(t::bubble(msg.from_me));

                if primeira && !msg.from_me {
                    lista = lista.push(Space::new().height(5));
                    if let Some(jm_core::messages::ChatKey::Channel { lid, .. }) =
                        jm_core::messages::ChatKey::parse(&msg.chat)
                    {
                        let nome = m
                            .groups
                            .iter()
                            .find(|g| g.lid == lid)
                            .and_then(|g| g.meta.name_of(&msg.author).map(|s| s.to_string()))
                            .unwrap_or_else(|| m.engine.name_of(&msg.author));
                        lista = lista.push(
                            container(text(nome).size(12).color(t::ACCENT))
                                .padding(iced::Padding::from([0, 2])),
                        );
                    }
                }

                lista = lista.push(if msg.from_me {
                    Element::from(
                        row![space().width(Length::Fill), balao].width(Length::Fill),
                    )
                } else {
                    Element::from(row![balao, space().width(Length::Fill)].width(Length::Fill))
                });
            }
        }
    }

    let painel_chamada: Element<'_, Msg> = match &m.call {
        Some(info) => call_panel(m, info),
        None => Element::from(Space::new()),
    };

    let aviso = match &m.error {
        Some(e) => Element::from(
            container(text(e.clone()).size(12.5).color(t::DANGER))
                .padding(iced::Padding::from([6, 18])),
        ),
        None => Element::from(Space::new()),
    };

    // Conversa de alguém que ainda não está nos seus contatos: antes de
    // responder, a pessoa decide se aceita. Até aceitar, nada é confirmado
    // para o outro lado.
    let pedido: Option<String> = match jm_core::messages::ChatKey::parse(chave) {
        Some(jm_core::messages::ChatKey::Direct { peer }) => m
            .engine
            .contact(&peer)
            .filter(|c| c.status == ContactStatus::Request)
            .map(|_| peer),
        _ => None,
    };

    if let Some(pk) = pedido {
        let nome = m.engine.name_of(&pk);
        let barra = container(
            column![
                text(format!(
                    "{nome} não está nos seus contatos. Ninguém sabe que você viu esta conversa até aceitar."
                ))
                .size(13)
                .color(t::FG_DIM),
                row![
                    button(text("Bloquear").size(13).center())
                        .on_press(Msg::BlockContact(pk.clone()))
                        .padding(10)
                        .style(t::ghost),
                    button(text("Aceitar").size(13).center())
                        .on_press(Msg::AcceptContact(pk))
                        .width(Length::Fill)
                        .padding(10)
                        .style(t::primary),
                ]
                .spacing(10),
            ]
            .spacing(10),
        )
        .width(Length::Fill)
        .padding(iced::Padding::from([12, 18]))
        .style(t::fill(t::SURFACE));

        return container(
            column![
                cabecalho,
                painel_chamada,
                scrollable(lista)
                    .height(Length::Fill)
                    .anchor_bottom()
                    .style(t::scroll),
                aviso,
                barra,
            ]
            .height(Length::Fill),
        )
        .width(Length::Fill)
        .height(Length::Fill)
        .style(t::fill(t::MAIN))
        .into();
    }

    let escrita = container(
        row![
            text_input("Mensagem", &m.draft)
                .on_input(Msg::DraftChanged)
                .on_submit(Msg::SendDraft)
                .padding(12)
                .style(t::input),
            button(text("Enviar").center())
                .on_press(Msg::SendDraft)
                .padding(12)
                .style(t::primary),
        ]
        .spacing(9)
        .align_y(Alignment::Center),
    )
    .padding(iced::Padding::from([10, 16]))
    .style(t::fill(t::MAIN));

    container(
        column![
            cabecalho,
            painel_chamada,
            scrollable(lista)
                .height(Length::Fill)
                .anchor_bottom()
                .style(t::scroll),
            aviso,
            escrita,
        ]
        .height(Length::Fill),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .style(t::fill(t::MAIN))
    .into()
}

/// Faixa da chamada em andamento, no topo da conversa.
fn call_panel<'a>(m: &'a MainState, info: &'a CallInfo) -> Element<'a, Msg> {
    let nome = m.engine.name_of(&info.peer);

    let (situacao, botoes): (String, Element<'a, Msg>) = match &info.state {
        CallState::Calling => (
            format!("Chamando {nome}…"),
            Element::from(
                button(text("Cancelar").size(12.5).center())
                    .on_press(Msg::HangUp)
                    .padding(8)
                    .style(t::danger),
            ),
        ),
        CallState::Ringing { video } => (
            format!(
                "{nome} está {}",
                if *video { "chamando em vídeo" } else { "ligando" }
            ),
            Element::from(
                row![
                    button(text("Atender").size(12.5).center())
                        .on_press(Msg::AcceptCall)
                        .padding(8)
                        .style(t::primary),
                    button(text("Recusar").size(12.5).center())
                        .on_press(Msg::DeclineCall)
                        .padding(8)
                        .style(t::danger),
                ]
                .spacing(8),
            ),
        ),
        CallState::Connecting => (
            "Conectando…".into(),
            Element::from(
                button(text("Desligar").size(12.5).center())
                    .on_press(Msg::HangUp)
                    .padding(8)
                    .style(t::danger),
            ),
        ),
        CallState::Active { since } => (
            format!(
                "Em chamada com {nome} · {}",
                crate::calls::duration_label(*since)
            ),
            Element::from(
                row![
                    button(
                        text(if info.muted { "🔇 Ativar microfone" } else { "🎙 Mudo" })
                            .size(12.5)
                            .center()
                    )
                    .on_press(Msg::ToggleMute)
                    .padding(8)
                    .style(t::ghost),
                    button(text("Desligar").size(12.5).center())
                        .on_press(Msg::HangUp)
                        .padding(8)
                        .style(t::danger),
                ]
                .spacing(8),
            ),
        ),
        CallState::Reconnecting => (
            "A conexão caiu, tentando voltar…".into(),
            Element::from(
                button(text("Desligar").size(12.5).center())
                    .on_press(Msg::HangUp)
                    .padding(8)
                    .style(t::danger),
            ),
        ),
        CallState::Ended { reason } => (reason.label().to_string(), Element::from(Space::new())),
    };

    // Medidor do microfone: mostra que o computador está mesmo captando som.
    let medidor: Element<'_, Msg> = if matches!(info.state, CallState::Active { .. }) {
        let cheio = (info.mic_level.clamp(0.0, 1.0) * 60.0).max(1.0);
        Element::from(
            row![
                container(Space::new())
                    .width(cheio)
                    .height(5.0)
                    .style(t::fill(if info.muted { t::FG_FAINT } else { t::ACCENT })),
                container(Space::new())
                    .width((60.0 - cheio).max(1.0))
                    .height(5.0)
                    .style(t::fill(t::SURFACE_HI)),
            ]
            .spacing(0),
        )
    } else {
        Element::from(Space::new())
    };

    let falando: Element<'_, Msg> = if info.remote_speaking {
        Element::from(text(format!("{nome} está falando")).size(11.5).color(t::ACCENT))
    } else {
        Element::from(Space::new())
    };

    container(
        row![
            column![
                text(situacao).size(14).color(t::FG),
                row![medidor, falando].spacing(10).align_y(Alignment::Center),
            ]
            .spacing(5)
            .width(Length::Fill),
            botoes,
        ]
        .spacing(12)
        .align_y(Alignment::Center),
    )
    .width(Length::Fill)
    .padding(iced::Padding::from([10, 18]))
    .style(t::fill(t::SURFACE))
    .into()
}

fn ellipsis(s: &str, max: usize) -> String {
    let limpo = s.replace('\n', " ");
    if limpo.chars().count() <= max {
        limpo
    } else {
        format!("{}…", limpo.chars().take(max.saturating_sub(1)).collect::<String>())
    }
}

/* ---------------- ponte com o motor ---------------- */

async fn start_engine(
    db: std::path::PathBuf,
    sk_hex: String,
    relays: Option<Vec<String>>,
) -> Result<StartedOk, String> {
    let store = Store::open(&db).map_err(|e| e.to_string())?;
    if let Some(r) = relays {
        let mut s: Settings = store.get_kv("settings").ok().flatten().unwrap_or_default();
        s.relays = r;
        store.set_kv("settings", &s).map_err(|e| e.to_string())?;
    }
    let keys = jm_core::identity::keys_from_any(&sk_hex).map_err(|e| e.to_string())?;
    let (engine, rx) = Engine::open(keys, store).map_err(|e| e.to_string())?;
    engine.start().await.map_err(|e| e.to_string())?;
    let chats = engine.chat_list().unwrap_or_default();

    // Entrega os avisos do motor para a interface.
    crate::bridge::install(rx);

    Ok(StartedOk { engine, chats })
}

/// Marca que um contato existe, usado pela interface ao abrir um perfil.
pub fn status_label(s: ContactStatus) -> &'static str {
    match s {
        ContactStatus::Request => "pedido de conversa",
        ContactStatus::Ok => "contato",
        ContactStatus::Blocked => "bloqueado",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corta_texto_longo_com_reticencias() {
        assert_eq!(ellipsis("curto", 10), "curto");
        assert_eq!(ellipsis("1234567890", 10), "1234567890");
        assert_eq!(ellipsis("12345678901", 10), "123456789…");
        assert_eq!(ellipsis("linha\nquebrada", 20), "linha quebrada");
        // Não pode partir caractere de vários bytes no meio.
        let acentos = "áéíóúãõçàâ e mais";
        assert!(ellipsis(acentos, 5).chars().count() <= 5);
    }

    #[test]
    fn nomes_de_situacao_do_contato() {
        assert_eq!(status_label(ContactStatus::Ok), "contato");
        assert_eq!(status_label(ContactStatus::Blocked), "bloqueado");
    }
}

impl std::fmt::Debug for Msg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Msg")
    }
}

impl std::fmt::Debug for StartedOk {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("StartedOk")
    }
}
