//! Запуск системных команд и проверка прав.
//!
//! Всё, что программа меняет — маршруты, метрики интерфейсов, политики QoS,
//! ключи реестра, — Windows отдаёт только процессу с правами администратора.
//! Поэтому проверка прав здесь же, где запуск: узнать об отказе после того,
//! как половина настроек применилась, а половина нет, — худший из возможных
//! исходов.

use std::process::Command;

/// Флаг CREATE_NO_WINDOW. Без него каждый вызов reg/route/powershell мигает
/// чёрным окном — а в фоновом режиме их сотни за сессию.
#[cfg(target_os = "windows")]
const NO_WINDOW: u32 = 0x0800_0000;

/// Переводит консоль в UTF-8.
///
/// Без этого весь русский текст в окне, открытом двойным кликом,
/// превращается в кракозябры: Rust пишет в поток UTF-8, а консоль Windows по
/// умолчанию читает его в кодировке 866. Через конвейер (когда вывод
/// перехватывает другая программа) этого не видно — и потому ошибку легко
/// не заметить до первого живого запуска.
#[cfg(target_os = "windows")]
pub fn console_utf8() {
    #[link(name = "kernel32")]
    extern "system" {
        fn SetConsoleOutputCP(page: u32) -> i32;
        fn SetConsoleCP(page: u32) -> i32;
    }
    const UTF8: u32 = 65001;
    unsafe {
        SetConsoleOutputCP(UTF8);
        SetConsoleCP(UTF8);
    }
}

#[cfg(not(target_os = "windows"))]
pub fn console_utf8() {}

/// Запускает программу и возвращает её вывод. Ненулевой код — ошибка.
#[cfg(target_os = "windows")]
pub fn run(program: &str, args: &[&str]) -> Result<String, String> {
    use std::os::windows::process::CommandExt;
    let out = Command::new(program)
        .args(args)
        .creation_flags(NO_WINDOW)
        .output()
        .map_err(|e| format!("не удалось запустить {program}: {e}"))?;
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    if out.status.success() {
        Ok(text)
    } else {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        Err(if err.is_empty() {
            format!("{program} завершился с ошибкой")
        } else {
            err
        })
    }
}

/// То же самое, но код возврата не важен.
///
/// Нужен там, где ненулевой код — это часть ответа, а не сбой: `route delete`
/// на несуществующем маршруте, `tasklist` без совпадений.
pub fn run_lossy(program: &str, args: &[&str]) -> String {
    run(program, args).unwrap_or_default()
}

/// Один запуск PowerShell.
///
/// Сетевые данные забираем через него, а не через netsh: вывод netsh
/// локализован («Настройка интерфейса "Ethernet"»), и разбор ломается при
/// смене языка системы. Командлеты Get-Net* отдают поля с именами.
pub fn powershell(script: &str) -> Result<String, String> {
    run(
        "powershell",
        &["-NoProfile", "-NonInteractive", "-Command", script],
    )
}

/// Разбирает вывод «ключ=значение», которым отвечают наши скрипты.
///
/// ConvertTo-Json не годится: в PowerShell 5.1 массив из одного элемента
/// сериализуется скаляром, и список из одного интерфейса приезжал бы не
/// списком.
pub fn kv(text: &str) -> Vec<Vec<(String, String)>> {
    let mut blocks = Vec::new();
    let mut current: Vec<(String, String)> = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line == "--" {
            if !current.is_empty() {
                blocks.push(std::mem::take(&mut current));
            }
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            current.push((k.trim().to_string(), v.trim().to_string()));
        }
    }
    if !current.is_empty() {
        blocks.push(current);
    }
    blocks
}

/// Значение поля из разобранного блока.
pub fn field<'a>(block: &'a [(String, String)], key: &str) -> Option<&'a str> {
    block
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
        .filter(|v| !v.is_empty())
}

// ── Права ────────────────────────────────────────────────────────────────

#[cfg(target_os = "windows")]
mod elevation {
    const TOKEN_QUERY: u32 = 0x0008;
    /// TokenElevation из TOKEN_INFORMATION_CLASS.
    const TOKEN_ELEVATION: i32 = 20;

    #[link(name = "advapi32")]
    extern "system" {
        fn OpenProcessToken(process: isize, access: u32, token: *mut isize) -> i32;
        fn GetTokenInformation(
            token: isize,
            class: i32,
            info: *mut core::ffi::c_void,
            len: u32,
            returned: *mut u32,
        ) -> i32;
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn GetCurrentProcess() -> isize;
        fn CloseHandle(handle: isize) -> i32;
    }

    pub fn is_elevated() -> bool {
        // Спрашиваем токен, а не пробуем записать в HKLM «на пробу»: пробная
        // запись меняет систему ради ответа на вопрос.
        unsafe {
            let mut token: isize = 0;
            if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
                return false;
            }
            let mut elevated: u32 = 0;
            let mut size: u32 = 0;
            let ok = GetTokenInformation(
                token,
                TOKEN_ELEVATION,
                &mut elevated as *mut u32 as *mut core::ffi::c_void,
                4,
                &mut size,
            );
            CloseHandle(token);
            ok != 0 && elevated != 0
        }
    }
}

#[cfg(target_os = "windows")]
pub fn is_elevated() -> bool {
    elevation::is_elevated()
}

/// Сообщение об отсутствии прав — одно на все команды, которые пишут.
pub fn require_admin() -> Result<(), String> {
    if is_elevated() {
        Ok(())
    } else {
        Err("Нужны права администратора: маршруты, метрики интерфейсов, \
             политики QoS и ключи реестра Windows обычному процессу менять не даёт.\n\
             Запусти fnroute.exe правой кнопкой → «Запуск от имени администратора»."
            .into())
    }
}

// Заглушки, чтобы крейт собирался и его разбор проверялся тестами не только
// на Windows.
#[cfg(not(target_os = "windows"))]
pub fn run(program: &str, _args: &[&str]) -> Result<String, String> {
    Err(format!("{program} есть только в Windows"))
}

#[cfg(not(target_os = "windows"))]
pub fn is_elevated() -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_split_on_separator() {
        let text = "index=5\nalias=Ethernet\n--\nindex=12\nalias=Wi-Fi\n";
        let blocks = kv(text);
        assert_eq!(blocks.len(), 2);
        assert_eq!(field(&blocks[1], "alias"), Some("Wi-Fi"));
    }

    #[test]
    fn empty_value_reads_as_missing() {
        // PowerShell печатает «gateway=» когда шлюза нет; пустая строка —
        // это отсутствие значения, а не значение из нуля символов.
        let blocks = kv("alias=Ethernet\ngateway=\n");
        assert_eq!(field(&blocks[0], "gateway"), None);
    }
}
