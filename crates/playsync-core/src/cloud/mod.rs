//! Backends de armazenamento em nuvem. Cada provedor implementa [`CloudBackend`];
//! o daemon so enxerga essa trait, nunca os detalhes de Google Drive/Box.

pub mod box_com;
pub mod gdrive;

use std::path::Path;

use anyhow::{Context, Result};
use async_trait::async_trait;

use crate::ipc::CloudProvider;

/// Um destino de backup na nuvem. A autenticacao (OAuth2) acontece uma vez via
/// CLI (`playsync cloud connect`); o daemon so precisa do token ja salvo.
#[async_trait]
pub trait CloudBackend: Send + Sync {
    fn provider(&self) -> CloudProvider;

    /// Envia `local_path` (ja compactado) para `remote_path`, um caminho
    /// logico tipo `PlaySync/<jogo>/save.zip` — os segmentos antes do ultimo
    /// sao nomes de pasta, criados sob demanda se ainda nao existirem.
    async fn upload(&self, local_path: &Path, remote_path: &str) -> Result<()>;

    /// Baixa o conteudo de `remote_path` (mesmo formato usado por `upload`).
    /// Erra se o arquivo ou alguma pasta do caminho nao existir.
    async fn download(&self, remote_path: &str) -> Result<Vec<u8>>;

    /// Verifica se ha um token valido salvo (i.e. `cloud connect` ja foi rodado).
    fn is_connected(&self) -> bool;
}

/// Instancia o backend correspondente ao provedor configurado.
pub fn backend_for(provider: CloudProvider) -> Box<dyn CloudBackend> {
    match provider {
        CloudProvider::GoogleDrive => Box::new(gdrive::GoogleDriveBackend::new()),
        CloudProvider::Box => Box::new(box_com::BoxBackend::new()),
    }
}

/// Sobe (bloqueante) um servidor HTTP so pra essa unica requisicao: e o
/// "redirect_uri" de loopback que os provedores (Google, Box) registram pra
/// apps desktop (RFC 8252). Roda dentro de `spawn_blocking` porque
/// `tiny_http` e sincrono. Compartilhado entre backends — o mesmo padrao de
/// captura serve pra qualquer Authorization Code flow local.
pub(crate) fn wait_for_redirect(port: u16) -> Result<(String, String)> {
    let server = tiny_http::Server::http(("127.0.0.1", port))
        .map_err(|err| anyhow::anyhow!("nao consegui escutar em 127.0.0.1:{port}: {err}"))?;

    let request = server
        .recv()
        .context("servidor de callback OAuth2 encerrou sem receber nada")?;

    let callback_url = format!("http://localhost{}", request.url());
    let parsed = oauth2::url::Url::parse(&callback_url)
        .context("URL de callback do navegador invalida")?;

    let mut code = None;
    let mut state = None;
    for (key, value) in parsed.query_pairs() {
        match key.as_ref() {
            "code" => code = Some(value.into_owned()),
            "state" => state = Some(value.into_owned()),
            _ => {}
        }
    }

    let response = tiny_http::Response::from_string(
        "PlaySync autorizado. Pode fechar esta aba e voltar ao terminal.",
    );
    let _ = request.respond(response);

    Ok((
        code.context("callback sem `code` na query string")?,
        state.context("callback sem `state` na query string")?,
    ))
}

/// Adaptador entre `oauth2::AsyncHttpClient` e o `reqwest::Client` que ja
/// existe no core (evita puxar a feature `reqwest` do crate `oauth2`, que
/// traria uma segunda copia do reqwest — ver comentario no Cargo.toml).
///
/// Precisa ser um tipo local (nao uma closure): a regra do orfao impede
/// `impl oauth2::AsyncHttpClient for reqwest::Client` diretamente aqui (os
/// dois sao de outro crate), e uma closure fechando sobre `&reqwest::Client`
/// esbarra num conflito de higher-ranked lifetimes com o `Send` que o
/// `async_trait` exige. `reqwest::Client` clona barato (e um Arc por baixo).
pub(crate) struct ReqwestHttpClient(pub reqwest::Client);

impl<'c> oauth2::AsyncHttpClient<'c> for ReqwestHttpClient {
    type Error = HttpAdapterError;
    type Future = std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<oauth2::HttpResponse, Self::Error>> + Send + 'c>,
    >;

    fn call(&'c self, request: oauth2::HttpRequest) -> Self::Future {
        Box::pin(async move {
            let response = self.0.execute(request.try_into()?).await?;

            let mut builder = oauth2::http::Response::builder().status(response.status());
            for (name, value) in response.headers().iter() {
                builder = builder.header(name, value);
            }
            Ok(builder.body(response.bytes().await?.to_vec())?)
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum HttpAdapterError {
    #[error("erro de rede: {0}")]
    Reqwest(#[from] reqwest::Error),
    #[error("resposta http invalida: {0}")]
    Http(#[from] oauth2::http::Error),
}
