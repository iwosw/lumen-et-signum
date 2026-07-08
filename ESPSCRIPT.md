# EspScript для Lumen et Signum

EspScript - небольшой язык для `lumen-et-signum`. Приложение компилирует его в UART-команды для ESP32-прошивки и отдельно выполняет host-шаги вроде ожиданий и проверок ответа.

## Быстрый Пример

```rust
let pulses: number = 3;
let delay: ms = ms(250);

fn pulse(times: number = 3, delay: ms = ms(250)) {
    repeat(times: times) {
        led.toggle();
        wait(ms: delay);
    }
}

status();
expect(text: "ok status", timeout_ms: ms(1000));
pulse(times: pulses, delay: delay);
```

## Основы

- Команды завершаются `;`.
- Комментарии: `// comment` или `# comment`.
- Значения: идентификаторы, числа, booleans, строки и типизированные единицы.
- Единицы: `ms(250)`, `hz(400000)`, `volt(3.3)`, `pin(21)`.
- `let` - compile-time переменная на стороне desktop-приложения; можно писать `let delay: ms = ms(250);`.
- `fn` - compile-time макрос с именованными параметрами и значениями по умолчанию; можно писать `fn pulse(times: number = 3)`.
- `repeat(times: N) { ... }` на верхнем уровне раскрывается desktop-компилятором.
- `repeat(...)` внутри `save`, `timer`, `on.button`, `on.pin` и board-side `if` выполняется самой платой.
- `every(...) { ... }` и `after(...) { ... }` - короткий синтаксис для повторяющихся и одноразовых timers на плате.
- `on.button(...) { ... }` - короткий синтаксис для кнопки: `input_pullup`, `falling`, debounce 30 ms по умолчанию.
- `on.boot(...) { ... }` - короткий синтаксис для `save(...)` + `autorun(...)`.
- `board.var(...)` - переменная на стороне прошивки, доступная в board-side `if` и блоках платы.
- `requires(...)` - явное требование к firmware caps; не отправляется на плату.
- Кнопка `Format` в окне скриптов нормализует отступы, блоки, `else`, пробелы в аргументах и сохраняет строки/комментарии.

## Типы

Явные типы необязательны, но помогают поймать ошибки до отправки на плату.

```rust
let delay: ms = ms(250);
let speed: hz = hz(400000);
let led_pin: pin = pin(2);
let message: text = "ready";
let enabled: bool = true;
let pulses: number = 3;

fn pulse(times: number = 3, delay: ms = ms(100)) {
    repeat(times: times) {
        led.toggle();
        wait(ms: delay);
    }
}
```

Доступные типы: `number`, `bool`, `text`, `ms`, `hz`, `volt`, `pin`.

## Host-Шаги

Эти шаги выполняет приложение, не прошивка.

```rust
wait(ms: ms(500));
expect(text: "ok led=on", timeout_ms: ms(1000));

if contains(text: "led=off") {
    led.on();
} else {
    echo(text: "already on");
}
```

`wait`, `expect` и `if contains` нельзя использовать внутри board-блоков.

## Strict-Запуск

В окне скриптов включён режим `Strict ok/err`: после каждой UART-команды приложение ждёт ответ прошивки `ok ...` или `err ...`. При `err ...` или таймауте выполнение останавливается.

`expect(...)` всё ещё нужен для специальных проверок текста. Если `expect(...)` стоит сразу после команды, strict-ответ можно использовать повторно, поэтому старые скрипты с ручными `expect` продолжают работать.

Если прошивка сообщает feature `script_done`, strict-запуск для `run(...)` и `boot()` ждёт финальный marker `ok run_done ...` или `ok boot_done ...`, а не первый промежуточный `ok` из вложенного скрипта.

Ошибки strict-запуска показывают исходную строку EspScript и UART-команду, например: `строка 18: strict timeout: нет ok/err после "led toggle"`.

## Firmware Requirements

`requires(...)` явно описывает возможности прошивки, без которых скрипт нельзя запускать:

```rust
requires(feature: pwm_real);
requires(features: "timer_do,on_pin_debounce");
```

Директива не создаёт UART-команду и видна только в preview как `require`. Если в скрипте есть `requires(...)`, приложение требует сначала получить `caps` от прошивки. Если нужной feature нет в `caps`, запуск блокируется до отправки команд.

## Lint Warnings

Редактор показывает предупреждения, которые не блокируют запуск, но помогают найти ошибки заранее:

```text
warning L4: переменная `delay` объявлена, но не используется
warning L12: функция `pulse` объявлена, но не используется
warning L18: board-переменная `counter` читается до первого board.var(...)
warning L22: timer do-блок 118 из 128 байт, близко к лимиту
```

`cmd(...)` тоже получает warning, потому что raw UART-команда обходит проверки типов, пинов и совместимости.

## Команды Платы

Эти вызовы превращаются в UART-команды, которые выполняет прошивка.

```rust
status();
ping();
caps();
vars();
programs();

led.on();
led.off();
led.toggle();
blink(ms: ms(500));
blink.off();
heartbeat(enabled: true);

pin(id: pin(4), mode: output);
pin.write(id: pin(4), state: on);
pin.read(id: pin(4));

pwm(pin: pin(5), freq: hz(1000), duty: 512);
pwm.stop(pin: pin(5));

adc(pin: pin(34), max: volt(3.3));
i2c(sda: pin(21), scl: pin(22), speed: hz(400000));
spi(sck: pin(18), miso: pin(19), mosi: pin(23), cs: pin(15), speed: hz(1000000));
uart(tx: pin(17), rx: pin(16), baud: hz(115200));
wifi(enabled: false);

sleep(duration: ms(1000));
cmd(text: "raw firmware command");
```

Важно: в текущей `esp32-rust-fw` драйвер `adc` уже читает реальные значения ADC и объявлен в firmware `caps`. Реальные драйверы для `i2c`, `spi`, `uart` и `wifi` ещё не закончены. Эти команды есть в EspScript как будущие аппаратные примитивы, но firmware `caps` не объявляет их поддержанными, поэтому приложение с проверенными caps остановит такой скрипт как несовместимый. Если отправить такую команду вручную по UART, прошивка вернёт `err *_driver_unimplemented` и не будет занимать GPIO.

## Board-Блоки

Board-блок компилируется в одну firmware-команду. Внутри должны быть только команды платы.

```rust
save(name: quick_blink) {
    led.toggle();
    sleep(duration: ms(100));
    led.toggle();
}

run(name: quick_blink);
autorun(name: quick_blink);
autorun.off();
delete(name: quick_blink);
boot();

on.boot() {
    led.on();
    sleep(duration: ms(100));
    led.off();
}

on.boot(name: startup) {
    led.toggle();
}

timer(id: 0, every: ms(1000)) {
    repeat(times: 3) {
        led.toggle();
        sleep(duration: ms(100));
    }
}
timer.stop(id: 0);

every(id: 1, ms: ms(1000)) {
    led.toggle();
}

after(id: 2, ms: ms(5000)) {
    led.off();
}

on.button(id: pin(0)) {
    led.toggle();
}

on.pin(id: pin(0), trigger: falling, debounce: ms(30)) {
    led.toggle();
}
on.pin.off(id: pin(0));
```

## On Boot

`on.boot` сохраняет блок как программу платы и включает её как autorun:

```rust
on.boot() {
    led.on();
    sleep(duration: ms(100));
    led.off();
}

on.boot(name: startup) {
    led.toggle();
}
```

Первый пример эквивалентен:

```text
save boot { led on; sleep 100; led off }
autorun boot
```

Если имя не указано, используется `boot`. Блок выполняется на плате, поэтому внутри нельзя использовать host-шаги `wait`, `expect` и `if contains`.

## Board-Переменные

`let` работает на ПК во время компиляции EspScript. Если нужно состояние на самой плате, используй `board.var(...)`.

```rust
board.var(name: counter, value: 0);

timer(id: 0, every: ms(1000)) {
    board.var(name: counter, value: counter + 1);

    repeat(times: counter) {
        led.toggle();
    }

    if counter >= 3 {
        led.off();
        timer.stop(id: 0);
    }
}
```

`value` принимает число, board-переменную или одно бинарное выражение: `+`, `-`, `*`, `/`, `%`. Прошивка хранит `u64`, максимум 16 переменных, имя до 16 ASCII-символов.

## Every / After

`every` и `after` не требуют отдельной поддержки в прошивке. Компилятор превращает их в обычные timer-команды:

```rust
every(id: 0, ms: ms(1000)) {
    led.toggle();
}

after(id: 1, delay: ms(5000)) {
    led.off();
}
```

Это эквивалентно:

```text
timer 0 every 1000 do { led toggle }
timer 1 after 5000 do { led off }
```

`id` необязателен и по умолчанию равен `0`. Для нескольких timers указывай разные `id` от `0` до `3`.

## Кнопки

`on.button` настраивает GPIO как `input_pullup` и регистрирует обработчик события на плате:

```rust
on.button(id: pin(0)) {
    led.toggle();
}

on.button(id: pin(4), trigger: rising, debounce: ms(50)) {
    led.off();
}
```

Первый пример эквивалентен:

```text
pin 0 mode input_pullup
on pin 0 falling debounce 30 do { led toggle }
```

По умолчанию `trigger: falling` и `debounce: ms(30)`. Для `trigger` доступны `falling`, `rising`, `change`.

## Условия Платы

Board-side `if` компилируется в ветвление на прошивке.

```rust
if led == off {
    led.on();
} else {
    led.off();
}

if pin(id: pin(0)) == on {
    led.toggle();
}

if counter >= 3 {
    led.off();
}
```

## Safety Checks

Компилятор проверяет типичные ошибки ESP32 до отправки команд:

- GPIO должен существовать на классическом ESP32.
- GPIO1/GPIO3 зарезервированы под UART0 serial monitor.
- GPIO6..GPIO11 зарезервированы под SPI flash.
- GPIO34..GPIO39 работают только как входы.
- ADC2 конфликтует с Wi-Fi на ESP32.
- Пины нельзя повторно занимать несовместимыми перифериями в одном скрипте.
- Одна UART-команда должна помещаться в лимит строки прошивки.
- Typed text-аргументы, которые попадают в одну UART-команду (`echo`, `wifi ssid/password`), не могут содержать `;`, `{` или `}`, потому что это разделители script-команд на прошивке. Для осознанной raw-команды остается `cmd(...)` с warning.
- `on.pin(..., debounce: ms(...))` и `on.button(...)` компилируются только для прошивки с `on_pin_debounce`.

Используй UART preview в приложении, чтобы видеть точные команды, которые уйдут на плату.
