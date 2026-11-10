//! Процессы Fortnite и процессы, которые ей мешают.
//!
//! Имён у Fortnite несколько, и какое из них запущено — зависит от того,
//! каким образом игра стартовала и какая версия античита в сборке.
//! Спрашивать это у человека бессмысленно: он этого не знает. Поэтому
//! смотрим, что реально работает.

use crate::sys;

/// Процессы самой игры. Первый — тот, в котором живёт сетевой код матча:
/// именно ему нужен приоритет.
pub const GAME: &[&str] = &[
    "FortniteClient-Win64-Shipping.exe",
    "FortniteClient-Win64-Shipping_EAC_EOS.exe",
    "FortniteLauncher.exe",
];

/// Процесс, по которому понятно, что человек играет прямо сейчас.
///
/// Только клиент матча: лаунчер живёт часами и включать по нему игровой
/// профиль значило бы держать его включённым весь день.
pub const GAME_MARKER: &str = "FortniteClient-Win64-Shipping.exe";

/// Кто отъедает канал, пока идёт матч.
///
/// Список не «всё подряд»: браузера и мессенджера тут нет намеренно — они
/// шлют мало и режут их незачем, а вот качалка обновлений в фоне забивает
/// исходящую очередь целиком, и именно из-за неё пинг в матче прыгает с 30
/// до 300.
pub const HOGS: &[(&str, &str)] = &[
    ("EpicGamesLauncher.exe", "загрузчик Epic Games"),
    ("steam.exe", "Steam"),
    ("SteamService.exe", "служба Steam"),
    ("Battle.net.exe", "Battle.net"),
    ("RiotClientServices.exe", "клиент Riot"),
    ("OneDrive.exe", "OneDrive"),
    ("Dropbox.exe", "Dropbox"),
    ("qbittorrent.exe", "qBittorrent"),
    ("Transmission.exe", "Transmission"),
];

/// Запущен ли процесс с таким именем.
///
/// tasklist с фильтром, а не перебор всех процессов: фильтр отрабатывает
/// внутри системной команды, и на список из трёхсот процессов уходит один
/// запуск вместо разбора мегабайта текста.
pub fn is_running(process: &str) -> bool {
    let out = sys::run_lossy(
        "tasklist",
        &["/FI", &format!("IMAGENAME eq {process}"), "/NH"],
    );
    // При отсутствии совпадений tasklist печатает локализованное «Нет задач…»
    // и код возврата у него нулевой — поэтому цепляемся за имя процесса в
    // выводе, а не за код и не за текст сообщения.
    out.to_lowercase().contains(&process.to_lowercase())
}

/// Играет ли человек прямо сейчас.
pub fn in_match() -> bool {
    is_running(GAME_MARKER)
}

/// Какие из мешающих программ запущены сейчас.
pub fn running_hogs() -> Vec<&'static str> {
    HOGS.iter()
        .filter(|(process, _)| is_running(process))
        .map(|(_, title)| *title)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_process_name_has_exe() {
        // tasklist ищет по имени файла — без .exe фильтр не найдёт ничего, и
        // программа молча решила бы, что игра не запущена.
        for p in GAME {
            assert!(p.ends_with(".exe"), "{p} без .exe");
        }
        for (p, _) in HOGS {
            assert!(p.ends_with(".exe"), "{p} без .exe");
        }
    }

    #[test]
    fn the_marker_is_the_match_client() {
        assert!(GAME.contains(&GAME_MARKER));
    }
}
