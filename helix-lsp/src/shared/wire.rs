use anyhow::{bail, ensure, Context, Result};
use serde::{de::DeserializeOwned, Serialize};
use serde_json::Value;
use tokio::io::{
    AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt,
};

pub(super) const LIMIT: usize = 16 * 1024 * 1024;

pub(super) async fn read<T: DeserializeOwned>(reader: &mut (impl AsyncRead + Unpin)) -> Result<T> {
    let size = reader.read_u32_le().await? as usize;
    ensure!(size > 0 && size <= LIMIT, "invalid shared LSP frame size");
    let mut bytes = vec![0; size];
    reader.read_exact(&mut bytes).await?;
    Ok(serde_json::from_slice(&bytes)?)
}

pub(super) async fn write(
    writer: &mut (impl AsyncWrite + Unpin),
    value: &impl Serialize,
) -> Result<()> {
    let bytes = serde_json::to_vec(value)?;
    ensure!(bytes.len() <= LIMIT, "shared LSP frame exceeds limit");
    writer.write_u32_le(bytes.len() as u32).await?;
    writer.write_all(&bytes).await?;
    writer.flush().await?;
    Ok(())
}

pub(super) async fn read_lsp(reader: &mut (impl AsyncBufRead + Unpin)) -> Result<Option<Value>> {
    let mut length = None;
    let mut budget = 8192;
    loop {
        let mut line = String::new();
        let size = (&mut *reader).take(budget).read_line(&mut line).await?;
        if size == 0 {
            if length.is_none() {
                return Ok(None);
            }
            bail!("truncated LSP headers");
        }
        ensure!(
            line.ends_with('\n') && size < budget as usize,
            "oversized LSP headers"
        );
        budget -= size as u64;
        if line == "\r\n" || line == "\n" {
            break;
        }
        let (name, value) = line.split_once(':').context("invalid LSP header")?;
        if name.eq_ignore_ascii_case("content-length") {
            ensure!(length.is_none(), "duplicate content length");
            length = Some(value.trim().parse::<usize>()?);
        }
    }
    let length = length.context("missing content length")?;
    ensure!(
        length > 0 && length <= LIMIT,
        "LSP message exceeds shared transport limit"
    );
    let mut bytes = vec![0; length];
    reader.read_exact(&mut bytes).await?;
    let value: Value = serde_json::from_slice(&bytes)?;
    ensure!(value.is_object(), "expected LSP object");
    Ok(Some(value))
}

pub(super) async fn write_lsp(writer: &mut (impl AsyncWrite + Unpin), value: &Value) -> Result<()> {
    let bytes = serde_json::to_vec(value)?;
    writer
        .write_all(format!("Content-Length: {}\r\n\r\n", bytes.len()).as_bytes())
        .await?;
    writer.write_all(&bytes).await?;
    writer.flush().await?;
    Ok(())
}
