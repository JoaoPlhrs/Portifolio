//! Conexão com os servidores (relays).
//!
//! O aplicativo nunca fala com um servidor só: publica em vários ao mesmo tempo
//! e aceita a mensagem como enviada assim que **um** confirmar. Assim um
//! servidor fora do ar não derruba a conversa, e nenhum deles sozinho tem a
//! visão completa de quem fala com quem.
//!
//! O que o servidor recebe é sempre um envelope já cifrado, endereçado a uma
//! chave pública. Ele não sabe quem enviou nem o que está escrito.

use crate::protocol::{KIND_WRAP, KIND_WRAP_EPH};
use futures_util::StreamExt;
use nostr_sdk::prelude::*;
use std::sync::Arc;
use std::time::Duration;

/// Quanto o filtro precisa recuar além da última sincronização, em segundos:
/// os 2 dias de embaralhamento de horário mais uma folga.
const TIME_FUZZ_WINDOW: u64 = 2 * 24 * 60 * 60 + 900;
use tokio::sync::mpsc;

#[derive(Debug, thiserror::Error)]
pub enum RelayError {
    #[error("endereço de servidor inválido: {0}")]
    BadUrl(String),
    #[error("falha ao falar com os servidores: {0}")]
    Sdk(String),
    #[error("nenhum servidor aceitou a mensagem")]
    NotDelivered,
}

/// Um envelope que chegou de algum servidor.
#[derive(Debug, Clone)]
pub struct Incoming {
    pub event: Event,
    pub relay: String,
}

/// Situação de cada servidor, para a barra de conexão da interface.
#[derive(Debug, Clone, PartialEq)]
pub struct RelayStatus {
    pub url: String,
    pub connected: bool,
}

/// Conjunto de servidores.
pub struct RelayPool {
    client: Client,
    keys: Keys,
}

impl RelayPool {
    /// Cria o conjunto. A chave entra para poder responder à autenticação
    /// NIP-42 que alguns servidores exigem — ela nunca é usada para assinar
    /// mensagens, que vão com chave descartável.
    pub fn new(keys: Keys) -> Self {
        let client = Client::builder()
            .authenticator(SignerAuthenticator::new(keys.clone()))
            .build();
        Self { client, keys }
    }

    pub fn public_key(&self) -> PublicKey {
        self.keys.public_key()
    }

    /// Define a lista de servidores, conectando aos novos e largando os que
    /// saíram da lista.
    pub async fn set_relays(&self, urls: &[String]) -> Result<(), RelayError> {
        let desired: Vec<RelayUrl> = urls
            .iter()
            .map(|u| RelayUrl::parse(u).map_err(|_| RelayError::BadUrl(u.clone())))
            .collect::<Result<_, _>>()?;

        let current: Vec<RelayUrl> = self.client.relays().await.into_keys().collect();

        for url in current.iter().filter(|u| !desired.contains(u)) {
            let _ = self.client.remove_relay(url.clone()).await;
        }
        for url in desired.iter().filter(|u| !current.contains(u)) {
            self.client
                .add_relay(url.clone())
                .await
                .map_err(|e| RelayError::Sdk(e.to_string()))?;
        }
        self.client.connect().await;
        Ok(())
    }

    /// Espera até pelo menos um servidor responder, ou desistir no prazo.
    pub async fn wait_connected(&self, timeout: Duration) -> bool {
        self.client.wait_for_connection(timeout).await;
        self.open_count().await > 0
    }

    pub async fn open_count(&self) -> usize {
        self.client
            .relays()
            .await
            .values()
            .filter(|r| r.status().is_connected())
            .count()
    }

    pub async fn status(&self) -> Vec<RelayStatus> {
        let mut v: Vec<RelayStatus> = self
            .client
            .relays()
            .await
            .into_iter()
            .map(|(url, r)| RelayStatus {
                url: url.to_string(),
                connected: r.status().is_connected(),
            })
            .collect();
        v.sort_by(|a, b| a.url.cmp(&b.url));
        v
    }

    /// Pede aos servidores os envelopes endereçados a estas chaves.
    ///
    /// São duas assinaturas: a dos envelopes guardados, com uma janela de
    /// tempo para trás (porque os horários são embaralhados no passado), e a
    /// dos efêmeros, que só valem a partir de agora.
    pub async fn subscribe_inbox(
        &self,
        pubkeys: &[PublicKey],
        since: Timestamp,
    ) -> Result<(), RelayError> {
        // Os horários dos envelopes guardados são embaralhados até 2 dias para
        // trás, então o filtro precisa olhar essa janela inteira: pedir só a
        // partir de "agora" deixaria mensagens recentes de fora.
        let janela = Timestamp::from_secs(since.as_secs().saturating_sub(TIME_FUZZ_WINDOW));
        let stored = Filter::new()
            .kind(Kind::Custom(KIND_WRAP))
            .pubkeys(pubkeys.iter().copied())
            .since(janela);

        let ephemeral = Filter::new()
            .kind(Kind::Custom(KIND_WRAP_EPH))
            .pubkeys(pubkeys.iter().copied())
            .since(Timestamp::now() - 60u64);

        for f in [stored, ephemeral] {
            self.client
                .subscribe(f)
                .await
                .map_err(|e| RelayError::Sdk(e.to_string()))?;
        }
        Ok(())
    }

    /// Publica um envelope. Devolve quantos servidores confirmaram.
    ///
    /// Basta um aceitar para a mensagem estar entregue à rede.
    pub async fn publish(&self, event: &Event) -> Result<usize, RelayError> {
        match self.client.send_event(event).await {
            Ok(out) => {
                let n = out.success.len();
                if n == 0 {
                    Err(RelayError::NotDelivered)
                } else {
                    Ok(n)
                }
            }
            Err(e) => Err(RelayError::Sdk(e.to_string())),
        }
    }

    /// Entrega os envelopes recebidos em um canal, para o motor consumir.
    ///
    /// Roda em uma tarefa separada: a interface nunca espera pela rede.
    pub fn listen(&self) -> mpsc::UnboundedReceiver<Incoming> {
        let (tx, rx) = mpsc::unbounded_channel();
        let client = self.client.clone();
        tokio::spawn(async move {
            let mut stream = client.notifications();
            while let Some(n) = stream.next().await {
                if let ClientNotification::Event {
                    relay_url, event, ..
                } = n
                {
                    let kind = event.kind.as_u16();
                    if kind != KIND_WRAP && kind != KIND_WRAP_EPH {
                        continue;
                    }
                    if tx
                        .send(Incoming {
                            event: *event,
                            relay: relay_url.to_string(),
                        })
                        .is_err()
                    {
                        break; // o motor foi embora
                    }
                }
            }
        });
        rx
    }

    /// Força uma verificação das conexões, usada quando o computador volta do
    /// modo de espera ou a rede muda.
    pub async fn wake(&self) {
        self.client.connect().await;
    }

    pub async fn shutdown(&self) {
        self.client.disconnect().await;
    }

    pub fn client(&self) -> &Client {
        &self.client
    }
}

/// Deixa o endereço de um servidor no formato canônico.
pub fn normalize_url(input: &str) -> Option<String> {
    let s = input.trim();
    if s.is_empty() {
        return None;
    }
    let s = if s.contains("://") {
        s.to_string()
    } else {
        format!("wss://{s}")
    };
    RelayUrl::parse(&s).ok().map(|u| u.to_string())
}

/// Permite compartilhar o conjunto entre a interface e as tarefas de rede.
pub type SharedPool = Arc<RelayPool>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normaliza_endereco_de_servidor() {
        assert_eq!(
            normalize_url("relay.damus.io").as_deref(),
            Some("wss://relay.damus.io")
        );
        assert_eq!(
            normalize_url("  wss://nos.lol  ").as_deref(),
            Some("wss://nos.lol")
        );
        assert_eq!(
            normalize_url("ws://127.0.0.1:7781").as_deref(),
            Some("ws://127.0.0.1:7781")
        );
        assert!(normalize_url("").is_none());
        assert!(normalize_url("   ").is_none());
    }
}
