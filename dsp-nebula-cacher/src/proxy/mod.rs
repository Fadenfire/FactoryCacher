use fastwebsockets::{WebSocketRead, WebSocketWrite};
use tokio::io::{AsyncRead, AsyncWrite};

pub mod client_proxy;
pub mod server_proxy;

async fn copy_ws_to_ws<R, W>(mut ws_read: WebSocketRead<R>, mut ws_write: WebSocketWrite<W>) -> anyhow::Result<()>
where
	R: AsyncRead + Unpin,
	W: AsyncWrite + Unpin,
{
	loop {
		// Since we aren't using auto close or auto pong, we don't need to supply a send function
		let frame = ws_read.read_frame(&mut async |_| anyhow::Ok(())).await?;
		
		ws_write.write_frame(frame).await?;
	}
}
