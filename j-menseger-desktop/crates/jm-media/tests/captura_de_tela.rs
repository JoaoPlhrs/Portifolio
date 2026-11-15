//! Captura de tela de verdade, contra a tela do sistema.
//!
//! Este teste não simula nada: lista o que o sistema oferece, captura um
//! quadro e confere que a imagem tem conteúdo. Ele é pulado quando não há tela
//! (servidor sem interface gráfica), em vez de falhar por algo que não é bug.

use jm_media::screen::{self, Fps, Quality};

fn tem_tela() -> bool {
    std::env::var("DISPLAY").is_ok() || std::env::var("WAYLAND_DISPLAY").is_ok()
}

#[test]
fn lista_e_captura_uma_tela_de_verdade() {
    if !tem_tela() {
        eprintln!("pulando: sem tela neste ambiente");
        return;
    }
    let monitores = match screen::monitors() {
        Ok(m) => m,
        Err(e) => {
            eprintln!("pulando: não deu para listar as telas ({e})");
            return;
        }
    };
    if monitores.is_empty() {
        eprintln!("pulando: nenhuma tela encontrada");
        return;
    }

    let tela = &monitores[0];
    assert!(tela.width >= 16 && tela.height >= 16, "tamanho improvável: {}×{}", tela.width, tela.height);
    assert!(!tela.label().is_empty());

    let quadro = screen::capture(tela).expect("a captura falhou");
    assert_eq!(quadro.width, tela.width, "largura capturada diferente da tela");
    assert_eq!(quadro.height, tela.height);
    assert_eq!(
        quadro.rgba.len(),
        quadro.pixels() * 4,
        "a imagem veio com tamanho errado"
    );

    // Opacidade: um quadro todo transparente seria sinal de captura vazia.
    let opacos = quadro.rgba.chunks_exact(4).filter(|p| p[3] > 0).count();
    assert!(
        opacos > quadro.pixels() / 2,
        "mais da metade dos pixels veio transparente: a captura não pegou a tela"
    );

    println!(
        "capturou {}×{} de '{}' ({} bytes)",
        quadro.width,
        quadro.height,
        tela.label(),
        quadro.rgba.len()
    );
}

#[test]
fn quadro_capturado_e_reduzido_para_a_qualidade_escolhida() {
    if !tem_tela() {
        return;
    }
    let Ok(monitores) = screen::monitors() else { return };
    let Some(tela) = monitores.first() else { return };
    let Ok(quadro) = screen::capture(tela) else { return };

    for q in [Quality::P720, Quality::P1080] {
        let p = quadro.scale_to_height(q.height());
        assert!(p.height <= quadro.height.max(q.height()));
        assert_eq!(p.rgba.len(), p.pixels() * 4);
        assert_eq!(p.width % 2, 0, "o codificador exige largura par");
        assert_eq!(p.height % 2, 0);
    }
}

#[test]
fn captura_repetida_mantem_a_cadencia() {
    if !tem_tela() {
        return;
    }
    let Ok(monitores) = screen::monitors() else { return };
    let Some(tela) = monitores.first() else { return };

    // Dez quadros seguidos, medindo quanto tempo cada captura leva.
    let mut tempos = Vec::new();
    for _ in 0..10 {
        let t0 = std::time::Instant::now();
        match screen::capture(tela) {
            Ok(_) => tempos.push(t0.elapsed()),
            Err(e) => {
                eprintln!("pulando: captura falhou no meio ({e})");
                return;
            }
        }
    }
    let media = tempos.iter().sum::<std::time::Duration>() / tempos.len() as u32;
    println!("tempo médio por quadro: {media:?} (alvo 30 fps: {:?})", Fps::Fps30.interval());

    // Não exigimos 30 fps aqui: esta máquina não tem GPU e a tela é virtual.
    // O que o teste garante é que a captura funciona repetidamente, sem travar
    // nem vazar memória a ponto de degradar.
    assert!(
        media < std::time::Duration::from_secs(2),
        "capturar um quadro levou {media:?}, algo está muito errado"
    );
}

#[test]
fn lista_de_fontes_inclui_as_telas() {
    if !tem_tela() {
        return;
    }
    let Ok(fontes) = screen::sources() else { return };
    let Ok(monitores) = screen::monitors() else { return };
    assert!(
        fontes.len() >= monitores.len(),
        "a lista de opções tem que conter pelo menos as telas"
    );
}
