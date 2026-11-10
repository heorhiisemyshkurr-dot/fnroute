//! Регионы матчмейкинга Fortnite и чем до них мерить.
//!
//! Это самый крупный рычаг во всей программе, и он не в реестре. Разница
//! между «EU» и «NA-East» — сотня миллисекунд; разница между удачным и
//! неудачным ключом реестра — единицы, и то не всегда. Поэтому раздел стоит
//! первым: если игра ищет матч не в том регионе, всё остальное не имеет
//! значения.
//!
//! Мерить сами игровые серверы нельзя: они поднимаются под матч и живут
//! минуты, у них нет постоянного адреса. Зато у дата-центров, в которых они
//! поднимаются, адреса постоянные — до них и меряем. Это не «пинг в игре»
//! (в игре к нему добавится путь до конкретной стойки), но разница между
//! регионами видна именно так, и это то, что нужно для выбора.

use crate::icmp::Sample;
use crate::tcp;
use std::net::Ipv4Addr;
use std::time::Duration;

/// Один регион матчмейкинга.
pub struct Region {
    /// Как регион зовётся у Epic — это же значение видно в настройках игры.
    pub id: &'static str,
    pub title: &'static str,
    /// Адреса дата-центров региона, по порядку значимости.
    ///
    /// Их бывает больше одного: у «NA-East» это Вирджиния и Огайо, и матч
    /// может подняться в любом. Берём лучший — матчмейкинг выберет тот же.
    ///
    /// Точки — конечные адреса Amazon: серверы Fortnite стоят в тех же
    /// зонах. Порт 443, а не эхо-проба: на ICMP в этих сетях не отвечают
    /// вовсе, и пингом регион не измерить в принципе.
    pub anchors: &'static [&'static str],
}

pub const REGIONS: &[Region] = &[
    Region {
        id: "EU",
        title: "Европа",
        anchors: &[
            "ec2.eu-central-1.amazonaws.com",
            "ec2.eu-west-2.amazonaws.com",
            "ec2.eu-north-1.amazonaws.com",
        ],
    },
    Region {
        id: "ME",
        title: "Ближний Восток",
        // ОАЭ, а не Бахрейн: серверы Fortnite стоят и там, и там, но точка
        // Бахрейна — регион с подпиской, и снаружи её 443-й порт закрыт.
        // Замер до неё возвращал бы «нет ответа» и выглядел бы как обрыв
        // связи, хотя связь есть.
        anchors: &["ec2.me-central-1.amazonaws.com"],
    },
    Region {
        id: "NAE",
        title: "Северная Америка — восток",
        anchors: &[
            "ec2.us-east-1.amazonaws.com",
            "ec2.us-east-2.amazonaws.com",
        ],
    },
    Region {
        id: "NAW",
        title: "Северная Америка — запад",
        anchors: &["ec2.us-west-2.amazonaws.com"],
    },
    Region {
        id: "BR",
        title: "Бразилия",
        anchors: &["ec2.sa-east-1.amazonaws.com"],
    },
    Region {
        id: "ASIA",
        title: "Азия",
        anchors: &[
            "ec2.ap-northeast-1.amazonaws.com",
            "ec2.ap-southeast-1.amazonaws.com",
        ],
    },
    Region {
        id: "OCE",
        title: "Океания",
        anchors: &["ec2.ap-southeast-2.amazonaws.com"],
    },
];

pub fn by_id(id: &str) -> Option<&'static Region> {
    REGIONS
        .iter()
        .find(|r| r.id.eq_ignore_ascii_case(id))
}

/// Замер одного региона.
#[derive(Clone)]
pub struct Measured {
    pub region: &'static Region,
    /// Адрес, по которому получился лучший результат, — его же потом
    /// трассируем и через него ищем MTU.
    pub anchor: Option<Ipv4Addr>,
    pub anchor_host: &'static str,
    pub sample: Sample,
}

/// Сколько раз соединяться с каждой точкой.
///
/// Шесть, а не двадцать: точек в списке одиннадцать, и при двадцати пробах
/// человек ждёт полторы минуты. Шести хватает, чтобы отличить 30 мс от 90;
/// отличать 30 от 33 нам и не нужно — в этот зазор не влезает ни одно
/// решение, которое программа умеет принять.
const PROBE_COUNT: u32 = 6;
const PROBE_TIMEOUT: Duration = Duration::from_millis(1500);

/// Меряет все регионы с одного интерфейса.
pub fn measure_all(source: Option<Ipv4Addr>, on_step: &dyn Fn(&str)) -> Vec<Measured> {
    REGIONS
        .iter()
        .map(|region| {
            on_step(region.title);
            measure(region, source)
        })
        .collect()
}

/// Меряет один регион: пробует все его точки и оставляет лучшую.
pub fn measure(region: &'static Region, source: Option<Ipv4Addr>) -> Measured {
    let mut best: Option<(Ipv4Addr, &'static str, Sample)> = None;
    for host in region.anchors {
        let Some(ip) = tcp::resolve(host) else {
            continue;
        };
        let sample = tcp::probe(ip, 443, source, PROBE_COUNT, PROBE_TIMEOUT);
        let better = best
            .as_ref()
            .map_or(true, |(_, _, prev)| sample.score() < prev.score());
        if better {
            best = Some((ip, host, sample));
        }
    }
    match best {
        Some((ip, host, sample)) => Measured {
            region,
            anchor: Some(ip),
            anchor_host: host,
            sample,
        },
        None => Measured {
            region,
            anchor: None,
            anchor_host: region.anchors.first().copied().unwrap_or(""),
            sample: Sample {
                sent: PROBE_COUNT,
                loss_pct: 100,
                ..Default::default()
            },
        },
    }
}

/// Лучший из замеренных регионов.
pub fn best(measured: &[Measured]) -> Option<&Measured> {
    measured
        .iter()
        .filter(|m| m.sample.ok())
        .min_by(|a, b| {
            a.sample
                .score()
                .partial_cmp(&b.sample.score())
                .unwrap_or(std::cmp::Ordering::Equal)
        })
}

/// Стоит ли вообще менять регион.
///
/// Порог не вкусовой: матчмейкинг сажает в регион не только по задержке, но
/// и по числу игроков, и уход в соседний регион ради пяти миллисекунд
/// оборачивается вдвое более долгим поиском матча. Двадцать миллисекунд —
/// это уже разница, которую видно в перестрелке.
pub const WORTH_SWITCHING_MS: f64 = 20.0;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_region_has_an_anchor() {
        for r in REGIONS {
            assert!(!r.anchors.is_empty(), "{} без адресов", r.id);
        }
    }

    #[test]
    fn region_ids_are_unique() {
        // По id регион ищется в аргументах командной строки: два одинаковых —
        // и половина команд ушла бы не в тот регион.
        let mut seen = std::collections::HashSet::new();
        for r in REGIONS {
            assert!(seen.insert(r.id), "повтор региона: {}", r.id);
        }
    }

    #[test]
    fn unreachable_regions_never_win() {
        // Регион, до которого не достучались, — это стопроцентная потеря, и
        // «победить» он не может: иначе программа посоветовала бы уйти туда,
        // где вообще нет связи.
        let dead = Sample {
            sent: 6,
            loss_pct: 100,
            ..Default::default()
        };
        let live = summarized(&[40.0, 41.0]);
        assert!(live.score() < dead.score());
    }

    fn summarized(times: &[f64]) -> Sample {
        crate::icmp::summarize(times.len() as u32, times)
    }
}
