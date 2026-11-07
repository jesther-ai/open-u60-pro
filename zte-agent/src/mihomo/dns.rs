//! Check the real managed DNS listeners before redirecting client traffic.

use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::time::Duration;

pub(super) fn available(port: u16) -> Result<(), String> {
    for host in ["0.0.0.0", "127.0.0.1", "::", "::1"] {
        let ip = host.parse::<std::net::IpAddr>().unwrap();
        let address = SocketAddr::new(ip, port);
        drop(TcpListener::bind(address).map_err(|_| "Mihomo DNS TCP port is already in use")?);
        drop(UdpSocket::bind(address).map_err(|_| "Mihomo DNS UDP port is already in use")?);
    }
    Ok(())
}

pub(super) fn ready(name: &str, expected: Ipv4Addr, port: u16) -> bool {
    ["127.0.0.1", "::1"].into_iter().all(|host| {
        let address = SocketAddr::new(host.parse().unwrap(), port);
        [false, true]
            .into_iter()
            .all(|tcp| probe(address, tcp, name, expected).unwrap_or(false))
    })
}

fn query(name: &str) -> Vec<u8> {
    let mut query = vec![0x60, 0x53, 1, 0, 0, 1, 0, 0, 0, 0, 0, 0];
    for label in name.split('.') {
        query.push(label.len() as u8);
        query.extend_from_slice(label.as_bytes());
    }
    query.extend_from_slice(&[0, 0, 1, 0, 1]);
    query
}

fn probe(address: SocketAddr, tcp: bool, name: &str, expected: Ipv4Addr) -> std::io::Result<bool> {
    let wait = Duration::from_millis(200);
    let query = query(name);
    let mut answer = [0; 2048];
    let size = if tcp {
        let mut socket = TcpStream::connect_timeout(&address, wait)?;
        socket.set_read_timeout(Some(wait))?;
        socket.set_write_timeout(Some(wait))?;
        socket.write_all(&(query.len() as u16).to_be_bytes())?;
        socket.write_all(&query)?;
        let mut size = [0; 2];
        socket.read_exact(&mut size)?;
        let size = usize::from(u16::from_be_bytes(size));
        if size > answer.len() {
            return Ok(false);
        }
        socket.read_exact(&mut answer[..size])?;
        size
    } else {
        let mut local = address;
        local.set_port(0);
        let socket = UdpSocket::bind(local)?;
        socket.set_read_timeout(Some(wait))?;
        socket.connect(address)?;
        socket.send(&query)?;
        socket.recv(&mut answer)?
    };
    Ok(matches_answer(&query, &answer[..size], expected))
}

fn matches_answer(query: &[u8], answer: &[u8], expected: Ipv4Addr) -> bool {
    if answer.len() < query.len()
        || answer[..2] != query[..2]
        || answer[2] & 0x82 != 0x80
        || answer[3] & 0x0f != 0
        || answer[4..6] != [0, 1]
        || answer[6..8] != [0, 1]
        || answer[12..query.len()] != query[12..]
    {
        return false;
    }
    let mut pos = query.len();
    loop {
        let Some(&length) = answer.get(pos) else {
            return false;
        };
        pos += 1;
        if length == 0 {
            break;
        }
        if length & 0xc0 == 0xc0 {
            pos += 1;
            break;
        }
        if length > 63 {
            return false;
        }
        pos += usize::from(length);
    }
    answer.get(pos..pos + 4) == Some(&[0, 1, 0, 1])
        && answer.get(pos + 8..pos + 10) == Some(&[0, 4])
        && answer.get(pos + 10..pos + 14) == Some(expected.octets().as_slice())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_busy_dns_port_is_not_a_ready_core() {
        let tcp = TcpListener::bind("127.0.0.1:0").unwrap();
        assert!(available(tcp.local_addr().unwrap().port()).is_err());
        let udp = UdpSocket::bind("127.0.0.1:0").unwrap();
        assert!(available(udp.local_addr().unwrap().port()).is_err());
    }

    #[test]
    fn readiness_requires_the_local_management_answer_not_an_arbitrary_dns_response() {
        let query = query("u60.home.arpa");
        let ip = Ipv4Addr::new(192, 168, 0, 1);
        let mut answer = query.clone();
        answer[2] |= 0x80;
        answer[7] = 1;
        answer.extend_from_slice(&[0xc0, 12, 0, 1, 0, 1, 0, 0, 0, 60, 0, 4]);
        answer.extend_from_slice(&ip.octets());
        assert!(matches_answer(&query, &answer, ip));
        assert!(!matches_answer(&query, &answer, Ipv4Addr::LOCALHOST));
        for size in 0..answer.len() {
            assert!(!matches_answer(&query, &answer[..size], ip));
        }
        answer[3] = 3;
        assert!(!matches_answer(&query, &answer, ip));
    }
}
