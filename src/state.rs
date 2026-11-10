//! Снимок «как было» и откат.
//!
//! Программа меняет чужие настройки: метрику интерфейса, MTU, ключи реестра,
//! политики QoS, таблицу маршрутов. Всё это переживает закрытие окна и
//! перезагрузку, поэтому единственная честная форма такой программы — та,
//! где каждое изменение записано вместе с прежним значением и снимается
//! одной командой.
//!
//! Файл лежит в ProgramData, а не рядом с exe: exe кладут в «Загрузки», где
//! он живёт до первой уборки, а вернуть настройки надо и после того, как
//! программу удалили.

use crate::reg;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

const FILE: &str = "state.json";

/// Один тронутый ключ реестра.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct RegWas {
    pub path: String,
    pub name: String,
    /// Что стояло до нас. `None` — значения не было вовсе.
    pub was: Option<reg::Value>,
    /// Зачем трогали — попадает в вывод `fnroute status`, чтобы человек
    /// видел не список ключей, а список решений.
    pub why: String,
}

/// Метрика интерфейса до вмешательства.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct MetricWas {
    pub index: u32,
    pub alias: String,
    /// Метрику считала Windows — вернуть надо автоматику, а не число.
    pub was_automatic: bool,
    pub was: u32,
}

/// MTU интерфейса до вмешательства.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct MtuWas {
    pub index: u32,
    pub alias: String,
    pub was: u32,
    pub now: u32,
}

#[derive(Default, serde::Serialize, serde::Deserialize)]
pub struct State {
    /// Когда применили — миллисекунды эпохи.
    pub applied_at: Option<u64>,
    /// Регион, под который настраивали.
    pub region: Option<String>,
    /// Интерфейс, выбранный как выход для игры.
    pub uplink: Option<String>,
    #[serde(default)]
    pub registry: Vec<RegWas>,
    /// Имена созданных нами политик QoS.
    #[serde(default)]
    pub qos: Vec<String>,
    #[serde(default)]
    pub metrics: Vec<MetricWas>,
    #[serde(default)]
    pub mtu: Vec<MtuWas>,
    /// Прибитые адреса.
    #[serde(default)]
    pub routes: Vec<String>,
}

impl State {
    /// Есть ли что откатывать.
    pub fn applied(&self) -> bool {
        self.applied_at.is_some()
            && (!self.registry.is_empty()
                || !self.qos.is_empty()
                || !self.metrics.is_empty()
                || !self.mtu.is_empty()
                || !self.routes.is_empty())
    }

    pub fn stamp(&mut self) {
        self.applied_at = Some(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0),
        );
    }

    /// Запоминает созданную политику QoS.
    ///
    /// С проверкой на повтор, потому что фоновый режим ставит и снимает
    /// политики качалок по многу раз за вечер, а из аварийного выхода
    /// (выключили питание, убили процесс) их имена остаются в файле. Без
    /// проверки список рос бы с каждым матчем, и `status` показывал бы
    /// «политик QoS: 47» там, где их девять.
    pub fn remember_qos(&mut self, name: String) {
        if !self.qos.contains(&name) {
            self.qos.push(name);
        }
    }

    /// Запоминает ключ реестра до записи.
    ///
    /// Повторное применение поверх уже применённого не должно затирать
    /// снимок собственным значением — иначе откат вернул бы наши же
    /// настройки и человек остался бы с ними навсегда.
    pub fn remember_reg(&mut self, path: &str, name: &str, why: &str) {
        if self
            .registry
            .iter()
            .any(|r| r.path == path && r.name == name)
        {
            return;
        }
        self.registry.push(RegWas {
            path: path.to_string(),
            name: name.to_string(),
            was: reg::read(path, name),
            why: why.to_string(),
        });
    }
}

pub fn dir() -> PathBuf {
    // ProgramData, потому что программа работает из-под администратора, а
    // возвращать настройки может другой пользователь той же машины.
    let base = std::env::var("ProgramData")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir());
    base.join("fnroute")
}

pub fn path() -> PathBuf {
    dir().join(FILE)
}

pub fn load() -> State {
    std::fs::read_to_string(path())
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

pub fn save(state: &State) -> Result<(), String> {
    std::fs::create_dir_all(dir()).map_err(|e| format!("не создать {}: {e}", dir().display()))?;
    let raw = serde_json::to_string_pretty(state).map_err(|e| e.to_string())?;
    std::fs::write(path(), raw).map_err(|e| format!("не записать снимок: {e}"))
}

/// Забывает снимок — после того, как всё из него вернули.
pub fn clear() {
    let _ = std::fs::remove_file(path());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_state_has_nothing_to_revert() {
        let mut s = State::default();
        s.stamp();
        assert!(!s.applied(), "штамп времени сам по себе — не изменение");
    }

    #[test]
    fn a_policy_is_remembered_once() {
        // Фоновый режим ставит политики качалок заново на каждый матч; в
        // снимке они должны остаться одной записью, иначе счётчик в status
        // растёт весь вечер.
        let mut s = State::default();
        s.remember_qos("fnroute-hog-0".into());
        s.remember_qos("fnroute-hog-0".into());
        s.remember_qos("fnroute-hog-1".into());
        assert_eq!(s.qos.len(), 2);
    }

    #[test]
    fn a_key_is_snapshotted_once() {
        // Второе применение подряд не должно записать в снимок наше же
        // значение как «прежнее».
        let mut s = State::default();
        s.registry.push(RegWas {
            path: "HKLM\\X".into(),
            name: "Y".into(),
            was: None,
            why: "первое".into(),
        });
        s.remember_reg("HKLM\\X", "Y", "второе");
        assert_eq!(s.registry.len(), 1);
        assert_eq!(s.registry[0].why, "первое");
    }
}
