use tokio::{io::{AsyncWriteExt, AsyncReadExt}, net::TcpStream};
use tokio::time::{timeout, Duration};

pub async fn handle_client_handshake(
    client_stream: &mut TcpStream,
) -> Result<(Option<String>, String, u16), std::io::Error> {
    let mut buffer = [0u8; 512];
    let handshake_timeout = Duration::from_secs(5);

    // Step 1: SOCKS5 Greeting
    let len = timeout(handshake_timeout, client_stream.read(&mut buffer)).await??;
    if len < 2 || buffer[0] != 0x05 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "Invalid SOCKS5 greeting packet",
        ));
    }

    let auth_methods = &buffer[2..len];
    if !auth_methods.contains(&0x00) && !auth_methods.contains(&0x02) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "Unsupported authentication methods",
        ));
    }

    let username = if auth_methods.contains(&0x02) {
        // Username/password authentication
        client_stream.write_all(&[0x05, 0x02]).await?;
        let len = timeout(handshake_timeout, client_stream.read(&mut buffer)).await??;
        if len < 3 || buffer[0] != 0x01 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Invalid username/password authentication packet",
            ));
        }

        let username_len = buffer[1] as usize;
        if len < 2 + username_len {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Incomplete username data",
            ));
        }

        let username = String::from_utf8(buffer[2..2 + username_len].to_vec())
            .map_err(|_| {
                std::io::Error::new(std::io::ErrorKind::InvalidData, "Invalid UTF-8 in username")
            })?;

        // Send authentication success response
        client_stream.write_all(&[0x01, 0x00]).await?;
        Some(username)
    } else {
        // No authentication required
        client_stream.write_all(&[0x05, 0x00]).await?;
        None
    };

    // Step 2: SOCKS5 Request Phase
    let len = timeout(handshake_timeout, client_stream.read(&mut buffer)).await??;
    if len < 10 || buffer[0] != 0x05 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "Invalid SOCKS5 request packet",
        ));
    }

    if buffer[1] != 0x01 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "Only CONNECT command is supported",
        ));
    }

    let addr_type = buffer[3];
    let (address, port) = match addr_type {
        0x01 => {
            // IPv4
            if len < 10 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "Incomplete IPv4 address",
                ));
            }
            let ip = format!(
                "{}.{}.{}.{}",
                buffer[4], buffer[5], buffer[6], buffer[7]
            );
            let port = u16::from_be_bytes([buffer[8], buffer[9]]);
            (ip, port)
        }
        0x03 => {
            // Domain name
            let domain_len = buffer[4] as usize;
            if len < 5 + domain_len + 2 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "Incomplete domain name address",
                ));
            }
            let domain = String::from_utf8(buffer[5..5 + domain_len].to_vec())
                .map_err(|_| {
                    std::io::Error::new(std::io::ErrorKind::InvalidData, "Invalid UTF-8 in domain")
                })?;
            let port = u16::from_be_bytes([buffer[5 + domain_len], buffer[6 + domain_len]]);
            (domain, port)
        }
        0x04 => {
            // IPv6
            if len < 22 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "Incomplete IPv6 address",
                ));
            }
            let ip = format!(
                "{:x}:{:x}:{:x}:{:x}:{:x}:{:x}:{:x}:{:x}",
                u16::from_be_bytes([buffer[4], buffer[5]]),
                u16::from_be_bytes([buffer[6], buffer[7]]),
                u16::from_be_bytes([buffer[8], buffer[9]]),
                u16::from_be_bytes([buffer[10], buffer[11]]),
                u16::from_be_bytes([buffer[12], buffer[13]]),
                u16::from_be_bytes([buffer[14], buffer[15]]),
                u16::from_be_bytes([buffer[16], buffer[17]]),
                u16::from_be_bytes([buffer[18], buffer[19]])
            );
            let port = u16::from_be_bytes([buffer[20], buffer[21]]);
            (ip, port)
        }
        _ => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Unsupported address type",
            ));
        }
    };

    // Send success response
    client_stream.write_all(&[0x05, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0]).await?;

    Ok((username, address, port))
}