//! Замеры ICMP через iphlpapi — пинг, трассировка, поиск MTU.
//!
//! Не через запуск ping.exe и tracert.exe по трём причинам. Первая: запуск
//! процесса стоит десятки миллисекунд, а мерим мы как раз десятки
//! миллисекунд — замер получился бы про Windows, а не про сеть. Вторая: их
//! вывод локализован, и разбор ломается о язык системы. Третья, главная:
//! IcmpSendEcho2Ex принимает адрес источника, то есть позволяет спросить
//! «сколько до этого сервера через Ethernet, а сколько через Wi-Fi». Без
//! этого выбирать аплинк не из чего.

use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

/// Ответ пришёл.
pub const IP_SUCCESS: u32 = 0;
/// Пакет умер по TTL на промежуточном узле — так и выглядит прыжок в
/// трассировке.
pub const IP_TTL_EXPIRED_TRANSIT: u32 = 11013;
/// Пакет больше MTU, а фрагментировать запрещено. По этому ответу и ищется
/// настоящий MTU канала.
pub const IP_PACKET_TOO_BIG: u32 = 11009;

/// Флаг «не фрагментировать» в IP_OPTION_INFORMATION.
const IP_FLAG_DF: u8 = 0x02;

#[repr(C)]
struct IpOptionInformation {
    ttl: u8,
    tos: u8,
    flags: u8,
    options_size: u8,
    options_data: *mut u8,
}

#[cfg(target_os = "windows")]
#[link(name = "iphlpapi")]
extern "system" {
    fn IcmpCreateFile() -> isize;
    fn IcmpCloseHandle(handle: isize) -> i32;
    fn IcmpSendEcho2(
        handle: isize,
        event: isize,
        apc_routine: isize,
        apc_context: isize,
        destination: u32,
        request: *const u8,
        request_size: u16,
        options: *const IpOptionInformation,
        reply: *mut u8,
        reply_size: u32,
        timeout: u32,
    ) -> u32;
    fn IcmpSendEcho2Ex(
        handle: isize,
        event: isize,
        apc_routine: isize,
        apc_context: isize,
        source: u32,
        destination: u32,
        request: *const u8,
        request_size: u16,
        options: *const IpOptionInformation,
        reply: *mut u8,
        reply_size: u32,
        timeout: u32,
    ) -> u32;
}

#[cfg(target_os = "windows")]
#[link(name = "kernel32")]
extern "system" {
    fn GetLastError() -> u32;
}

/// Один ответ на одну отправку.
pub struct Echo {
    /// Код из ICMP_ECHO_REPLY: успех, истёкший TTL, «пакет великоват».
    pub status: u32,
    /// Кто ответил. У истёкшего TTL это адрес промежуточного узла.
    pub from: Option<Ipv4Addr>,
    /// Время до ответа. Меряем часами процесса, а не полем RoundTripTime:
    /// то поле целое в миллисекундах, и вся разница между 0.3 и 1.4 мс — а
    /// с ней и весь джиттер на близких узлах — в нём пропадает.
    pub ms: f64,
}

/// Открытый дескриптор ICMP. Один на серию замеров: IcmpCreateFile на каждый
/// пакет — это лишний системный вызов внутри того, что мы измеряем.
pub struct Icmp {
    #[allow(dead_code)]
    handle: isize,
}

impl Icmp {
    #[cfg(target_os = "windows")]
    pub fn open() -> Result<Self, String> {
        let handle = unsafe { IcmpCreateFile() };
        // INVALID_HANDLE_VALUE
        if handle == -1 || handle == 0 {
            return Err("не удалось открыть ICMP-дескриптор".into());
        }
        Ok(Self { handle })
    }

    #[cfg(not(target_os = "windows"))]
    pub fn open() -> Result<Self, String> {
        Err("ICMP через iphlpapi есть только в Windows".into())
    }

    /// Отправляет одну эхо-пробу и ждёт ответа.
    ///
    /// `source` — адрес интерфейса, с которого слать. `None` означает «пусть
    /// решает таблица маршрутизации», и это как раз то, что делает игра.
    #[cfg(target_os = "windows")]
    pub fn echo(
        &self,
        dest: Ipv4Addr,
        source: Option<Ipv4Addr>,
        payload: u16,
        ttl: u8,
        no_fragment: bool,
        timeout: Duration,
    ) -> Option<Echo> {
        let request = vec![0x61u8; payload as usize];
        // Буфер должен вместить ICMP_ECHO_REPLY, наш payload и запас на
        // опции IP; документация просит «размер структуры + payload + 8»,
        // берём с явным запасом — экономить тут нечего.
        let mut reply = vec![0u8; 1024 + payload as usize];
        let options = IpOptionInformation {
            ttl,
            tos: 0,
            flags: if no_fragment { IP_FLAG_DF } else { 0 },
            options_size: 0,
            options_data: std::ptr::null_mut(),
        };
        let timeout_ms = timeout.as_millis().min(u32::MAX as u128) as u32;

        let started = Instant::now();
        let count = unsafe {
            match source {
                Some(src) => IcmpSendEcho2Ex(
                    self.handle,
                    0,
                    0,
                    0,
                    ip_to_raw(src),
                    ip_to_raw(dest),
                    request.as_ptr(),
                    payload,
                    &options,
                    reply.as_mut_ptr(),
                    reply.len() as u32,
                    timeout_ms,
                ),
                None => IcmpSendEcho2(
                    self.handle,
                    0,
                    0,
                    0,
                    ip_to_raw(dest),
                    request.as_ptr(),
                    payload,
                    &options,
                    reply.as_mut_ptr(),
                    reply.len() as u32,
                    timeout_ms,
                ),
            }
        };
        let ms = started.elapsed().as_secs_f64() * 1000.0;

        // Поля читаем по смещениям, а не через #[repr(C)] структуру:
        // ICMP_ECHO_REPLY после третьего поля содержит указатель, и её размер
        // отличается у 32- и 64-битной сборки, а первые двенадцать байт —
        // адрес, статус и время — лежат одинаково всегда.
        let parsed = parse_reply(&reply);

        if count > 0 {
            return parsed.map(|(from, status, _)| Echo {
                status,
                from: Some(from),
                ms,
            });
        }

        // Ноль ответов — это ещё не провал. При истёкшем TTL IcmpSendEcho2
        // возвращает ноль, кладёт причину в GetLastError и при этом
        // заполняет буфер адресом ответившего узла: на этом и стоит
        // трассировка.
        let err = unsafe { GetLastError() };
        match parsed {
            Some((from, status, _)) if status != IP_SUCCESS => Some(Echo {
                status,
                from: Some(from),
                ms,
            }),
            _ if err == IP_TTL_EXPIRED_TRANSIT || err == IP_PACKET_TOO_BIG => Some(Echo {
                status: err,
                from: None,
                ms,
            }),
            _ => None,
        }
    }

    #[cfg(not(target_os = "windows"))]
    pub fn echo(
        &self,
        _dest: Ipv4Addr,
        _source: Option<Ipv4Addr>,
        _payload: u16,
        _ttl: u8,
        _no_fragment: bool,
        _timeout: Duration,
    ) -> Option<Echo> {
        None
    }
}

#[cfg(target_os = "windows")]
impl Drop for Icmp {
    fn drop(&mut self) {
        unsafe {
            IcmpCloseHandle(self.handle);
        }
    }
}

/// Адрес → IPAddr. В памяти IPAddr лежит октетами в сетевом порядке, то есть
/// на little-endian машине это ровно from_le_bytes от октетов.
#[cfg(target_os = "windows")]
fn ip_to_raw(ip: Ipv4Addr) -> u32 {
    u32::from_le_bytes(ip.octets())
}

fn raw_to_ip(raw: u32) -> Ipv4Addr {
    Ipv4Addr::from(raw.to_le_bytes())
}

/// Первые двенадцать байт ICMP_ECHO_REPLY: адрес, статус, RoundTripTime.
#[allow(dead_code)]
fn parse_reply(buf: &[u8]) -> Option<(Ipv4Addr, u32, u32)> {
    if buf.len() < 12 {
        return None;
    }
    let addr = u32::from_le_bytes(buf[0..4].try_into().ok()?);
    let status = u32::from_le_bytes(buf[4..8].try_into().ok()?);
    let rtt = u32::from_le_bytes(buf[8..12].try_into().ok()?);
    if addr == 0 {
        return None;
    }
    Some((raw_to_ip(addr), status, rtt))
}

// ── Серия замеров ────────────────────────────────────────────────────────

/// Итог серии: то, что человек называет «пингом».
#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Sample {
    pub sent: u32,
    pub received: u32,
    pub min: f64,
    pub avg: f64,
    pub max: f64,
    /// Средний модуль разницы между соседними ответами. В игре он заметнее
    /// среднего: ровные 60 мс играются лучше, чем скачущие от 20 до 90.
    pub jitter: f64,
    pub loss_pct: u32,
}

impl Sample {
    pub fn ok(&self) -> bool {
        self.received > 0
    }

    /// Одна цифра, по которой сравниваются варианты.
    ///
    /// Не средний пинг: канал со средним 40 и джиттером 25 играется хуже
    /// канала со средним 50 и джиттером 2, а потеря пакета в UDP-игре стоит
    /// дороже десятка миллисекунд — её нечем переспросить, персонаж просто
    /// прыгает. Отсюда веса: джиттер вдвое, каждый процент потерь — как
    /// пять миллисекунд.
    pub fn score(&self) -> f64 {
        if !self.ok() {
            return f64::MAX;
        }
        self.avg + self.jitter * 2.0 + self.loss_pct as f64 * 5.0
    }
}

/// Серия эхо-проб к одному адресу.
pub fn ping(
    icmp: &Icmp,
    dest: Ipv4Addr,
    source: Option<Ipv4Addr>,
    count: u32,
    size: u16,
    timeout: Duration,
) -> Sample {
    let mut times: Vec<f64> = Vec::with_capacity(count as usize);
    for i in 0..count {
        if let Some(echo) = icmp.echo(dest, source, size, 128, false, timeout) {
            if echo.status == IP_SUCCESS {
                times.push(echo.ms);
            }
        }
        // Пауза между пакетами: очередь на аплинке рассасывается за единицы
        // миллисекунд, и подряд отправленные пробы мерили бы собственный
        // хвост, а не сеть.
        if i + 1 < count {
            std::thread::sleep(Duration::from_millis(120));
        }
    }
    summarize(count, &times)
}

/// Считает статистику по временам ответов. Отдельной функцией — чтобы её
/// проверял тест, а не только живая сеть.
pub fn summarize(sent: u32, times: &[f64]) -> Sample {
    let received = times.len() as u32;
    if received == 0 {
        return Sample {
            sent,
            loss_pct: 100,
            ..Default::default()
        };
    }
    let sum: f64 = times.iter().sum();
    let jitter = if times.len() > 1 {
        times.windows(2).map(|w| (w[1] - w[0]).abs()).sum::<f64>() / (times.len() - 1) as f64
    } else {
        0.0
    };
    Sample {
        sent,
        received,
        min: times.iter().cloned().fold(f64::MAX, f64::min),
        avg: sum / received as f64,
        max: times.iter().cloned().fold(0.0, f64::max),
        jitter,
        loss_pct: ((sent - received) * 100).div_ceil(sent.max(1)),
    }
}

// ── Трассировка ──────────────────────────────────────────────────────────

/// Один прыжок маршрута.
pub struct Hop {
    pub number: u8,
    pub ip: Option<Ipv4Addr>,
    /// Лучшее из трёх времён. Лучшее, а не среднее: промежуточный
    /// маршрутизатор формирует ICMP-ответ по остаточному принципу, и
    /// случайная задержка в его процессоре — это не задержка канала.
    pub ms: Option<f64>,
    /// Адрес из серых диапазонов: домашняя сеть, сеть провайдера, CGNAT.
    pub private: bool,
}

/// Трассировка до адреса — своя, а не через tracert.exe.
pub fn trace(icmp: &Icmp, dest: Ipv4Addr, max_hops: u8, timeout: Duration) -> Vec<Hop> {
    let mut hops = Vec::new();
    /// Сколько молчащих прыжков подряд считать концом маршрута.
    ///
    /// Молчащий узел посередине — норма: многие маршрутизаторы не отвечают
    /// на ICMP принципиально. А вот пять подряд означают, что дальше сеть
    /// дата-центра, которая не отвечает вообще, — и ждать ещё пятнадцать
    /// таймаутов незачем: это полминуты ожидания ради пустых строк.
    const SILENCE_ENDS_ROUTE: u32 = 5;
    let mut silent = 0;
    for ttl in 1..=max_hops {
        let mut ip = None;
        let mut best: Option<f64> = None;
        let mut arrived = false;
        for _ in 0..3 {
            let Some(echo) = icmp.echo(dest, None, 32, ttl, false, timeout) else {
                continue;
            };
            if echo.from.is_some() {
                ip = echo.from;
            }
            if echo.status == IP_SUCCESS || echo.status == IP_TTL_EXPIRED_TRANSIT {
                best = Some(best.map_or(echo.ms, |b: f64| b.min(echo.ms)));
            }
            if echo.status == IP_SUCCESS {
                arrived = true;
            }
        }
        silent = if ip.is_none() { silent + 1 } else { 0 };
        hops.push(Hop {
            number: ttl,
            ip,
            ms: best,
            private: ip.is_some_and(is_private),
        });
        if arrived || silent >= SILENCE_ENDS_ROUTE {
            break;
        }
    }
    // Молчащий хвост в выводе — это строки «нет ответа» без единого факта.
    // Убираем: маршрут кончается там, где кончились ответы.
    while hops.last().is_some_and(|h| h.ip.is_none()) {
        hops.pop();
    }
    hops
}

/// Серый адрес: до такого узла «оптимизировать маршрут» бессмысленно — он
/// внутри дома или внутри сети провайдера.
pub fn is_private(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        // CGNAT 100.64.0.0/10 — провайдер раздаёт такие адреса вместо белых,
        // и лишний уровень NAT у него на выходе виден в трассировке.
        || (o[0] == 100 && (64..128).contains(&o[1]))
}

// ── MTU ──────────────────────────────────────────────────────────────────

/// Ищет наибольший пакет, который проходит без фрагментации.
///
/// Двоичным поиском по размеру полезной части с запретом фрагментации: узел,
/// которому пакет великоват, отвечает IP_PACKET_TOO_BIG. Это нужно тем, у
/// кого PPPoE или туннель: у них канал 1492 или 1420 вместо 1500, Windows
/// об этом не знает и продолжает слать 1500-байтные пакеты, которые кто-то
/// по дороге режет надвое — а половинку теряет.
pub fn probe_mtu(icmp: &Icmp, dest: Ipv4Addr, timeout: Duration) -> Option<u32> {
    // Границы полезной части: 1472 = 1500 − 20 (IP) − 8 (ICMP).
    let (mut low, mut high) = (500u16, 1472u16);
    let passes = |size: u16| -> bool {
        (0..2).any(|_| {
            icmp.echo(dest, None, size, 128, true, timeout)
                .is_some_and(|e| e.status == IP_SUCCESS)
        })
    };
    if !passes(low) {
        // Даже маленький с DF не прошёл — значит, дело не в MTU, а в том,
        // что узел не отвечает на ICMP вовсе. Врать про «нашли MTU 500»
        // нельзя.
        return None;
    }
    if passes(high) {
        return Some(high as u32 + 28);
    }
    while high - low > 4 {
        let mid = low + (high - low) / 2;
        if passes(mid) {
            low = mid;
        } else {
            high = mid;
        }
    }
    Some(low as u32 + 28)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loss_is_counted_from_sent() {
        let s = summarize(10, &[20.0, 20.0, 20.0, 20.0, 20.0]);
        assert_eq!(s.loss_pct, 50);
        assert_eq!(s.received, 5);
    }

    #[test]
    fn silence_is_total_loss_not_zero_ping() {
        // Пустая выборка не должна превращаться в «пинг 0 мс» — это ровно
        // та ложь, ради которой всё и меряется.
        let s = summarize(10, &[]);
        assert!(!s.ok());
        assert_eq!(s.loss_pct, 100);
        assert_eq!(s.avg, 0.0);
    }

    #[test]
    fn jitter_is_the_gap_between_neighbours() {
        let s = summarize(3, &[10.0, 20.0, 10.0]);
        assert!((s.jitter - 10.0).abs() < 0.001);
    }

    #[test]
    fn steady_channel_wins_over_jumpy_one() {
        // 50 мс ровно против 40 мс с разбросом 25: в игре первое лучше.
        let steady = summarize(4, &[50.0, 50.0, 50.0, 50.0]);
        let jumpy = summarize(4, &[15.0, 65.0, 15.0, 65.0]);
        assert!(steady.score() < jumpy.score());
    }

    #[test]
    fn cgnat_counts_as_private() {
        assert!(is_private("100.64.1.1".parse().unwrap()));
        assert!(!is_private("100.128.1.1".parse().unwrap()));
        assert!(is_private("192.168.0.1".parse().unwrap()));
        assert!(!is_private("8.8.8.8".parse().unwrap()));
    }
}
