use fastwebsockets::{Frame, WebSocketError, WebSocketRead, WebSocketWrite};
use tokio::io::{AsyncRead, AsyncWrite};

pub mod client_proxy;
pub mod server_proxy;

async fn copy_ws_to_ws<R, W>(mut ws_read: WebSocketRead<R>, mut ws_write: WebSocketWrite<W>) -> anyhow::Result<()>
where
	R: AsyncRead + Unpin,
	W: AsyncWrite + Unpin,
{
	while let Some(frame) = read_ws_frame(&mut ws_read).await? {
		ws_write.write_frame(frame).await?;
	}
	
	Ok(())
}

async fn read_ws_frame<R>(ws_read: &mut WebSocketRead<R>) -> Result<Option<Frame<'_>>, WebSocketError>
where
	R: AsyncRead + Unpin,
{
	// Since we aren't using auto close or auto pong, we don't need to supply a send function
	match ws_read.read_frame(&mut async |_| anyhow::Ok(())).await {
		Ok(frame) => Ok(Some(frame)),
		Err(WebSocketError::ConnectionClosed | WebSocketError::UnexpectedEOF) => Ok(None),
		Err(err) => Err(err),
	}
}