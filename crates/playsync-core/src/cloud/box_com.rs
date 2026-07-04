//! Backend do Box.com: Authorization Code via `oauth2`, captura do redirect
//! com `tiny_http` em `localhost:8086` (mesmo padrao do Google Drive, porta
//! diferente pra nao colidir), upload via Box Content API v2.0.
//!
//! Sem PKCE aqui: ao contrario do Drive (client "Desktop app" sem secret
//! seguro), o app Box e confidencial (tem `client_secret`), entao o
//! Authorization Code puro ja e seguro o suficiente.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use oauth2::basic::BasicClient;
use oauth2::{
    AuthUrl, AuthorizationCode, ClientId, ClientSecret, CsrfToken, RedirectUrl, RefreshToken,
    TokenResponse, TokenUrl,
};
use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::ipc::CloudProvider;

use super::{wait_for_redirect, CloudBackend, ReqwestHttpClient};

const REDIRECT_PORT: u16 = 8086;
const AUTH_URL: &str = "https://account.box.com/api/oauth2/authorize";
const TOKEN_URL: &str = "https://api.box.com/oauth2/token";

pub struct BoxBackend {
    http: reqwest::Client,
}

impl BoxBackend {
    pub fn new() -> Self {
        Self {
            http: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .expect("configuracao do reqwest::Client e estatica e valida"),
        }
    }

    /// Fluxo interativo: abre o navegador, escuta em `localhost:8086` pelo
    /// redirect com o `code`, troca por um token e salva tudo em disco.
    /// Chamado por `playsync cloud connect box`.
    pub async fn connect(&self) -> Result<()> {
        let creds = BoxClientCredentials::load()?;

        let client = BasicClient::new(ClientId::new(creds.client_id))
            .set_client_secret(ClientSecret::new(creds.client_secret))
            .set_auth_uri(AuthUrl::new(AUTH_URL.to_string())?)
            .set_token_uri(TokenUrl::new(TOKEN_URL.to_string())?)
            .set_redirect_uri(RedirectUrl::new(format!(
                "http://localhost:{REDIRECT_PORT}"
            ))?);

        let (auth_url, csrf_token) = client.authorize_url(CsrfToken::new_random).url();

        println!("Abrindo o navegador para autorizar o PlaySync no Box...");
        if webbrowser::open(auth_url.as_str()).is_err() {
            println!("Nao consegui abrir o navegador automaticamente. Acesse manualmente:\n{auth_url}");
        }

        let (code, state) = tokio::task::spawn_blocking(|| wait_for_redirect(REDIRECT_PORT))
            .await
            .context("a thread que esperava o redirect do navegador falhou")??;

        if state != *csrf_token.secret() {
            bail!("o `state` devolvido pelo Box nao bate com o esperado (possivel CSRF) — abortando");
        }

        let token = client
            .exchange_code(AuthorizationCode::new(code))
            .request_async(&ReqwestHttpClient(self.http.clone()))
            .await
            .map_err(|err| anyhow::anyhow!("falha ao trocar o codigo por um token: {err}"))?;

        save_token(&StoredToken {
            access_token: token.access_token().secret().clone(),
            refresh_token: token.refresh_token().map(|t| t.secret().clone()),
            expires_at: token
                .expires_in()
                .and_then(|d| Duration::from_std(d).ok())
                .map(|d| Utc::now() + d),
        })?;

        let mut config = Config::load_or_default()?;
        config.cloud_provider = Some("box".to_string());
        config.save()?;

        println!("Box conectado com sucesso.");
        Ok(())
    }

    /// Retorna um access token valido, renovando via `refresh_token` se necessario.
    async fn access_token(&self) -> Result<String> {
        let mut token =
            load_token()?.context("Box nao conectado — rode `playsync cloud connect box`")?;

        let expired = token
            .expires_at
            .map(|exp| exp <= Utc::now() + Duration::seconds(60))
            .unwrap_or(false);

        if !expired {
            return Ok(token.access_token);
        }

        let refresh_token = token
            .refresh_token
            .clone()
            .context("token expirado e sem refresh_token salvo — rode `playsync cloud connect box` de novo")?;

        let creds = BoxClientCredentials::load()?;
        let client = BasicClient::new(ClientId::new(creds.client_id))
            .set_client_secret(ClientSecret::new(creds.client_secret))
            .set_auth_uri(AuthUrl::new(AUTH_URL.to_string())?)
            .set_token_uri(TokenUrl::new(TOKEN_URL.to_string())?);

        let refreshed = client
            .exchange_refresh_token(&RefreshToken::new(refresh_token.clone()))
            .request_async(&ReqwestHttpClient(self.http.clone()))
            .await
            .map_err(|err| anyhow::anyhow!("falha ao renovar o token do Box: {err}"))?;

        token = StoredToken {
            access_token: refreshed.access_token().secret().clone(),
            // O Box normalmente reemite (e invalida o antigo) a cada refresh;
            // so cai pro antigo se por algum motivo a resposta nao trouxer um novo.
            refresh_token: refreshed
                .refresh_token()
                .map(|t| t.secret().clone())
                .or(Some(refresh_token)),
            expires_at: refreshed
                .expires_in()
                .and_then(|d| Duration::from_std(d).ok())
                .map(|d| Utc::now() + d),
        };
        save_token(&token)?;

        Ok(token.access_token)
    }

    /// Acha a subpasta `name` dentro de `parent_id` (ou cria uma nova) e
    /// devolve o `id` dela. Ao contrario do Drive, a Box API nao filtra por
    /// nome no servidor nem devolve o id do conflito num 409 de pasta —
    /// listamos os itens do pai e comparamos o nome na mao.
    async fn ensure_folder(&self, access_token: &str, name: &str, parent_id: &str) -> Result<String> {
        if let Some(id) = self.find_folder(access_token, name, parent_id).await? {
            return Ok(id);
        }

        let body = serde_json::json!({ "name": name, "parent": { "id": parent_id } });
        let response = self
            .http
            .post("https://api.box.com/2.0/folders")
            .bearer_auth(access_token)
            .json(&body)
            .send()
            .await
            .context("falha ao criar pasta no Box")?;

        let status = response.status();
        if status.as_u16() == 409 {
            // Corrida com outra criacao concorrente (ou ja existia e a
            // listagem anterior nao pegou por paginacao): relista em vez de
            // tentar interpretar o corpo do 409, que nao traz o id aqui.
            return self
                .find_folder(access_token, name, parent_id)
                .await?
                .context("Box respondeu 409 ao criar pasta mas ela nao aparece na listagem do pai")
        }
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            bail!("Box respondeu {status} ao criar pasta \"{name}\": {text}");
        }

        let parsed: serde_json::Value = response
            .json()
            .await
            .context("resposta do Box nao veio no formato JSON esperado")?;
        parsed["id"]
            .as_str()
            .map(str::to_string)
            .context("Box nao devolveu o id da pasta criada")
    }

    async fn find_folder(
        &self,
        access_token: &str,
        name: &str,
        parent_id: &str,
    ) -> Result<Option<String>> {
        self.find_entry(access_token, name, parent_id, "folder").await
    }

    /// Acha o `id` de uma pasta ou arquivo chamado `name` dentro de `parent_id`.
    async fn find_entry(
        &self,
        access_token: &str,
        name: &str,
        parent_id: &str,
        entry_type: &str,
    ) -> Result<Option<String>> {
        let mut url = oauth2::url::Url::parse(&format!(
            "https://api.box.com/2.0/folders/{parent_id}/items"
        ))
        .expect("URL valida");
        url.query_pairs_mut()
            .append_pair("fields", "id,name,type")
            .append_pair("limit", "1000");

        let response = self
            .http
            .get(url)
            .bearer_auth(access_token)
            .send()
            .await
            .context("falha ao listar pasta no Box")?;

        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            bail!("Box respondeu {status} ao listar pasta pai (id={parent_id}): {text}");
        }

        let parsed: serde_json::Value = response
            .json()
            .await
            .context("resposta do Box nao veio no formato JSON esperado")?;
        let Some(entries) = parsed["entries"].as_array() else {
            return Ok(None);
        };

        Ok(entries
            .iter()
            .find(|entry| entry["type"] == entry_type && entry["name"] == name)
            .and_then(|entry| entry["id"].as_str())
            .map(str::to_string))
    }

    /// Resolve `remote_path` (`PlaySync/<jogo>/save.zip`) num `(parent_id,
    /// file_name)`, sem criar nada — usado por `download`. Erra se qualquer
    /// pasta no caminho nao existir.
    async fn resolve_path<'a>(
        &self,
        access_token: &str,
        remote_path: &'a str,
    ) -> Result<(String, &'a str)> {
        let mut segments = remote_path.split('/').filter(|s| !s.is_empty());
        let file_name = segments
            .next_back()
            .context("remote_path vazio — sem nome de arquivo")?;

        let mut parent = "0".to_string();
        for folder_name in segments {
            parent = self
                .find_folder(access_token, folder_name, &parent)
                .await?
                .with_context(|| format!("pasta \"{folder_name}\" nao encontrada no Box"))?;
        }

        Ok((parent, file_name))
    }

    /// Resolve `remote_dir` (todos os segmentos sao pastas) pro id da
    /// ultima, sem criar nada. `None` se alguma pasta no caminho nao existir
    /// ainda (ex: primeiro backup desse jogo).
    async fn resolve_folder_path(&self, access_token: &str, remote_dir: &str) -> Result<Option<String>> {
        let mut parent = "0".to_string();
        for folder_name in remote_dir.split('/').filter(|s| !s.is_empty()) {
            match self.find_folder(access_token, folder_name, &parent).await? {
                Some(id) => parent = id,
                None => return Ok(None),
            }
        }
        Ok(Some(parent))
    }

    /// Nomes de todos os itens do tipo `entry_type` ("file" ou "folder")
    /// dentro de `parent_id` — mesma chamada de listagem que `find_entry`
    /// usa, so que devolvendo todos os nomes em vez de procurar um so.
    async fn list_entries(&self, access_token: &str, parent_id: &str, entry_type: &str) -> Result<Vec<String>> {
        let mut url = oauth2::url::Url::parse(&format!(
            "https://api.box.com/2.0/folders/{parent_id}/items"
        ))
        .expect("URL valida");
        url.query_pairs_mut()
            .append_pair("fields", "id,name,type")
            .append_pair("limit", "1000");

        let response = self
            .http
            .get(url)
            .bearer_auth(access_token)
            .send()
            .await
            .context("falha ao listar pasta no Box")?;

        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            bail!("Box respondeu {status} ao listar pasta (id={parent_id}): {text}");
        }

        let parsed: serde_json::Value = response
            .json()
            .await
            .context("resposta do Box nao veio no formato JSON esperado")?;
        Ok(parsed["entries"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|entry| entry["type"] == entry_type)
            .filter_map(|entry| entry["name"].as_str().map(str::to_string))
            .collect())
    }
}

impl Default for BoxBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl CloudBackend for BoxBackend {
    fn provider(&self) -> CloudProvider {
        CloudProvider::Box
    }

    /// Upload via Box Content API. `remote_path` tipo `PlaySync/<jogo>/save.zip`:
    /// os segmentos antes do ultimo sao pastas, criadas/reaproveitadas antes
    /// do upload. Se um arquivo com o mesmo nome ja existir na pasta (409),
    /// sobe como nova versao dele em vez de falhar.
    async fn upload(&self, local_path: &Path, remote_path: &str) -> Result<()> {
        let access_token = self.access_token().await?;

        let mut segments = remote_path.split('/').filter(|s| !s.is_empty());
        let file_name = segments
            .next_back()
            .context("remote_path vazio — sem nome de arquivo")?
            .to_string();

        let mut parent = "0".to_string(); // "0" e a raiz no Box
        for folder_name in segments {
            parent = self.ensure_folder(&access_token, folder_name, &parent).await?;
        }

        let bytes = tokio::fs::read(local_path)
            .await
            .with_context(|| format!("nao consegui ler {}", local_path.display()))?;

        let attributes = serde_json::json!({ "name": file_name, "parent": { "id": parent } });
        let form = reqwest::multipart::Form::new()
            .text("attributes", attributes.to_string())
            .part(
                "file",
                reqwest::multipart::Part::bytes(bytes.clone()).file_name(file_name.clone()),
            );

        let response = self
            .http
            .post("https://upload.box.com/api/2.0/files/content")
            .bearer_auth(&access_token)
            .multipart(form)
            .send()
            .await
            .context("falha ao enviar o arquivo para o Box")?;

        let status = response.status();
        if status.as_u16() == 409 {
            let text = response.text().await.unwrap_or_default();
            let parsed: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
            // A API documenta `conflicts` como array, mas na pratica (upload de
            // arquivo unico) ela volta um objeto solto — aceita as duas formas.
            let conflicts = &parsed["context_info"]["conflicts"];
            let existing_id = conflicts["id"]
                .as_str()
                .or_else(|| conflicts[0]["id"].as_str())
                .context("Box respondeu 409 (arquivo ja existe) mas sem id do arquivo existente")?
                .to_string();

            return self.overwrite(&access_token, &existing_id, &file_name, bytes).await;
        }

        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            bail!("Box respondeu {status}: {text}");
        }

        let parsed: serde_json::Value = response
            .json()
            .await
            .context("resposta do Box nao veio no formato JSON esperado")?;
        let file_id = parsed["entries"][0]["id"].as_str().unwrap_or("?");
        tracing::info!(file_id, remote_path, "upload para o Box concluido");

        Ok(())
    }

    /// Baixa o conteudo de `remote_path`. O endpoint de download do Box
    /// responde com um 302 pra `dl.boxcloud.com` (URL pre-assinada, sem
    /// precisar do Authorization nessa segunda chamada) — como o cliente
    /// HTTP do backend nao segue redirect (protecao contra SSRF), seguimos
    /// esse unico hop na mao, e so pra esse host especifico.
    async fn download(&self, remote_path: &str) -> Result<Vec<u8>> {
        let access_token = self.access_token().await?;
        let (parent, file_name) = self.resolve_path(&access_token, remote_path).await?;
        let file_id = self
            .find_entry(&access_token, file_name, &parent, "file")
            .await?
            .with_context(|| format!("arquivo nao encontrado no Box: {remote_path}"))?;

        let response = self
            .http
            .get(format!("https://api.box.com/2.0/files/{file_id}/content"))
            .bearer_auth(&access_token)
            .send()
            .await
            .context("falha ao baixar o arquivo do Box")?;

        let status = response.status();
        if status.is_redirection() {
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .context("Box respondeu redirect sem header Location")?
                .to_str()
                .context("header Location do Box nao e ASCII valido")?
                .to_string();

            let host = oauth2::url::Url::parse(&location)
                .ok()
                .and_then(|u| u.host_str().map(str::to_string))
                .unwrap_or_default();
            anyhow::ensure!(
                host == "dl.boxcloud.com" || host.ends_with(".boxcloud.com"),
                "redirect de download do Box apontou pra um host inesperado: {host}"
            );

            let download = self
                .http
                .get(&location)
                .send()
                .await
                .context("falha ao seguir o redirect de download do Box")?;
            let download_status = download.status();
            if !download_status.is_success() {
                bail!("boxcloud.com respondeu {download_status} ao baixar {remote_path}");
            }
            return Ok(download
                .bytes()
                .await
                .context("falha ao ler o corpo da resposta de dl.boxcloud.com")?
                .to_vec());
        }

        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            bail!("Box respondeu {status} ao baixar {remote_path}: {text}");
        }

        Ok(response
            .bytes()
            .await
            .context("falha ao ler o corpo da resposta do Box")?
            .to_vec())
    }

    /// Lista os nomes dos arquivos dentro de `remote_dir`. Vazio se a pasta
    /// ainda nao existir (nenhum backup desse jogo ainda).
    async fn list_files(&self, remote_dir: &str) -> Result<Vec<String>> {
        let access_token = self.access_token().await?;
        let Some(parent) = self.resolve_folder_path(&access_token, remote_dir).await? else {
            return Ok(Vec::new());
        };
        self.list_entries(&access_token, &parent, "file").await
    }

    /// Apaga o arquivo em `remote_path`. Sem erro se o arquivo (ou alguma
    /// pasta do caminho) ja nao existir — podar algo que ja sumiu nao e falha.
    async fn delete(&self, remote_path: &str) -> Result<()> {
        let access_token = self.access_token().await?;

        let (remote_dir, file_name) = remote_path
            .rsplit_once('/')
            .context("remote_path sem \"/\" — esperado algo tipo \"PlaySync/<jogo>/<arquivo>\"")?;

        let Some(parent) = self.resolve_folder_path(&access_token, remote_dir).await? else {
            return Ok(());
        };
        let Some(file_id) = self.find_entry(&access_token, file_name, &parent, "file").await? else {
            return Ok(());
        };

        let response = self
            .http
            .delete(format!("https://api.box.com/2.0/files/{file_id}"))
            .bearer_auth(access_token)
            .send()
            .await
            .context("falha ao apagar arquivo no Box")?;

        let status = response.status();
        if !status.is_success() && status.as_u16() != 404 {
            let text = response.text().await.unwrap_or_default();
            bail!("Box respondeu {status} ao apagar \"{remote_path}\": {text}");
        }
        Ok(())
    }

    fn is_connected(&self) -> bool {
        token_path().map(|p| p.exists()).unwrap_or(false)
    }
}

impl BoxBackend {
    async fn overwrite(
        &self,
        access_token: &str,
        file_id: &str,
        file_name: &str,
        bytes: Vec<u8>,
    ) -> Result<()> {
        let attributes = serde_json::json!({ "name": file_name });
        let form = reqwest::multipart::Form::new()
            .text("attributes", attributes.to_string())
            .part(
                "file",
                reqwest::multipart::Part::bytes(bytes).file_name(file_name.to_string()),
            );

        let response = self
            .http
            .post(format!("https://upload.box.com/api/2.0/files/{file_id}/content"))
            .bearer_auth(access_token)
            .multipart(form)
            .send()
            .await
            .context("falha ao atualizar o arquivo existente no Box")?;

        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            bail!("Box respondeu {status} ao atualizar arquivo existente: {text}");
        }

        tracing::info!(file_id, "upload (nova versao) para o Box concluido");
        Ok(())
    }
}

/// Credenciais do *app* Box (client_id/client_secret de um "Custom App" no
/// Box Developer Console, com autenticacao "User Authentication (OAuth 2.0)").
/// Formato simples e proprio do PlaySync (o Box nao tem um download de
/// client_secret.json padronizado como o Google).
#[derive(Debug, Clone, Deserialize)]
struct BoxClientCredentials {
    client_id: String,
    client_secret: String,
}

impl BoxClientCredentials {
    fn load() -> Result<Self> {
        let path = client_secret_path()?;
        let text = std::fs::read_to_string(&path).with_context(|| {
            format!(
                "credenciais OAuth do Box nao encontradas em {} — crie um Custom App em \
                 https://app.box.com/developers/console, habilite \"User Authentication \
                 (OAuth 2.0)\", adicione http://localhost:{REDIRECT_PORT} como redirect URI \
                 e salve `{{\"client_id\": \"...\", \"client_secret\": \"...\"}}` nesse caminho",
                path.display()
            )
        })?;
        serde_json::from_str(&text)
            .with_context(|| format!("{} nao e um box_client_secret.json valido", path.display()))
    }
}

/// Token de acesso/refresh do *usuario*, persistido apos o fluxo interativo.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredToken {
    access_token: String,
    refresh_token: Option<String>,
    expires_at: Option<DateTime<Utc>>,
}

fn client_secret_path() -> Result<PathBuf> {
    Ok(Config::config_path()?
        .parent()
        .context("config_path sem diretorio pai")?
        .join("box_client_secret.json"))
}

fn token_path() -> Result<PathBuf> {
    Ok(Config::config_path()?
        .parent()
        .context("config_path sem diretorio pai")?
        .join("tokens")
        .join("box.json"))
}

fn load_token() -> Result<Option<StoredToken>> {
    let path = token_path()?;
    if !path.exists() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("nao consegui ler {}", path.display()))?;
    Ok(Some(serde_json::from_str(&text)?))
}

fn save_token(token: &StoredToken) -> Result<()> {
    let path = token_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, serde_json::to_string_pretty(token)?)
        .with_context(|| format!("nao consegui escrever {}", path.display()))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}
