# Lumen et Signum

Lumen et Signum - desktop-панель для ESP32: проверка платы, UART-монитор, запуск команд и EspScript, а также совместимая Rust-прошивка в `firmware/`.

Название переводится как "свет и сигнал": проект управляет светодиодом, GPIO, таймерами, событиями и командами платы через простой текстовый протокол.

## Возможности

- Поиск и выбор serial/USB COM-портов.
- Проверка ESP32 через `espflash board-info`.
- Reset платы через `espflash reset`.
- Встроенный UART-монитор с отправкой строк на плату.
- Запрос firmware capabilities через команду `caps`.
- Редактор EspScript с preview UART-команд.
- Strict-режим запуска: после каждой команды ожидается `ok ...` или `err ...`.
- Host-шаги `wait`, `expect`, `if contains` поверх UART-ответов.
- Проверки безопасности для GPIO и совместимости с firmware caps.
- Companion firmware на Rust для классического ESP32.

## Структура

```text
.
├── Cargo.toml          # desktop-приложение Lumen et Signum
├── Cargo.lock
├── README.md           # основная документация проекта
├── ESPSCRIPT.md        # полный справочник языка EspScript
├── src/
│   ├── main.rs         # UI, serial monitor, espflash, запуск скриптов
│   └── espscript.rs    # parser/compiler/formatter EspScript
└── firmware/
    ├── Cargo.toml      # Rust firmware crate для ESP32
    ├── README.md       # сборка и прошивка firmware
    └── src/            # UART-протокол, GPIO, timers, persist, events
```

## Требования

- Rust и Cargo с поддержкой edition 2024.
- `espflash` в `PATH` для проверки платы, reset и прошивки firmware.
- ESP32 DevKit или совместимая плата на классическом ESP32.
- USB-UART драйвер платы, если он не установлен системой автоматически.
- Для firmware: ESP Rust toolchain (`espup`) и target `xtensa-esp32-none-elf`.

## Быстрый старт desktop-приложения

```powershell
git clone https://github.com/iwosw/lumen-et-signum.git
cd lumen-et-signum
cargo run
```

Сборка release-версии:

```powershell
cargo build --release
```

Windows-бинарь после release-сборки:

```text
target\release\lumen-et-signum.exe
```

## Первый запуск с платой

1. Подключи ESP32 по USB.
2. Запусти `cargo run`.
3. Нажми `Обновить`, если порт не появился автоматически.
4. Выбери COM-порт и baud rate, обычно `115200`.
5. Нажми `Проверить плату`, чтобы получить `espflash board-info`.
6. Нажми `Open monitor`, чтобы открыть UART-монитор.
7. Нажми `Проверить firmware caps`, чтобы приложение узнало возможности прошивки.
8. Отправь `status`, `ping`, `led toggle` или открой окно EspScript.

## UART-протокол

Приложение и firmware общаются строками. Команда отправляется как текстовая строка с переводом строки, ответ firmware начинается с `ok` или `err`.

Примеры команд:

```text
ping
status
caps
led on
led off
led toggle
blink 500
heartbeat on
pin 4 mode output
pin 4 write on
adc 34 max 3.3
save blink { led toggle; sleep 100; led toggle }
run blink
timer 0 every 1000 do { led toggle }
on pin 0 falling debounce 30 do { led toggle }
```

Пример `caps`-ответа:

```text
ok caps name=esp32-rust-fw version=0.1.0 protocol=1 features=status,ping,help,caps,vars,programs,save,run,delete,autorun,persist,persist_slots,persist_clear,safe_boot,reboot,reset_reason,boot,led,blink,heartbeat,echo,pin,pwm,pwm_real,adc,adc_samples,on_pin,on_pin_debounce,timer,timer_do,sleep,repeat,board_if,let,script_budget,script_done
```

Приложение использует `caps`, чтобы не запускать скрипты, которые требуют отсутствующих возможностей firmware.

## EspScript

EspScript - DSL поверх UART-протокола. Он нужен, чтобы писать сценарии ближе к коду, а не отправлять руками каждую строку.

Минимальный пример:

```rust
let delay: ms = ms(250);

status();
expect(text: "ok status", timeout_ms: ms(1000));

repeat(times: 3) {
    led.toggle();
    wait(ms: delay);
}
```

Что делает приложение:

- Парсит EspScript на desktop-стороне.
- Раскрывает `let`, `fn`, top-level `repeat`.
- Компилирует команды платы в UART-строки.
- Выполняет `wait`, `expect`, `if contains` на стороне desktop-приложения.
- Проверяет типы `number`, `bool`, `text`, `ms`, `hz`, `volt`, `pin`.
- Проверяет опасные GPIO и несовместимые peripheral-операции.
- Показывает preview команд до отправки.

Полный справочник языка находится в [`ESPSCRIPT.md`](ESPSCRIPT.md).

## Strict-режим

Strict-режим включен по умолчанию в окне EspScript. После каждой UART-команды приложение ждет `ok ...` или `err ...`.

Если firmware возвращает `err ...`, запуск останавливается. Если ответа нет до timeout, запуск тоже останавливается. Для `run(...)` и `boot()` при наличии feature `script_done` приложение ждет финальный marker `ok run_done ...` или `ok boot_done ...`.

## Firmware

Исходники firmware лежат в [`firmware/`](firmware/). Она реализует UART-протокол для ESP32 и объявляет capabilities через `caps`.

Кратко:

```powershell
cd firmware
cargo build
cargo run --release
```

Подробности по установке ESP toolchain, прошивке и host-тестам описаны в [`firmware/README.md`](firmware/README.md).

## Проверки безопасности

Desktop-компилятор EspScript заранее проверяет типичные ошибки:

- GPIO должен существовать на классическом ESP32.
- GPIO1/GPIO3 зарезервированы под UART0 serial monitor.
- GPIO6..GPIO11 зарезервированы под SPI flash.
- GPIO34..GPIO39 работают только как входы.
- ADC2 конфликтует с Wi-Fi на ESP32.
- Пины нельзя повторно занимать несовместимыми peripheral-операциями в одном скрипте.
- UART-команда должна помещаться в лимит строки firmware.
- `requires(...)` блокирует запуск, если firmware caps не подтверждают нужную feature.

## Разработка

Команды для desktop-приложения:

```powershell
cargo fmt
cargo test
cargo run
```

Команды для firmware:

```powershell
cd firmware
cargo fmt
cargo +stable test-host
cargo build
```

Desktop-приложение состоит из двух основных частей:

- `src/main.rs`: egui UI, serial monitor, integration с `espflash`, strict-runner.
- `src/espscript.rs`: parser, type checks, formatter, compiler EspScript в UART-команды.

Firmware состоит из:

- `firmware/src/bin/main.rs`: runtime ESP32, UART handlers, GPIO/PWM/ADC/timers/persistence.
- `firmware/src/protocol.rs`: общие parser-утилиты для блоков, условий и арифметики.

## Troubleshooting

`Не удалось запустить espflash`

Проверь, что `espflash` установлен и доступен в `PATH`.

`Serial monitor не открывается`

Проверь COM-порт, baud rate и закрой другие serial monitor tools, которые могут держать порт.

`caps unsupported`

На плате старая или несовместимая firmware. Обнови firmware из `firmware/` или запускай команды без проверки capabilities на свой риск.

`strict timeout`

Firmware не ответила `ok` или `err` за отведенное время. Проверь baud rate, питание платы и то, что команда поддерживается прошивкой.

`caps mismatch`

Скрипт требует feature, которой нет в `caps`. Убери соответствующую команду или обнови firmware.

## Лицензия

Лицензия пока не выбрана. До добавления LICENSE-файла права остаются за автором репозитория.
