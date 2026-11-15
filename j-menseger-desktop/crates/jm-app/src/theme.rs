//! Cores e estilos da interface.
//!
//! Tema escuro próprio do J-Menseger: cinzas frios e o verde do aplicativo
//! mobile como cor de destaque. Tudo em um lugar só para a aparência ficar
//! coerente entre as telas.

use iced::widget::{button, container, scrollable, text_input};
use iced::{Background, Border, Color, Shadow, Theme, Vector};

pub const fn rgb(r: u8, g: u8, b: u8) -> Color {
    Color {
        r: r as f32 / 255.0,
        g: g as f32 / 255.0,
        b: b as f32 / 255.0,
        a: 1.0,
    }
}

/// Barra de navegação, a coluna mais escura.
pub const RAIL: Color = rgb(0x0a, 0x0b, 0x0d);
/// Barra lateral com a lista de conversas e canais.
pub const SIDEBAR: Color = rgb(0x11, 0x13, 0x16);
/// Área das mensagens.
pub const MAIN: Color = rgb(0x16, 0x18, 0x1c);
/// Campos de texto e caixas elevadas.
pub const SURFACE: Color = rgb(0x1e, 0x21, 0x26);
pub const SURFACE_HI: Color = rgb(0x26, 0x2a, 0x30);
pub const LINE: Color = rgb(0x24, 0x27, 0x2c);

pub const FG: Color = rgb(0xe6, 0xe8, 0xeb);
pub const FG_DIM: Color = rgb(0x9a, 0xa2, 0xad);
pub const FG_FAINT: Color = rgb(0x6b, 0x72, 0x7c);

/// Verde do J-Menseger.
pub const ACCENT: Color = rgb(0x2f, 0xd4, 0x7c);
pub const ACCENT_INK: Color = rgb(0x04, 0x1a, 0x0e);
pub const DANGER: Color = rgb(0xe5, 0x5b, 0x5b);
pub const WARN: Color = rgb(0xe8, 0xb3, 0x39);

/// Balão de mensagem enviada por mim.
pub const BUBBLE_ME: Color = rgb(0x1d, 0x4f, 0x37);
/// Balão de mensagem recebida.
pub const BUBBLE_THEM: Color = rgb(0x22, 0x26, 0x2c);

pub fn with_alpha(c: Color, a: f32) -> Color {
    Color { a, ..c }
}

/* ---------------- recipientes ---------------- */

pub fn fill(color: Color) -> impl Fn(&Theme) -> container::Style {
    move |_| container::Style {
        background: Some(Background::Color(color)),
        text_color: Some(FG),
        ..Default::default()
    }
}

pub fn panel(_: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(SURFACE)),
        text_color: Some(FG),
        border: Border {
            color: LINE,
            width: 1.0,
            radius: 12.0.into(),
        },
        ..Default::default()
    }
}

/// Faixa superior da área de mensagens, com uma linha de separação embaixo.
pub fn header(_: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(MAIN)),
        text_color: Some(FG),
        border: Border {
            color: LINE,
            width: 0.0,
            radius: 0.0.into(),
        },
        shadow: Shadow {
            color: with_alpha(Color::BLACK, 0.35),
            offset: Vector::new(0.0, 2.0),
            blur_radius: 6.0,
        },
        ..Default::default()
    }
}

pub fn bubble(mine: bool) -> impl Fn(&Theme) -> container::Style {
    move |_| container::Style {
        background: Some(Background::Color(if mine { BUBBLE_ME } else { BUBBLE_THEM })),
        text_color: Some(FG),
        border: Border {
            radius: 14.0.into(),
            ..Default::default()
        },
        ..Default::default()
    }
}

/// Aviso do sistema no meio da conversa.
pub fn system_note(_: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(with_alpha(WARN, 0.10))),
        text_color: Some(FG_DIM),
        border: Border {
            radius: 10.0.into(),
            ..Default::default()
        },
        ..Default::default()
    }
}

/// Círculo com as iniciais de alguém.
pub fn avatar(tone: u8) -> impl Fn(&Theme) -> container::Style {
    let tons = [
        rgb(0x3d, 0x47, 0x4f),
        rgb(0x4b, 0x4b, 0x4f),
        rgb(0x46, 0x4a, 0x53),
        rgb(0x4a, 0x45, 0x44),
        rgb(0x41, 0x42, 0x4d),
        rgb(0x5a, 0x5e, 0x67),
    ];
    let c = tons[(tone as usize) % tons.len()];
    move |_| container::Style {
        background: Some(Background::Color(c)),
        text_color: Some(FG),
        border: Border {
            radius: 999.0.into(),
            ..Default::default()
        },
        ..Default::default()
    }
}

/// Bolinha de não lidas.
pub fn badge(_: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(ACCENT)),
        text_color: Some(ACCENT_INK),
        border: Border {
            radius: 999.0.into(),
            ..Default::default()
        },
        ..Default::default()
    }
}

/* ---------------- botões ---------------- */

pub fn primary(_: &Theme, status: button::Status) -> button::Style {
    let bg = match status {
        button::Status::Hovered => rgb(0x45, 0xdf, 0x90),
        button::Status::Pressed => rgb(0x24, 0xb0, 0x66),
        button::Status::Disabled => SURFACE_HI,
        _ => ACCENT,
    };
    button::Style {
        background: Some(Background::Color(bg)),
        text_color: if matches!(status, button::Status::Disabled) { FG_FAINT } else { ACCENT_INK },
        border: Border {
            radius: 10.0.into(),
            ..Default::default()
        },
        ..Default::default()
    }
}

pub fn ghost(_: &Theme, status: button::Status) -> button::Style {
    let bg = match status {
        button::Status::Hovered => SURFACE_HI,
        button::Status::Pressed => SURFACE,
        _ => Color::TRANSPARENT,
    };
    button::Style {
        background: Some(Background::Color(bg)),
        text_color: FG,
        border: Border {
            radius: 10.0.into(),
            ..Default::default()
        },
        ..Default::default()
    }
}

/// Linha da lista de conversas.
pub fn row_item(selected: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_, status| {
        let bg = if selected {
            SURFACE_HI
        } else {
            match status {
                button::Status::Hovered => with_alpha(SURFACE, 0.8),
                button::Status::Pressed => SURFACE,
                _ => Color::TRANSPARENT,
            }
        };
        button::Style {
            background: Some(Background::Color(bg)),
            text_color: if selected { FG } else { FG_DIM },
            border: Border {
                radius: 8.0.into(),
                ..Default::default()
            },
            ..Default::default()
        }
    }
}

/// Ícone grande da barra de navegação.
pub fn rail_item(selected: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_, status| {
        let bg = if selected {
            ACCENT
        } else {
            match status {
                button::Status::Hovered => SURFACE_HI,
                _ => SURFACE,
            }
        };
        button::Style {
            background: Some(Background::Color(bg)),
            text_color: if selected { ACCENT_INK } else { FG },
            border: Border {
                radius: if selected { 14.0 } else { 22.0 }.into(),
                ..Default::default()
            },
            ..Default::default()
        }
    }
}

pub fn danger(_: &Theme, status: button::Status) -> button::Style {
    button::Style {
        background: Some(Background::Color(match status {
            button::Status::Hovered => rgb(0xf0, 0x6e, 0x6e),
            _ => DANGER,
        })),
        text_color: Color::WHITE,
        border: Border {
            radius: 10.0.into(),
            ..Default::default()
        },
        ..Default::default()
    }
}

/* ---------------- campos ---------------- */

pub fn input(_: &Theme, status: text_input::Status) -> text_input::Style {
    let border_color = match status {
        text_input::Status::Focused { .. } => ACCENT,
        _ => LINE,
    };
    text_input::Style {
        background: Background::Color(SURFACE),
        border: Border {
            color: border_color,
            width: 1.0,
            radius: 10.0.into(),
        },
        icon: FG_DIM,
        placeholder: FG_FAINT,
        value: FG,
        selection: with_alpha(ACCENT, 0.35),
    }
}

pub fn scroll(_: &Theme, _status: scrollable::Status) -> scrollable::Style {
    let rail = scrollable::Rail {
        background: None,
        border: Border::default(),
        scroller: scrollable::Scroller {
            background: Background::Color(SURFACE_HI),
            border: Border {
                radius: 999.0.into(),
                ..Default::default()
            },
        },
    };
    scrollable::Style {
        container: container::Style::default(),
        vertical_rail: rail,
        horizontal_rail: rail,
        gap: None,
        auto_scroll: scrollable::AutoScroll {
            background: Background::Color(SURFACE),
            border: Border { radius: 999.0.into(), ..Default::default() },
            shadow: Shadow::default(),
            icon: FG_DIM,
        },
    }
}

/// Iniciais de um nome, para o avatar.
pub fn initials(name: &str) -> String {
    let partes: Vec<&str> = name.split_whitespace().take(2).collect();
    if partes.is_empty() {
        return "?".into();
    }
    partes
        .iter()
        .filter_map(|p| p.chars().next())
        .collect::<String>()
        .to_uppercase()
}

/// Hora no formato que aparece ao lado da mensagem.
pub fn hhmm(ts_ms: i64) -> String {
    let secs = ts_ms / 1000;
    let dia = secs % 86_400;
    format!("{:02}:{:02}", dia / 3600, (dia % 3600) / 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iniciais_de_um_nome() {
        assert_eq!(initials("Ana Souza"), "AS");
        assert_eq!(initials("Beto"), "B");
        assert_eq!(initials("ana maria souza"), "AM");
        assert_eq!(initials(""), "?");
        assert_eq!(initials("   "), "?");
    }

    #[test]
    fn hora_formatada() {
        assert_eq!(hhmm(0), "00:00");
        assert_eq!(hhmm(3_600_000), "01:00");
        assert_eq!(hhmm(45_296_000), "12:34");
    }
}
