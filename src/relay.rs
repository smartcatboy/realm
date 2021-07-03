use futures::future::join_all;
use futures::future::try_join;
use futures::FutureExt;
use std::cell::RefCell;
use std::net::{IpAddr, SocketAddr};
use std::rc::Rc;
use tokio::io;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net;
use tokio::net::tcp::{ReadHalf, WriteHalf};
use tokio::task;

use crate::resolver;
use crate::udp;
use realm::RelayConfig;

// Initialize DNS recolver
// Set up channel between listener and resolver

pub async fn start_relay(configs: Vec<RelayConfig>) {
    let default_ip: IpAddr = String::from("0.0.0.0").parse::<IpAddr>().unwrap();
    let remote_addrs: Vec<String> =
        configs.iter().map(|x| x.remote_address.clone()).collect();

    let mut remote_ips = Vec::new();
    for _ in 0..remote_addrs.len() {
        remote_ips.push(Rc::new(RefCell::new(default_ip.clone())));
    }

    task::spawn_local(resolver::resolve(remote_addrs, remote_ips.clone()));

    let mut iter = configs.into_iter().zip(remote_ips);
    let mut runners = vec![];
    while let Some((config, remote_ip)) = iter.next() {
        runners.push(task::spawn_local(run(config, remote_ip)));
    }
    join_all(runners).await;
}

pub async fn run(config: RelayConfig, remote_ip: Rc<RefCell<IpAddr>>) {
    let client_socket: SocketAddr =
        format!("{}:{}", config.listening_address, config.listening_port)
            .parse()
            .unwrap();
    let tcp_listener = net::TcpListener::bind(&client_socket).await.unwrap();

    let mut remote_socket: SocketAddr =
        format!("{}:{}", remote_ip.borrow(), config.remote_port)
            .parse()
            .unwrap();

    // Start UDP connection
    task::spawn_local(udp::transfer_udp(
        client_socket.clone(),
        remote_socket.port(),
        remote_ip.clone(),
    ));

    // Start TCP connection
    loop {
        match tcp_listener.accept().await {
            Ok((inbound, _)) => {
                remote_socket =
                    format!("{}:{}", remote_ip.borrow(), config.remote_port)
                        .parse()
                        .unwrap();
                let transfer = transfer_tcp(inbound, remote_socket.clone())
                    .map(|r| {
                        if let Err(_) = r {
                            return;
                        }
                    });
                task::spawn_local(transfer);
            }
            Err(_) => {
                break;
            }
        }
    }
}

async fn transfer_tcp(
    mut inbound: net::TcpStream,
    remote_socket: SocketAddr,
) -> io::Result<()> {
    let mut outbound = net::TcpStream::connect(remote_socket).await?;
    inbound.set_nodelay(true)?;
    outbound.set_nodelay(true)?;
    let (mut ri, mut wi) = inbound.split();
    let (mut ro, mut wo) = outbound.split();

    let client_to_server = async {
        copy_tcp(&mut ri, &mut wo).await?;
        wo.shutdown().await
    };

    let server_to_client = async {
        copy_tcp(&mut ro, &mut wi).await?;
        wi.shutdown().await
    };

    try_join(client_to_server, server_to_client).await?;

    Ok(())
}

const BUFFERSIZE: usize = 0x4000;

async fn copy_tcp(
    r: &mut ReadHalf<'_>,
    w: &mut WriteHalf<'_>,
) -> io::Result<()> {
    let mut buf = vec![0u8; BUFFERSIZE];
    let mut n: usize;
    loop {
        n = r.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        w.write(&buf[..n]).await?;
        w.flush().await?;
    }
    Ok(())
}
