//! Очередь: приоритет трафику игры и потолок для качалок.
//!
//! Это вторая половина работы маршрутизатора — не «куда выпускать пакет», а
//! «в каком порядке». Windows умеет это сама, через политики QoS, и умеет
//! ровно две вещи: пометить пакеты приложения полем DSCP и ограничить
//! скорость приложения.
//!
//! Про DSCP надо сказать прямо, потому что вокруг него больше всего вранья.
//! Метка — это просьба к оборудованию по дороге. Домашний роутер её обычно
//! игнорирует, провайдер почти наверняка обнуляет. Пользы от неё ровно
//! столько: свой же роутер, если в нём включён WMM или умный QoS, отдаёт
//! помеченному трафику очередь получше — на Wi-Fi это заметно, на кабеле
//! почти нет.
//!
//! А вот что работает всегда — второе: потолок для качалок. Пинг в матче
//! растёт не потому, что пакет игры «неприоритетный», а потому, что перед
//! ним в исходящей очереди стоит полсотни килобайт чужой загрузки. Убрать
//! эту очередь можно только одним способом — не давать её создать.

use crate::game;
use crate::state::State;
use crate::sys;

/// Приставка ко всем нашим политикам. По ней же они и находятся при откате:
/// удалять надо только своё, а на машине могут быть чужие политики — от
/// работодателя, от антивируса, от предыдущей «оптимизировалки».
pub const PREFIX: &str = "fnroute-";

/// DSCP 46 — Expedited Forwarding, класс для трафика реального времени.
/// Тот же класс используют голосовые звонки, и оборудование, которое вообще
/// смотрит на DSCP, знает именно его.
const DSCP_GAME: u8 = 46;

/// Ключ, без которого политики QoS на домашней машине не применяются.
///
/// Windows считает политику QoS частью групповой политики и применяет её
/// только когда служба определения сетей опознала сеть как доменную. На
/// домашнем ПК сеть не доменная никогда, и без этого ключа программа
/// создала бы политики, отчиталась «готово» — и они бы молча не работали.
const QOS_POLICY_KEY: &str = "HKLM\\SOFTWARE\\Policies\\Microsoft\\Windows\\QoS";
const NLA_VALUE: &str = "Do not use NLA";

/// Доля канала, которую планировщик пакетов резервирует под QoS.
///
/// Двадцать процентов «зарезервированной полосы» — самая известная городская
/// легенда про Windows: на самом деле резерв занимают только приложения,
/// которые сами просят QoS, и у большинства людей он не занят ничем. Но мы
/// как раз такие политики и создаём, поэтому обнуление лимита здесь не
/// суеверие, а следствие: иначе наш же приоритетный трафик упрётся в
/// собственный потолок.
const PSCHED_KEY: &str = "HKLM\\SOFTWARE\\Policies\\Microsoft\\Windows\\Psched";
const PSCHED_VALUE: &str = "NonBestEffortLimit";

/// Готовит систему к тому, что политики вообще будут применяться.
pub fn prepare(state: &mut State) -> Result<(), String> {
    state.remember_reg(
        QOS_POLICY_KEY,
        NLA_VALUE,
        "без этого политики QoS на недоменной сети не применяются",
    );
    crate::reg::write(QOS_POLICY_KEY, NLA_VALUE, "REG_SZ", "1")?;

    state.remember_reg(
        PSCHED_KEY,
        PSCHED_VALUE,
        "снимает потолок планировщика пакетов для нашего же приоритетного трафика",
    );
    crate::reg::write(PSCHED_KEY, PSCHED_VALUE, "REG_DWORD", "0")?;
    Ok(())
}

/// Помечает трафик игры как трафик реального времени.
pub fn prioritize_game(state: &mut State) -> Result<usize, String> {
    let mut created = 0;
    for (n, process) in game::GAME.iter().enumerate() {
        let name = format!("{PREFIX}game-{n}");
        // Своя же политика с прошлого запуска мешает создать новую — снимаем
        // молча: это не чужая настройка, а наш мусор.
        remove(&name);
        let script = format!(
            "New-NetQosPolicy -Name '{name}' \
             -AppPathNameMatchCondition '{process}' \
             -IPProtocolMatchCondition Both \
             -NetworkProfile All \
             -DSCPAction {DSCP_GAME} \
             -ErrorAction Stop | Out-Null"
        );
        sys::powershell(&script)?;
        state.remember_qos(name);
        created += 1;
    }
    Ok(created)
}

/// Ставит потолок скорости качалкам.
///
/// `mbit` — сколько им оставить. Числа по умолчанию тут нет намеренно:
/// «оставить 25%» требует знать ширину канала, а Windows её не знает —
/// LinkSpeed адаптера показывает скорость до роутера, а не до провайдера.
/// Придумать её за человека значило бы с равной вероятностью не ограничить
/// ничего или отрезать ему интернет.
pub fn throttle_hogs(state: &mut State, mbit: u32) -> Result<Vec<&'static str>, String> {
    let bits = (mbit as u64) * 1_000_000;
    let mut limited = Vec::new();
    for (n, (process, title)) in game::HOGS.iter().enumerate() {
        let name = format!("{PREFIX}hog-{n}");
        remove(&name);
        let script = format!(
            "New-NetQosPolicy -Name '{name}' \
             -AppPathNameMatchCondition '{process}' \
             -IPProtocolMatchCondition Both \
             -NetworkProfile All \
             -ThrottleRateActionBitsPerSecond {bits} \
             -ErrorAction Stop | Out-Null"
        );
        // Отказ на одной качалке не должен рушить остальные: программ в
        // списке девять, и ни одна из них не обязана быть установленной.
        if sys::powershell(&script).is_ok() {
            state.remember_qos(name);
            limited.push(*title);
        }
    }
    Ok(limited)
}

/// Удаляет одну политику. Отсутствие — не ошибка.
pub fn remove(name: &str) {
    let _ = sys::powershell(&format!(
        "Remove-NetQosPolicy -Name '{name}' -Confirm:$false -ErrorAction SilentlyContinue"
    ));
}

/// Снимает все политики, которые создали мы.
///
/// По списку из снимка, а потом ещё раз по приставке в именах: снимок мог
/// потеряться (переустановка Windows поверх, чужая уборка ProgramData), а
/// политики остаться — и тогда единственный способ их найти — имя.
pub fn remove_all(state: &State) -> usize {
    let mut removed = 0;
    for name in &state.qos {
        remove(name);
        removed += 1;
    }
    for name in ours() {
        if !state.qos.contains(&name) {
            remove(&name);
            removed += 1;
        }
    }
    removed
}

/// Имена наших политик, как их видит система.
pub fn ours() -> Vec<String> {
    let script = format!(
        "$ErrorActionPreference='SilentlyContinue'; \
         Get-NetQosPolicy | Where-Object {{ $_.Name -like '{PREFIX}*' }} | \
         ForEach-Object {{ Write-Output ('name=' + $_.Name) }}"
    );
    let Ok(out) = sys::powershell(&script) else {
        return Vec::new();
    };
    out.lines()
        .filter_map(|l| l.trim().strip_prefix("name=").map(str::to_string))
        .filter(|n| !n.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn our_policies_are_told_apart_by_prefix() {
        // Удалять по приставке — единственный способ убрать своё, не тронув
        // чужие политики на той же машине.
        assert!(format!("{PREFIX}game-0").starts_with(PREFIX));
        assert!(!"QoS-Corporate-Voice".starts_with(PREFIX));
    }

    #[test]
    fn every_game_process_gets_its_own_policy_name() {
        // Имена политик уникальны в пределах системы: совпади они — вторая
        // не создалась бы, и часть процессов игры осталась без пометки.
        let names: std::collections::HashSet<String> = (0..game::GAME.len())
            .map(|n| format!("{PREFIX}game-{n}"))
            .collect();
        assert_eq!(names.len(), game::GAME.len());
    }
}
