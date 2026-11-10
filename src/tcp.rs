//! Замер задержки установкой TCP-соединения — с выбором интерфейса.
//!
//! Зачем это рядом с ICMP: дата-центры, в которых стоят серверы Fortnite, на
//! ping не отвечают. Ни один из региональных адресов Amazon не вернёт эхо, и
//! замерить регион эхо-пробой невозможно в принципе. Зато у каждого открыт
//! 443-й порт, а время между отправкой SYN и приходом SYN-ACK — это ровно
//! один оборот по тому же маршруту, каким пойдёт игра.
//!
//! Своя реализация, а не TcpStream::connect_timeout, по одной причине:
//! стандартная библиотека не умеет привязать исходящее соединение к
//! конкретному интерфейсу. А весь смысл выбора аплинка в том, чтобы
//! спросить «сколько отсюда, а сколько оттуда», и ответ должен быть про
//! разные провода.

#![allow(dead_code)]

use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

#[cfg(target_os = "windows")]
mod ffi {
    pub const AF_INET: i32 = 2;
    pub const SOCK_STREAM: i32 = 1;
    pub const IPPROTO_TCP: i32 = 6;
    pub const INVALID_SOCKET: usize = usize::MAX;
    pub const SOCKET_ERROR: i32 = -1;
    pub const FIONBIO: i32 = -2147195266; // 0x8004667E
    pub const WSAEWOULDBLOCK: i32 = 10035;
    pub const SOL_SOCKET: i32 = 0xffff;
    pub const SO_ERROR: i32 = 0x1007;

    #[repr(C)]
    pub struct SockAddrIn {
        pub family: u16,
        /// Порт в сетевом порядке байт.
        pub port: u16,
        pub addr: u32,
        pub zero: [u8; 8],
    }

    #[repr(C)]
    pub struct TimeVal {
        pub sec: i32,
        pub usec: i32,
    }

    /// fd_set из winsock2.h: у Windows это счётчик и массив, а не битовая
    /// маска, как в POSIX.
    #[repr(C)]
    pub struct FdSet {
        pub count: u32,
        pub array: [usize; 64],
    }

    impl FdSet {
        pub fn one(socket: usize) -> Self {
            let mut array = [0usize; 64];
            array[0] = socket;
            Self { count: 1, array }
        }
    }

    #[link(name = "ws2_32")]
    extern "system" {
        pub fn WSAStartup(version: u16, data: *mut u8) -> i32;
        pub fn WSAGetLastError() -> i32;
        pub fn socket(af: i32, ty: i32, protocol: i32) -> usize;
        pub fn bind(s: usize, addr: *const SockAddrIn, len: i32) -> i32;
        pub fn connect(s: usize, addr: *const SockAddrIn, len: i32) -> i32;
        pub fn closesocket(s: usize) -> i32;
        pub fn ioctlsocket(s: usize, cmd: i32, arg: *mut u32) -> i32;
        pub fn getsockopt(s: usize, level: i32, name: i32, value: *mut u8, len: *mut i32) -> i32;
        pub fn select(
            nfds: i32,
            readfds: *mut FdSet,
            writefds: *mut FdSet,
            exceptfds: *mut FdSet,
            timeout: *const TimeVal,
        ) -> i32;
    }
}

#[cfg(target_os = "windows")]
fn winsock_ready() {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| unsafe {
        // WSADATA на x64 — 408 байт; берём с запасом, содержимое нам не нужно.
        let mut data = vec![0u8; 512];
        ffi::WSAStartup(0x0202, data.as_mut_ptr());
    });
}

/// Время до SYN-ACK. `None` — не ответил за отведённое время.
///
/// `source` — адрес интерфейса, с которого идти. `None` означает «как решит
/// таблица маршрутизации».
#[cfg(target_os = "windows")]
pub fn connect_rtt(
    dest: Ipv4Addr,
    port: u16,
    source: Option<Ipv4Addr>,
    timeout: Duration,
) -> Option<f64> {
    use ffi::*;
    winsock_ready();

    unsafe {
        let sock = socket(AF_INET, SOCK_STREAM, IPPROTO_TCP);
        if sock == INVALID_SOCKET {
            return None;
        }
        // Закрывать сокет надо на каждом выходе, включая ошибочные, поэтому
        // дальше — замыкание, а результат закрывается один раз внизу.
        let result = (|| {
            if let Some(src) = source {
                let local = SockAddrIn {
                    family: AF_INET as u16,
                    port: 0,
                    addr: u32::from_le_bytes(src.octets()),
                    zero: [0; 8],
                };
                if bind(sock, &local, std::mem::size_of::<SockAddrIn>() as i32) == SOCKET_ERROR {
                    // Адреса нет на машине (интерфейс отключили между
                    // перечислением и замером) — это не «медленно», это
                    // «нечем мерить».
                    return None;
                }
            }

            let mut nonblocking: u32 = 1;
            if ioctlsocket(sock, FIONBIO, &mut nonblocking) == SOCKET_ERROR {
                return None;
            }

            let remote = SockAddrIn {
                family: AF_INET as u16,
                port: port.to_be(),
                addr: u32::from_le_bytes(dest.octets()),
                zero: [0; 8],
            };

            let started = Instant::now();
            if connect(sock, &remote, std::mem::size_of::<SockAddrIn>() as i32) != SOCKET_ERROR {
                // Мгновенный успех бывает только на локальной петле, но
                // обработать его надо: иначе select ждал бы события, которое
                // уже произошло.
                return Some(started.elapsed().as_secs_f64() * 1000.0);
            }
            if WSAGetLastError() != WSAEWOULDBLOCK {
                return None;
            }

            let mut write = FdSet::one(sock);
            let mut except = FdSet::one(sock);
            let tv = TimeVal {
                sec: timeout.as_secs() as i32,
                usec: timeout.subsec_micros() as i32,
            };
            // Первый аргумент Windows игнорирует — размер набора она берёт
            // из самого набора.
            let ready = select(
                0,
                std::ptr::null_mut(),
                &mut write,
                &mut except,
                &tv,
            );
            let ms = started.elapsed().as_secs_f64() * 1000.0;
            if ready <= 0 || write.count == 0 {
                return None;
            }

            // Готовность к записи — ещё не успех: отказ в соединении
            // приходит тем же событием. Настоящий ответ лежит в SO_ERROR.
            let mut err: i32 = 0;
            let mut len = std::mem::size_of::<i32>() as i32;
            if getsockopt(
                sock,
                SOL_SOCKET,
                SO_ERROR,
                &mut err as *mut i32 as *mut u8,
                &mut len,
            ) == SOCKET_ERROR
                || err != 0
            {
                return None;
            }
            Some(ms)
        })();
        closesocket(sock);
        result
    }
}

#[cfg(not(target_os = "windows"))]
pub fn connect_rtt(
    _dest: Ipv4Addr,
    _port: u16,
    _source: Option<Ipv4Addr>,
    _timeout: Duration,
) -> Option<f64> {
    None
}

/// Серия соединений к одному адресу — тот же итог, что у эхо-проб.
pub fn probe(
    dest: Ipv4Addr,
    port: u16,
    source: Option<Ipv4Addr>,
    count: u32,
    timeout: Duration,
) -> crate::icmp::Sample {
    let mut times = Vec::with_capacity(count as usize);
    for i in 0..count {
        if let Some(ms) = connect_rtt(dest, port, source, timeout) {
            times.push(ms);
        }
        if i + 1 < count {
            // Пауза больше, чем у ICMP: подряд идущие SYN с одного адреса на
            // один порт часть провайдеров и сам дата-центр считают сканом и
            // придерживают — придержанный SYN приехал бы в замер как
            // «плохой канал».
            std::thread::sleep(Duration::from_millis(200));
        }
    }
    crate::icmp::summarize(count, &times)
}

/// Разрешает имя в первый IPv4-адрес.
///
/// Именно IPv4: маршруты, метрики и политики QoS программа настраивает для
/// IPv4, и мерить при этом IPv6 значило бы мерить не тот путь, который потом
/// правишь.
pub fn resolve(host: &str) -> Option<Ipv4Addr> {
    use std::net::ToSocketAddrs;
    (host, 0u16)
        .to_socket_addrs()
        .ok()?
        .find_map(|addr| match addr.ip() {
            std::net::IpAddr::V4(v4) => Some(v4),
            std::net::IpAddr::V6(_) => None,
        })
}
