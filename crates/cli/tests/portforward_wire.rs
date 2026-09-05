//! Verifie que le client de tunnel de la CLI (`crate::tunnel`, tache 9.2)
//! parle bien le meme protocole que le VRAI binaire `atelier-net-proxy`
//! (pas un double reimplemente a la main) : demarre `atelier-net-proxy` en
//! sous-processus, pointe sur un serveur TCP reel (pas un mock — un simple
//! echo qui met en majuscules), et verifie qu'un message envoye sur le
//! canal 0 (donnees, port d'indice 0) revient bien transforme.
//!
//! Necessite que `cargo build -p atelier-net-proxy` ait deja produit le
//! binaire (meme profil que ce test) : ignore silencieusement sinon, pour
//! ne pas faire echouer `cargo test --workspace` dans un environnement qui
//! n'a construit que `atelier-cli`.

use futures_util::{SinkExt, StreamExt};
use std::process::{Child, Command, Stdio};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::Message;

const DATA_CHANNEL: u8 = 0;

struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

async fn spawn_echo_upper_server() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                loop {
                    let n = match stream.read(&mut buf).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => n,
                    };
                    let upper: Vec<u8> = buf[..n].to_ascii_uppercase();
                    if stream.write_all(&upper).await.is_err() {
                        return;
                    }
                }
            });
        }
    });
    port
}

fn net_proxy_binary() -> Option<std::path::PathBuf> {
    let candidate = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/debug/atelier-net-proxy");
    candidate.exists().then_some(candidate)
}

async fn wait_for_port(port: u16) {
    for _ in 0..100 {
        if TcpStream::connect(("127.0.0.1", port)).await.is_ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("net-proxy n'a jamais ouvert le port de controle {port}");
}

#[tokio::test]
async fn cli_wire_format_is_compatible_with_real_net_proxy() {
    let Some(bin) = net_proxy_binary() else {
        eprintln!("atelier-net-proxy non construit, test ignore (voir doc de module)");
        return;
    };

    let echo_port = spawn_echo_upper_server().await;
    let control_addr = "127.0.0.1:0";
    let control_listener = std::net::TcpListener::bind(control_addr).unwrap();
    let control_port = control_listener.local_addr().unwrap().port();
    drop(control_listener);

    let child = Command::new(bin)
        .env(
            "ATELIER_NET_PROXY_CONTROL_ADDR",
            format!("127.0.0.1:{control_port}"),
        )
        .env("ATELIER_VM_ADDR", "127.0.0.1")
        .env("ATELIER_EGRESS_ALLOWLIST", "example.com")
        .env("ATELIER_NET_PROXY_LISTEN_ADDR", "127.0.0.1:0")
        .env("ATELIER_NET_PROXY_ADMIN_ADDR", "127.0.0.1:0")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("lancement de atelier-net-proxy");
    let _guard = ChildGuard(child);

    wait_for_port(control_port).await;

    let url = format!("ws://127.0.0.1:{control_port}/portforward?ports={echo_port}");
    let (mut ws, _) = tokio_tungstenite::connect_async(url)
        .await
        .expect("connexion websocket au port-forward de net-proxy");

    let mut payload = vec![DATA_CHANNEL];
    payload.extend_from_slice(b"hello atelier\n");
    ws.send(Message::Binary(payload.into())).await.unwrap();

    let reply = tokio::time::timeout(Duration::from_secs(5), ws.next())
        .await
        .expect("timeout en attente de la reponse de net-proxy")
        .expect("le websocket s'est ferme sans reponse")
        .expect("message websocket invalide");

    let Message::Binary(data) = reply else {
        panic!("reponse non binaire inattendue: {reply:?}");
    };
    assert_eq!(data[0], DATA_CHANNEL, "canal inattendu (donnees=0 attendu)");
    assert_eq!(&data[1..], b"HELLO ATELIER\n");
}
