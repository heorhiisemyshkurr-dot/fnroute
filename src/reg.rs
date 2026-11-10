//! Чтение и запись реестра через reg.exe.
//!
//! Через внешнюю программу, а не через winreg: единственное, что программа
//! делает с реестром, — читает значение, запоминает его и пишет своё. Ради
//! трёх операций тянуть зависимость и unsafe-обёртки не за чем, а reg.exe
//! есть в любой Windows и его вывод для значений не локализован.
//!
//! Главное здесь не запись, а чтение перед ней: без снимка прежнего значения
//! кнопка «вернуть как было» — это ложь, потому что вернуть будет нечего.

use crate::sys;

/// Значение реестра: тип и данные ровно в том виде, в каком их вернул
/// reg.exe, — чтобы записать обратно тем же reg.exe без преобразований.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Value {
    pub kind: String,
    pub data: String,
}

/// Читает значение. `None` — значения нет (это тоже состояние, и его надо
/// запомнить: вернуть «как было» тогда означает удалить).
pub fn read(path: &str, name: &str) -> Option<Value> {
    let out = sys::run("reg", &["query", path, "/v", name]).ok()?;
    parse(&out, name)
}

/// Разбирает вывод `reg query`. Отдельной функцией — чтобы её проверял тест.
pub fn parse(text: &str, name: &str) -> Option<Value> {
    for line in text.lines() {
        let line = line.trim();
        // get, а не срез по индексу: в выводе встречаются строки с
        // кириллицей, и срез по байтовой длине имени упал бы на середине
        // символа.
        let Some(head) = line.get(..name.len()) else {
            continue;
        };
        if !head.eq_ignore_ascii_case(name) {
            continue;
        }
        let rest = line[name.len()..].trim_start();
        // Тип начинается с REG_; если сразу за именем идёт не он, значит
        // совпало начало другого имени (NetworkThrottlingIndexOld).
        let Some((kind, data)) = rest.split_once(char::is_whitespace) else {
            continue;
        };
        if !kind.starts_with("REG_") {
            continue;
        }
        return Some(Value {
            kind: kind.to_string(),
            // Данные строкового значения могут содержать пробелы, поэтому
            // берём весь остаток, а не первое слово.
            data: data.trim().to_string(),
        });
    }
    None
}

pub fn write(path: &str, name: &str, kind: &str, data: &str) -> Result<(), String> {
    sys::run(
        "reg",
        &["add", path, "/v", name, "/t", kind, "/d", data, "/f"],
    )
    .map(|_| ())
}

/// Удаляет значение. Отсутствие — не ошибка: при откате значение могло уже
/// исчезнуть само (обновление Windows, чужая «оптимизировалка»).
pub fn delete(path: &str, name: &str) {
    let _ = sys::run("reg", &["delete", path, "/v", name, "/f"]);
}

/// Возвращает значение в прежнее состояние: было — пишем обратно, не было —
/// удаляем.
pub fn restore(path: &str, name: &str, was: &Option<Value>) {
    match was {
        Some(v) => {
            let _ = write(path, name, &v.kind, &v.data);
        }
        None => delete(path, name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const QUERY: &str = "\r\nHKEY_LOCAL_MACHINE\\SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion\\Multimedia\\SystemProfile\r\n    NetworkThrottlingIndex    REG_DWORD    0xa\r\n\r\n";

    #[test]
    fn reads_type_and_data() {
        let v = parse(QUERY, "NetworkThrottlingIndex").unwrap();
        assert_eq!(v.kind, "REG_DWORD");
        assert_eq!(v.data, "0xa");
    }

    #[test]
    fn a_longer_name_is_not_a_match() {
        // Иначе снимок «как было» для NetworkThrottlingIndex мог бы уехать
        // из чужого значения, и откат записал бы не туда.
        let text = "    NetworkThrottlingIndexBackup    REG_DWORD    0x1\r\n";
        assert!(parse(text, "NetworkThrottlingIndex").is_none());
    }

    #[test]
    fn string_data_keeps_its_spaces() {
        let text = "    Do not use NLA    REG_SZ    1\r\n";
        assert_eq!(parse(text, "Do not use NLA").unwrap().data, "1");
    }
}
