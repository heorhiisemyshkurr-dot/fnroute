//! Разбор: где именно набирается задержка.
//!
//! Смысл раздела в том, чтобы отделить то, что программа может починить, от
//! того, что не может. Задержка бывает четырёх видов, и лечатся они разным:
//!
//! * набежала в первом прыжке — это домашняя сеть: Wi-Fi, дешёвый роутер,
//!   забитый канал. Чинится здесь;
//! * набежала внутри сети провайдера — чинится только звонком провайдеру;
//! * набежала на транзитном узле по дороге в дата-центр — не чинится ничем,
//!   кроме смены региона или провайдера;
//! * распределена ровно по маршруту — это физика: расстояние. Не чинится.
//!
//! Программа, которая пишет ключи реестра, не спросив, какой из четырёх
//! случаев перед ней, с вероятностью три к одному делает вид.

use crate::icmp::{self, Hop, Icmp};
use std::net::Ipv4Addr;
use std::time::Duration;

/// Адреса, отвечающие на эхо-пробы.
///
/// Дата-центры Amazon, в которых стоят серверы Fortnite, на ICMP не
/// отвечают, поэтому всё, что требует эха — поиск MTU и проверка канала
/// большими пакетами, — идёт до публичных резолверов. Они не там, где
/// игровой сервер, но и MTU, и толщина исходящего канала — это свойства
/// первого участка пути, общего для любого направления.
pub const ICMP_ANCHORS: [&str; 2] = ["1.1.1.1", "8.8.8.8"];

/// Первый из опорных адресов, который отвечает.
pub fn icmp_anchor(icmp: &Icmp) -> Option<Ipv4Addr> {
    ICMP_ANCHORS.iter().find_map(|a| {
        let ip: Ipv4Addr = a.parse().ok()?;
        icmp.echo(ip, None, 32, 128, false, Duration::from_millis(1200))
            .filter(|e| e.status == icmp::IP_SUCCESS)
            .map(|_| ip)
    })
}

/// Куда указывает трассировка.
#[derive(PartialEq, Debug)]
pub enum Blame {
    /// Задержка выросла на первом прыжке — виновата домашняя сеть.
    Home,
    /// Внутри серых адресов провайдера.
    Provider,
    /// На транзитном узле в белых адресах.
    Transit,
    /// Ровный маршрут: задержка — это расстояние.
    Distance,
    /// Маршрут не проследился.
    Unknown,
}

/// Итог разбора трассировки.
pub struct Verdict {
    pub blame: Blame,
    /// Прыжок, на котором задержка выросла сильнее всего.
    pub hop: Option<u8>,
    /// На сколько миллисекунд она там выросла.
    pub jump: f64,
    pub text: String,
}

/// Ищет прыжок, на котором задержка выросла сильнее всего, и объясняет, что
/// это значит.
pub fn analyse(hops: &[Hop]) -> Verdict {
    let answered: Vec<&Hop> = hops.iter().filter(|h| h.ms.is_some()).collect();
    if answered.len() < 2 {
        return Verdict {
            blame: Blame::Unknown,
            hop: None,
            jump: 0.0,
            text: "Маршрут не проследился: узлы по дороге не отвечают на пробы. \
                   Это нормально для части провайдеров и само по себе не проблема — \
                   но и сказать, где набирается задержка, в таком случае нельзя."
                .into(),
        };
    }

    let total = answered.last().and_then(|h| h.ms).unwrap_or(0.0);
    let mut worst: Option<(&Hop, f64)> = None;
    let mut previous = 0.0;
    for hop in &answered {
        let ms = hop.ms.unwrap_or(previous);
        let jump = ms - previous;
        if worst.map_or(true, |(_, best)| jump > best) {
            worst = Some((hop, jump));
        }
        previous = ms;
    }

    let Some((hop, jump)) = worst else {
        return Verdict {
            blame: Blame::Unknown,
            hop: None,
            jump: 0.0,
            text: "Разобрать маршрут не вышло.".into(),
        };
    };

    // Первый прыжок — особый случай: до собственного роутера не бывает
    // «далеко», и десять миллисекунд там означают беду в доме, каким бы
    // длинным ни был остаток маршрута.
    let first_hop_slow = answered
        .first()
        .is_some_and(|h| h.number <= 1 && h.ms.unwrap_or(0.0) >= 10.0);

    // Порог для остальных: скачок должен быть и крупным сам по себе, и
    // большей частью всего пути.
    //
    // Числа выбраны по тому, что должно и что не должно ловиться. Не должен:
    // переход из сети провайдера в международный магистральный канал — это
    // одно длинное плечо, на маршруте в 30 мс оно честно стоит 17, и никакой
    // аварии в нём нет; отправить из-за него человека ругаться с
    // провайдером — значит потратить его время впустую. Должен: узел, на
    // котором пинг из 20 мс становится 90, — вот там действительно что-то
    // сломано, и жалоба обоснована.
    let significant = jump >= 25.0 && jump >= total * 0.5;

    if first_hop_slow {
        // Отмечаем именно первый прыжок, а не самый крупный скачок: виноват
        // он, и в таблице стрелка должна стоять напротив него.
        return Verdict {
            blame: Blame::Home,
            hop: Some(answered[0].number),
            jump: answered[0].ms.unwrap_or(0.0),
            text: format!(
                "До собственного роутера {:.0} мс — это уже беда, и до интернета дело ещё \
                 не дошло. Так выглядит Wi-Fi со слабым сигналом или канал, забитый чужой \
                 загрузкой.",
                answered[0].ms.unwrap_or(0.0)
            ),
        };
    }

    let (blame, text) = if !significant {
        (
            Blame::Distance,
            format!(
                "Задержка набирается ровно по всему маршруту: {total:.0} мс за {} прыжков, \
                 без единого узкого места. Это расстояние до дата-центра, и оно не \
                 лечится ничем, кроме выбора региона поближе.",
                answered.len()
            ),
        )
    } else if hop.private {
        (
            Blame::Provider,
            format!(
                "Задержка набегает на прыжке {} (+{jump:.0} мс), и это ещё серый адрес — \
                 то есть узел внутри сети провайдера. Ни один твик Windows на него не \
                 влияет: это вопрос к провайдеру, и говорить с ним надо этими цифрами.",
                hop.number
            ),
        )
    } else {
        (
            Blame::Transit,
            format!(
                "Задержка набегает на прыжке {} (+{jump:.0} мс) — это транзитный узел уже \
                 за пределами провайдера. Со стороны машины он недосягаем: помогает \
                 только другой регион или другой провайдер.",
                hop.number
            ),
        )
    };

    Verdict {
        // На ровном маршруте виноватого прыжка нет, и отмечать в таблице
        // нечего: стрелка напротив самого крупного из мелких скачков рядом
        // со словами «узкого места нет» противоречила бы сама себе.
        hop: (blame != Blame::Distance).then_some(hop.number),
        blame,
        jump,
        text,
    }
}

/// Насколько канал проседает под большими пакетами.
///
/// Сравниваем время ответа на пакет в 32 байта и на пакет в 1400. Разница —
/// это время, которое пакет ждёт в исходящей очереди плюс время его выдачи в
/// канал. На толстом канале она в единицах миллисекунд; десятки означают,
/// что очередь на аплинке длинная, и любая фоновая загрузка будет ронять
/// пинг в матче.
///
/// Это не полноценная проверка на bufferbloat — та требует занять канал
/// целиком, то есть скачать несколько десятков мегабайт с чужого сервера.
/// Делать это без спроса программа не будет.
pub fn uplink_delay(icmp: &Icmp, dest: Ipv4Addr) -> Option<(f64, f64)> {
    let small = icmp::ping(icmp, dest, None, 8, 32, Duration::from_millis(1200));
    let large = icmp::ping(icmp, dest, None, 8, 1400, Duration::from_millis(1500));
    if !small.ok() || !large.ok() {
        return None;
    }
    Some((small.avg, large.avg))
}

/// Порог, после которого разница между большими и маленькими пакетами
/// перестаёт быть свойством физики и становится проблемой очереди.
pub const UPLINK_DELAY_BAD_MS: f64 = 25.0;

#[cfg(test)]
mod tests {
    use super::*;

    fn hop(number: u8, ms: f64, private: bool) -> Hop {
        Hop {
            number,
            ip: Some(if private {
                Ipv4Addr::new(10, 0, 0, number as u8)
            } else {
                Ipv4Addr::new(8, 8, 8, number as u8)
            }),
            ms: Some(ms),
            private,
        }
    }

    #[test]
    fn a_smooth_route_blames_distance() {
        let hops = vec![
            hop(1, 1.0, true),
            hop(2, 12.0, true),
            hop(3, 25.0, false),
            hop(4, 38.0, false),
        ];
        assert_eq!(analyse(&hops).blame, Blame::Distance);
    }

    #[test]
    fn a_slow_first_hop_blames_the_house() {
        let hops = vec![hop(1, 45.0, true), hop(2, 50.0, true), hop(3, 58.0, false)];
        let v = analyse(&hops);
        assert_eq!(v.blame, Blame::Home);
        assert_eq!(v.hop, Some(1));
    }

    #[test]
    fn a_jump_on_a_grey_address_blames_the_provider() {
        let hops = vec![
            hop(1, 1.0, true),
            hop(2, 3.0, true),
            hop(3, 70.0, true),
            hop(4, 74.0, false),
        ];
        let v = analyse(&hops);
        assert_eq!(v.blame, Blame::Provider);
        assert_eq!(v.hop, Some(3));
    }

    #[test]
    fn a_small_bump_on_a_long_route_is_not_a_culprit() {
        // Скачок в 8 мс на маршруте в 120 мс — это шум, и отправлять из-за
        // него человека к провайдеру нельзя.
        let hops = vec![
            hop(1, 1.0, true),
            hop(2, 40.0, false),
            hop(3, 48.0, false),
            hop(4, 120.0, false),
        ];
        // Здесь самый крупный скачок — последний (72 мс), и он значим;
        // проверяем именно то, что маленькие скачки не побеждают крупные.
        assert_eq!(analyse(&hops).hop, Some(4));
    }

    #[test]
    fn a_long_haul_leg_is_not_a_fault() {
        // Настоящая трассировка из Киева до Франкфурта: плечо в 17 мс между
        // сетью провайдера и магистралью — это география, а не авария.
        // Ловить её значило бы отправлять человека жаловаться на скорость
        // света.
        let hops = vec![
            hop(1, 0.3, true),
            hop(2, 0.8, true),
            hop(3, 0.9, false),
            hop(4, 1.5, false),
            hop(6, 18.1, false),
            hop(7, 20.4, false),
            hop(9, 30.4, false),
        ];
        assert_eq!(analyse(&hops).blame, Blame::Distance);
    }

    #[test]
    fn a_slow_home_router_outweighs_a_long_route() {
        // 25 мс до своего же роутера — беда, даже если весь маршрут 200 мс и
        // где-то дальше есть скачок покрупнее.
        let hops = vec![hop(1, 25.0, true), hop(2, 30.0, true), hop(3, 200.0, false)];
        let v = analyse(&hops);
        assert_eq!(v.blame, Blame::Home);
        assert_eq!(v.hop, Some(1));
    }

    #[test]
    fn silence_is_not_a_verdict() {
        let hops = vec![Hop {
            number: 1,
            ip: None,
            ms: None,
            private: false,
        }];
        assert_eq!(analyse(&hops).blame, Blame::Unknown);
    }
}
