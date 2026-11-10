//! Настройки стека Windows, у которых есть измеримый смысл.
//!
//! Списки «100 твиков для сети» из интернета сюда не переехали, и это
//! осознанно: большая их часть либо не существует в современной Windows,
//! либо меняет то, что и так стоит по умолчанию, либо ломает загрузку файлов
//! ради миллисекунды в игре, которой там нет. Здесь только то, про что можно
//! сказать, что именно оно делает, — и рядом честная оговорка, когда рычаг
//! слабый.

use crate::iface::Iface;
use crate::reg;
use crate::state::State;

/// Профиль мультимедийного планировщика. Тот самый, где живёт ограничение
/// сетевой обработки для не-мультимедийных процессов.
const MM_PROFILE: &str =
    "HKLM\\SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion\\Multimedia\\SystemProfile";

/// Одна применённая настройка — для отчёта человеку.
pub struct Applied {
    pub title: &'static str,
    pub detail: String,
    /// Насколько рычаг сильный. Слабые тоже применяем, но подписываем
    /// слабыми: программа, которая называет всё «ускорением», врёт.
    pub weak: bool,
}

/// Снимает ограничение сетевой обработки и поднимает долю переднего плана.
pub fn system(state: &mut State) -> Result<Vec<Applied>, String> {
    let mut done = Vec::new();

    // MMCSS ограничивает обработку сети «не-мультимедийных» процессов
    // десятью пакетами на миллисекунду. Для веба это незаметно, для игры,
    // которая шлёт и принимает мелкие пакеты по расписанию тика, — это
    // ровно та лишняя миллисекунда, которая копится в дрожание.
    state.remember_reg(
        MM_PROFILE,
        "NetworkThrottlingIndex",
        "снимает лимит MMCSS в 10 пакетов на миллисекунду",
    );
    reg::write(
        MM_PROFILE,
        "NetworkThrottlingIndex",
        "REG_DWORD",
        "0xffffffff",
    )?;
    done.push(Applied {
        title: "Ограничение сетевой обработки MMCSS",
        detail: "снято".into(),
        weak: false,
    });

    // Доля процессорного времени, которую планировщик держит для фоновых
    // задач. По умолчанию 20%. Ставим 10, а не 0: на нуле мультимедийные
    // потоки — в том числе звук самой игры — начинают заикаться, и человек
    // меняет треск в наушниках на миллисекунду пинга.
    state.remember_reg(
        MM_PROFILE,
        "SystemResponsiveness",
        "доля времени для фоновых задач: 20% по умолчанию, ставим 10%",
    );
    reg::write(MM_PROFILE, "SystemResponsiveness", "REG_DWORD", "10")?;
    done.push(Applied {
        title: "Резерв планировщика для фона",
        detail: "20% → 10%".into(),
        weak: false,
    });

    Ok(done)
}

/// Настройки конкретного интерфейса.
///
/// Ключи лежат под GUID интерфейса, и это важно: те же имена в общей ветке
/// Tcpip\Parameters Windows игнорирует — половина руководств в интернете
/// советует писать именно туда, и потому «не помогает».
pub fn interface(state: &mut State, iface: &Iface) -> Result<Vec<Applied>, String> {
    if iface.guid.is_empty() {
        return Err(format!(
            "у интерфейса «{}» не удалось узнать GUID — настраивать нечего",
            iface.alias
        ));
    }
    let key = format!(
        "HKLM\\SYSTEM\\CurrentControlSet\\Services\\Tcpip\\Parameters\\Interfaces\\{}",
        iface.guid
    );

    // Отложенное подтверждение: Windows придерживает ACK до 200 мс в
    // надежде приклеить его к данным. И честная оговорка: матч Fortnite
    // идёт по UDP, и на сам игровой трафик это не влияет вообще. Влияет на
    // всё остальное, что игра делает по TCP, — вход, магазин, лобби,
    // подгрузку. Рычаг слабый, и подписан слабым.
    for (name, value, title) in [
        ("TcpAckFrequency", "1", "Отложенное подтверждение TCP"),
        ("TCPNoDelay", "1", "Алгоритм Нейгла"),
        ("TcpDelAckTicks", "0", "Таймер отложенного ACK"),
    ] {
        state.remember_reg(&key, name, "ускоряет TCP-часть игры (лобби, вход, магазин)");
        reg::write(&key, name, "REG_DWORD", value)?;
        let _ = title;
    }

    Ok(vec![Applied {
        title: "Задержки TCP на интерфейсе",
        detail: format!(
            "сняты на «{}» — на UDP-трафик матча не влияют, только на лобби и вход",
            iface.alias
        ),
        weak: true,
    }])
}

/// Ставит MTU, если измеренный меньше текущего.
///
/// Только вниз: поднять MTU выше того, что пропускает канал, — это
/// гарантированная фрагментация и потери, то есть ровно та беда, от которой
/// лечимся.
pub fn mtu(state: &mut State, iface: &Iface, measured: u32) -> Option<Applied> {
    if measured >= iface.mtu || iface.mtu == 0 {
        return None;
    }
    if crate::iface::set_mtu(iface.index, measured).is_err() {
        return None;
    }
    state.mtu.push(crate::state::MtuWas {
        index: iface.index,
        alias: iface.alias.clone(),
        was: iface.mtu,
        now: measured,
    });
    Some(Applied {
        title: "MTU интерфейса",
        detail: format!(
            "{} → {measured}: пакеты в {} байт по дороге резались надвое",
            iface.mtu, iface.mtu
        ),
        weak: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn iface_with_mtu(mtu: u32) -> Iface {
        Iface {
            index: 1,
            alias: "Ethernet".into(),
            description: String::new(),
            guid: "{X}".into(),
            media: "802.3".into(),
            speed: String::new(),
            ip: "192.168.0.2".parse().unwrap(),
            mtu,
            metric: 25,
            automatic_metric: true,
            gateway: Some("192.168.0.1".parse().unwrap()),
        }
    }

    #[test]
    fn mtu_is_never_raised() {
        // Замер 1500 при текущих 1420 не должен поднимать MTU: канал шире не
        // станет, а фрагментация начнётся.
        let mut s = State::default();
        assert!(mtu(&mut s, &iface_with_mtu(1420), 1500).is_none());
        assert!(s.mtu.is_empty());
    }

    #[test]
    fn equal_mtu_changes_nothing() {
        let mut s = State::default();
        assert!(mtu(&mut s, &iface_with_mtu(1500), 1500).is_none());
    }
}
