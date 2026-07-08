# Firmware для Lumen et Signum

Эта папка содержит companion firmware для классического ESP32. Firmware принимает UART-команды от desktop-приложения Lumen et Signum и возвращает ответы формата `ok ...` или `err ...`.

## Возможности firmware

- `status`, `ping`, `help`, `caps`.
- Управление встроенным LED: `led on`, `led off`, `led toggle`.
- Blink и heartbeat.
- GPIO mode/read/write.
- PWM с реальным LEDC-драйвером.
- ADC readings.
- Сохранение программ: `save`, `run`, `delete`, `programs`.
- Autorun и boot-сценарии.
- Persist в flash storage.
- Timers: `timer ... every/after ... do { ... }`.
- Events: `on pin ... do { ... }` с debounce.
- Board-side переменные `let` и простые условия.
- Финальные script markers `ok run_done ...` и `ok boot_done ...` для strict-режима desktop-приложения.

Не все будущие bus-команды включены в `caps`: `i2c`, `spi`, auxiliary `uart` и `wifi` существуют как reserved handlers, но пока возвращают `*_driver_unimplemented` и не объявляются как поддержанные capabilities.

## Требования

- ESP Rust toolchain, устанавливается через `espup`.
- `espflash` в `PATH`.
- Классический ESP32 target `xtensa-esp32-none-elf`.
- Плата ESP32, подключенная по USB.

Файл `rust-toolchain.toml` выбирает channel `esp`, а `.cargo/config.toml` задает target и runner:

```toml
target = "xtensa-esp32-none-elf"
runner = "espflash flash --monitor --chip esp32"
```

## Сборка

```powershell
cd firmware
cargo build
```

Release-сборка:

```powershell
cargo build --release
```

## Прошивка платы

```powershell
cargo run --release
```

Из-за runner в `.cargo/config.toml` команда соберет firmware, прошьет ESP32 через `espflash` и откроет monitor.

Если нужно явно указать порт, используй `espflash` напрямую после сборки или настрой окружение под свою плату.

## Host-тесты

По умолчанию crate собирается под Xtensa target. Для тестов parser/protocol-кода на Windows добавлен alias:

```powershell
cargo +stable test-host
```

Alias разворачивается в:

```text
cargo +stable test --lib --target x86_64-pc-windows-msvc
```

## UART-протокол

Команда - одна текстовая строка. Несколько команд в script-body разделяются `;`.

Успешный ответ начинается с `ok`:

```text
ok pong
ok led=on blink_ms=0
ok save blink bytes=28 changed=1
```

Ошибка начинается с `err`:

```text
err unknown_command
err pin_reserved_uart0
err program_not_found
```

`caps` возвращает имя firmware, версию, protocol и feature-list:

```text
ok caps name=esp32-rust-fw version=0.1.0 protocol=1 features=status,ping,help,caps,vars,programs,save,run,delete,autorun,persist,persist_slots,persist_clear,safe_boot,reboot,reset_reason,boot,led,blink,heartbeat,echo,pin,pwm,pwm_real,adc,adc_samples,on_pin,on_pin_debounce,timer,timer_do,sleep,repeat,board_if,let,script_budget,script_done
```

Desktop-приложение использует этот список, чтобы проверять `requires(...)` и совместимость EspScript до отправки команд.

## Примеры команд

```text
ping
status
caps
led toggle
blink 500
heartbeat on
pin 4 mode output
pin 4 write on
pin 4 read
pwm 5 freq 1000 duty 512
pwm 5 stop
adc 34 max 3.3
save pulse { led toggle; sleep 100; led toggle }
run pulse
autorun pulse
boot
timer 0 every 1000 do { led toggle }
on pin 0 falling debounce 30 do { led toggle }
```

## Ограничения

- Имя программы: до 16 ASCII-символов.
- Board-side переменные: до 16 переменных, имя до 16 ASCII-символов.
- Script-body и timer-body ограничены внутренними fixed-size buffers.
- GPIO6..GPIO11 заняты SPI flash.
- GPIO1/GPIO3 используются UART0 monitor.
- GPIO34..GPIO39 доступны только как inputs.

## Связь с desktop-приложением

Рекомендуемый workflow:

1. Прошей firmware через `cargo run --release`.
2. Открой Lumen et Signum из корня проекта через `cargo run`.
3. Выбери COM-порт и открой monitor.
4. Нажми `Проверить firmware caps`.
5. Запускай команды или EspScript из desktop-приложения.
