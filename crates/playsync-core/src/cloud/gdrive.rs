//! Backend do Google Drive: Authorization Code + PKCE via `oauth2`, captura do
//! redirect com um servidor `tiny_http` efemero em `localhost:8085`, e upload
//! simples (multipart) via Drive API v3.
//!
//! Escopo usado: `drive.file` (nao `drive`) — o app so enxerga os arquivos que
//! ele mesmo cria, nunca o Drive inteiro do usuario. Alem de ser o minimo
//! necessario pra um backup, evita cair na revisao de "escopos restritos" do
//! Google que se aplica ao escopo `drive` completo.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use oauth2::basic::BasicClient;
use oauth2::{
    AuthUrl, AuthorizationCode, ClientId, ClientSecret, CsrfToken, PkceCodeChallenge, RedirectUrl,
    RefreshToken, Scope, TokenResponse, TokenUrl,
};
use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::ipc::CloudProvider;

use super::{wait_for_redirect, CloudBackend, ReqwestHttpClient};

const REDIRECT_PORT: u16 = 8085;
const DRIVE_SCOPE: &str = "https://www.googleapis.com/auth/drive.file";
const MULTIPART_BOUNDARY: &str = "playsync-boundary-7d1f2c94";

pub struct GoogleDriveBackend {
    http: reqwest::Client,
}

impl GoogleDriveBackend {
    pub fn new() -> Self {
        Self {
            // Sem seguir redirects: nenhuma etapa do fluxo OAuth2/Drive precisa
            // disso, e seguir redirects "de graca" numa chamada autenticada e
            // um vetor classico de SSRF.
            http: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .expect("configuracao do reqwest::Client e estatica e valida"),
        }
    }

    /// Fluxo interativo: abre o navegador, escuta em `localhost:8085` pelo
    /// redirect com o `code`, troca por um token e salva tudo em disco.
    /// Chamado por `playsync cloud connect google-drive`.
    pub async fn connect(&self) -> Result<()> {
        let creds = GdriveClientCredentials::load()?;

        let client = BasicClient::new(ClientId::new(creds.client_id))
            .set_client_secret(ClientSecret::new(creds.client_secret))
            .set_auth_uri(AuthUrl::new(creds.auth_uri)?)
            .set_token_uri(TokenUrl::new(creds.token_uri)?)
            .set_redirect_uri(RedirectUrl::new(format!(
                "http://localhost:{REDIRECT_PORT}"
            ))?);

        let (pkce_challenge, pkce_verifier) = PkceCodeChallenge::new_random_sha256();

        let (auth_url, csrf_token) = client
            .authorize_url(CsrfToken::new_random)
            .add_scope(Scope::new(DRIVE_SCOPE.to_string()))
            .set_pkce_challenge(pkce_challenge)
            .url();

        println!("Abrindo o navegador para autorizar o PlaySync no Google Drive...");
        if webbrowser::open(auth_url.as_str()).is_err() {
            println!("Nao consegui abrir o navegador automaticamente. Acesse manualmente:\n{auth_url}");
        }

        let (code, state) = tokio::task::spawn_blocking(|| wait_for_redirect(REDIRECT_PORT))
            .await
            .context("a thread que esperava o redirect do navegador falhou")??;

        if state != *csrf_token.secret() {
            bail!("o `state` devolvido pelo Google nao bate com o esperado (possivel CSRF) — abortando");
        }

        let token = client
            .exchange_code(AuthorizationCode::new(code))
            .set_pkce_verifier(pkce_verifier)
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
        config.cloud_provider = Some("google-drive".to_string());
        config.save()?;

        println!("Google Drive conectado com sucesso.");
        Ok(())
    }

    /// Retorna um access token valido, renovando via `refresh_token` se necessario.
    async fn access_token(&self) -> Result<String> {
        let mut token = load_token()?.context(
            "Google Drive nao conectado — rode `playsync cloud connect google-drive`",
        )?;

        let expired = token
            .expires_at
            .map(|exp| exp <= Utc::now() + Duration::seconds(60))
            .unwrap_or(false);

        if !expired {
            return Ok(token.access_token);
        }

        let refresh_token = token.refresh_token.clone().context(
            "token expirado e sem refresh_token salvo — rode \
             `playsync cloud connect google-drive` de novo",
        )?;

        let creds = GdriveClientCredentials::load()?;
        let client = BasicClient::new(ClientId::new(creds.client_id))
            .set_client_secret(ClientSecret::new(creds.client_secret))
            .set_auth_uri(AuthUrl::new(creds.auth_uri)?)
            .set_token_uri(TokenUrl::new(creds.token_uri)?);

        let refreshed = client
            .exchange_refresh_token(&RefreshToken::new(refresh_token.clone()))
            .request_async(&ReqwestHttpClient(self.http.clone()))
            .await
            .map_err(|err| anyhow::anyhow!("falha ao renovar o token do Google Drive: {err}"))?;

        token = StoredToken {
            access_token: refreshed.access_token().secret().clone(),
            // O Google normalmente nao reemite refresh_token num refresh; mantem o antigo.
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

    /// Acha a pasta `name` dentro de `parent` (ou cria uma nova, se nao
    /// existir ainda) e devolve o `id` dela. Idempotente entre chamadas —
    /// nao ha cache local de ids, cada upload resolve a cadeia de pastas de
    /// novo, mas isso e so uma chamada de API extra por nivel, aceitavel pro
    /// volume de um backup de saves.
    async fn ensure_folder(&self, access_token: &str, name: &str, parent: &str) -> Result<String> {
        let escaped_name = name.replace('\\', "\\\\").replace('\'', "\\'");
        let query = format!(
            "name = '{escaped_name}' and '{parent}' in parents \
             and mimeType = 'application/vnd.google-apps.folder' and trashed = false"
        );

        let mut url = oauth2::url::Url::parse("https://www.googleapis.com/drive/v3/files")
            .expect("URL estatica valida");
        url.query_pairs_mut()
            .append_pair("q", &query)
            .append_pair("fields", "files(id)");

        let response = self
            .http
            .get(url)
            .bearer_auth(access_token)
            .send()
            .await
            .context("falha ao procurar pasta no Google Drive")?;

        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            bail!("Google Drive respondeu {status} ao procurar pasta \"{name}\": {text}");
        }

        let parsed: serde_json::Value = response
            .json()
            .await
            .context("resposta do Google Drive nao veio no formato JSON esperado")?;
        if let Some(id) = parsed["files"][0]["id"].as_str() {
            return Ok(id.to_string());
        }

        let metadata = serde_json::json!({
            "name": name,
            "mimeType": "application/vnd.google-apps.folder",
            "parents": [parent],
        });
        let response = self
            .http
            .post("https://www.googleapis.com/drive/v3/files")
            .bearer_auth(access_token)
            .json(&metadata)
            .send()
            .await
            .context("falha ao criar pasta no Google Drive")?;

        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            bail!("Google Drive respondeu {status} ao criar pasta \"{name}\": {text}");
        }

        let parsed: serde_json::Value = response
            .json()
            .await
            .context("resposta do Google Drive nao veio no formato JSON esperado")?;
        parsed["id"]
            .as_str()
            .map(str::to_string)
            .context("Google Drive nao devolveu o id da pasta criada")
    }
}

impl Default for GoogleDriveBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl CloudBackend for GoogleDriveBackend {
    fn provider(&self) -> CloudProvider {
        CloudProvider::GoogleDrive
    }

    /// Upload simples (nao-resumavel) via Drive API v3. Serve bem pra arquivos
    /// pequenos/medios (saves compactados); saves gigantes vao precisar de
    /// upload resumavel mais adiante.
    ///
    /// `remote_path` tipo `PlaySync/<jogo>/save.zip`: os segmentos antes do
    /// ultimo sao pastas, criadas (ou reaproveitadas, se ja existirem) antes
    /// do upload em si.
    async fn upload(&self, local_path: &Path, remote_path: &str) -> Result<()> {
        let access_token = self.access_token().await?;

        let mut segments = remote_path.split('/').filter(|s| !s.is_empty());
        let file_name = segments
            .next_back()
            .context("remote_path vazio — sem nome de arquivo")?;

        let mut parent = "root".to_string();
        for folder_name in segments {
            parent = self.ensure_folder(&access_token, folder_name, &parent).await?;
        }

        let bytes = tokio::fs::read(local_path)
            .await
            .with_context(|| format!("nao consegui ler {}", local_path.display()))?;

        let metadata =
            serde_json::json!({ "name": file_name, "parents": [parent] }).to_string();
        let body = build_multipart_body(&metadata, &bytes);

        let response = self
            .http
            .post("https://www.googleapis.com/upload/drive/v3/files?uploadType=multipart")
            .bearer_auth(access_token)
            .header(
                reqwest::header::CONTENT_TYPE,
                format!("multipart/related; boundary={MULTIPART_BOUNDARY}"),
            )
            .body(body)
            .send()
            .await
            .context("falha ao enviar o arquivo para o Google Drive")?;

        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            bail!("Google Drive respondeu {status}: {text}");
        }

        let parsed: serde_json::Value = response
            .json()
            .await
            .context("resposta do Google Drive nao veio no formato JSON esperado")?;
        let file_id = parsed.get("id").and_then(|v| v.as_str()).unwrap_or("?");
        tracing::info!(file_id, remote_path, "upload para o Google Drive concluido");

        Ok(())
    }

    fn is_connected(&self) -> bool {
        token_path().map(|p| p.exists()).unwrap_or(false)
    }
}

/// Credenciais do *app* (client_id/client_secret registrados no Google Cloud
/// Console), nao do usuario final. Espelham exatamente o JSON que o console
/// gera pra um OAuth client do tipo "Desktop app" (chave raiz `"installed"`):
///
/// ```json
/// { "installed": { "client_id": "...", "client_secret": "...", ... } }
/// ```
///
/// Isso e proposital: pra trocar de credenciais basta substituir o arquivo em
/// `~/.config/playsync/gdrive_client_secret.json` pelo seu proprio download do
/// console — sem editar ou recompilar nada.
#[derive(Debug, Clone, Deserialize)]
struct ClientSecretFile {
    installed: GdriveClientCredentials,
}

#[derive(Debug, Clone, Deserialize)]
struct GdriveClientCredentials {
    client_id: String,
    client_secret: String,
    auth_uri: String,
    token_uri: String,
}

impl GdriveClientCredentials {
    fn load() -> Result<Self> {
        let path = client_secret_path()?;
        let text = std::fs::read_to_string(&path).with_context(|| {
            format!(
                "credenciais OAuth do Google Drive nao encontradas em {} — crie um \
                 \"OAuth client ID\" do tipo Desktop app em \
                 https://console.cloud.google.com/apis/credentials, adicione \
                 http://localhost:{REDIRECT_PORT} como redirect URI e salve o \
                 JSON baixado nesse caminho",
                path.display()
            )
        })?;
        let file: ClientSecretFile = serde_json::from_str(&text)
            .with_context(|| format!("{} nao e um client_secret.json valido", path.display()))?;
        Ok(file.installed)
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
        .join("gdrive_client_secret.json"))
}

fn token_path() -> Result<PathBuf> {
    Ok(Config::config_path()?
        .parent()
        .context("config_path sem diretorio pai")?
        .join("tokens")
        .join("gdrive.json"))
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

    // access_token/refresh_token sao segredos: restringe a leitura ao dono.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

/// Monta o corpo `multipart/related` exigido pelo upload multipart da Drive
/// API v3 — nao e o mesmo formato que `reqwest::multipart::Form` (que gera
/// `multipart/form-data`), entao montamos os bytes na mao.
fn build_multipart_body(metadata_json: &str, file_bytes: &[u8]) -> Vec<u8> {
    let mut body = Vec::with_capacity(metadata_json.len() + file_bytes.len() + 256);

    body.extend_from_slice(
        format!("--{MULTIPART_BOUNDARY}\r\nContent-Type: application/json; charset=UTF-8\r\n\r\n")
            .as_bytes(),
    );
    body.extend_from_slice(metadata_json.as_bytes());

    body.extend_from_slice(
        format!("\r\n--{MULTIPART_BOUNDARY}\r\nContent-Type: application/zip\r\n\r\n").as_bytes(),
    );
    body.extend_from_slice(file_bytes);

    body.extend_from_slice(format!("\r\n--{MULTIPART_BOUNDARY}--").as_bytes());
    body
}
