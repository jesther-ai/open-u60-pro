//! Standalone QA peer/plan driver. Never run the plan command in the host namespace.

use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream, UdpSocket};
use std::path::Path;
use std::process::Command;
use std::thread;
use std::time::Duration;

use serde_json::Value;

const V4: &str = "203.0.113.1";
const V6: &str = "2001:db8:ffff::1";
const PORT: u16 = 45679;

fn endpoint(address: &str) -> String {
    if address.contains(':') {
        format!("[{address}]:{PORT}")
    } else {
        format!("{address}:{PORT}")
    }
}

fn server() {
    for address in [V4, V6] {
        let tcp = TcpListener::bind(endpoint(address)).unwrap();
        thread::spawn(move || {
            for stream in tcp.incoming() {
                let mut stream = stream.unwrap();
                thread::spawn(move || {
                    let peer = stream.peer_addr().unwrap().ip().to_string();
                    let mut data = [0; 4096];
                    while stream.read(&mut data).is_ok_and(|n| n > 0) {
                        if stream.write_all(peer.as_bytes()).is_err() {
                            break;
                        }
                    }
                });
            }
        });
        let udp = UdpSocket::bind(endpoint(address)).unwrap();
        thread::spawn(move || {
            let mut data = [0; 4096];
            loop {
                let (_, peer) = udp.recv_from(&mut data).unwrap();
                let _ = udp.send_to(peer.ip().to_string().as_bytes(), peer);
            }
        });
    }
    loop {
        thread::sleep(Duration::from_secs(60));
    }
}

fn client(protocol: &str, destination: &str) -> String {
    let mut bytes = [0; 4096];
    if protocol == "tcp" {
        let mut stream = TcpStream::connect_timeout(
            &endpoint(destination).parse().unwrap(),
            Duration::from_secs(4),
        )
        .unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(4)))
            .unwrap();
        stream.write_all(b"QA").unwrap();
        let n = stream.read(&mut bytes).unwrap();
        String::from_utf8_lossy(&bytes[..n]).into()
    } else {
        let socket = UdpSocket::bind(if destination.contains(':') {
            "[::]:0"
        } else {
            "0.0.0.0:0"
        })
        .unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(4)))
            .unwrap();
        socket.connect(endpoint(destination)).unwrap();
        socket.send(b"QA").unwrap();
        let n = socket.recv(&mut bytes).unwrap();
        String::from_utf8_lossy(&bytes[..n]).into()
    }
}

fn persistent(root: &str) {
    let socket = UdpSocket::bind("0.0.0.0:0").unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    socket.connect(endpoint(V4)).unwrap();
    for stage in [
        "cell-ready",
        "wifi-ready",
        "v6-ready",
        "cell-again",
        "wifi2-ready",
        "wifi3-ready",
        "stalled-ready",
    ] {
        while !Path::new(root).join(stage).exists() {
            thread::sleep(Duration::from_millis(50));
        }
        socket.send(b"persistent").unwrap();
        let mut bytes = [0; 4096];
        let n = socket.recv(&mut bytes).unwrap();
        fs::write(Path::new(root).join(format!("udp-{stage}")), &bytes[..n]).unwrap();
    }
}

fn plan(path: &str, group: &str, action: &str) {
    assert!(Path::new("/u60-ipv6-qa-disposable").exists());
    let plans: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    let mut steps: Vec<_> = plans[group].as_array().unwrap().iter().collect();
    if action == "undo" {
        steps.reverse();
    }
    for step in steps {
        let command = &step[if action == "undo" { "undo" } else { "apply" }];
        let name = command["program"].as_str().unwrap();
        let args: Vec<_> = command["args"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        if name == "@u60-relay-ipv6-dad" {
            thread::sleep(Duration::from_secs(3));
            continue;
        }
        assert!(!name.starts_with('@'));
        let out = Command::new(name).args(&args).output().unwrap();
        assert!(
            out.status.success(),
            "QA command failed: {name} {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

fn main() {
    let args: Vec<_> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("server") => server(),
        Some("client") => println!("{}", client(&args[2], &args[3])),
        Some("persistent") => persistent(&args[2]),
        Some("plan") => plan(&args[2], &args[3], &args[4]),
        _ => panic!("QA helper: server | client tcp/udp destination | persistent dir | plan file group apply/undo"),
    }
}
