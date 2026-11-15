//! Grupos no estilo servidor: canais de texto, canais de voz e membros.
//!
//! Um grupo tem **a sua própria chave**. Quem recebe um convite recebe essa
//! chave, e é ela que abre as mensagens do grupo — não existe servidor
//! decidindo quem entra. Por isso, tirar alguém do grupo não é apagar um nome
//! de uma lista: é trocar a chave e entregar a nova só a quem fica.

use nostr::prelude::*;
use serde::{Deserialize, Serialize};

/// Tipo de canal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChannelKind {
    Text,
    Voice,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Channel {
    pub id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub kind: ChannelKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub topic: Option<String>,
}

/// Dados do grupo que todos os membros compartilham.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GroupMeta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lid: Option<String>,
    pub name: String,
    #[serde(default)]
    pub desc: String,
    #[serde(default)]
    pub t: u8,
    pub channels: Vec<Channel>,
    pub owner: String,
    #[serde(default)]
    pub admins: Vec<String>,
    #[serde(default)]
    pub members: Vec<String>,
    /// Nome que cada membro escolheu mostrar dentro do grupo.
    #[serde(default)]
    pub names: std::collections::HashMap<String, String>,
}

/// Limites que evitam que um grupo malformado (ou mal-intencionado) estoure a
/// memória ou a tela de quem recebe.
const MAX_CHANNELS: usize = 20;
const MAX_MEMBERS: usize = 64;
const MAX_ADMINS: usize = 10;

impl GroupMeta {
    pub fn is_admin(&self, pk: &str) -> bool {
        self.owner == pk || self.admins.iter().any(|a| a == pk)
    }

    pub fn text_channels(&self) -> impl Iterator<Item = &Channel> {
        self.channels.iter().filter(|c| c.kind == ChannelKind::Text)
    }

    pub fn voice_channels(&self) -> impl Iterator<Item = &Channel> {
        self.channels.iter().filter(|c| c.kind == ChannelKind::Voice)
    }

    /// Nome de um membro dentro do grupo.
    pub fn name_of(&self, pk: &str) -> Option<&str> {
        self.names.get(pk).map(|s| s.as_str())
    }

    /// Valida e apara dados vindos de fora.
    ///
    /// Tudo isto chega pela rede, de outro aparelho: nada pode ser aceito do
    /// jeito que veio.
    pub fn sanitize(mut self, owner_fallback: &str) -> Option<GroupMeta> {
        self.name = clamp(&self.name, 40);
        if self.name.is_empty() {
            self.name = "Grupo".into();
        }
        self.desc = clamp(&self.desc, 140);
        self.t %= 6;

        self.channels.truncate(MAX_CHANNELS);
        self.channels.retain(|c| valid_channel_id(&c.id));
        for c in &mut self.channels {
            c.name = clamp(&c.name, 24);
            if c.name.is_empty() {
                c.name = c.id.clone();
            }
            c.topic = c.topic.as_deref().map(|t| clamp(t, 80)).filter(|t| !t.is_empty());
        }
        // Um grupo sem canal de texto não teria onde conversar.
        if !self.channels.iter().any(|c| c.kind == ChannelKind::Text) {
            return None;
        }

        if !is_hex64(&self.owner) {
            self.owner = owner_fallback.to_string();
        }
        if !is_hex64(&self.owner) {
            return None;
        }

        self.members.retain(|m| is_hex64(m));
        self.members.truncate(MAX_MEMBERS);
        self.admins.retain(|a| is_hex64(a));
        self.admins.truncate(MAX_ADMINS);
        self.names.retain(|k, _| is_hex64(k));
        for v in self.names.values_mut() {
            *v = clamp(v, 40);
        }
        Some(self)
    }
}

/// Um grupo como ele fica guardado neste aparelho.
#[derive(Debug, Clone)]
pub struct Group {
    /// Identificador estável do grupo. Não muda quando a chave é trocada.
    pub lid: String,
    /// Chave pública atual: é o endereço das mensagens do grupo.
    pub pk: String,
    /// Chave privada atual: é o que permite ler.
    pub sk: String,
    /// Chaves antigas, para continuar lendo o histórico depois de uma troca.
    pub old_keys: Vec<(String, String)>,
    pub meta: GroupMeta,
    pub ver: u64,
    pub pic: Option<String>,
    pub pic_v: u64,
    pub muted: bool,
    pub joined_at: i64,
}

impl Group {
    /// Cria um grupo novo, com a chave sorteada neste aparelho.
    pub fn create(name: &str, owner: &PublicKey, owner_name: &str, template: Template) -> Self {
        let keys = Keys::generate();
        let pk = keys.public_key().to_hex();
        let owner_hex = owner.to_hex();

        let mut usados = std::collections::HashSet::new();
        let mut canais = Vec::new();
        for (nome, kind) in template.channels() {
            let mut id = slug(nome);
            while !usados.insert(id.clone()) {
                id.push_str("-2");
            }
            canais.push(Channel {
                id,
                name: nome.to_string(),
                kind,
                topic: None,
            });
        }

        let mut names = std::collections::HashMap::new();
        names.insert(owner_hex.clone(), owner_name.to_string());

        Self {
            lid: pk.clone(),
            pk,
            sk: keys.secret_key().to_secret_hex(),
            old_keys: Vec::new(),
            meta: GroupMeta {
                lid: None,
                name: clamp(name, 40),
                desc: String::new(),
                t: 0,
                channels: canais,
                owner: owner_hex.clone(),
                admins: vec![owner_hex.clone()],
                members: vec![owner_hex],
                names,
            },
            ver: 1,
            pic: None,
            pic_v: 0,
            muted: false,
            joined_at: crate::engine::now_ms(),
        }
    }

    pub fn keys(&self) -> Option<Keys> {
        Keys::parse(&self.sk).ok()
    }

    /// Todas as chaves que este aparelho ainda consegue abrir, incluindo as
    /// antigas: sem isso o histórico sumiria a cada troca de chave.
    pub fn all_keys(&self) -> Vec<(String, Keys)> {
        let mut v = Vec::new();
        if let Ok(k) = Keys::parse(&self.sk) {
            v.push((self.pk.clone(), k));
        }
        for (pk, sk) in &self.old_keys {
            if let Ok(k) = Keys::parse(sk) {
                v.push((pk.clone(), k));
            }
        }
        v
    }

    /// Primeiro canal de texto, usado para avisos do próprio grupo.
    pub fn main_channel(&self) -> Option<&Channel> {
        self.meta.text_channels().next()
    }

    /// Troca a chave do grupo, mantendo a antiga para ler o histórico.
    pub fn rekey(&mut self) -> Keys {
        let novas = Keys::generate();
        self.old_keys.push((self.pk.clone(), self.sk.clone()));
        if self.old_keys.len() > 5 {
            self.old_keys.remove(0);
        }
        self.pk = novas.public_key().to_hex();
        self.sk = novas.secret_key().to_secret_hex();
        self.ver += 1;
        novas
    }
}

/// Conjuntos de canais prontos, para não começar com o grupo vazio.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Template {
    Amigos,
    Estudos,
    Jogos,
    Branco,
}

impl Template {
    pub fn channels(&self) -> Vec<(&'static str, ChannelKind)> {
        match self {
            Template::Amigos => vec![
                ("geral", ChannelKind::Text),
                ("fotos", ChannelKind::Text),
                ("Sala de estar", ChannelKind::Voice),
            ],
            Template::Estudos => vec![
                ("avisos", ChannelKind::Text),
                ("dúvidas", ChannelKind::Text),
                ("materiais", ChannelKind::Text),
                ("Estudo em grupo", ChannelKind::Voice),
            ],
            Template::Jogos => vec![
                ("geral", ChannelKind::Text),
                ("clipes", ChannelKind::Text),
                ("Jogatina", ChannelKind::Voice),
                ("Sala 2", ChannelKind::Voice),
            ],
            Template::Branco => vec![("geral", ChannelKind::Text), ("Voz", ChannelKind::Voice)],
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Template::Amigos => "Amigos",
            Template::Estudos => "Estudos",
            Template::Jogos => "Jogos",
            Template::Branco => "Em branco",
        }
    }

    pub fn all() -> [Template; 4] {
        [Template::Amigos, Template::Estudos, Template::Jogos, Template::Branco]
    }
}

/* ---------------- apoio ---------------- */

fn clamp(s: &str, max: usize) -> String {
    s.trim().chars().take(max).collect()
}

fn is_hex64(s: &str) -> bool {
    s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit())
}

fn valid_channel_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 24
        && s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Transforma o nome digitado em um identificador curto e previsível.
pub fn slug(s: &str) -> String {
    let sem_acento: String = s
        .to_lowercase()
        .chars()
        .map(|c| match c {
            'á' | 'à' | 'â' | 'ã' | 'ä' => 'a',
            'é' | 'è' | 'ê' | 'ë' => 'e',
            'í' | 'ì' | 'î' | 'ï' => 'i',
            'ó' | 'ò' | 'ô' | 'õ' | 'ö' => 'o',
            'ú' | 'ù' | 'û' | 'ü' => 'u',
            'ç' => 'c',
            'ñ' => 'n',
            c => c,
        })
        .collect();
    let mut out = String::new();
    let mut hifen = false;
    for c in sem_acento.chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            out.push(c);
            hifen = false;
        } else if !out.is_empty() && !hifen {
            out.push('-');
            hifen = true;
        }
        if out.len() >= 20 {
            break;
        }
    }
    let limpo = out.trim_matches('-').to_string();
    if limpo.is_empty() {
        "canal".into()
    } else {
        limpo
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dono() -> Keys {
        Keys::generate()
    }

    #[test]
    fn grupo_novo_nasce_com_canais_e_dono() {
        let k = dono();
        let g = Group::create("Squad Beta", &k.public_key(), "Ana", Template::Amigos);

        assert_eq!(g.meta.name, "Squad Beta");
        assert_eq!(g.meta.owner, k.public_key().to_hex());
        assert!(g.meta.is_admin(&k.public_key().to_hex()));
        assert_eq!(g.meta.members, vec![k.public_key().to_hex()]);
        assert_eq!(g.meta.text_channels().count(), 2);
        assert_eq!(g.meta.voice_channels().count(), 1);
        assert_eq!(g.main_channel().unwrap().id, "geral");
        assert_eq!(g.meta.name_of(&k.public_key().to_hex()), Some("Ana"));
        // A chave do grupo é própria, não a de quem criou.
        assert_ne!(g.pk, k.public_key().to_hex());
        assert!(g.keys().is_some());
    }

    #[test]
    fn identificadores_de_canal_nao_se_repetem() {
        let k = dono();
        let g = Group::create("G", &k.public_key(), "Ana", Template::Jogos);
        let ids: Vec<&str> = g.meta.channels.iter().map(|c| c.id.as_str()).collect();
        let unicos: std::collections::HashSet<_> = ids.iter().collect();
        assert_eq!(ids.len(), unicos.len(), "ids repetidos: {ids:?}");
    }

    #[test]
    fn trocar_a_chave_preserva_o_historico() {
        let k = dono();
        let mut g = Group::create("G", &k.public_key(), "Ana", Template::Branco);
        let pk_antiga = g.pk.clone();
        let ver_antes = g.ver;

        g.rekey();

        assert_ne!(g.pk, pk_antiga, "a chave tem que mudar");
        assert_eq!(g.ver, ver_antes + 1);
        assert_eq!(g.old_keys.len(), 1);
        // Continua conseguindo abrir o que foi escrito com a chave antiga.
        let abriveis: Vec<String> = g.all_keys().into_iter().map(|(pk, _)| pk).collect();
        assert!(abriveis.contains(&pk_antiga));
        assert!(abriveis.contains(&g.pk));
    }

    #[test]
    fn guarda_no_maximo_cinco_chaves_antigas() {
        let k = dono();
        let mut g = Group::create("G", &k.public_key(), "Ana", Template::Branco);
        for _ in 0..8 {
            g.rekey();
        }
        assert_eq!(g.old_keys.len(), 5, "não pode crescer para sempre");
    }

    #[test]
    fn dados_vindos_de_fora_sao_aparados() {
        let dono_hex = "a".repeat(64);
        let bruto = GroupMeta {
            lid: None,
            name: " ".to_string() + &"n".repeat(80),
            desc: "d".repeat(400),
            t: 99,
            channels: vec![
                Channel { id: "geral".into(), name: "x".repeat(60), kind: ChannelKind::Text, topic: Some("t".repeat(200)) },
                Channel { id: "INVÁLIDO!".into(), name: "mau".into(), kind: ChannelKind::Text, topic: None },
            ],
            owner: "não-é-chave".into(),
            admins: vec!["b".repeat(64), "curta".into()],
            members: vec!["c".repeat(64), "ruim".into()],
            names: std::collections::HashMap::from([
                ("d".repeat(64), "z".repeat(90)),
                ("xx".into(), "ignorado".into()),
            ]),
        };

        let m = bruto.sanitize(&dono_hex).expect("deveria sobrar um canal de texto");
        assert_eq!(m.name.chars().count(), 40, "nome aparado");
        assert_eq!(m.desc.chars().count(), 140);
        assert!(m.t < 6);
        assert_eq!(m.channels.len(), 1, "canal com id inválido é descartado");
        assert_eq!(m.channels[0].name.chars().count(), 24);
        assert_eq!(m.channels[0].topic.as_ref().unwrap().chars().count(), 80);
        assert_eq!(m.owner, dono_hex, "dono inválido cai para o esperado");
        assert_eq!(m.admins.len(), 1, "admin com chave inválida sai");
        assert_eq!(m.members.len(), 1);
        assert_eq!(m.names.len(), 1, "nome com chave inválida sai");
        assert_eq!(m.names.values().next().unwrap().chars().count(), 40);
    }

    #[test]
    fn grupo_sem_canal_de_texto_e_recusado() {
        let m = GroupMeta {
            lid: None,
            name: "G".into(),
            desc: String::new(),
            t: 0,
            channels: vec![Channel { id: "voz".into(), name: "Voz".into(), kind: ChannelKind::Voice, topic: None }],
            owner: "a".repeat(64),
            admins: vec![],
            members: vec![],
            names: Default::default(),
        };
        assert!(m.sanitize(&"a".repeat(64)).is_none());
    }

    #[test]
    fn nome_de_canal_vira_identificador_limpo() {
        assert_eq!(slug("Sala de estar"), "sala-de-estar");
        assert_eq!(slug("dúvidas"), "duvidas");
        assert_eq!(slug("  Avisos!!  "), "avisos");
        assert_eq!(slug("ção"), "cao");
        assert_eq!(slug("!!!"), "canal");
        assert_eq!(slug(""), "canal");
        assert!(slug(&"palavra ".repeat(20)).len() <= 20);
    }

    #[test]
    fn modelos_sempre_tem_canal_de_texto() {
        for t in Template::all() {
            let tem = t.channels().iter().any(|(_, k)| *k == ChannelKind::Text);
            assert!(tem, "modelo {} sem canal de texto", t.label());
        }
    }
}
