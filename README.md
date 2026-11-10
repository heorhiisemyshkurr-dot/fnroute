# fnroute — a traffic router for Fortnite

*Русская версия: [README.ru.md](README.ru.md)*

One small program that does on a single PC what a gaming router does on a
network: it picks which link the game goes out through, marks its traffic as
real-time, and keeps downloads from filling the upload queue.

All of it through Windows itself — **no drivers, no packet interception,
nothing that Easy Anti-Cheat could object to.** That constraint shaped the
whole design: everything is done with routing tables, the registry and the
built-in QoS policy, so the program never touches the game process.

```
cargo build --release --manifest-path fnroute/Cargo.toml
```

The result is a single `fnroute.exe` with no dependencies.

## Order of commands

The order follows the order of causes, not the order of buttons:

```
fnroute probe     # matchmaking region — this is hundreds of milliseconds
fnroute diag      # where the latency is actually added, and whether it can be fixed at all
fnroute apply     # and only now, settings
```

There is no point tuning QoS if the match is hosted on another continent.
`probe` answers that first; `diag` shows which hop adds the delay; only then
does `apply` change anything.

`probe`, `diag` and `status` change nothing and need no administrator
rights. `apply`, `run`, `pin` and `unpin` do need them — Windows will not
hand over routes, the registry or QoS otherwise. The program asks for
elevation itself, when the chosen action needs it.

Double-clicking the file opens a menu: it is a console program, and without
a menu the window would close right after printing help.

## What it actually changes

- chooses the outgoing interface when the machine has more than one
- writes a QoS policy that marks the game's packets as real-time (DSCP)
- limits the upload queue so a background download cannot starve the game
- pins or unpins a route, and can undo everything it did

Written in Rust. Windows only.
