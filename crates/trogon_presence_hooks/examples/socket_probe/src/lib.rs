use std::net::{TcpStream, ToSocketAddrs};

wit_bindgen::generate!({
    path: "../../wit",
    world: "hooks",
});

struct SocketProbe;

impl Guest for SocketProbe {
    fn enrich(_op: Op, topic: String, _key: String, _meta: Vec<u8>) -> Result<Vec<u8>, HookError> {
        let lookup = format!("localhost:{topic}").to_socket_addrs().map(|_| ());
        let connect = TcpStream::connect(format!("127.0.0.1:{topic}")).map(|_| ());
        Err(HookError::Error(format!("lookup={lookup:?} connect={connect:?}")))
    }
}

export!(SocketProbe);
