//! The Unix socket server that the MCP bridge (`help-mcp`) connects to.
//! Each request goes to the guest agent in the VM.

use std::path::PathBuf;

use protocol::{Request, Response, ResponseBody};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;

pub fn serve(runtime: &tokio::runtime::Handle, socket: PathBuf, mac: vm::AgentMac) {
    runtime.spawn(async move {
        let _ = std::fs::remove_file(&socket);
        let listener = match UnixListener::bind(&socket) {
            Ok(l) => l,
            Err(e) => {
                tracing::error!("cannot listen on {}: {e}", socket.display());
                return;
            }
        };
        loop {
            let Ok((stream, _)) = listener.accept().await else { continue };
            let mac = mac.clone();
            tokio::spawn(async move {
                let (read, mut write) = stream.into_split();
                let mut lines = BufReader::new(read).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let resp = match serde_json::from_str::<Request>(&line) {
                        Ok(req) => {
                            let mac = mac.clone();
                            let id = req.id;
                            let body = tokio::task::spawn_blocking(move || mac.request(req.op))
                                .await
                                .map_err(|e| anyhow::anyhow!(e))
                                .and_then(|r| r);
                            Response {
                                id,
                                body: match body {
                                    Ok(reply) => ResponseBody::Ok(reply),
                                    Err(e) => ResponseBody::Error(format!("{e:#}")),
                                },
                            }
                        }
                        Err(e) => Response {
                            id: 0,
                            body: ResponseBody::Error(format!("bad request: {e}")),
                        },
                    };
                    let mut out = serde_json::to_vec(&resp).unwrap_or_default();
                    out.push(b'\n');
                    if write.write_all(&out).await.is_err() {
                        break;
                    }
                }
            });
        }
    });
}
