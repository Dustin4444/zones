use std::io;

use serde::{Serialize, de::DeserializeOwned};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::{TcpListener, TcpStream, ToSocketAddrs},
};

pub const DEFAULT_VSOCK_PORT: u32 = 5005;
pub const DEFAULT_MAX_REQUEST_BYTES: usize = 8_388_608;
pub const DEFAULT_MAX_RESPONSE_BYTES: usize = 2_097_152;

pub async fn write_json_frame<W, T>(writer: &mut W, value: &T) -> io::Result<()>
where
    W: AsyncWrite + Unpin,
    T: Serialize,
{
    let payload = serde_json::to_vec(value).map_err(io::Error::other)?;
    let frame_len = u32::try_from(payload.len()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "frame too large for u32 length prefix",
        )
    })?;
    writer.write_all(&frame_len.to_be_bytes()).await?;
    writer.write_all(&payload).await?;
    writer.flush().await
}

pub async fn read_json_frame<R, T>(reader: &mut R, max_bytes: usize) -> io::Result<T>
where
    R: AsyncRead + Unpin,
    T: DeserializeOwned,
{
    let mut len = [0u8; 4];
    reader.read_exact(&mut len).await?;
    let len = usize::try_from(u32::from_be_bytes(len)).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "frame length does not fit usize",
        )
    })?;
    if len > max_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("frame length {len} exceeds cap {max_bytes}"),
        ));
    }

    let mut payload = vec![0u8; len];
    reader.read_exact(&mut payload).await?;
    serde_json::from_slice(&payload).map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))
}

pub async fn connect_tcp<A: ToSocketAddrs>(addr: A) -> io::Result<TcpStream> {
    TcpStream::connect(addr).await
}

pub async fn bind_tcp<A: ToSocketAddrs>(addr: A) -> io::Result<TcpListener> {
    TcpListener::bind(addr).await
}

#[cfg(target_os = "linux")]
pub use tokio_vsock::{VsockListener, VsockStream};

#[cfg(target_os = "linux")]
pub fn bind_vsock(port: u32) -> io::Result<VsockListener> {
    use tokio_vsock::{VMADDR_CID_ANY, VsockAddr};

    VsockListener::bind(VsockAddr::new(VMADDR_CID_ANY, port))
}

#[cfg(target_os = "linux")]
pub async fn connect_vsock(cid: u32, port: u32) -> io::Result<VsockStream> {
    use tokio_vsock::VsockAddr;

    VsockStream::connect(VsockAddr::new(cid, port)).await
}

#[cfg(not(target_os = "linux"))]
#[derive(Debug)]
pub struct VsockListener;

#[cfg(not(target_os = "linux"))]
impl VsockListener {
    pub async fn accept(&self) -> io::Result<(tokio::io::DuplexStream, ())> {
        Err(unsupported_vsock())
    }
}

#[cfg(not(target_os = "linux"))]
pub fn bind_vsock(_port: u32) -> io::Result<VsockListener> {
    Err(unsupported_vsock())
}

#[cfg(not(target_os = "linux"))]
pub async fn connect_vsock(_cid: u32, _port: u32) -> io::Result<tokio::io::DuplexStream> {
    Err(unsupported_vsock())
}

#[cfg(not(target_os = "linux"))]
fn unsupported_vsock() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "AF_VSOCK is supported only on Linux Nitro hosts/enclaves",
    )
}

#[cfg(test)]
mod tests {
    use serde::{Deserialize, Serialize};

    use super::*;

    #[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
    struct Msg {
        value: u64,
    }

    #[tokio::test]
    async fn json_frame_round_trips() {
        let (mut writer, mut reader) = tokio::io::duplex(1024);
        write_json_frame(&mut writer, &Msg { value: 42 })
            .await
            .unwrap();
        let decoded: Msg = read_json_frame(&mut reader, DEFAULT_MAX_RESPONSE_BYTES)
            .await
            .unwrap();
        assert_eq!(decoded, Msg { value: 42 });
    }

    #[tokio::test]
    async fn tcp_transport_round_trips() {
        let listener = bind_tcp("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let msg: Msg = read_json_frame(&mut stream, DEFAULT_MAX_REQUEST_BYTES)
                .await
                .unwrap();
            write_json_frame(
                &mut stream,
                &Msg {
                    value: msg.value.checked_add(1).expect("test value fits u64"),
                },
            )
            .await
            .unwrap();
        });

        let mut stream = connect_tcp(addr).await.unwrap();
        write_json_frame(&mut stream, &Msg { value: 10 })
            .await
            .unwrap();
        let response: Msg = read_json_frame(&mut stream, DEFAULT_MAX_RESPONSE_BYTES)
            .await
            .unwrap();
        handle.await.unwrap();
        assert_eq!(response, Msg { value: 11 });
    }
}
