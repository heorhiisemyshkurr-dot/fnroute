//! fnroute — маршрутизатор трафика Fortnite для одной машины.
//!
//! Программа делает то же, что делает игровой роутер, только средствами
//! самой Windows: выбирает, через какой канал выпускать игру, помечает её
//! трафик как трафик реального времени и не даёт качалкам занять исходящую
//! очередь. Всё, что она меняет, записано вместе с прежним значением и
//! снимается командой `revert`.
//!
//! Порядок команд не случаен и повторяет порядок причин. Сначала `probe` —
//! потому что регион матчмейкинга стоит сотни миллисекунд, а всё остальное
//! единицы. Потом `diag` — потому что задержку, набежавшую у провайдера,
//! бессмысленно лечить ключами реестра. И только потом `apply`.

mod diag;
mod game;
mod iface;
mod icmp;
mod qos;
mod reg;
mod regions;
mod state;
mod sys;
mod tcp;
mod tune;

use icmp::{Icmp, Sample};
use std::net::Ipv4Addr;
use std::time::Duration;

fn main() {
    sys::console_utf8();
    let args: Vec<String> = std::env::args().skip(1).collect();

    // Без аргументов — значит, запустили двойным кликом, а не из терминала.
    // Печатать справку и закрываться в этом случае бессмысленно: окно
    // исчезает раньше, чем её успевают прочитать.
    if args.is_empty() {
        menu();
        return;
    }

    let command = args.first().map(String::as_str).unwrap_or("help");
    let opts = Options::parse(&args);

    let result = match command {
        "status" => cmd_status(),
        "probe" => cmd_probe(&opts),
        "diag" => cmd_diag(&opts),
        "apply" => cmd_apply(&opts),
        "revert" => cmd_revert(),
        "run" => cmd_run(&opts),
        "pin" => cmd_pin(&opts),
        "unpin" => cmd_unpin(),
        "help" | "--help" | "-h" | "/?" => {
            help();
            Ok(())
        }
        other => Err(format!(
            "Неизвестная команда «{other}». Список команд — fnroute help."
        )),
    };

    if let Err(e) = result {
        eprintln!("\n  Ошибка: {e}");
        if opts.hold {
            hold();
        }
        std::process::exit(1);
    }
    if opts.hold {
        hold();
    }
}

/// Держит окно открытым до нажатия Enter.
///
/// Нужно только тому окну, которое программа открыла сама — второму, уже с
/// правами администратора. Оно закрылось бы вместе с концом работы, и
/// человек не увидел бы ни отчёта, ни ошибки.
fn hold() {
    println!("\n  Нажми Enter, чтобы закрыть окно.");
    let mut line = String::new();
    let _ = std::io::stdin().read_line(&mut line);
}

// ── Меню ─────────────────────────────────────────────────────────────────

/// Текст меню отдельной константой, а не строкой внутри println.
///
/// Причина техническая: перенос строки через `\` в Rust съедает не только
/// сам перенос, но и отступ следующей строки — рамка меню разъезжается, и
/// заметно это только в живом окне. Здесь строка написана ровно так, как
/// выводится.
const MENU: &str = "
  fnroute — маршрутизатор трафика Fortnite

    1  Замерить регионы и выбрать лучший
    2  Разобрать, где набирается задержка
    3  Показать, что сейчас в системе

    4  Применить настройки            (нужен администратор)
    5  Фоновый режим на время матча   (нужен администратор)
    6  Вернуть всё как было           (нужен администратор)

    0  Выход";

/// Что предлагаем при запуске двойным кликом.
///
/// Порядок тот же, что у команд: сначала замеры, потом настройки. Пункты,
/// меняющие систему, помечены — человек должен видеть разницу до нажатия, а
/// не после.
fn menu() {
    loop {
        println!("{MENU}");
        let choice = ask("\n  Номер: ");
        let args: Vec<String> = match choice.trim() {
            "1" => vec!["probe".into()],
            "2" => vec!["diag".into()],
            "3" => vec!["status".into()],
            "4" => vec!["apply".into()],
            "5" => {
                let mbit = ask(
                    "\n  Сколько Мбит/с оставить качалкам на время матча?\n  \
                     Ориентир — примерно половина исходящей скорости канала: ",
                );
                match mbit.trim().parse::<u32>() {
                    Ok(n) if n > 0 => vec!["run".into(), "--throttle".into(), n.to_string()],
                    _ => {
                        println!("  Это должно быть число больше нуля.");
                        continue;
                    }
                }
            }
            "6" => vec!["revert".into()],
            "0" | "" => return,
            other => {
                println!("  Нет такого пункта: «{other}».");
                continue;
            }
        };

        // Пункты, меняющие систему, без прав администратора выполнить
        // нельзя — но и упираться в отказ незачем: перезапускаем себя же с
        // запросом прав, и Windows покажет привычное окно согласия.
        let needs_admin = matches!(args[0].as_str(), "apply" | "run" | "revert");
        if needs_admin && !sys::is_elevated() {
            match relaunch_elevated(&args) {
                Ok(_) => println!(
                    "\n  Открыл второе окно с правами администратора — работа идёт там."
                ),
                Err(e) => println!("\n  Не вышло получить права: {e}"),
            }
            continue;
        }

        let opts = Options::parse(&args);
        let result = match args[0].as_str() {
            "probe" => cmd_probe(&opts),
            "diag" => cmd_diag(&opts),
            "status" => cmd_status(),
            "apply" => cmd_apply(&opts),
            "run" => cmd_run(&opts),
            "revert" => cmd_revert(),
            _ => Ok(()),
        };
        if let Err(e) = result {
            println!("\n  Ошибка: {e}");
        }
        println!("\n  ──────────────────────────────────────────────");
    }
}

fn ask(prompt: &str) -> String {
    use std::io::Write;
    print!("{prompt}");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    let _ = std::io::stdin().read_line(&mut line);
    line
}

/// Перезапускает себя же с правами администратора.
///
/// Через Start-Process -Verb RunAs, а не через ShellExecuteExW: вызов из
/// PowerShell — одна строка вместо ручного описания структуры на два десятка
/// полей, а окно согласия Windows показывает одно и то же в обоих случаях.
///
/// К аргументам добавляется --hold: у нового окна свой сеанс консоли, и без
/// задержки оно закроется вместе с концом работы, унеся отчёт с собой.
fn relaunch_elevated(args: &[String]) -> Result<(), String> {
    let exe = std::env::current_exe()
        .map_err(|e| format!("не найти собственный путь: {e}"))?
        .display()
        .to_string();
    let list: Vec<String> = args
        .iter()
        .cloned()
        .chain(std::iter::once("--hold".to_string()))
        // Одинарные кавычки внутри пути ломали бы строку скрипта; в
        // PowerShell они удваиваются.
        .map(|a| format!("'{}'", a.replace('\'', "''")))
        .collect();
    sys::powershell(&format!(
        "Start-Process -FilePath '{}' -ArgumentList {} -Verb RunAs",
        exe.replace('\'', "''"),
        list.join(",")
    ))
    .map(|_| ())
}

fn help() {
    println!(
        "\
fnroute — маршрутизатор трафика Fortnite

  fnroute probe            замерить все регионы матчмейкинга и выбрать лучший
  fnroute diag             разобрать, где именно набирается задержка
  fnroute status           что сейчас в системе и что из этого поставили мы
  fnroute apply            применить: приоритет игре, выбор канала, MTU, стек
  fnroute run              фоновый режим: включать профиль на время матча
  fnroute revert           вернуть всё, как было
  fnroute pin <адрес>      прибить адрес к выбранному интерфейсу
  fnroute unpin            снять прибитые адреса

Ключи:
  --region EU|NAE|NAW|BR|ME|ASIA|OCE   не искать регион, а взять этот
  --iface <номер>                      не выбирать канал, а взять этот
  --throttle <Мбит/с>                  потолок скорости для качалок на время матча
  --quick                              короткие замеры (меньше точности, быстрее)

Команды probe, diag и status ничего не меняют и прав администратора не
требуют. Меняют систему только apply, run, pin и unpin.

Запуск без команды (двойным кликом по файлу) открывает меню — права
администратора программа запросит сама, когда они понадобятся."
    );
}

// ── Аргументы ────────────────────────────────────────────────────────────

#[derive(Default)]
struct Options {
    region: Option<String>,
    iface: Option<u32>,
    throttle: Option<u32>,
    quick: bool,
    /// Дождаться Enter перед выходом. Ставится только тогда, когда программа
    /// сама открыла себе окно с правами администратора.
    hold: bool,
    /// Первый позиционный аргумент после команды — адрес для `pin`.
    target: Option<String>,
}

impl Options {
    fn parse(args: &[String]) -> Self {
        let mut o = Options::default();
        let mut rest = args.iter().skip(1);
        while let Some(arg) = rest.next() {
            match arg.as_str() {
                "--region" => o.region = rest.next().cloned(),
                "--iface" => o.iface = rest.next().and_then(|v| v.parse().ok()),
                "--throttle" => o.throttle = rest.next().and_then(|v| v.parse().ok()),
                "--quick" => o.quick = true,
                "--hold" => o.hold = true,
                other if !other.starts_with('-') && o.target.is_none() => {
                    o.target = Some(other.to_string());
                }
                _ => {}
            }
        }
        o
    }
}

// ── Общие куски ──────────────────────────────────────────────────────────

fn show(sample: &Sample) -> String {
    if !sample.ok() {
        return "нет ответа".into();
    }
    format!(
        "{:>5.0} мс · джиттер {:>4.1} · потери {}%",
        sample.avg, sample.jitter, sample.loss_pct
    )
}

/// Интерфейсы, которые вообще могут быть выходом в интернет.
fn uplinks(list: &[iface::Iface]) -> Vec<&iface::Iface> {
    list.iter().filter(|i| i.routable()).collect()
}

/// Находит регион: либо взяли из аргументов, либо замерили.
///
/// Возвращает замер целиком, а не только цифры: в нём лежит адрес точки,
/// которая ответила лучше всех, а трассировать и искать MTU надо именно до
/// неё — до той, куда пойдёт игра, а не до первой в списке.
fn choose_region(opts: &Options) -> Result<regions::Measured, String> {
    if let Some(id) = &opts.region {
        let region = regions::by_id(id)
            .ok_or_else(|| format!("Регион «{id}» неизвестен. Список — в fnroute help."))?;
        return Ok(regions::measure(region, None));
    }
    println!("  Меряю регионы матчмейкинга…");
    let measured = regions::measure_all(None, &|title| println!("    {title}…"));
    regions::best(&measured)
        .cloned()
        .ok_or_else(|| "Ни один регион не ответил — похоже, интернета сейчас нет.".to_string())
}

// ── status ───────────────────────────────────────────────────────────────

fn cmd_status() -> Result<(), String> {
    let list = iface::list()?;
    let active = iface::active(&list);

    println!("\n  Интерфейсы\n");
    for i in &list {
        let mark = if Some(i.index) == active.map(|a| a.index) {
            "→"
        } else {
            " "
        };
        println!(
            "  {mark} [{:>3}] {:<24} {:<12} {:<15} метрика {:<5} MTU {:<5} {}",
            i.index,
            truncate(&i.alias, 24),
            i.kind(),
            i.ip,
            if i.automatic_metric {
                format!("{}*", i.metric)
            } else {
                i.metric.to_string()
            },
            i.mtu,
            // Скорость до роутера, а не до провайдера, — но 100 Мбит/с на
            // гигабитной карте означают севшую обжимку или старый кабель, и
            // это видно только здесь.
            i.speed
        );
    }
    println!("\n  → тот, через который система сейчас ходит в интернет; * метрику считает Windows");

    let defaults = iface::default_routes();
    if defaults.len() > 1 {
        println!(
            "\n  Маршрутов по умолчанию: {}. Windows выберет тот, у которого метрика меньше —\n  \
             если это туннель VPN, трафик игры уйдёт в него.",
            defaults.len()
        );
    }

    println!(
        "\n  Fortnite: {}",
        if game::in_match() {
            "запущена"
        } else {
            "не запущена"
        }
    );
    let hogs = game::running_hogs();
    if !hogs.is_empty() {
        println!("  Сейчас качают канал: {}", hogs.join(", "));
    }

    let saved = state::load();
    println!("\n  Наши настройки");
    if !saved.applied() {
        println!("  Не применялись. Изменить систему — fnroute apply.");
        return Ok(());
    }
    if let Some(region) = &saved.region {
        println!("  Регион, под который настраивали: {region}");
    }
    if let Some(uplink) = &saved.uplink {
        println!("  Канал для игры: {uplink}");
    }
    for r in &saved.registry {
        println!("  · реестр {} — {}", r.name, r.why);
    }
    for m in &saved.metrics {
        println!(
            "  · метрика «{}»: было {}",
            m.alias,
            if m.was_automatic {
                "автоматически".to_string()
            } else {
                m.was.to_string()
            }
        );
    }
    for m in &saved.mtu {
        println!("  · MTU «{}»: {} → {}", m.alias, m.was, m.now);
    }
    if !saved.qos.is_empty() {
        println!("  · политик QoS: {}", saved.qos.len());
    }
    for r in &saved.routes {
        println!("  · прибитый адрес: {r}");
    }
    println!("\n  Вернуть всё как было — fnroute revert.");
    Ok(())
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    text.chars().take(max.saturating_sub(1)).collect::<String>() + "…"
}

// ── probe ────────────────────────────────────────────────────────────────

fn cmd_probe(_opts: &Options) -> Result<(), String> {
    println!("\n  Меряю регионы матчмейкинга Fortnite.");
    println!("  Это время до дата-центров, в которых поднимаются серверы, — не пинг в матче,");
    println!("  но разница между регионами видна именно здесь.\n");

    let measured = regions::measure_all(None, &|title| println!("    {title}…"));

    let mut sorted: Vec<&regions::Measured> = measured.iter().collect();
    sorted.sort_by(|a, b| {
        a.sample
            .score()
            .partial_cmp(&b.sample.score())
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    println!("\n  Регион                       результат");
    for m in &sorted {
        println!(
            "  {:<6} {:<20} {}",
            m.region.id,
            truncate(m.region.title, 20),
            show(&m.sample)
        );
    }

    let Some(best) = regions::best(&measured) else {
        return Err("Ни один регион не ответил — похоже, интернета сейчас нет.".into());
    };
    println!(
        "\n  Лучший регион: {} ({}), {}",
        best.region.id,
        best.region.title,
        show(&best.sample)
    );
    println!("  Поставь его в игре: Настройки → Игра → Регион матчмейкинга.");

    // Второй по счёту — чтобы человек видел цену выбора. Если разница
    // невелика, менять регион не стоит: в соседнем дольше ищется матч.
    if let Some(second) = sorted.get(1).filter(|m| m.sample.ok()) {
        let gap = second.sample.avg - best.sample.avg;
        if gap < regions::WORTH_SWITCHING_MS {
            println!(
                "  До следующего ({}) всего {gap:.0} мс — разница в пределах шума,\n  \
                 выбирай тот, где быстрее находится матч.",
                second.region.id
            );
        }
    }

    // Второй интерфейс — второй ответ на тот же вопрос. Меряем лучший
    // регион с каждого канала: это и есть выбор, который делает роутер.
    let list = iface::list()?;
    let ups = uplinks(&list);
    if ups.len() > 1 {
        println!("\n  Каналов в системе больше одного — меряю {} с каждого:\n", best.region.id);
        for up in &ups {
            let m = regions::measure(best.region, Some(up.ip));
            println!(
                "  [{:>3}] {:<24} {:<12} {}",
                up.index,
                truncate(&up.alias, 24),
                up.kind(),
                show(&m.sample)
            );
        }
        println!("\n  Закрепить лучший — fnroute apply --iface <номер>.");
    }
    Ok(())
}

// ── diag ─────────────────────────────────────────────────────────────────

fn cmd_diag(opts: &Options) -> Result<(), String> {
    let list = iface::list()?;
    let active = iface::active(&list).ok_or("Нет интерфейса со шлюзом — интернета нет.")?;
    println!("\n  Канал: «{}» ({}), MTU {}", active.alias, active.kind(), active.mtu);

    let chosen = choose_region(opts)?;
    println!(
        "\n  Регион {} ({}): {}",
        chosen.region.id,
        chosen.region.title,
        show(&chosen.sample)
    );

    let icmp = Icmp::open()?;

    // Трассировка до дата-центра региона. Последний узел не ответит — в
    // сетях Amazon эхо не пропускают, — но весь путь до их края виден, и
    // задержка набирается как раз там.
    let anchor = chosen
        .anchor
        .ok_or_else(|| format!("До региона {} не достучались.", chosen.region.id))?;
    println!("\n  Трассирую до {} ({anchor})…\n", chosen.anchor_host);
    let hops = icmp::trace(&icmp, anchor, if opts.quick { 15 } else { 20 }, Duration::from_millis(900));
    // Разбираем до печати, чтобы отметить виноватый прыжок прямо в таблице:
    // иначе человеку пришлось бы искать его глазами по номеру из вердикта.
    let verdict = diag::analyse(&hops);
    for hop in &hops {
        let guilty = verdict.hop == Some(hop.number);
        println!(
            "  {} {:>2}  {:<16} {}{}",
            if guilty { "→" } else { " " },
            hop.number,
            hop.ip.map(|i| i.to_string()).unwrap_or_else(|| "—".into()),
            match hop.ms {
                Some(ms) => format!("{ms:>6.1} мс{}", if hop.private { "  (серый адрес)" } else { "" }),
                None => "     нет ответа".into(),
            },
            if guilty {
                format!("   +{:.0} мс здесь", verdict.jump)
            } else {
                String::new()
            }
        );
    }

    println!("\n  {}", verdict.text);
    // Что из этого умеет программа — говорим сразу: половина смысла разбора
    // в том, чтобы не обещать починить чужой транзитный узел.
    println!(
        "  {}",
        match verdict.blame {
            diag::Blame::Home =>
                "Это как раз то, что fnroute apply чинит: потолок для качалок, приоритет игре, MTU.",
            diag::Blame::Provider =>
                "Программа тут бессильна: до этого узла её настройки не доходят.",
            diag::Blame::Transit =>
                "Программа тут бессильна: попробуй соседний регион — fnroute probe покажет цену.",
            diag::Blame::Distance =>
                "Узкого места нет — значит, и чинить нечего; остаётся выбор региона.",
            diag::Blame::Unknown => "Разбор без маршрута неполон: смотри замеры ниже.",
        }
    );

    // Всё, что дальше, требует эха, а дата-центр на него не отвечает —
    // поэтому меряем до публичного резолвера: MTU и длина исходящей очереди
    // это свойства первого участка пути, общего для любого направления.
    if let Some(anchor) = diag::icmp_anchor(&icmp) {
        if let Some((small, large)) = diag::uplink_delay(&icmp, anchor) {
            let delta = large - small;
            println!(
                "\n  Пакет 32 байта — {small:.1} мс, 1400 байт — {large:.1} мс (разница {delta:.1})."
            );
            if delta >= diag::UPLINK_DELAY_BAD_MS {
                println!(
                    "  Разница великовата: исходящая очередь длинная. Любая фоновая загрузка\n  \
                     будет ронять пинг в матче — это лечится потолком для качалок\n  \
                     (fnroute apply --throttle <Мбит/с>)."
                );
            } else {
                println!("  Очередь на аплинке короткая — фон пинг не топит.");
            }
        }
        if !opts.quick {
            match icmp::probe_mtu(&icmp, anchor, Duration::from_millis(1200)) {
                Some(mtu) if mtu < active.mtu => println!(
                    "\n  MTU канала {mtu}, а интерфейс настроен на {}. Пакеты режутся по дороге —\n  \
                     это чинит fnroute apply.",
                    active.mtu
                ),
                Some(mtu) => println!("\n  MTU канала {mtu} — настройка интерфейса ему соответствует."),
                None => println!("\n  MTU измерить не вышло: опорные адреса не отвечают на эхо."),
            }
        }
    }

    if active.is_wifi() {
        println!(
            "\n  Игра идёт по Wi-Fi. Это главная причина скачущего пинга, и её не лечит\n  \
             ни один ключ реестра: эфир делится с соседями и микроволновкой. Кабель\n  \
             обычно снимает половину джиттера."
        );
    }
    let hogs = game::running_hogs();
    if !hogs.is_empty() {
        println!("\n  Прямо сейчас канал качают: {}.", hogs.join(", "));
    }
    Ok(())
}

// ── apply ────────────────────────────────────────────────────────────────

fn cmd_apply(opts: &Options) -> Result<(), String> {
    sys::require_admin()?;
    let list = iface::list()?;
    let ups = uplinks(&list);
    if ups.is_empty() {
        return Err("Нет ни одного интерфейса со шлюзом — настраивать нечего.".into());
    }

    let chosen = choose_region(opts)?;
    let region = chosen.region;

    // Выбор канала — та самая работа маршрутизатора. При одном проводе
    // выбора нет, и делать вид, что он есть, незачем.
    let uplink: &iface::Iface = match opts.iface {
        Some(index) => ups
            .iter()
            .find(|i| i.index == index)
            .copied()
            .ok_or_else(|| format!("Интерфейса с номером {index} среди выходов в интернет нет."))?,
        None if ups.len() == 1 => ups[0],
        None => {
            println!("\n  Каналов больше одного — меряю {} с каждого:", region.id);
            let mut best: Option<(&iface::Iface, Sample)> = None;
            for up in &ups {
                let m = regions::measure(region, Some(up.ip));
                println!(
                    "    [{:>3}] {:<20} {}",
                    up.index,
                    truncate(&up.alias, 20),
                    show(&m.sample)
                );
                if best
                    .as_ref()
                    .map_or(true, |(_, prev)| m.sample.score() < prev.score())
                {
                    best = Some((up, m.sample));
                }
            }
            best.map(|(i, _)| i)
                .ok_or("Ни один канал не ответил — интернета нет.")?
        }
    };

    let mut saved = state::load();
    if saved.applied() {
        // Поверх уже применённого — сначала снимаем: иначе снимок «как
        // было» пополнится нашими же значениями, и вернуть настоящие
        // прежние станет нечем.
        println!("\n  Прежние настройки ещё стоят — снимаю их перед новыми.");
        revert(&mut saved);
        saved = state::State::default();
    }
    saved.region = Some(region.id.to_string());
    saved.uplink = Some(format!("{} (#{})", uplink.alias, uplink.index));

    println!("\n  Применяю.\n");
    let mut done: Vec<tune::Applied> = Vec::new();

    // 1. Очередь: приоритет игре.
    qos::prepare(&mut saved)?;
    let policies = qos::prioritize_game(&mut saved)?;
    done.push(tune::Applied {
        title: "Приоритет трафику игры",
        detail: format!(
            "{policies} политик QoS, метка DSCP 46 — сработает, если роутер её уважает"
        ),
        weak: true,
    });

    // 2. Потолок качалкам — только если человек назвал число.
    match opts.throttle {
        Some(mbit) => {
            let limited = qos::throttle_hogs(&mut saved, mbit)?;
            done.push(tune::Applied {
                title: "Потолок для качалок",
                detail: if limited.is_empty() {
                    format!("{mbit} Мбит/с — ни одна из известных качалок не установлена")
                } else {
                    format!("{mbit} Мбит/с: {}", limited.join(", "))
                },
                weak: false,
            });
        }
        None => println!(
            "  Потолок для качалок не ставлю: ширину канала за тебя не угадать.\n  \
             Если пинг в матче прыгает при загрузках — fnroute apply --throttle <Мбит/с>.\n"
        ),
    }

    // 3. Стек.
    done.extend(tune::system(&mut saved)?);
    done.extend(tune::interface(&mut saved, uplink)?);

    // 4. MTU — по измерению, а не по списку «поставь 1472».
    if let Ok(icmp) = Icmp::open() {
        if let Some(anchor) = diag::icmp_anchor(&icmp) {
            if let Some(measured) = icmp::probe_mtu(&icmp, anchor, Duration::from_millis(1200)) {
                if let Some(applied) = tune::mtu(&mut saved, uplink, measured) {
                    done.push(applied);
                }
            }
        }
    }

    // 5. Канал: делаем выбранный интерфейс предпочтительным.
    if ups.len() > 1 {
        let lowest = ups.iter().map(|i| i.metric).min().unwrap_or(uplink.metric);
        if uplink.metric > lowest {
            // Метрику ставим на единицу ниже лучшей чужой, а не в ноль:
            // ноль Windows трактует как «считать автоматически», и настройка
            // молча не применилась бы.
            let target = lowest.saturating_sub(1).max(1);
            iface::set_metric(uplink.index, target)?;
            saved.metrics.push(state::MetricWas {
                index: uplink.index,
                alias: uplink.alias.clone(),
                was_automatic: uplink.automatic_metric,
                was: uplink.metric,
            });
            done.push(tune::Applied {
                title: "Канал для игры",
                detail: format!(
                    "«{}» стал предпочтительным (метрика {} → {target})",
                    uplink.alias, uplink.metric
                ),
                weak: false,
            });
        }
    }

    saved.stamp();
    state::save(&saved)?;

    for a in &done {
        println!(
            "  {} {:<32} {}",
            if a.weak { "·" } else { "✓" },
            a.title,
            a.detail
        );
    }
    println!("\n  ✓ — рычаг с измеримым эффектом, · — слабый или зависящий от роутера.");
    println!(
        "\n  Регион {} ({}), {}",
        region.id,
        region.title,
        show(&chosen.sample)
    );
    println!("  Поставь его в игре: Настройки → Игра → Регион матчмейкинга.");
    println!("\n  Вернуть всё как было — fnroute revert.");
    println!("  Держать профиль включённым только на время матча — fnroute run.");
    Ok(())
}

// ── revert ───────────────────────────────────────────────────────────────

fn cmd_revert() -> Result<(), String> {
    sys::require_admin()?;
    let mut saved = state::load();
    if !saved.applied() {
        println!("\n  Возвращать нечего: программа ничего не меняла.");
        return Ok(());
    }
    let report = revert(&mut saved);
    state::clear();
    println!("\n  Вернул как было:");
    for line in report {
        println!("  · {line}");
    }
    println!(
        "\n  Часть настроек (профиль планировщика, политики QoS) Windows перечитывает\n  \
         при входе в систему — если что-то ещё видно, перезагрузка добьёт."
    );
    Ok(())
}

/// Снимает всё, что записано в снимке. Возвращает список того, что сняли.
fn revert(saved: &mut state::State) -> Vec<String> {
    let mut report = Vec::new();

    for r in &saved.registry {
        reg::restore(&r.path, &r.name, &r.was);
        report.push(format!(
            "реестр {} — {}",
            r.name,
            match &r.was {
                Some(v) => format!("вернул {}", v.data),
                None => "удалил (его не было)".into(),
            }
        ));
    }

    let policies = qos::remove_all(saved);
    if policies > 0 {
        report.push(format!("политик QoS снято: {policies}"));
    }

    for m in &saved.metrics {
        let ok = if m.was_automatic {
            iface::reset_metric(m.index)
        } else {
            iface::set_metric(m.index, m.was)
        };
        report.push(format!(
            "метрика «{}» — {}",
            m.alias,
            match ok {
                Ok(_) if m.was_automatic => "вернул автоматический расчёт".to_string(),
                Ok(_) => format!("вернул {}", m.was),
                Err(e) => format!("не вышло: {e}"),
            }
        ));
    }

    for m in &saved.mtu {
        match iface::set_mtu(m.index, m.was) {
            Ok(_) => report.push(format!("MTU «{}» — вернул {}", m.alias, m.was)),
            Err(e) => report.push(format!("MTU «{}» — не вышло: {e}", m.alias)),
        }
    }

    for r in &saved.routes {
        if let Ok(ip) = r.parse::<Ipv4Addr>() {
            iface::unpin(ip);
            report.push(format!("снял прибитый маршрут {ip}"));
        }
    }

    saved.registry.clear();
    saved.qos.clear();
    saved.metrics.clear();
    saved.mtu.clear();
    saved.routes.clear();
    saved.applied_at = None;
    report
}

// ── run ──────────────────────────────────────────────────────────────────

/// Фоновый режим: включать потолок для качалок на время матча.
///
/// Постоянный потолок был бы вредом: человек качает обновления не назло себе,
/// а потому что они нужны. Роутер с игровым режимом делает ровно это —
/// придерживает фон, пока идёт игра, и отпускает, когда игра закрыта.
fn cmd_run(opts: &Options) -> Result<(), String> {
    sys::require_admin()?;
    let mbit = opts.throttle.ok_or(
        "Фоновому режиму нужен потолок для качалок: fnroute run --throttle <Мбит/с>.\n\
         Число — сколько оставить фоновым загрузкам, пока идёт матч. Ориентир: \
         примерно половина исходящей скорости канала.",
    )?;

    stop_on_ctrl_c();
    println!("\n  Слежу за Fortnite. Пока идёт матч, качалкам остаётся {mbit} Мбит/с.");
    println!("  Ctrl+C — выйти и снять потолок.\n");

    let mut throttling = false;
    let mut saved = state::load();
    while !should_stop() {
        let playing = game::in_match();
        if playing && !throttling {
            match qos::throttle_hogs(&mut saved, mbit) {
                Ok(limited) => {
                    println!(
                        "  {} матч начался — потолок поставлен{}",
                        now(),
                        if limited.is_empty() {
                            String::new()
                        } else {
                            format!(" ({})", limited.join(", "))
                        }
                    );
                    let _ = state::save(&saved);
                    throttling = true;
                }
                Err(e) => println!("  {} не вышло поставить потолок: {e}", now()),
            }
        } else if !playing && throttling {
            drop_throttles(&mut saved);
            println!("  {} игра закрыта — потолок снят", now());
            throttling = false;
        }
        std::thread::sleep(Duration::from_secs(5));
    }

    if throttling {
        drop_throttles(&mut saved);
        println!("  {} выхожу — потолок снят", now());
    }
    Ok(())
}

/// Снимает только политики потолка, не трогая приоритет игры: он поставлен
/// командой apply и живёт до revert.
fn drop_throttles(saved: &mut state::State) {
    let hogs: Vec<String> = saved
        .qos
        .iter()
        .filter(|n| n.starts_with(&format!("{}hog-", qos::PREFIX)))
        .cloned()
        .collect();
    for name in &hogs {
        qos::remove(name);
    }
    saved.qos.retain(|n| !hogs.contains(n));
    let _ = state::save(saved);
}

fn now() -> String {
    // Часы без даты: строка в журнале нужна, чтобы соотнести событие с
    // матчем, а не чтобы вести архив.
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("[{:02}:{:02}:{:02}]", secs / 3600 % 24, secs / 60 % 60, secs % 60)
}

// ── pin / unpin ──────────────────────────────────────────────────────────

fn cmd_pin(opts: &Options) -> Result<(), String> {
    sys::require_admin()?;
    let raw = opts
        .target
        .as_deref()
        .ok_or("Укажи адрес: fnroute pin 51.83.128.101")?;
    // Адрес сервера копируют вместе с портом — маршрут о нём не знает.
    let host = raw.split(':').next().unwrap_or(raw);
    let dest: Ipv4Addr = host
        .parse()
        .or_else(|_| tcp::resolve(host).ok_or(()))
        .map_err(|_| format!("«{raw}» не похоже на адрес IPv4."))?;

    let list = iface::list()?;
    let ups = uplinks(&list);
    let target = match opts.iface {
        Some(index) => ups.iter().find(|i| i.index == index).copied(),
        None => iface::active(&list),
    }
    .ok_or("Не нашёл интерфейс, через который вести маршрут.")?;
    let gateway = target
        .gateway
        .ok_or_else(|| format!("У «{}» нет шлюза — маршрут вести не через что.", target.alias))?;

    iface::pin(dest, gateway, target.index, 1)?;
    let mut saved = state::load();
    saved.routes.push(dest.to_string());
    saved.stamp();
    state::save(&saved)?;
    println!(
        "\n  {dest} теперь идёт через «{}» ({gateway}).\n  Снять — fnroute unpin.",
        target.alias
    );
    Ok(())
}

fn cmd_unpin() -> Result<(), String> {
    sys::require_admin()?;
    let mut saved = state::load();
    if saved.routes.is_empty() {
        println!("\n  Прибитых адресов нет.");
        return Ok(());
    }
    for r in &saved.routes {
        if let Ok(ip) = r.parse::<Ipv4Addr>() {
            iface::unpin(ip);
            println!("  снял {ip}");
        }
    }
    saved.routes.clear();
    state::save(&saved)?;
    Ok(())
}

// ── Ctrl+C ───────────────────────────────────────────────────────────────

use std::sync::atomic::{AtomicBool, Ordering};
static STOP: AtomicBool = AtomicBool::new(false);

fn should_stop() -> bool {
    STOP.load(Ordering::Relaxed)
}

#[cfg(target_os = "windows")]
mod ctrlc {
    use super::{Ordering, STOP};

    #[link(name = "kernel32")]
    extern "system" {
        fn SetConsoleCtrlHandler(
            handler: Option<unsafe extern "system" fn(u32) -> i32>,
            add: i32,
        ) -> i32;
    }

    unsafe extern "system" fn handler(_event: u32) -> i32 {
        // Возвращаем «обработано», чтобы Windows не убила процесс сразу:
        // иначе потолок для качалок остался бы стоять после выхода, и
        // человек полдня гадал бы, почему у него медленно качается.
        STOP.store(true, Ordering::Relaxed);
        1
    }

    pub fn install() {
        unsafe {
            SetConsoleCtrlHandler(Some(handler), 1);
        }
    }
}

#[cfg(target_os = "windows")]
fn stop_on_ctrl_c() {
    ctrlc::install();
}

#[cfg(not(target_os = "windows"))]
fn stop_on_ctrl_c() {}
