//! Интерфейсы и таблица маршрутизации — то, чем программа работает как
//! маршрутизатор.
//!
//! Домашний роутер решает две задачи: по какому каналу выпускать пакет и в
//! какой очереди его держать. Вторая половина живёт в `qos.rs`, первая —
//! здесь. На обычном ПК с одним проводом выбирать не из чего, и это надо
//! честно говорить; но стоит появиться Wi-Fi рядом с Ethernet, телефону,
//! раздающему интернет, или клиенту VPN — и выбор появляется, а Windows
//! делает его по таблице метрик, которая про игру ничего не знает.

use crate::sys::{self, field, kv};
use std::net::Ipv4Addr;

/// Сетевой интерфейс с адресом IPv4.
#[derive(Clone, Debug)]
pub struct Iface {
    pub index: u32,
    pub alias: String,
    pub description: String,
    /// GUID нужен для ключей реестра Tcpip\Parameters\Interfaces.
    pub guid: String,
    pub media: String,
    pub speed: String,
    pub ip: Ipv4Addr,
    pub mtu: u32,
    pub metric: u32,
    /// Метрику считает Windows. Пока это так, «вернуть как было» означает
    /// «вернуть автоматический расчёт», а не «поставить прежнее число».
    pub automatic_metric: bool,
    pub gateway: Option<Ipv4Addr>,
}

impl Iface {
    /// Беспроводной канал. Отдельным вопросом, потому что это первая причина
    /// скачущего пинга и единственная, которую не лечит ни один твик:
    /// эфир делится с соседями, микроволновкой и телефоном.
    pub fn is_wifi(&self) -> bool {
        self.media.contains("802.11") || self.description.to_lowercase().contains("wi-fi")
    }

    /// Виртуальный адаптер: VPN, гипервизор, песочница.
    ///
    /// Такие перехватывают маршрут по умолчанию целиком, и трафик игры
    /// уходит в туннель, добавляя к пингу дорогу до сервера VPN и обратно.
    /// Человек при этом уверен, что играет напрямую.
    pub fn is_virtual(&self) -> bool {
        const MARKS: [&str; 12] = [
            "tap-", "tun", "wireguard", "openvpn", "hyper-v", "virtualbox", "vmware", "hamachi",
            "zerotier", "radmin", "nordlynx", "wan miniport",
        ];
        let text = self.description.to_lowercase();
        MARKS.iter().any(|m| text.contains(m))
    }

    /// Годится ли интерфейс как выход в интернет.
    pub fn routable(&self) -> bool {
        self.gateway.is_some()
    }

    pub fn kind(&self) -> &'static str {
        if self.is_virtual() {
            "виртуальный"
        } else if self.is_wifi() {
            "Wi-Fi"
        } else {
            "кабель"
        }
    }
}

/// Перечисляет включённые интерфейсы с адресом IPv4.
///
/// Одним запуском PowerShell, а не тремя запусками netsh: разбирать
/// локализованный вывод netsh пришлось бы с оглядкой на язык системы, а
/// командлеты Get-Net* отдают именованные поля.
pub fn list() -> Result<Vec<Iface>, String> {
    let script = r#"
$ErrorActionPreference='SilentlyContinue';
Get-NetAdapter | Where-Object { $_.Status -eq 'Up' } | ForEach-Object {
  $a = $_;
  $ip = Get-NetIPAddress -InterfaceIndex $a.ifIndex -AddressFamily IPv4 |
        Where-Object { $_.IPAddress -notlike '169.254.*' } | Select-Object -First 1;
  if ($ip) {
    $cfg = Get-NetIPInterface -InterfaceIndex $a.ifIndex -AddressFamily IPv4 | Select-Object -First 1;
    $gw = Get-NetRoute -InterfaceIndex $a.ifIndex -DestinationPrefix '0.0.0.0/0' -AddressFamily IPv4 |
          Sort-Object RouteMetric | Select-Object -First 1;
    Write-Output ('index=' + $a.ifIndex);
    Write-Output ('alias=' + $a.Name);
    Write-Output ('desc=' + $a.InterfaceDescription);
    Write-Output ('guid=' + $a.InterfaceGuid);
    Write-Output ('media=' + $a.MediaType);
    Write-Output ('speed=' + $a.LinkSpeed);
    Write-Output ('ip=' + $ip.IPAddress);
    Write-Output ('mtu=' + $cfg.NlMtu);
    Write-Output ('metric=' + $cfg.InterfaceMetric);
    Write-Output ('automatic=' + $cfg.AutomaticMetric);
    Write-Output ('gateway=' + $gw.NextHop);
    Write-Output '--';
  }
}"#;
    let out = sys::powershell(script)?;
    let list = parse(&out);
    if list.is_empty() {
        return Err("Ни одного включённого интерфейса с адресом IPv4. Похоже, интернета сейчас нет."
            .into());
    }
    Ok(list)
}

/// Разбирает вывод скрипта. Отдельной функцией — чтобы её проверял тест, а
/// не только живая машина.
pub fn parse(text: &str) -> Vec<Iface> {
    kv(text)
        .into_iter()
        .filter_map(|block| {
            let ip: Ipv4Addr = field(&block, "ip")?.parse().ok()?;
            Some(Iface {
                index: field(&block, "index")?.parse().ok()?,
                alias: field(&block, "alias").unwrap_or_default().to_string(),
                description: field(&block, "desc").unwrap_or_default().to_string(),
                guid: field(&block, "guid").unwrap_or_default().to_string(),
                media: field(&block, "media").unwrap_or_default().to_string(),
                speed: field(&block, "speed").unwrap_or_default().to_string(),
                ip,
                mtu: field(&block, "mtu").and_then(|v| v.parse().ok()).unwrap_or(0),
                metric: field(&block, "metric")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0),
                // Get-NetIPInterface печатает это поле не булевым, а словом
                // «Enabled»/«Disabled» — и заодно «True» в некоторых сборках
                // Windows. Принимаем оба: от значения зависит откат, а
                // спутать «считает Windows» с «стояло число 35» значит
                // навсегда оставить человеку ручную метрику.
                automatic_metric: field(&block, "automatic").is_some_and(|v| {
                    v.eq_ignore_ascii_case("enabled") || v.eq_ignore_ascii_case("true")
                }),
                gateway: field(&block, "gateway").and_then(|v| v.parse().ok()),
            })
        })
        .collect()
}

/// Интерфейс, через который система сейчас реально ходит в интернет.
///
/// Не «первый включённый»: на машине с гипервизором и клиентом VPN
/// включённых интерфейсов бывает пять, и настроить не тот — значит не
/// настроить ничего, но написать «готово».
pub fn active(list: &[Iface]) -> Option<&Iface> {
    list.iter()
        .filter(|i| i.routable())
        .min_by_key(|i| i.metric)
}

// ── Метрика ──────────────────────────────────────────────────────────────

/// Ставит интерфейсу ручную метрику.
///
/// Это и есть «выпускать игру через этот провод»: из двух маршрутов по
/// умолчанию Windows берёт тот, у которого сумма метрик меньше. Меняем не
/// маршруты, а метрику интерфейса — маршрут по умолчанию создаёт DHCP, и
/// переписанный руками он вернулся бы при следующем обновлении аренды.
pub fn set_metric(index: u32, metric: u32) -> Result<(), String> {
    sys::powershell(&format!(
        "Set-NetIPInterface -InterfaceIndex {index} -AddressFamily IPv4 \
         -InterfaceMetric {metric} -ErrorAction Stop"
    ))
    .map(|_| ())
}

/// Возвращает расчёт метрики Windows.
pub fn reset_metric(index: u32) -> Result<(), String> {
    sys::powershell(&format!(
        "Set-NetIPInterface -InterfaceIndex {index} -AddressFamily IPv4 \
         -AutomaticMetric Enabled -ErrorAction Stop"
    ))
    .map(|_| ())
}

// ── MTU ──────────────────────────────────────────────────────────────────

pub fn set_mtu(index: u32, mtu: u32) -> Result<(), String> {
    sys::powershell(&format!(
        "Set-NetIPInterface -InterfaceIndex {index} -AddressFamily IPv4 \
         -NlMtuBytes {mtu} -ErrorAction Stop"
    ))
    .map(|_| ())
}

// ── Маршруты ─────────────────────────────────────────────────────────────

/// Прибивает адрес к конкретному интерфейсу.
///
/// Маршрут /32 на один адрес, а не на сеть: адреса игровых серверов Epic
/// разбросаны по всему диапазону Amazon, и «прибить регион» означало бы
/// добавить тысячи маршрутов. Один адрес имеет смысл прибивать тогда, когда
/// он известен точно — например, человек увидел его сам и хочет пустить
/// игру мимо VPN.
pub fn pin(dest: Ipv4Addr, gateway: Ipv4Addr, index: u32, metric: u32) -> Result<(), String> {
    sys::run(
        "route",
        &[
            "add",
            &dest.to_string(),
            "mask",
            "255.255.255.255",
            &gateway.to_string(),
            "metric",
            &metric.to_string(),
            "if",
            &index.to_string(),
        ],
    )
    .map(|_| ())
}

/// Снимает прибитый маршрут. Отсутствие маршрута — не ошибка: команда
/// вызывается и при откате, где часть маршрутов уже могла исчезнуть сама
/// (перезагрузка, смена сети).
pub fn unpin(dest: Ipv4Addr) {
    let _ = sys::run("route", &["delete", &dest.to_string()]);
}

/// Все маршруты по умолчанию — по одному на каждый интерфейс, который
/// считает себя выходом в интернет.
pub fn default_routes() -> Vec<(u32, String, u32)> {
    let script = "\
$ErrorActionPreference='SilentlyContinue';
Get-NetRoute -DestinationPrefix '0.0.0.0/0' -AddressFamily IPv4 | ForEach-Object {
  Write-Output ('index=' + $_.InterfaceIndex);
  Write-Output ('next=' + $_.NextHop);
  Write-Output ('metric=' + $_.RouteMetric);
  Write-Output '--';
}";
    let Ok(out) = sys::powershell(script) else {
        return Vec::new();
    };
    kv(&out)
        .into_iter()
        .filter_map(|b| {
            Some((
                field(&b, "index")?.parse().ok()?,
                field(&b, "next")?.to_string(),
                field(&b, "metric").and_then(|v| v.parse().ok()).unwrap_or(0),
            ))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // «Enabled» — то, что Get-NetIPInterface печатает на живой машине;
    // «True» встречается в других сборках Windows, и разбор обязан понимать
    // оба слова.
    const SAMPLE: &str = "index=11\nalias=Ethernet\ndesc=Realtek Gaming 2.5GbE Family Controller\nguid={AAA}\nmedia=802.3\nspeed=1 Gbps\nip=192.168.0.15\nmtu=1500\nmetric=25\nautomatic=Enabled\ngateway=192.168.0.1\n--\nindex=8\nalias=Wi-Fi\ndesc=Intel(R) Wi-Fi 6 AX200\nguid={BBB}\nmedia=Native 802.11\nspeed=866 Mbps\nip=192.168.0.31\nmtu=1500\nmetric=45\nautomatic=True\ngateway=192.168.0.1\n--\nindex=40\nalias=NordLynx\ndesc=NordLynx Tunnel\nguid={CCC}\nmedia=Unspecified\nspeed=100 Mbps\nip=10.5.0.2\nmtu=1420\nmetric=5\nautomatic=Disabled\ngateway=\n";

    #[test]
    fn parses_every_interface() {
        let list = parse(SAMPLE);
        assert_eq!(list.len(), 3);
        assert_eq!(list[0].alias, "Ethernet");
        assert_eq!(list[0].ip, "192.168.0.15".parse::<Ipv4Addr>().unwrap());
        assert_eq!(list[0].mtu, 1500);
    }

    #[test]
    fn wifi_and_tunnel_are_told_apart() {
        let list = parse(SAMPLE);
        assert!(!list[0].is_wifi() && !list[0].is_virtual());
        assert!(list[1].is_wifi());
        assert!(list[2].is_virtual());
    }

    #[test]
    fn interface_without_gateway_is_not_a_way_out() {
        // У туннеля в примере шлюза нет: он не должен считаться выходом в
        // интернет, иначе программа настроила бы игру на тупик.
        let list = parse(SAMPLE);
        assert!(!list[2].routable());
        assert_eq!(active(&list).unwrap().alias, "Ethernet");
    }

    #[test]
    fn automatic_metric_is_remembered_as_automatic() {
        // От этого зависит откат: у автоматической метрики «как было» — это
        // автоматика, а не число 25, которое Windows пересчитает сама.
        let list = parse(SAMPLE);
        assert!(list[0].automatic_metric, "«Enabled» — это автоматика");
        assert!(list[1].automatic_metric, "«True» — тоже автоматика");
        assert!(!list[2].automatic_metric);
    }
}
