//! Chamadas: tela da chamada, chamada chegando, sala de voz e o painel
//! "voz conectada" da barra lateral.

use crate::app::Msg;
use crate::calls::{duration_label, CallInfo, CallState, RoomInfo, SoundBanner};
use crate::icons::{icon, Icon};
use crate::state::*;
use crate::theme::{self as t, CallBtn};
use crate::ui::{self, El, BOLD, SEMI};
use iced::widget::{button, column, container, image, row, stack, text, tooltip, Space};
use iced::{Alignment, ContentFit, Length, Padding};

fn round_btn<'a>(i: Icon, kind: CallBtn, label: &str, msg: Option<Msg>) -> El<'a> {
    let cor = match kind {
        CallBtn::On => t::rgb(0x11, 0x13, 0x16),
        CallBtn::Accept => t::accent_ink(),
        _ => t::FG,
    };
    ui::tip(button(container(icon(i, 22.0, cor)).center(22)).padding(14).style(t::call_btn(kind)).on_press_maybe(msg), label.to_string(), tooltip::Position::Top)
}

fn banner<'a>(b: &'a SoundBanner) -> El<'a> {
    let mut r = row![text(b.emoji.clone()).size(22), text(format!("{} tocou {}", b.who, b.name)).size(13.5).font(SEMI).color(t::FG)]
        .spacing(10)
        .align_y(Alignment::Center);
    if b.save.is_some() {
        r = r.push(ui::ghost(Some(Icon::Plus), "Salvar no meu soundboard", Some(Msg::SaveBannerSound)));
    }
    container(r).padding([6, 14]).style(t::toast).into()
}

/// Chamada de duas pessoas, em cima da conversa.
pub fn call_panel<'a>(m: &'a Main, c: &'a CallInfo) -> El<'a> {
    let nome = m.name_of(&c.peer);
    let estado: String = match &c.state {
        CallState::Calling => "Chamando…".into(),
        CallState::Ringing { .. } => "Tocando…".into(),
        CallState::Connecting => "Conectando…".into(),
        CallState::Active { since } => duration_label(*since),
        CallState::Reconnecting => "Reconectando…".into(),
        CallState::Ended { reason } => reason.label().into(),
    };
    let acabou = matches!(c.state, CallState::Ended { .. });
    let ativo = matches!(c.state, CallState::Active { .. } | CallState::Reconnecting);

    let palco: El<'a> = match (&m.remote_frame, c.receiving_video) {
        (Some((_, h)), true) => container(image(h.clone()).content_fit(ContentFit::Contain).width(Length::Fill).height(Length::Fill))
            .height(Length::Fixed((m.size.height * 0.48).clamp(220.0, 560.0)))
            .width(Length::Fill)
            .style(t::rounded(iced::Color::BLACK, 12.0))
            .into(),
        _ => {
            let anel = (c.remote_speaking && !c.remote_muted).then(t::accent);
            let mut nome_linha = row![text(nome.clone()).size(18).font(BOLD).color(t::FG)].spacing(6).align_y(Alignment::Center);
            if c.remote_muted && ativo {
                nome_linha = nome_linha.push(ui::tip(icon(Icon::MicOff, 15.0, t::DANGER), "Microfone desligado", tooltip::Position::Right));
            }
            column![
                ui::ringed(ui::avatar(&nome, m.tone_of(&c.peer), m.pic_of(&c.peer), 84.0), 84.0, anel),
                nome_linha,
                text(estado.clone()).size(13.5).color(if acabou { t::FG_DIM } else { t::accent() }),
            ]
            .spacing(8)
            .align_x(Alignment::Center)
            .into()
        }
    };

    let mut avisos = column![].spacing(6).align_x(Alignment::Center);
    if c.receiving_video {
        avisos = avisos.push(row![text(nome.clone()).size(14).font(SEMI), text(estado).size(13).color(t::accent())].spacing(10));
    }
    if let Some(s) = &c.sharing {
        avisos = avisos.push(
            container(row![icon(Icon::Screen, 15.0, t::accent()), text(format!("Transmitindo: {}", ui::ellipsis(s, 40))).size(13).color(t::FG)].spacing(8).align_y(Alignment::Center))
                .padding([5, 12])
                .style(t::chip(t::accent())),
        );
    }
    if c.sem_microfone && !acabou {
        avisos = avisos.push(
            container(row![icon(Icon::MicOff, 14.0, t::WARN), text("Sem microfone: você só ouve").size(13).color(t::WARN)].spacing(8).align_y(Alignment::Center))
                .padding([5, 12])
                .style(t::chip(t::WARN)),
        );
    }
    if c.video && !acabou && !c.receiving_video {
        avisos = avisos.push(ui::note("Chamada de vídeo: aqui você vê a câmera de quem está no celular. O computador manda a sua tela, não a câmera."));
    }
    if let Some(b) = &c.banner {
        avisos = avisos.push(banner(b));
    }

    let controles: El<'a> = if acabou {
        Space::new().height(0).into()
    } else {
        let mut r = row![].spacing(14).align_y(Alignment::Center);
        r = r.push(round_btn(
            if c.muted { Icon::MicOff } else { Icon::Mic },
            if c.muted { CallBtn::On } else { CallBtn::Normal },
            if c.muted { "Ligar microfone" } else { "Desligar microfone" },
            Some(Msg::ToggleMute),
        ));
        r = r.push(round_btn(
            Icon::Wave,
            if m.popup == Some(Popup::CallSounds) { CallBtn::On } else { CallBtn::Normal },
            "Soundboard",
            ativo.then(|| Msg::SetPopup(Some(Popup::CallSounds))),
        ));
        r = r.push(if c.sharing.is_some() {
            round_btn(Icon::Screen, CallBtn::On, "Parar de transmitir", Some(Msg::StopSharing))
        } else {
            round_btn(Icon::Screen, CallBtn::Normal, "Transmitir a tela", ativo.then_some(Msg::OpenShare))
        });
        r = r.push(round_btn(Icon::PhoneOff, CallBtn::Danger, "Desligar", Some(Msg::HangUp)));
        if !c.muted && !c.sem_microfone {
            r = r.push(mic_meter(c.mic_level));
        }
        r.into()
    };

    let col = column![palco, avisos, controles].spacing(14).align_x(Alignment::Center).width(Length::Fill);
    container(col).padding([18, 20]).width(Length::Fill).style(t::fill(t::mix(t::main_bg(), iced::Color::BLACK, 0.35))).into()
}

/// Chamada chegando.
pub fn incoming<'a>(m: &'a Main, c: &'a CallInfo, video: bool) -> El<'a> {
    let nome = m.name_of(&c.peer);
    container(
        column![
            ui::ringed(ui::avatar(&nome, m.tone_of(&c.peer), m.pic_of(&c.peer), 96.0), 96.0, Some(t::accent())),
            text(nome).size(22).font(BOLD).color(t::FG),
            text(if video { "Chamada de vídeo chegando" } else { "Chamada de voz chegando" }).size(14).color(t::accent()),
            ui::vspace(6.0),
            row![
                column![round_btn(Icon::PhoneOff, CallBtn::Danger, "Recusar", Some(Msg::DeclineCall)), text("Recusar").size(12.5).color(t::FG_DIM)]
                    .spacing(6)
                    .align_x(Alignment::Center),
                ui::hspace(36.0),
                column![round_btn(Icon::Phone, CallBtn::Accept, "Atender", Some(Msg::AcceptCall)), text("Atender").size(12.5).color(t::FG_DIM)]
                    .spacing(6)
                    .align_x(Alignment::Center),
            ]
            .align_y(Alignment::Center),
        ]
        .spacing(10)
        .width(Length::Fill)
        .align_x(Alignment::Center),
    )
    .padding(32)
    .width(340)
    .style(t::panel)
    .into()
}

/// Sala de voz do grupo aberto.
pub fn room_panel<'a>(m: &'a Main, r: &'a RoomInfo) -> El<'a> {
    let me = m.me_pk();
    let canal = m
        .group(&r.lid)
        .and_then(|g| g.meta.channels.iter().find(|c| c.id == r.ch))
        .map(|c| c.name.clone())
        .unwrap_or(r.ch.clone());
    let mut cartoes: Vec<El<'a>> = Vec::new();
    cartoes.push(tile(m, &me, r.me_speaking && !r.muted, r.muted, true, true));
    for p in &r.peers {
        cartoes.push(tile(m, &p.pk, p.speaking, !p.mic, p.connected, false));
    }
    let grade = row(cartoes).spacing(12).wrap();
    let mut col = column![
        row![
            icon(Icon::Volume, 18.0, t::accent()),
            text(canal).size(15).font(BOLD).color(t::FG),
            text(duration_label(r.since)).size(13).color(t::FG_DIM),
            ui::fill(),
        ]
        .spacing(8)
        .align_y(Alignment::Center),
        grade,
    ]
    .spacing(12);
    if r.sem_microfone {
        col = col.push(ui::note("Sem microfone: você só ouve."));
    }
    if let Some(b) = &r.banner {
        col = col.push(container(banner(b)).center_x(Length::Fill));
    }
    col = col.push(
        container(
            row![
                round_btn(
                    if r.muted { Icon::MicOff } else { Icon::Mic },
                    if r.muted { CallBtn::On } else { CallBtn::Normal },
                    if r.muted { "Ligar microfone" } else { "Desligar microfone" },
                    Some(Msg::ToggleMute)
                ),
                round_btn(
                    Icon::Wave,
                    if m.popup == Some(Popup::CallSounds) { CallBtn::On } else { CallBtn::Normal },
                    "Soundboard",
                    Some(Msg::SetPopup(Some(Popup::CallSounds)))
                ),
                round_btn(Icon::PhoneOff, CallBtn::Danger, "Sair da sala", Some(Msg::LeaveRoom)),
                if !r.muted && !r.sem_microfone { mic_meter(r.mic_level) } else { Space::new().into() },
            ]
            .spacing(14)
            .align_y(Alignment::Center),
        )
        .center_x(Length::Fill),
    );
    container(col).padding([16, 20]).width(Length::Fill).style(t::fill(t::mix(t::main_bg(), iced::Color::BLACK, 0.35))).into()
}

/// Nível do seu microfone (para conferir se ele está pegando a voz).
fn mic_meter<'a>(level: f32) -> El<'a> {
    let n = 8;
    let acesos = ((level.clamp(0.0, 1.0).sqrt()) * n as f32).round() as usize;
    ui::tip(
        row((0..n)
            .map(|i| {
                let cor = if i < acesos { t::accent() } else { t::surface_hi() };
                container(Space::new()).width(4).height(6.0 + i as f32 * 2.2).style(t::rounded(cor, 2.0)).into()
            })
            .collect::<Vec<El<'a>>>())
        .spacing(2)
        .align_y(Alignment::End),
        "Seu microfone",
        tooltip::Position::Top,
    )
}

fn tile<'a>(m: &'a Main, pk: &str, falando: bool, mudo: bool, conectado: bool, eu: bool) -> El<'a> {
    let nome = if eu { format!("{} (você)", m.first_name(pk)) } else { m.name_of(pk) };
    let mut base = column![
        ui::ringed(ui::avatar(&m.name_of(pk), m.tone_of(pk), m.pic_of(pk), 56.0), 56.0, falando.then(t::accent)),
        text(ui::ellipsis(&nome, 16)).size(13).font(SEMI).color(t::FG),
    ]
    .spacing(6)
    .align_x(Alignment::Center);
    if !conectado {
        base = base.push(text("conectando…").size(11).color(t::FG_FAINT));
    }
    let mut camadas: Vec<El<'a>> = vec![container(base).padding([12, 10]).width(132).center_x(132).into()];
    if mudo {
        camadas.push(
            container(container(icon(Icon::MicOff, 13.0, t::FG)).center(24).style(t::rounded(t::DANGER, 999.0)))
                .align_right(132)
                .padding(Padding { top: 8.0, right: 8.0, ..Padding::ZERO })
                .into(),
        );
    }
    container(stack(camadas))
        .style(move |_| iced::widget::container::Style {
            background: Some(iced::Background::Color(t::surface())),
            border: iced::Border { color: if falando { t::accent() } else { t::line() }, width: if falando { 2.0 } else { 1.0 }, radius: 14.0.into() },
            ..Default::default()
        })
        .into()
}

/// "Voz conectada": fica na barra lateral enquanto há chamada ou sala.
pub fn voice_panel(m: &Main) -> Option<El<'_>> {
    let (titulo, sub, acao, cor): (String, String, Msg, iced::Color) = if let Some(r) = &m.room {
        let g = m.group(&r.lid);
        let canal = g.and_then(|g| g.meta.channels.iter().find(|c| c.id == r.ch)).map(|c| c.name.clone()).unwrap_or_default();
        ("Voz conectada".into(), format!("{canal} / {}", g.map(|g| g.meta.name.clone()).unwrap_or_default()), Msg::LeaveRoom, t::accent())
    } else if m.joining_room {
        ("Entrando na sala…".into(), String::new(), Msg::LeaveRoom, t::WARN)
    } else if let Some(c) = &m.call {
        match &c.state {
            CallState::Ended { .. } | CallState::Ringing { .. } => return None,
            CallState::Active { since } => (format!("Em chamada · {}", duration_label(*since)), m.name_of(&c.peer), Msg::HangUp, t::accent()),
            _ => ("Chamando…".into(), m.name_of(&c.peer), Msg::HangUp, t::WARN),
        }
    } else {
        return None;
    };
    let ir: Option<Msg> = m.room.as_ref().map(|r| Msg::Go(Section::Group(r.lid.clone())));
    let info = button(
        column![
            row![icon(Icon::Volume, 14.0, cor), text(titulo).size(13).font(BOLD).color(cor)].spacing(6).align_y(Alignment::Center),
            text(ui::ellipsis(&sub, 30)).size(12).color(t::FG_DIM)
        ]
        .spacing(2),
    )
    .padding([4, 6])
    .width(Length::Fill)
    .style(t::row_item(false))
    .on_press_maybe(ir);
    Some(
        container(row![info, ui::icon_button(Icon::PhoneOff, "Desconectar", Some(acao), false)].spacing(4).align_y(Alignment::Center))
            .padding([6, 8])
            .style(t::fill(t::mix(t::sidebar(), iced::Color::BLACK, 0.15)))
            .into(),
    )
}
