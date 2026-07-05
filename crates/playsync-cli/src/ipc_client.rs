//! Cliente do socket UDS exposto pelo `playsyncd`. Ver `playsync_core::ipc`
//! para o formato das mensagens (uma linha de JSON por requisicao/resposta).

use anyhow::{bail, Context, Result};
use playsync_core::ipc::{socket_path, Request, Response};
use rust_i18n::t;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

pub async fn send(request: Request) -> Result<Response> {
    let path = socket_path();
    let stream = UnixStream::connect(&path)
        .await
        .with_context(|| t!("cli.common.daemon_not_running", path = path.display()).to_string())?;

    let (reader, mut writer) = stream.into_split();

    let mut payload = serde_json::to_string(&request)?;
    payload.push('\n');
    writer.write_all(payload.as_bytes()).await?;

    let mut lines = BufReader::new(reader).lines();
    match lines.next_line().await? {
        Some(line) => Ok(serde_json::from_str(&line)?),
        None => bail!(t!("cli.common.daemon_closed_connection")),
    }
}
