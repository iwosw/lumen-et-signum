use std::collections::{HashMap, HashSet};

#[derive(Clone)]
pub enum ScriptStep {
    Send {
        command: String,
        line: usize,
    },
    Requires {
        features: Vec<String>,
        line: usize,
    },
    Wait {
        ms: u64,
        line: usize,
    },
    Expect {
        text: String,
        timeout_ms: u64,
        line: usize,
    },
    IfContains {
        text: String,
        line: usize,
        then_steps: Vec<ScriptStep>,
        else_steps: Vec<ScriptStep>,
    },
}

impl ScriptStep {
    pub fn line(&self) -> usize {
        match self {
            Self::Send { line, .. }
            | Self::Requires { line, .. }
            | Self::Wait { line, .. }
            | Self::Expect { line, .. }
            | Self::IfContains { line, .. } => *line,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScriptLint {
    pub line: usize,
    pub message: String,
}

pub fn default_script_text() -> String {
    [
        "// EspScript: Rust-like команды превращаются в UART",
        "// Безопасные типы: ms(...), hz(...), volt(...), pin(...)",
        "// Реализовано сейчас: pin, pwm, adc, timer, sleep",
        "// Driver-dependent/future: i2c, spi, uart, wifi (проверяй firmware caps)",
        "// Сценарии на плате: save/run/autorun, on.boot { ... }, every/after { ... }, timer { ... }, on.button/on.pin { ... }, if led == on, repeat",
        "// Явные требования к прошивке: requires(feature: pwm_real);",
        "// Например после реализации I2C: i2c(sda: pin(21), scl: pin(22), speed: hz(400000));",
        "let pulse_delay: ms = ms(250);",
        "let pulses: number = 3;",
        "let blink_delay: ms = ms(500);",
        "let timeout: ms = ms(1000);",
        "let greeting: text = \"hello from EspScript\";",
        "",
        "fn pulse(times: number = 3, delay: ms = ms(250)) {",
        "    repeat(times: times) {",
        "        led(state: toggle);",
        "        wait(ms: delay);",
        "    }",
        "}",
        "",
        "status();",
        "expect(text: \"ok status\", timeout_ms: timeout);",
        "",
        "if contains(text: \"led=off\") {",
        "    led(state: on);",
        "    expect(text: \"ok led=on\", timeout_ms: timeout);",
        "} else {",
        "    echo(text: \"led was already active\");",
        "    expect(text: \"ok echo led was already active\", timeout_ms: timeout);",
        "}",
        "",
        "echo(text: greeting);",
        "expect(text: \"ok echo hello from EspScript\", timeout_ms: timeout);",
        "",
        "pulse(times: pulses, delay: pulse_delay);",
        "",
        "blink(ms: blink_delay);",
        "expect(text: \"ok blink_ms=500\", timeout_ms: timeout);",
        "wait(ms: ms(1500));",
        "blink(state: off);",
        "expect(text: \"ok blink_ms=0\", timeout_ms: timeout);",
        "heartbeat(enabled: true);",
        "expect(text: \"ok heartbeat=on\", timeout_ms: timeout);",
        "",
        "// Примеры команд, которые сохраняются/выполняются на плате:",
        "// save(name: quick_blink) { led.toggle(); sleep(duration: ms(100)); led.toggle(); }",
        "// on.boot() { led.on(); sleep(duration: ms(100)); led.off(); }",
        "// run(name: quick_blink);",
        "// autorun(name: quick_blink);",
        "// board.var(name: counter, value: 0);",
        "// every(id: 0, ms: ms(1000)) { repeat(times: 3) { led.toggle(); sleep(duration: ms(100)); } }",
        "// after(id: 1, ms: ms(5000)) { led.off(); }",
        "// timer(id: 1, every: ms(1000)) { board.var(name: counter, value: counter + 1); }",
        "// on.button(id: pin(0)) { led.toggle(); }",
        "// on.pin(id: pin(0), trigger: falling, debounce: ms(30)) { led.toggle(); }",
        "// if led == off { led.on(); } else { led.off(); }",
    ]
    .join("\n")
}

pub fn compile_script(script: &str) -> Result<Vec<ScriptStep>, String> {
    ScriptParser::new(script).parse()
}

pub fn lint_script(script: &str, steps: &[ScriptStep]) -> Vec<ScriptLint> {
    let mut lints = Vec::new();
    lint_source_text(script, &mut lints);
    lint_compiled_steps(steps, &mut lints);
    lints.sort_by_key(|lint| lint.line);
    lints.dedup();
    lints
}

pub fn format_script(script: &str) -> String {
    ScriptFormatter::new(script).format()
}

struct ScriptArg {
    name: Option<String>,
    value: ScriptValue,
}

#[derive(Clone)]
enum ScriptValue {
    Ident(String),
    Number(u64),
    Bool(bool),
    Text(String),
    DurationMs(u64),
    FrequencyHz(u64),
    Millivolts(u64),
    Pin(u8),
    BoardNumberExpr {
        left: Box<ScriptValue>,
        op: &'static str,
        right: Box<ScriptValue>,
    },
}

#[derive(Clone, Copy)]
enum ScriptType {
    Number,
    Bool,
    Text,
    Ms,
    Hz,
    Volt,
    Pin,
}

struct ScriptParser<'a> {
    source: &'a str,
    pos: usize,
    line: usize,
    column: usize,
    variables: HashMap<String, ScriptValue>,
    functions: HashMap<String, ScriptFunction>,
    resources: BoardResources,
    call_depth: usize,
    board_depth: usize,
}

#[derive(Clone)]
struct ScriptFunction {
    params: Vec<FunctionParam>,
    body: String,
    body_line: usize,
}

#[derive(Clone)]
struct FunctionParam {
    name: String,
    ty: Option<ScriptType>,
    default: ScriptValue,
}

impl<'a> ScriptParser<'a> {
    fn new(source: &'a str) -> Self {
        Self {
            source,
            pos: 0,
            line: 1,
            column: 1,
            variables: HashMap::new(),
            functions: HashMap::new(),
            resources: BoardResources::default(),
            call_depth: 0,
            board_depth: 0,
        }
    }

    fn parse(mut self) -> Result<Vec<ScriptStep>, String> {
        self.parse_steps()
    }

    fn parse_steps(&mut self) -> Result<Vec<ScriptStep>, String> {
        let steps = self.parse_block(false)?;
        self.skip_ws_and_comments();

        if self.is_eof() {
            Ok(steps)
        } else {
            Err(self.error("ожидался конец скрипта"))
        }
    }

    fn parse_block(&mut self, nested: bool) -> Result<Vec<ScriptStep>, String> {
        let mut steps = Vec::new();

        loop {
            self.skip_ws_and_comments();

            if self.is_eof() {
                return if nested {
                    Err(self.error("ожидалась закрывающая фигурная скобка `}`"))
                } else {
                    Ok(steps)
                };
            }

            if self.consume_char('}') {
                return if nested {
                    Ok(steps)
                } else {
                    Err(self.error("лишняя закрывающая фигурная скобка `}`"))
                };
            }

            self.parse_statement(&mut steps)?;

            if expanded_step_count(&steps) > 10_000 {
                return Err(
                    self.error("слишком много шагов после раскрытия repeat, максимум 10000")
                );
            }
        }
    }

    fn parse_statement(&mut self, steps: &mut Vec<ScriptStep>) -> Result<(), String> {
        let line = self.line;
        let name = self.parse_path()?;

        if name == "let" {
            return self.parse_let_statement(line);
        }

        if name == "fn" {
            return self.parse_function_definition(line);
        }

        if name == "requires" {
            let args = self.parse_arg_list()?;
            self.expect_char(';')?;
            return compile_requires(&args, line, steps);
        }

        if name == "if" {
            return self.parse_if_statement(line, steps);
        }

        let args = self.parse_arg_list()?;

        if name == "repeat" {
            if self.board_depth > 0 {
                let times = board_repeat_times(&args, line)?;
                self.expect_char('{')?;
                let saved_variables = self.variables.clone();
                let body_steps = self.parse_block(true)?;
                self.variables = saved_variables;
                self.skip_ws_and_comments();
                self.consume_char(';');

                let body = steps_as_board_script(&body_steps, line, "repeat")?;
                return push_command(steps, line, format!("repeat {times} {{ {body} }}"));
            }

            let times = host_repeat_times(&args, line)?;
            self.expect_char('{')?;
            let saved_variables = self.variables.clone();
            let body = self.parse_block(true)?;
            self.variables = saved_variables;
            self.skip_ws_and_comments();
            self.consume_char(';');

            for _ in 0..times {
                steps.extend(body.iter().cloned());
            }

            return Ok(());
        }

        if name == "save" {
            return self.parse_save_statement(&args, line, steps);
        }

        if matches!(name.as_str(), "every" | "after") {
            if self.next_is_block() {
                return self.parse_timer_sugar_block(&name, &args, line, steps);
            }

            return Err(script_error(
                line,
                format!("{name}(...) требует блок `{{ ... }}`"),
            ));
        }

        if name == "timer" && self.next_is_block() {
            return self.parse_timer_block(&args, line, steps);
        }

        if name == "on.button" {
            if self.next_is_block() {
                return self.parse_on_button_block(&args, line, steps);
            }

            return Err(script_error(line, "on.button(...) требует блок `{ ... }`"));
        }

        if name == "on.boot" {
            if self.next_is_block() {
                return self.parse_on_boot_block(&args, line, steps);
            }

            return Err(script_error(line, "on.boot(...) требует блок `{ ... }`"));
        }

        if name == "on.pin" {
            if self.next_is_block() {
                return self.parse_on_pin_block(&args, line, steps);
            }

            return Err(script_error(line, "on.pin(...) требует блок `{ ... }`"));
        }

        self.expect_char(';')?;
        if self.compile_function_call(&name, &args, line, steps)? {
            return Ok(());
        }

        compile_call(&name, &args, line, steps, &mut self.resources)
    }

    fn parse_let_statement(&mut self, line: usize) -> Result<(), String> {
        let name = self.parse_identifier()?.to_owned();

        if is_reserved_variable_name(&name) {
            return Err(script_error(
                line,
                format!("`{name}` нельзя использовать как имя переменной"),
            ));
        }

        self.skip_ws_and_comments();
        let ty = if self.consume_char(':') {
            let type_name = self.parse_identifier()?.to_owned();
            Some(script_type(&type_name, line)?)
        } else {
            None
        };

        self.expect_char('=')?;
        let value = self.parse_value()?;
        if let Some(ty) = ty {
            validate_value_type(&value, ty, line, format!("let `{name}`"))?;
        }
        self.expect_char(';')?;
        self.variables.insert(name, value);
        Ok(())
    }

    fn parse_function_definition(&mut self, line: usize) -> Result<(), String> {
        let name = self.parse_identifier()?.to_owned();

        if is_reserved_function_name(&name) {
            return Err(script_error(
                line,
                format!("`{name}` нельзя использовать как имя функции"),
            ));
        }

        let params = self.parse_function_params(line)?;
        self.skip_ws_and_comments();
        let body_line = self.line;
        let body = self.capture_block_source()?;
        self.skip_ws_and_comments();
        self.consume_char(';');

        self.functions.insert(
            name,
            ScriptFunction {
                params,
                body,
                body_line,
            },
        );
        Ok(())
    }

    fn parse_save_statement(
        &mut self,
        args: &[ScriptArg],
        line: usize,
        steps: &mut Vec<ScriptStep>,
    ) -> Result<(), String> {
        let signature = "save(name: program) { ... }";
        expect_named_args(args, &["name", "program"], line, signature)?;
        let name = value_as_program_name(
            named_arg_any(args, &["name", "program"], line, signature)?,
            line,
        )?;
        let body = self.parse_board_block(line, "save")?;
        push_command(steps, line, format!("save {name} {{ {body} }}"))
    }

    fn parse_timer_block(
        &mut self,
        args: &[ScriptArg],
        line: usize,
        steps: &mut Vec<ScriptStep>,
    ) -> Result<(), String> {
        let (id, mode, duration) = timer_parts(args, line)?;
        let body = self.parse_board_block(line, "timer")?;
        if body.len() > 128 {
            return Err(script_error(line, "timer do-блок максимум 128 байт"));
        }

        push_command(
            steps,
            line,
            format!("timer {id} {mode} {duration} do {{ {body} }}"),
        )
    }

    fn parse_timer_sugar_block(
        &mut self,
        mode: &str,
        args: &[ScriptArg],
        line: usize,
        steps: &mut Vec<ScriptStep>,
    ) -> Result<(), String> {
        let (id, duration) = timer_sugar_parts(mode, args, line)?;
        let body = self.parse_board_block(line, mode)?;
        if body.len() > 128 {
            return Err(script_error(
                line,
                format!("{mode} do-блок максимум 128 байт"),
            ));
        }

        push_command(
            steps,
            line,
            format!("timer {id} {mode} {duration} do {{ {body} }}"),
        )
    }

    fn parse_on_pin_block(
        &mut self,
        args: &[ScriptArg],
        line: usize,
        steps: &mut Vec<ScriptStep>,
    ) -> Result<(), String> {
        let signature = "on.pin(id: pin(0), trigger: falling, debounce: ms(30)) { ... }";
        expect_named_args(args, &["id", "pin", "trigger", "debounce"], line, signature)?;
        let pin = value_as_pin(named_arg_any(args, &["id", "pin"], line, signature)?, line)?;
        validate_esp32_gpio(pin, line)?;
        self.resources.claim_pin(pin, "pin", line)?;
        let trigger = value_as_pin_trigger(named_arg(args, "trigger", line, signature)?, line)?;
        let debounce_ms = optional_named_arg(args, "debounce")
            .map(|value| value_as_ms(value, line))
            .transpose()?;
        if debounce_ms.is_some_and(|ms| ms > 60_000) {
            return Err(script_error(line, "on.pin debounce максимум 60000 ms"));
        }
        let body = self.parse_board_block(line, "on.pin")?;
        if body.len() > 128 {
            return Err(script_error(line, "on.pin do-блок максимум 128 байт"));
        }

        let debounce = debounce_ms
            .filter(|ms| *ms > 0)
            .map(|ms| format!(" debounce {ms}"))
            .unwrap_or_default();
        push_command(
            steps,
            line,
            format!("on pin {pin} {trigger}{debounce} do {{ {body} }}"),
        )
    }

    fn parse_on_button_block(
        &mut self,
        args: &[ScriptArg],
        line: usize,
        steps: &mut Vec<ScriptStep>,
    ) -> Result<(), String> {
        let signature = "on.button(id: pin(0), debounce: ms(30)) { ... }";
        expect_named_args(args, &["id", "pin", "trigger", "debounce"], line, signature)?;
        let pin = value_as_pin(named_arg_any(args, &["id", "pin"], line, signature)?, line)?;
        validate_esp32_gpio(pin, line)?;
        self.resources.claim_pin(pin, "pin", line)?;
        let trigger = optional_named_arg(args, "trigger")
            .map(|value| value_as_pin_trigger(value, line))
            .transpose()?
            .unwrap_or("falling");
        let debounce_ms = optional_named_arg(args, "debounce")
            .map(|value| value_as_ms(value, line))
            .transpose()?
            .unwrap_or(30);
        if debounce_ms > 60_000 {
            return Err(script_error(line, "on.button debounce максимум 60000 ms"));
        }

        let body = self.parse_board_block(line, "on.button")?;
        if body.len() > 128 {
            return Err(script_error(line, "on.button do-блок максимум 128 байт"));
        }

        push_command(steps, line, format!("pin {pin} mode input_pullup"))?;
        let debounce = if debounce_ms > 0 {
            format!(" debounce {debounce_ms}")
        } else {
            String::new()
        };
        push_command(
            steps,
            line,
            format!("on pin {pin} {trigger}{debounce} do {{ {body} }}"),
        )
    }

    fn parse_on_boot_block(
        &mut self,
        args: &[ScriptArg],
        line: usize,
        steps: &mut Vec<ScriptStep>,
    ) -> Result<(), String> {
        let signature = "on.boot(name: boot) { ... }";
        expect_named_args(args, &["name", "program"], line, signature)?;
        let name = if args.is_empty() {
            "boot".to_owned()
        } else {
            value_as_program_name(
                named_arg_any(args, &["name", "program"], line, signature)?,
                line,
            )?
        };
        let body = self.parse_board_block(line, "on.boot")?;

        push_command(steps, line, format!("save {name} {{ {body} }}"))?;
        push_command(steps, line, format!("autorun {name}"))
    }

    fn parse_board_block(&mut self, line: usize, context: &str) -> Result<String, String> {
        self.expect_char('{')?;
        self.board_depth += 1;
        let body = self.parse_block(true);
        self.board_depth -= 1;
        let body = body?;
        self.skip_ws_and_comments();
        self.consume_char(';');
        steps_as_board_script(&body, line, context)
    }

    fn compile_function_call(
        &mut self,
        name: &str,
        args: &[ScriptArg],
        line: usize,
        steps: &mut Vec<ScriptStep>,
    ) -> Result<bool, String> {
        let Some(function) = self.functions.get(name).cloned() else {
            return Ok(false);
        };

        if self.call_depth >= 32 {
            return Err(script_error(
                line,
                "слишком глубокие вызовы функций, максимум 32",
            ));
        }

        let mut variables = self.variables.clone();
        for param in &function.params {
            variables.insert(param.name.clone(), param.default.clone());
        }

        let mut used_args = Vec::new();
        for arg in args {
            let Some(arg_name) = &arg.name else {
                return Err(script_error(
                    line,
                    format!("аргументы функции `{name}` должны быть именованными"),
                ));
            };

            if used_args.iter().any(|used| used == arg_name) {
                return Err(script_error(
                    line,
                    format!("аргумент `{arg_name}` указан дважды"),
                ));
            }

            if !function.params.iter().any(|param| param.name == *arg_name) {
                return Err(script_error(
                    line,
                    format!("у функции `{name}` нет аргумента `{arg_name}`"),
                ));
            }

            used_args.push(arg_name.clone());
            if let Some(param) = function.params.iter().find(|param| param.name == *arg_name)
                && let Some(ty) = param.ty
            {
                validate_value_type(&arg.value, ty, line, format!("аргумент `{arg_name}`"))?;
            }
            variables.insert(arg_name.clone(), arg.value.clone());
        }

        let mut parser = ScriptParser::new(&function.body);
        parser.line = function.body_line;
        parser.variables = variables;
        parser.functions = self.functions.clone();
        parser.resources = self.resources.clone();
        parser.call_depth = self.call_depth + 1;
        parser.board_depth = self.board_depth;
        steps.extend(parser.parse_steps()?);
        self.resources = parser.resources;
        Ok(true)
    }

    fn parse_if_statement(
        &mut self,
        line: usize,
        steps: &mut Vec<ScriptStep>,
    ) -> Result<(), String> {
        let saved_pos = self.pos;
        let saved_line = self.line;

        if let Ok(condition) = self.parse_path()
            && condition == "contains"
        {
            let args = self.parse_arg_list()?;
            let text = compile_contains_condition(&args, line)?;

            self.expect_char('{')?;
            let saved_variables = self.variables.clone();
            let then_steps = self.parse_block(true)?;
            self.variables = saved_variables.clone();

            self.skip_ws_and_comments();
            let else_steps = if self.consume_keyword("else") {
                self.expect_char('{')?;
                let steps = self.parse_block(true)?;
                self.variables = saved_variables;
                steps
            } else {
                self.variables = saved_variables;
                Vec::new()
            };

            self.skip_ws_and_comments();
            self.consume_char(';');
            steps.push(ScriptStep::IfContains {
                text,
                line,
                then_steps,
                else_steps,
            });

            return Ok(());
        }

        self.pos = saved_pos;
        self.line = saved_line;
        self.parse_board_if_statement(line, steps)
    }

    fn parse_board_if_statement(
        &mut self,
        line: usize,
        steps: &mut Vec<ScriptStep>,
    ) -> Result<(), String> {
        let condition = self.parse_board_condition(line)?;
        self.expect_char('{')?;
        let saved_variables = self.variables.clone();
        self.board_depth += 1;
        let then_steps = self.parse_block(true);
        self.board_depth -= 1;
        let then_steps = steps_as_board_script(&then_steps?, line, "if")?;
        self.variables = saved_variables.clone();

        self.skip_ws_and_comments();
        let else_steps = if self.consume_keyword("else") {
            self.expect_char('{')?;
            self.board_depth += 1;
            let steps = self.parse_block(true);
            self.board_depth -= 1;
            let steps = steps_as_board_script(&steps?, line, "if else")?;
            self.variables = saved_variables;
            Some(steps)
        } else {
            self.variables = saved_variables;
            None
        };

        self.skip_ws_and_comments();
        self.consume_char(';');

        let command = if let Some(else_steps) = else_steps {
            format!("if {condition} {{ {then_steps} }} else {{ {else_steps} }}")
        } else {
            format!("if {condition} {{ {then_steps} }}")
        };
        push_command(steps, line, command)?;

        Ok(())
    }

    fn parse_board_condition(&mut self, line: usize) -> Result<String, String> {
        let saved_pos = self.pos;
        let saved_line = self.line;

        if let Ok(path) = self.parse_path()
            && matches!(path.as_str(), "pin" | "pin.read")
            && self.peek_char() == Some('(')
        {
            let signature = "pin(id: pin(0)) == on";
            let args = self.parse_arg_list()?;
            let pin = condition_pin_arg(&args, line, signature)?;
            let op = self.parse_compare_operator()?;
            ensure_bool_compare_operator(op, line)?;
            let expected = value_as_board_level(&self.parse_value()?, line)?;
            self.ensure_condition_end(line)?;
            return Ok(format!("pin {pin} {op} {expected}"));
        }

        self.pos = saved_pos;
        self.line = saved_line;

        let left = self.parse_value()?;
        let op = self.parse_compare_operator()?;
        let right = self.parse_value()?;
        self.ensure_condition_end(line)?;

        match &left {
            ScriptValue::Pin(pin) => {
                ensure_bool_compare_operator(op, line)?;
                Ok(format!(
                    "pin {pin} {op} {}",
                    value_as_board_level(&right, line)?
                ))
            }
            ScriptValue::Ident(name) if matches!(name.as_str(), "led" | "heartbeat" | "wifi") => {
                ensure_bool_compare_operator(op, line)?;
                Ok(format!(
                    "{name} {op} {}",
                    value_as_board_level(&right, line)?
                ))
            }
            _ => Ok(format!(
                "{} {op} {}",
                value_as_board_number_token(&left, line)?,
                value_as_board_number_token(&right, line)?
            )),
        }
    }

    fn parse_compare_operator(&mut self) -> Result<&'static str, String> {
        self.skip_ws_and_comments();
        for op in ["==", "!=", "<=", ">=", "<", ">"] {
            if self.starts_with(op) {
                self.pos += op.len();
                return Ok(op);
            }
        }

        Err(self.error("ожидался оператор сравнения: ==, !=, <, <=, > или >="))
    }

    fn ensure_condition_end(&mut self, line: usize) -> Result<(), String> {
        self.skip_ws_and_comments();
        if self.peek_char() == Some('{') {
            Ok(())
        } else {
            Err(script_error(
                line,
                "после условия if ожидался блок `{ ... }`",
            ))
        }
    }

    fn parse_path(&mut self) -> Result<String, String> {
        self.skip_ws_and_comments();
        let mut path = self.parse_identifier()?.to_owned();

        loop {
            self.skip_ws_and_comments();
            if !self.consume_char('.') {
                return Ok(path);
            }

            self.skip_ws_and_comments();
            path.push('.');
            path.push_str(self.parse_identifier()?);
        }
    }

    fn next_is_block(&mut self) -> bool {
        self.skip_ws_and_comments();
        self.peek_char() == Some('{')
    }

    fn parse_arg_list(&mut self) -> Result<Vec<ScriptArg>, String> {
        self.skip_ws_and_comments();
        self.expect_char('(')?;
        self.skip_ws_and_comments();

        let mut args = Vec::new();
        if self.consume_char(')') {
            return Ok(args);
        }

        loop {
            let saved_pos = self.pos;
            let saved_line = self.line;
            let mut name = None;

            if let Ok(candidate) = self.parse_identifier() {
                let candidate = candidate.to_owned();
                self.skip_ws_and_comments();
                if self.consume_char(':') {
                    name = Some(candidate);
                } else {
                    self.pos = saved_pos;
                    self.line = saved_line;
                }
            } else {
                self.pos = saved_pos;
                self.line = saved_line;
            }

            let value = self.parse_arg_value()?;
            args.push(ScriptArg { name, value });

            self.skip_ws_and_comments();
            if self.consume_char(',') {
                self.skip_ws_and_comments();
                continue;
            }

            self.expect_char(')')?;
            return Ok(args);
        }
    }

    fn parse_function_params(&mut self, line: usize) -> Result<Vec<FunctionParam>, String> {
        self.skip_ws_and_comments();
        self.expect_char('(')?;
        self.skip_ws_and_comments();

        let mut params = Vec::new();
        if self.consume_char(')') {
            return Ok(params);
        }

        loop {
            let name = self.parse_identifier()?.to_owned();
            if is_reserved_variable_name(&name) {
                return Err(script_error(
                    line,
                    format!("`{name}` нельзя использовать как параметр функции"),
                ));
            }

            if params
                .iter()
                .any(|param: &FunctionParam| param.name == name)
            {
                return Err(script_error(
                    line,
                    format!("параметр `{name}` указан дважды"),
                ));
            }

            self.expect_char(':')?;
            let (ty, default) = self.parse_function_param_default(line, &name)?;
            params.push(FunctionParam { name, ty, default });

            self.skip_ws_and_comments();
            if self.consume_char(',') {
                self.skip_ws_and_comments();
                continue;
            }

            self.expect_char(')')?;
            return Ok(params);
        }
    }

    fn parse_function_param_default(
        &mut self,
        line: usize,
        name: &str,
    ) -> Result<(Option<ScriptType>, ScriptValue), String> {
        self.skip_ws_and_comments();
        let saved_pos = self.pos;
        let saved_line = self.line;
        let saved_column = self.column;

        if let Ok(type_name) = self.parse_identifier() {
            let type_name = type_name.to_owned();
            if let Some(ty) = optional_script_type(&type_name) {
                self.skip_ws_and_comments();
                if self.consume_char('=') {
                    let default = self.parse_arg_value()?;
                    validate_value_type(&default, ty, line, format!("параметр `{name}`"))?;
                    return Ok((Some(ty), default));
                }
            }
        }

        self.pos = saved_pos;
        self.line = saved_line;
        self.column = saved_column;
        let default = self.parse_arg_value()?;
        Ok((None, default))
    }

    fn parse_arg_value(&mut self) -> Result<ScriptValue, String> {
        let left = self.parse_value()?;
        self.skip_ws_and_comments();

        let Some(op) = self.consume_arithmetic_operator() else {
            return Ok(left);
        };

        let right = self.parse_value()?;
        Ok(ScriptValue::BoardNumberExpr {
            left: Box::new(left),
            op,
            right: Box::new(right),
        })
    }

    fn consume_arithmetic_operator(&mut self) -> Option<&'static str> {
        self.skip_ws_and_comments();
        for op in ["+", "-", "*", "/", "%"] {
            if self.starts_with(op) {
                self.pos += op.len();
                self.column += op.len();
                return Some(op);
            }
        }

        None
    }

    fn parse_value(&mut self) -> Result<ScriptValue, String> {
        self.skip_ws_and_comments();

        if self.peek_char() == Some('"') {
            return self.parse_string().map(ScriptValue::Text);
        }

        if self.peek_char().is_some_and(|ch| ch.is_ascii_digit()) {
            return self.parse_number().map(ScriptValue::Number);
        }

        let ident = self.parse_identifier()?.to_owned();
        if is_unit_constructor(&ident) {
            self.skip_ws_and_comments();
            if self.peek_char() == Some('(') {
                return self.parse_unit_value(&ident);
            }
        }

        match ident.as_str() {
            "true" => Ok(ScriptValue::Bool(true)),
            "false" => Ok(ScriptValue::Bool(false)),
            _ => Ok(self
                .variables
                .get(&ident)
                .cloned()
                .unwrap_or(ScriptValue::Ident(ident))),
        }
    }

    fn parse_unit_value(&mut self, unit: &str) -> Result<ScriptValue, String> {
        self.expect_char('(')?;

        let value = match unit {
            "ms" => ScriptValue::DurationMs(self.parse_number()?),
            "hz" => ScriptValue::FrequencyHz(self.parse_number()?),
            "volt" => ScriptValue::Millivolts(self.parse_millivolts()?),
            "pin" => {
                let pin = self.parse_number()?;
                if pin > u8::MAX as u64 {
                    return Err(self.error("pin(...) должен быть в диапазоне 0..255"));
                }
                ScriptValue::Pin(pin as u8)
            }
            _ => return Err(self.error(format!("неизвестный тип `{unit}`"))),
        };

        self.expect_char(')')?;
        Ok(value)
    }

    fn parse_identifier(&mut self) -> Result<&'a str, String> {
        self.skip_ws_and_comments();
        let start = self.pos;
        let Some(first) = self.peek_char() else {
            return Err(self.error("ожидалось имя"));
        };

        if !is_ident_start(first) {
            return Err(self.error("ожидалось имя"));
        }

        self.next_char();
        while self.peek_char().is_some_and(is_ident_continue) {
            self.next_char();
        }

        Ok(&self.source[start..self.pos])
    }

    fn parse_number(&mut self) -> Result<u64, String> {
        self.skip_ws_and_comments();
        let mut value = 0_u64;
        let mut has_digit = false;

        while let Some(ch) = self.peek_char() {
            if !ch.is_ascii_digit() {
                break;
            }

            has_digit = true;
            value = value
                .checked_mul(10)
                .and_then(|value| value.checked_add((ch as u8 - b'0') as u64))
                .ok_or_else(|| self.error("число слишком большое"))?;
            self.next_char();
        }

        if has_digit {
            Ok(value)
        } else {
            Err(self.error("ожидалось число"))
        }
    }

    fn parse_millivolts(&mut self) -> Result<u64, String> {
        self.skip_ws_and_comments();
        let whole = self.parse_number()?;
        let mut millivolts = whole
            .checked_mul(1000)
            .ok_or_else(|| self.error("значение volt(...) слишком большое"))?;

        if !self.consume_char('.') {
            return Ok(millivolts);
        }

        let mut digits = 0_u8;
        let mut fraction = 0_u64;
        while let Some(ch) = self.peek_char() {
            if !ch.is_ascii_digit() {
                break;
            }

            if digits >= 3 {
                return Err(self.error("volt(...) поддерживает максимум 3 знака после точки"));
            }

            digits += 1;
            fraction = fraction * 10 + (ch as u8 - b'0') as u64;
            self.next_char();
        }

        if digits == 0 {
            return Err(self.error("после точки в volt(...) ожидалась цифра"));
        }

        for _ in digits..3 {
            fraction *= 10;
        }

        millivolts = millivolts
            .checked_add(fraction)
            .ok_or_else(|| self.error("значение volt(...) слишком большое"))?;
        Ok(millivolts)
    }

    fn parse_string(&mut self) -> Result<String, String> {
        self.expect_char('"')?;
        let mut value = String::new();

        loop {
            let Some(ch) = self.next_char() else {
                return Err(self.error("строка не закрыта"));
            };

            match ch {
                '"' => return Ok(value),
                '\\' => {
                    let Some(escaped) = self.next_char() else {
                        return Err(self.error("escape-последовательность не закрыта"));
                    };

                    match escaped {
                        '"' => value.push('"'),
                        '\\' => value.push('\\'),
                        'n' => value.push('\n'),
                        'r' => value.push('\r'),
                        't' => value.push('\t'),
                        _ => {
                            return Err(self.error(format!(
                                "неизвестная escape-последовательность `\\{escaped}`"
                            )));
                        }
                    }
                }
                '\n' | '\r' => return Err(self.error("строка должна быть в одной строке")),
                _ => value.push(ch),
            }
        }
    }

    fn capture_block_source(&mut self) -> Result<String, String> {
        self.expect_char('{')?;
        let start = self.pos;
        let mut depth = 1_usize;

        while !self.is_eof() {
            if self.starts_with("//") || self.starts_with("#") {
                while let Some(ch) = self.next_char() {
                    if ch == '\n' {
                        break;
                    }
                }
                continue;
            }

            let Some(ch) = self.next_char() else {
                break;
            };

            match ch {
                '"' => self.skip_string_body()?,
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        let end = self.pos - ch.len_utf8();
                        return Ok(self.source[start..end].to_owned());
                    }
                }
                _ => {}
            }
        }

        Err(self.error("ожидалась закрывающая фигурная скобка `}`"))
    }

    fn skip_string_body(&mut self) -> Result<(), String> {
        loop {
            let Some(ch) = self.next_char() else {
                return Err(self.error("строка не закрыта"));
            };

            match ch {
                '"' => return Ok(()),
                '\\' => {
                    if self.next_char().is_none() {
                        return Err(self.error("escape-последовательность не закрыта"));
                    }
                }
                '\n' | '\r' => return Err(self.error("строка должна быть в одной строке")),
                _ => {}
            }
        }
    }

    fn skip_ws_and_comments(&mut self) {
        loop {
            while self.peek_char().is_some_and(char::is_whitespace) {
                self.next_char();
            }

            if self.starts_with("//") || self.starts_with("#") {
                while let Some(ch) = self.peek_char() {
                    if ch == '\n' {
                        break;
                    }
                    self.next_char();
                }
            } else {
                return;
            }
        }
    }

    fn expect_char(&mut self, expected: char) -> Result<(), String> {
        self.skip_ws_and_comments();
        if self.consume_char(expected) {
            Ok(())
        } else {
            Err(self.error(format!("ожидался символ `{expected}`")))
        }
    }

    fn consume_char(&mut self, expected: char) -> bool {
        if self.peek_char() == Some(expected) {
            self.next_char();
            true
        } else {
            false
        }
    }

    fn consume_keyword(&mut self, expected: &str) -> bool {
        self.skip_ws_and_comments();

        let Some(rest) = self.source.get(self.pos..) else {
            return false;
        };

        if !rest.starts_with(expected) {
            return false;
        }

        let after = self.pos + expected.len();
        if self
            .source
            .get(after..)
            .and_then(|value| value.chars().next())
            .is_some_and(is_ident_continue)
        {
            return false;
        }

        self.pos = after;
        true
    }

    fn starts_with(&self, value: &str) -> bool {
        self.source[self.pos..].starts_with(value)
    }

    fn peek_char(&self) -> Option<char> {
        self.source[self.pos..].chars().next()
    }

    fn next_char(&mut self) -> Option<char> {
        let ch = self.peek_char()?;
        self.pos += ch.len_utf8();
        if ch == '\n' {
            self.line += 1;
            self.column = 1;
        } else {
            self.column += 1;
        }
        Some(ch)
    }

    fn is_eof(&self) -> bool {
        self.pos >= self.source.len()
    }

    fn error(&self, message: impl Into<String>) -> String {
        script_error_at(self.line, self.column, message)
    }
}

fn compile_call(
    name: &str,
    args: &[ScriptArg],
    line: usize,
    steps: &mut Vec<ScriptStep>,
    resources: &mut BoardResources,
) -> Result<(), String> {
    match name {
        "status" | "board.status" => {
            expect_no_args(args, line, "status()")?;
            push_command(steps, line, "status")
        }
        "ping" => {
            expect_no_args(args, line, "ping()")?;
            push_command(steps, line, "ping")
        }
        "help" => {
            expect_no_args(args, line, "help()")?;
            push_command(steps, line, "help")
        }
        "caps" => {
            expect_no_args(args, line, "caps()")?;
            push_command(steps, line, "caps")
        }
        "vars" => {
            expect_no_args(args, line, "vars()")?;
            push_command(steps, line, "vars")
        }
        "programs" => {
            expect_no_args(args, line, "programs()")?;
            push_command(steps, line, "programs")
        }
        "run" => compile_run(args, line, steps),
        "delete" => compile_delete(args, line, steps),
        "autorun" => compile_autorun(args, line, steps),
        "autorun.off" => {
            expect_no_args(args, line, "autorun.off()")?;
            push_command(steps, line, "autorun off")
        }
        "boot" => {
            expect_no_args(args, line, "boot()")?;
            push_command(steps, line, "boot")
        }
        "led" => {
            resources.claim_pin(2, "led", line)?;
            let state =
                value_as_state(single_arg(args, &["state"], line, "led(state: on)")?, line)?;
            push_command(steps, line, format!("led {state}"))
        }
        "led.on" => {
            expect_no_args(args, line, "led.on()")?;
            resources.claim_pin(2, "led", line)?;
            push_command(steps, line, "led on")
        }
        "led.off" => {
            expect_no_args(args, line, "led.off()")?;
            resources.claim_pin(2, "led", line)?;
            push_command(steps, line, "led off")
        }
        "led.toggle" => {
            expect_no_args(args, line, "led.toggle()")?;
            resources.claim_pin(2, "led", line)?;
            push_command(steps, line, "led toggle")
        }
        "blink" => compile_blink(args, line, steps, resources),
        "blink.off" => {
            expect_no_args(args, line, "blink.off()")?;
            resources.claim_pin(2, "led", line)?;
            push_command(steps, line, "blink off")
        }
        "heartbeat" => {
            let value = single_arg(
                args,
                &["enabled", "state"],
                line,
                "heartbeat(enabled: true)",
            )?;
            let state = value_as_state(value, line)?;
            if state == "toggle" {
                return Err(script_error(
                    line,
                    "heartbeat поддерживает только on/off или true/false",
                ));
            }
            push_command(steps, line, format!("heartbeat {state}"))
        }
        "heartbeat.on" => {
            expect_no_args(args, line, "heartbeat.on()")?;
            push_command(steps, line, "heartbeat on")
        }
        "heartbeat.off" => {
            expect_no_args(args, line, "heartbeat.off()")?;
            push_command(steps, line, "heartbeat off")
        }
        "wait" => {
            let ms =
                value_as_ms_or_number(single_arg(args, &["ms"], line, "wait(ms: ms(250))")?, line)?;
            if ms > 600_000 {
                return Err(script_error(line, "wait(ms) максимум 600000"));
            }
            steps.push(ScriptStep::Wait { ms, line });
            Ok(())
        }
        "expect" => compile_expect(args, line, steps),
        "board.var" => compile_board_var(args, line, steps),
        "echo" => {
            let text = value_as_command_text(
                single_arg(args, &["text"], line, "echo(text: \"hello\")")?,
                line,
                "echo(text)",
            )?;
            push_command(steps, line, format!("echo {text}"))
        }
        "cmd" => {
            let command = value_as_text(single_arg(
                args,
                &["text", "command"],
                line,
                "cmd(text: \"status\")",
            )?);
            push_command(steps, line, command)
        }
        "pin" => compile_pin(args, line, steps, resources),
        "pin.read" => compile_pin_read(args, line, steps, resources),
        "pin.write" => compile_pin_write(args, line, steps, resources),
        "pwm" => compile_pwm(args, line, steps, resources),
        "pwm.stop" => compile_pwm_stop(args, line, steps, resources),
        "adc" | "adc.read" => compile_adc(args, line, steps, resources),
        "i2c" => compile_i2c(args, line, steps, resources),
        "spi" => compile_spi(args, line, steps, resources),
        "uart" => compile_uart(args, line, steps, resources),
        "wifi" => compile_wifi(args, line, steps, resources),
        "timer" => compile_timer(args, line, steps),
        "timer.stop" => compile_timer_stop(args, line, steps),
        "on.pin.off" => compile_on_pin_off(args, line, steps, resources),
        "sleep" => compile_sleep(args, line, steps),
        _ => Err(script_error(
            line,
            format!(
                "неизвестная функция `{name}`. Реализованные примитивы: pin, pwm, adc, timer, sleep. Driver-dependent/future: i2c, spi, uart, wifi. Сценарии платы: board.var, save, run, autorun, every/after, on.button, on.pin, timer. Для прямой UART-команды используй cmd(text: \"...\")"
            ),
        )),
    }
}

fn compile_expect(
    args: &[ScriptArg],
    line: usize,
    steps: &mut Vec<ScriptStep>,
) -> Result<(), String> {
    let signature = "expect(text: \"ok\", timeout_ms: ms(1000))";
    expect_named_args(args, &["text", "timeout_ms"], line, signature)?;

    let text = value_as_text(named_arg(args, "text", line, signature)?);
    if text.is_empty() {
        return Err(script_error(line, "expect(text) не может быть пустым"));
    }

    let timeout_ms = value_as_ms_or_number(named_arg(args, "timeout_ms", line, signature)?, line)?;
    if !(1..=600_000).contains(&timeout_ms) {
        return Err(script_error(
            line,
            "expect(timeout_ms) должен быть в диапазоне 1..600000",
        ));
    }

    steps.push(ScriptStep::Expect {
        text,
        timeout_ms,
        line,
    });
    Ok(())
}

fn compile_requires(
    args: &[ScriptArg],
    line: usize,
    steps: &mut Vec<ScriptStep>,
) -> Result<(), String> {
    let signature = "requires(feature: pwm_real) или requires(features: \"pwm,pwm_real\")";
    expect_named_args(args, &["feature", "features"], line, signature)?;

    let feature = optional_named_arg(args, "feature");
    let features = optional_named_arg(args, "features");
    let features = match (feature, features) {
        (Some(value), None) => vec![value_as_feature_name(value, line)?],
        (None, Some(value)) => value_as_feature_list(value, line)?,
        (Some(_), Some(_)) => {
            return Err(script_error(
                line,
                "requires принимает либо feature, либо features, но не оба сразу",
            ));
        }
        (None, None) => {
            return Err(script_error(
                line,
                format!("нет аргумента `feature` или `features`, ожидался {signature}"),
            ));
        }
    };

    steps.push(ScriptStep::Requires { features, line });
    Ok(())
}

fn compile_contains_condition(args: &[ScriptArg], line: usize) -> Result<String, String> {
    let signature = "contains(text: \"ok\")";
    expect_named_args(args, &["text"], line, signature)?;

    let text = value_as_text(named_arg(args, "text", line, signature)?);
    if text.is_empty() {
        return Err(script_error(line, "contains(text) не может быть пустым"));
    }

    Ok(text)
}

fn compile_board_var(
    args: &[ScriptArg],
    line: usize,
    steps: &mut Vec<ScriptStep>,
) -> Result<(), String> {
    let signature = "board.var(name: counter, value: counter + 1)";
    expect_named_args(args, &["name", "value"], line, signature)?;

    let name = value_as_board_variable_name(named_arg(args, "name", line, signature)?, line)?;
    let value = value_as_board_number_expression(named_arg(args, "value", line, signature)?, line)?;
    push_command(steps, line, format!("let {name} = {value}"))
}

fn compile_run(args: &[ScriptArg], line: usize, steps: &mut Vec<ScriptStep>) -> Result<(), String> {
    let signature = "run(name: program)";
    expect_named_args(args, &["name", "program"], line, signature)?;
    let name = value_as_program_name(
        named_arg_any(args, &["name", "program"], line, signature)?,
        line,
    )?;
    push_command(steps, line, format!("run {name}"))
}

fn compile_delete(
    args: &[ScriptArg],
    line: usize,
    steps: &mut Vec<ScriptStep>,
) -> Result<(), String> {
    let signature = "delete(name: program)";
    expect_named_args(args, &["name", "program"], line, signature)?;
    let name = value_as_program_name(
        named_arg_any(args, &["name", "program"], line, signature)?,
        line,
    )?;
    push_command(steps, line, format!("delete {name}"))
}

fn compile_autorun(
    args: &[ScriptArg],
    line: usize,
    steps: &mut Vec<ScriptStep>,
) -> Result<(), String> {
    if args.is_empty() {
        return push_command(steps, line, "autorun");
    }

    let signature = "autorun(name: program) или autorun(state: off)";
    expect_named_args(
        args,
        &["name", "program", "state", "enabled"],
        line,
        signature,
    )?;

    let state_arg =
        optional_named_arg(args, "state").or_else(|| optional_named_arg(args, "enabled"));
    let name_arg = optional_named_arg(args, "name").or_else(|| optional_named_arg(args, "program"));
    if state_arg.is_some() && name_arg.is_some() {
        return Err(script_error(
            line,
            "autorun принимает либо name/program, либо state/enabled, но не оба сразу",
        ));
    }

    if let Some(value) = state_arg {
        let state = value_as_state(value, line)?;
        if state == "toggle" {
            return Err(script_error(
                line,
                "autorun поддерживает только off/false для отключения",
            ));
        }

        if state == "on" {
            return Err(script_error(
                line,
                "autorun(state: on) требует имя: autorun(name: program)",
            ));
        }

        return push_command(steps, line, "autorun off");
    }

    let name = value_as_program_name(
        named_arg_any(args, &["name", "program"], line, signature)?,
        line,
    )?;
    push_command(steps, line, format!("autorun {name}"))
}

fn compile_blink(
    args: &[ScriptArg],
    line: usize,
    steps: &mut Vec<ScriptStep>,
    resources: &mut BoardResources,
) -> Result<(), String> {
    resources.claim_pin(2, "led", line)?;
    let arg = single_arg(
        args,
        &["ms", "state", "enabled"],
        line,
        "blink(ms: ms(500))",
    )?;

    match arg {
        ScriptValue::Number(ms) | ScriptValue::DurationMs(ms) => {
            if !(50..=60_000).contains(ms) {
                return Err(script_error(
                    line,
                    "blink(ms) должен быть в диапазоне 50..60000",
                ));
            }
            push_command(steps, line, format!("blink {ms}"))
        }
        ScriptValue::Bool(false) => push_command(steps, line, "blink off"),
        ScriptValue::Bool(true) => Err(script_error(
            line,
            "для blink(enabled: true) нужен ms, например blink(ms: 500)",
        )),
        ScriptValue::Ident(value) | ScriptValue::Text(value)
            if value.eq_ignore_ascii_case("off") =>
        {
            push_command(steps, line, "blink off")
        }
        ScriptValue::Ident(_) | ScriptValue::Text(_) => Err(script_error(
            line,
            "blink принимает ms: число или state: off",
        )),
        _ => Err(script_error(
            line,
            "blink(ms) ожидает ms(...), число или state: off",
        )),
    }
}

fn compile_pin(
    args: &[ScriptArg],
    line: usize,
    steps: &mut Vec<ScriptStep>,
    resources: &mut BoardResources,
) -> Result<(), String> {
    let signature = "pin(id: pin(2), mode: output)";
    expect_named_args(args, &["id", "pin", "mode", "state"], line, signature)?;

    let pin = value_as_pin(named_arg_any(args, &["id", "pin"], line, signature)?, line)?;
    let mode = optional_named_arg(args, "mode");
    let state = optional_named_arg(args, "state");

    match (mode, state) {
        (Some(mode), None) => {
            let mode = value_as_pin_mode(mode, line)?;
            if pin_mode_needs_output(mode) {
                ensure_output_capable_pin(pin, line, "pin output")?;
            }
            resources.claim_pin(pin, "pin", line)?;
            push_command(steps, line, format!("pin {pin} mode {mode}"))
        }
        (None, Some(state)) => {
            ensure_output_capable_pin(pin, line, "pin.write")?;
            resources.claim_pin(pin, "pin", line)?;
            let state = value_as_state(state, line)?;
            if state == "toggle" {
                push_command(steps, line, format!("pin {pin} toggle"))
            } else {
                push_command(steps, line, format!("pin {pin} write {state}"))
            }
        }
        (None, None) => {
            resources.claim_pin(pin, "pin", line)?;
            push_command(steps, line, format!("pin {pin} read"))
        }
        (Some(_), Some(_)) => Err(script_error(
            line,
            "pin(...) принимает либо mode, либо state, но не оба сразу",
        )),
    }
}

fn compile_pin_read(
    args: &[ScriptArg],
    line: usize,
    steps: &mut Vec<ScriptStep>,
    resources: &mut BoardResources,
) -> Result<(), String> {
    let signature = "pin.read(id: pin(2))";
    expect_named_args(args, &["id", "pin"], line, signature)?;
    let pin = value_as_pin(named_arg_any(args, &["id", "pin"], line, signature)?, line)?;
    resources.claim_pin(pin, "pin", line)?;
    push_command(steps, line, format!("pin {pin} read"))
}

fn compile_pin_write(
    args: &[ScriptArg],
    line: usize,
    steps: &mut Vec<ScriptStep>,
    resources: &mut BoardResources,
) -> Result<(), String> {
    let signature = "pin.write(id: pin(2), state: on)";
    expect_named_args(args, &["id", "pin", "state"], line, signature)?;
    let pin = value_as_pin(named_arg_any(args, &["id", "pin"], line, signature)?, line)?;
    ensure_output_capable_pin(pin, line, "pin.write")?;
    resources.claim_pin(pin, "pin", line)?;
    let state = value_as_state(named_arg(args, "state", line, signature)?, line)?;
    if state == "toggle" {
        push_command(steps, line, format!("pin {pin} toggle"))
    } else {
        push_command(steps, line, format!("pin {pin} write {state}"))
    }
}

fn compile_pwm(
    args: &[ScriptArg],
    line: usize,
    steps: &mut Vec<ScriptStep>,
    resources: &mut BoardResources,
) -> Result<(), String> {
    let signature = "pwm(pin: pin(2), freq: hz(1000), duty: 512)";
    expect_named_args(
        args,
        &["pin", "id", "freq", "frequency", "duty"],
        line,
        signature,
    )?;

    let pin = value_as_pin(named_arg_any(args, &["pin", "id"], line, signature)?, line)?;
    ensure_pwm_capable_pin(pin, line)?;
    let freq = value_as_hz(
        named_arg_any(args, &["freq", "frequency"], line, signature)?,
        line,
    )?;
    if !(1..=1_000_000).contains(&freq) {
        return Err(script_error(
            line,
            "pwm(freq) должен быть в диапазоне 1..1000000 Hz",
        ));
    }

    let duty = value_as_number(named_arg(args, "duty", line, signature)?, line)?;
    if duty > 1023 {
        return Err(script_error(
            line,
            "pwm(duty) должен быть в диапазоне 0..1023",
        ));
    }

    resources.claim_pin(pin, "pwm", line)?;
    push_command(steps, line, format!("pwm {pin} freq={freq} duty={duty}"))
}

fn compile_pwm_stop(
    args: &[ScriptArg],
    line: usize,
    steps: &mut Vec<ScriptStep>,
    resources: &mut BoardResources,
) -> Result<(), String> {
    let signature = "pwm.stop(pin: pin(2))";
    expect_named_args(args, &["pin", "id"], line, signature)?;
    let pin = value_as_pin(named_arg_any(args, &["pin", "id"], line, signature)?, line)?;
    validate_esp32_gpio(pin, line)?;
    resources.release_pin_if_owner(pin, "pwm");
    push_command(steps, line, format!("pwm {pin} stop"))
}

fn compile_adc(
    args: &[ScriptArg],
    line: usize,
    steps: &mut Vec<ScriptStep>,
    resources: &mut BoardResources,
) -> Result<(), String> {
    let signature = "adc(pin: pin(34), max: volt(3.3))";
    expect_named_args(args, &["pin", "id", "max", "reference"], line, signature)?;
    let pin = value_as_pin(named_arg_any(args, &["pin", "id"], line, signature)?, line)?;
    ensure_adc_capable_pin(pin, line, resources)?;

    let max_mv = optional_named_arg(args, "max")
        .or_else(|| optional_named_arg(args, "reference"))
        .map(|value| value_as_millivolts(value, line))
        .transpose()?;
    if let Some(max_mv) = max_mv
        && !(1..=3900).contains(&max_mv)
    {
        return Err(script_error(
            line,
            "adc(max) должен быть в диапазоне volt(0.001)..volt(3.9)",
        ));
    }

    resources.claim_pin(pin, "adc", line)?;
    resources.note_adc_pin(pin);

    if let Some(max_mv) = max_mv {
        push_command(steps, line, format!("adc read {pin} max_mv={max_mv}"))
    } else {
        push_command(steps, line, format!("adc read {pin}"))
    }
}

fn compile_i2c(
    args: &[ScriptArg],
    line: usize,
    steps: &mut Vec<ScriptStep>,
    resources: &mut BoardResources,
) -> Result<(), String> {
    let signature = "i2c(sda: pin(21), scl: pin(22), speed: hz(400000))";
    expect_named_args(
        args,
        &["sda", "scl", "speed", "freq", "frequency"],
        line,
        signature,
    )?;
    let sda = value_as_pin(named_arg(args, "sda", line, signature)?, line)?;
    let scl = value_as_pin(named_arg(args, "scl", line, signature)?, line)?;
    if sda == scl {
        return Err(script_error(
            line,
            "i2c: SDA и SCL должны быть разными GPIO",
        ));
    }

    ensure_output_capable_pin(sda, line, "i2c SDA")?;
    ensure_output_capable_pin(scl, line, "i2c SCL")?;
    let speed = value_as_hz(
        named_arg_any(args, &["speed", "freq", "frequency"], line, signature)?,
        line,
    )?;
    if !(1..=1_000_000).contains(&speed) {
        return Err(script_error(
            line,
            "i2c(speed) должен быть в диапазоне 1..1000000 Hz",
        ));
    }

    resources.claim_i2c(sda, scl, line)?;
    resources.claim_pin(sda, "i2c", line)?;
    resources.claim_pin(scl, "i2c", line)?;
    push_command(
        steps,
        line,
        format!("i2c sda={sda} scl={scl} speed={speed}"),
    )
}

fn compile_spi(
    args: &[ScriptArg],
    line: usize,
    steps: &mut Vec<ScriptStep>,
    resources: &mut BoardResources,
) -> Result<(), String> {
    let signature =
        "spi(sck: pin(18), miso: pin(19), mosi: pin(23), cs: pin(5), speed: hz(1000000))";
    expect_named_args(
        args,
        &["sck", "miso", "mosi", "cs", "speed", "freq", "frequency"],
        line,
        signature,
    )?;

    let sck = value_as_pin(named_arg(args, "sck", line, signature)?, line)?;
    let miso = value_as_pin(named_arg(args, "miso", line, signature)?, line)?;
    let mosi = value_as_pin(named_arg(args, "mosi", line, signature)?, line)?;
    let cs = optional_named_arg(args, "cs")
        .map(|value| value_as_pin(value, line))
        .transpose()?;
    ensure_distinct_pins(&[Some(sck), Some(miso), Some(mosi), cs], line, "spi")?;
    ensure_output_capable_pin(sck, line, "spi SCK")?;
    ensure_output_capable_pin(mosi, line, "spi MOSI")?;
    validate_esp32_gpio(miso, line)?;
    if let Some(cs) = cs {
        ensure_output_capable_pin(cs, line, "spi CS")?;
    }

    let speed = value_as_hz(
        named_arg_any(args, &["speed", "freq", "frequency"], line, signature)?,
        line,
    )?;
    if !(1..=80_000_000).contains(&speed) {
        return Err(script_error(
            line,
            "spi(speed) должен быть в диапазоне 1..80000000 Hz",
        ));
    }

    resources.claim_spi(sck, miso, mosi, cs, line)?;
    resources.claim_pin(sck, "spi", line)?;
    resources.claim_pin(miso, "spi", line)?;
    resources.claim_pin(mosi, "spi", line)?;
    if let Some(cs) = cs {
        resources.claim_pin(cs, "spi", line)?;
        push_command(
            steps,
            line,
            format!("spi sck={sck} miso={miso} mosi={mosi} cs={cs} speed={speed}"),
        )
    } else {
        push_command(
            steps,
            line,
            format!("spi sck={sck} miso={miso} mosi={mosi} speed={speed}"),
        )
    }
}

fn compile_uart(
    args: &[ScriptArg],
    line: usize,
    steps: &mut Vec<ScriptStep>,
    resources: &mut BoardResources,
) -> Result<(), String> {
    if is_raw_uart_call(args) {
        let command = value_as_text(single_arg(
            args,
            &["text", "command"],
            line,
            "uart(text: \"status\")",
        )?);
        return push_command(steps, line, command);
    }

    let signature = "uart(tx: pin(17), rx: pin(16), baud: hz(115200))";
    expect_named_args(args, &["tx", "rx", "baud", "speed"], line, signature)?;
    let tx = value_as_pin(named_arg(args, "tx", line, signature)?, line)?;
    let rx = value_as_pin(named_arg(args, "rx", line, signature)?, line)?;
    if tx == rx {
        return Err(script_error(line, "uart: TX и RX должны быть разными GPIO"));
    }

    ensure_output_capable_pin(tx, line, "uart TX")?;
    validate_esp32_gpio(rx, line)?;
    let baud = value_as_hz(
        named_arg_any(args, &["baud", "speed"], line, signature)?,
        line,
    )?;
    if !(300..=5_000_000).contains(&baud) {
        return Err(script_error(
            line,
            "uart(baud) должен быть в диапазоне 300..5000000",
        ));
    }

    resources.claim_pin(tx, "uart", line)?;
    resources.claim_pin(rx, "uart", line)?;
    push_command(steps, line, format!("uart tx={tx} rx={rx} baud={baud}"))
}

fn compile_wifi(
    args: &[ScriptArg],
    line: usize,
    steps: &mut Vec<ScriptStep>,
    resources: &mut BoardResources,
) -> Result<(), String> {
    let signature = "wifi(ssid: \"name\", password: \"secret\")";
    expect_named_args(
        args,
        &["ssid", "password", "enabled", "state"],
        line,
        signature,
    )?;

    if let Some(value) =
        optional_named_arg(args, "enabled").or_else(|| optional_named_arg(args, "state"))
    {
        let state = value_as_state(value, line)?;
        if state == "toggle" {
            return Err(script_error(
                line,
                "wifi поддерживает только on/off или true/false",
            ));
        }

        if state == "on" {
            resources.enable_wifi(line)?;
        } else {
            resources.disable_wifi();
        }
        return push_command(steps, line, format!("wifi {state}"));
    }

    let ssid = value_as_command_text(
        named_arg(args, "ssid", line, signature)?,
        line,
        "wifi(ssid)",
    )?;
    if ssid.is_empty() {
        return Err(script_error(line, "wifi(ssid) не может быть пустым"));
    }

    let password = optional_named_arg(args, "password")
        .map(|value| value_as_command_text(value, line, "wifi(password)"))
        .transpose()?;
    resources.enable_wifi(line)?;

    if let Some(password) = password {
        push_command(
            steps,
            line,
            format!(
                "wifi connect ssid={} password={}",
                quote_command_arg(&ssid),
                quote_command_arg(&password)
            ),
        )
    } else {
        push_command(
            steps,
            line,
            format!("wifi connect ssid={}", quote_command_arg(&ssid)),
        )
    }
}

fn compile_timer(
    args: &[ScriptArg],
    line: usize,
    steps: &mut Vec<ScriptStep>,
) -> Result<(), String> {
    let (id, mode, duration) = timer_parts(args, line)?;
    push_command(steps, line, format!("timer {id} {mode} {duration}"))
}

fn compile_timer_stop(
    args: &[ScriptArg],
    line: usize,
    steps: &mut Vec<ScriptStep>,
) -> Result<(), String> {
    let signature = "timer.stop(id: 0)";
    expect_named_args(args, &["id"], line, signature)?;
    let id = timer_id(named_arg(args, "id", line, signature)?, line)?;
    push_command(steps, line, format!("timer {id} stop"))
}

fn timer_parts(args: &[ScriptArg], line: usize) -> Result<(u64, &'static str, u64), String> {
    let signature = "timer(id: 0, every: ms(1000))";
    expect_named_args(args, &["id", "every", "after"], line, signature)?;
    let id = timer_id(named_arg(args, "id", line, signature)?, line)?;

    let every = optional_named_arg(args, "every");
    let after = optional_named_arg(args, "after");
    let (mode, duration) = match (every, after) {
        (Some(value), None) => ("every", value_as_ms(value, line)?),
        (None, Some(value)) => ("after", value_as_ms(value, line)?),
        (None, None) => {
            return Err(script_error(
                line,
                "timer требует every: ms(...) или after: ms(...)",
            ));
        }
        (Some(_), Some(_)) => {
            return Err(script_error(
                line,
                "timer принимает либо every, либо after, но не оба сразу",
            ));
        }
    };

    if !(1..=86_400_000).contains(&duration) {
        return Err(script_error(
            line,
            "timer duration должен быть в диапазоне 1..86400000 ms",
        ));
    }

    Ok((id, mode, duration))
}

fn timer_sugar_parts(mode: &str, args: &[ScriptArg], line: usize) -> Result<(u64, u64), String> {
    const EVERY_ARGS: &[&str] = &["id", "ms", "interval", "duration", "every"];
    const AFTER_ARGS: &[&str] = &["id", "ms", "delay", "duration", "after"];

    let (signature, names): (&str, &[&str]) = match mode {
        "every" => ("every(id: 0, ms: ms(1000)) { ... }", EVERY_ARGS),
        "after" => ("after(id: 1, ms: ms(5000)) { ... }", AFTER_ARGS),
        _ => {
            return Err(script_error(
                line,
                format!("неизвестный timer-sugar `{mode}`"),
            ));
        }
    };
    expect_named_args(args, names, line, signature)?;

    let id = optional_named_arg(args, "id")
        .map(|value| timer_id(value, line))
        .transpose()?
        .unwrap_or(0);
    let duration_arg = if mode == "every" {
        named_arg_any(
            args,
            &["ms", "interval", "duration", "every"],
            line,
            signature,
        )?
    } else {
        named_arg_any(args, &["ms", "delay", "duration", "after"], line, signature)?
    };
    let duration = value_as_ms(duration_arg, line)?;
    if !(1..=86_400_000).contains(&duration) {
        return Err(script_error(
            line,
            format!("{mode} duration должен быть в диапазоне 1..86400000 ms"),
        ));
    }

    Ok((id, duration))
}

fn timer_id(value: &ScriptValue, line: usize) -> Result<u64, String> {
    let id = value_as_number(value, line)?;
    if id > 3 {
        return Err(script_error(line, "timer(id) должен быть в диапазоне 0..3"));
    }

    Ok(id)
}

fn compile_sleep(
    args: &[ScriptArg],
    line: usize,
    steps: &mut Vec<ScriptStep>,
) -> Result<(), String> {
    let signature = "sleep(duration: ms(1000))";
    expect_named_args(args, &["duration", "ms"], line, signature)?;
    let duration = value_as_ms(
        named_arg_any(args, &["duration", "ms"], line, signature)?,
        line,
    )?;
    if !(1..=86_400_000).contains(&duration) {
        return Err(script_error(
            line,
            "sleep duration должен быть в диапазоне 1..86400000 ms",
        ));
    }

    push_command(steps, line, format!("sleep {duration}"))
}

fn compile_on_pin_off(
    args: &[ScriptArg],
    line: usize,
    steps: &mut Vec<ScriptStep>,
    resources: &mut BoardResources,
) -> Result<(), String> {
    let signature = "on.pin.off(id: pin(0))";
    expect_named_args(args, &["id", "pin"], line, signature)?;
    let pin = value_as_pin(named_arg_any(args, &["id", "pin"], line, signature)?, line)?;
    validate_esp32_gpio(pin, line)?;
    resources.release_pin_if_owner(pin, "pin");
    push_command(steps, line, format!("on pin {pin} off"))
}

fn host_repeat_times(args: &[ScriptArg], line: usize) -> Result<usize, String> {
    let times = value_as_number(
        single_arg(args, &["times"], line, "repeat(times: 3)")?,
        line,
    )?;
    if times > 1_000 {
        return Err(script_error(line, "repeat(times) максимум 1000"));
    }

    Ok(times as usize)
}

fn board_repeat_times(args: &[ScriptArg], line: usize) -> Result<String, String> {
    let value = single_arg(args, &["times"], line, "repeat(times: 3)")?;

    match value {
        ScriptValue::Number(times) => {
            if !(1..=1_000).contains(times) {
                return Err(script_error(
                    line,
                    "board-side repeat(times) должен быть 1..1000",
                ));
            }

            Ok(times.to_string())
        }
        ScriptValue::Ident(name)
            if is_valid_board_name(name, 16) && !is_reserved_board_variable_name(name) =>
        {
            Ok(name.clone())
        }
        _ => Err(script_error(
            line,
            "board-side repeat(times) ожидает число 1..1000 или переменную платы",
        )),
    }
}

fn expect_no_args(args: &[ScriptArg], line: usize, signature: &str) -> Result<(), String> {
    if args.is_empty() {
        Ok(())
    } else {
        Err(script_error(
            line,
            format!("{signature} не принимает аргументы"),
        ))
    }
}

fn single_arg<'a>(
    args: &'a [ScriptArg],
    names: &[&str],
    line: usize,
    signature: &str,
) -> Result<&'a ScriptValue, String> {
    if args.len() != 1 {
        return Err(script_error(
            line,
            format!("ожидался один аргумент: {signature}"),
        ));
    }

    if let Some(name) = &args[0].name
        && !names.iter().any(|allowed| name == *allowed)
    {
        return Err(script_error(
            line,
            format!("неизвестный аргумент `{name}`, ожидался {signature}"),
        ));
    }

    Ok(&args[0].value)
}

fn expect_named_args(
    args: &[ScriptArg],
    names: &[&str],
    line: usize,
    signature: &str,
) -> Result<(), String> {
    let mut seen = Vec::new();

    for arg in args {
        let Some(name) = &arg.name else {
            return Err(script_error(
                line,
                format!("аргументы должны быть именованными: {signature}"),
            ));
        };

        if seen.contains(&name.as_str()) {
            return Err(script_error(
                line,
                format!("аргумент `{name}` указан дважды"),
            ));
        }

        if !names.iter().any(|allowed| name == *allowed) {
            return Err(script_error(
                line,
                format!("неизвестный аргумент `{name}`, ожидался {signature}"),
            ));
        }

        seen.push(name.as_str());
    }

    Ok(())
}

fn named_arg<'a>(
    args: &'a [ScriptArg],
    name: &str,
    line: usize,
    signature: &str,
) -> Result<&'a ScriptValue, String> {
    args.iter()
        .find(|arg| arg.name.as_deref() == Some(name))
        .map(|arg| &arg.value)
        .ok_or_else(|| {
            script_error(
                line,
                format!("нет аргумента `{name}`, ожидался {signature}"),
            )
        })
}

fn named_arg_any<'a>(
    args: &'a [ScriptArg],
    names: &[&str],
    line: usize,
    signature: &str,
) -> Result<&'a ScriptValue, String> {
    let mut found = None;

    for arg in args {
        let Some(name) = &arg.name else {
            continue;
        };

        if names.iter().any(|allowed| name == *allowed) {
            if found.is_some() {
                return Err(script_error(
                    line,
                    format!("укажи только один из аргументов: {}", names.join(", ")),
                ));
            }

            found = Some(&arg.value);
        }
    }

    found.ok_or_else(|| {
        script_error(
            line,
            format!(
                "нет аргумента `{}`, ожидался {signature}",
                names.join("` или `")
            ),
        )
    })
}

fn optional_named_arg<'a>(args: &'a [ScriptArg], name: &str) -> Option<&'a ScriptValue> {
    args.iter()
        .find(|arg| arg.name.as_deref() == Some(name))
        .map(|arg| &arg.value)
}

#[derive(Clone, Default)]
struct BoardResources {
    pins: HashMap<u8, String>,
    i2c: Option<(u8, u8)>,
    spi: Option<SpiPins>,
    wifi_enabled: bool,
    adc2_in_use: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct SpiPins {
    sck: u8,
    miso: u8,
    mosi: u8,
    cs: Option<u8>,
}

impl BoardResources {
    fn claim_pin(&mut self, pin: u8, owner: &str, line: usize) -> Result<(), String> {
        validate_esp32_gpio(pin, line)?;

        if let Some(existing) = self.pins.get(&pin) {
            if existing == owner {
                return Ok(());
            }

            return Err(script_error(
                line,
                format!("GPIO {pin} уже занят для {existing}; нельзя использовать для {owner}"),
            ));
        }

        self.pins.insert(pin, owner.to_owned());
        Ok(())
    }

    fn release_pin_if_owner(&mut self, pin: u8, owner: &str) {
        if self
            .pins
            .get(&pin)
            .is_some_and(|existing| existing == owner)
        {
            self.pins.remove(&pin);
        }
    }

    fn claim_i2c(&mut self, sda: u8, scl: u8, line: usize) -> Result<(), String> {
        if let Some((used_sda, used_scl)) = self.i2c {
            if used_sda == sda && used_scl == scl {
                return Ok(());
            }

            return Err(script_error(
                line,
                format!(
                    "I2C уже настроен на SDA GPIO{used_sda}, SCL GPIO{used_scl}; второй I2C пока не поддержан"
                ),
            ));
        }

        self.i2c = Some((sda, scl));
        Ok(())
    }

    fn claim_spi(
        &mut self,
        sck: u8,
        miso: u8,
        mosi: u8,
        cs: Option<u8>,
        line: usize,
    ) -> Result<(), String> {
        let pins = SpiPins {
            sck,
            miso,
            mosi,
            cs,
        };
        if let Some(used) = self.spi {
            if used == pins {
                return Ok(());
            }

            return Err(script_error(
                line,
                format!(
                    "SPI уже настроен на SCK GPIO{}, MISO GPIO{}, MOSI GPIO{}; второй SPI пока не поддержан",
                    used.sck, used.miso, used.mosi
                ),
            ));
        }

        self.spi = Some(pins);
        Ok(())
    }

    fn enable_wifi(&mut self, line: usize) -> Result<(), String> {
        if self.adc2_in_use {
            return Err(script_error(
                line,
                "Wi-Fi конфликтует с ADC2 на ESP32; используй ADC1 GPIO32..39 или отключи ADC2",
            ));
        }

        self.wifi_enabled = true;
        Ok(())
    }

    fn disable_wifi(&mut self) {
        self.wifi_enabled = false;
    }

    fn note_adc_pin(&mut self, pin: u8) {
        if is_adc2_pin(pin) {
            self.adc2_in_use = true;
        }
    }
}

fn value_as_pin(value: &ScriptValue, line: usize) -> Result<u8, String> {
    match value {
        ScriptValue::Pin(pin) => Ok(*pin),
        _ => Err(script_error(
            line,
            "ожидался тип pin(...), например pin(21)",
        )),
    }
}

fn condition_pin_arg(args: &[ScriptArg], line: usize, signature: &str) -> Result<u8, String> {
    if args.len() != 1 {
        return Err(script_error(
            line,
            format!("ожидался один GPIO: {signature}"),
        ));
    }

    if let Some(name) = &args[0].name
        && !matches!(name.as_str(), "id" | "pin")
    {
        return Err(script_error(
            line,
            format!("неизвестный аргумент `{name}`, ожидался {signature}"),
        ));
    }

    value_as_pin_or_number(&args[0].value, line)
}

fn value_as_pin_or_number(value: &ScriptValue, line: usize) -> Result<u8, String> {
    match value {
        ScriptValue::Pin(pin) => Ok(*pin),
        ScriptValue::Number(pin) if *pin <= u8::MAX as u64 => Ok(*pin as u8),
        ScriptValue::Number(_) => Err(script_error(line, "GPIO должен быть в диапазоне 0..255")),
        _ => Err(script_error(
            line,
            "ожидался GPIO: pin(...), например pin(21)",
        )),
    }
}

fn value_as_program_name(value: &ScriptValue, line: usize) -> Result<String, String> {
    let name = match value {
        ScriptValue::Ident(value) | ScriptValue::Text(value) => value.clone(),
        _ => {
            return Err(script_error(
                line,
                "имя программы должно быть идентификатором или строкой",
            ));
        }
    };

    if is_valid_board_name(&name, 16) {
        Ok(name)
    } else {
        Err(script_error(
            line,
            "имя программы: ASCII буква или `_` в начале, дальше ASCII буквы/цифры/`_`, максимум 16 символов",
        ))
    }
}

fn value_as_board_variable_name(value: &ScriptValue, line: usize) -> Result<String, String> {
    let name = match value {
        ScriptValue::Ident(value) | ScriptValue::Text(value) => value.clone(),
        _ => {
            return Err(script_error(
                line,
                "имя board-переменной должно быть идентификатором или строкой",
            ));
        }
    };

    if !is_valid_board_name(&name, 16) {
        return Err(script_error(
            line,
            "имя board-переменной: ASCII буква или `_` в начале, дальше ASCII буквы/цифры/`_`, максимум 16 символов",
        ));
    }

    if is_reserved_board_variable_name(&name) {
        return Err(script_error(
            line,
            format!("`{name}` нельзя использовать как имя board-переменной"),
        ));
    }

    Ok(name)
}

fn value_as_pin_trigger(value: &ScriptValue, line: usize) -> Result<&'static str, String> {
    match value {
        ScriptValue::Ident(value) | ScriptValue::Text(value)
            if value.eq_ignore_ascii_case("rising") =>
        {
            Ok("rising")
        }
        ScriptValue::Ident(value) | ScriptValue::Text(value)
            if value.eq_ignore_ascii_case("falling") =>
        {
            Ok("falling")
        }
        ScriptValue::Ident(value) | ScriptValue::Text(value)
            if value.eq_ignore_ascii_case("change") =>
        {
            Ok("change")
        }
        _ => Err(script_error(
            line,
            "trigger должен быть rising, falling или change",
        )),
    }
}

fn value_as_board_level(value: &ScriptValue, line: usize) -> Result<&'static str, String> {
    match value {
        ScriptValue::Bool(true) => Ok("on"),
        ScriptValue::Bool(false) => Ok("off"),
        ScriptValue::Number(1) => Ok("on"),
        ScriptValue::Number(0) => Ok("off"),
        ScriptValue::Ident(value) | ScriptValue::Text(value)
            if matches_ignore_ascii_case(value, &["on", "high", "true", "1"]) =>
        {
            Ok("on")
        }
        ScriptValue::Ident(value) | ScriptValue::Text(value)
            if matches_ignore_ascii_case(value, &["off", "low", "false", "0"]) =>
        {
            Ok("off")
        }
        _ => Err(script_error(
            line,
            "условие платы ожидает уровень on/off, high/low, true/false или 1/0",
        )),
    }
}

fn value_as_board_number_token(value: &ScriptValue, line: usize) -> Result<String, String> {
    match value {
        ScriptValue::Ident(value) => Ok(value.clone()),
        ScriptValue::Number(value)
        | ScriptValue::DurationMs(value)
        | ScriptValue::FrequencyHz(value)
        | ScriptValue::Millivolts(value) => Ok(value.to_string()),
        _ => Err(script_error(
            line,
            "числовое условие платы ожидает число или переменную платы",
        )),
    }
}

fn value_as_board_number_expression(value: &ScriptValue, line: usize) -> Result<String, String> {
    match value {
        ScriptValue::BoardNumberExpr { left, op, right } => Ok(format!(
            "{} {op} {}",
            value_as_board_number_token(left, line)?,
            value_as_board_number_token(right, line)?
        )),
        _ => value_as_board_number_token(value, line),
    }
}

fn ensure_bool_compare_operator(op: &str, line: usize) -> Result<(), String> {
    if matches!(op, "==" | "!=") {
        Ok(())
    } else {
        Err(script_error(
            line,
            "условия led/heartbeat/wifi/pin поддерживают только == или !=",
        ))
    }
}

fn value_as_ms(value: &ScriptValue, line: usize) -> Result<u64, String> {
    match value {
        ScriptValue::DurationMs(value) => Ok(*value),
        _ => Err(script_error(line, "ожидался тип ms(...), например ms(250)")),
    }
}

fn value_as_ms_or_number(value: &ScriptValue, line: usize) -> Result<u64, String> {
    match value {
        ScriptValue::DurationMs(value) | ScriptValue::Number(value) => Ok(*value),
        ScriptValue::FrequencyHz(_) => Err(script_error(
            line,
            "ожидалось время ms(...), получено hz(...)",
        )),
        ScriptValue::Millivolts(_) => Err(script_error(
            line,
            "ожидалось время ms(...), получено volt(...)",
        )),
        ScriptValue::Pin(_) => Err(script_error(
            line,
            "ожидалось время ms(...), получено pin(...)",
        )),
        _ => Err(script_error(line, "ожидалось время ms(...)")),
    }
}

fn value_as_hz(value: &ScriptValue, line: usize) -> Result<u64, String> {
    match value {
        ScriptValue::FrequencyHz(value) => Ok(*value),
        _ => Err(script_error(
            line,
            "ожидался тип hz(...), например hz(400000)",
        )),
    }
}

fn value_as_millivolts(value: &ScriptValue, line: usize) -> Result<u64, String> {
    match value {
        ScriptValue::Millivolts(value) => Ok(*value),
        _ => Err(script_error(
            line,
            "ожидался тип volt(...), например volt(3.3)",
        )),
    }
}

fn value_as_pin_mode(value: &ScriptValue, line: usize) -> Result<&'static str, String> {
    match value {
        ScriptValue::Ident(value) | ScriptValue::Text(value)
            if value.eq_ignore_ascii_case("input") =>
        {
            Ok("input")
        }
        ScriptValue::Ident(value) | ScriptValue::Text(value)
            if value.eq_ignore_ascii_case("input_pullup") =>
        {
            Ok("input_pullup")
        }
        ScriptValue::Ident(value) | ScriptValue::Text(value)
            if value.eq_ignore_ascii_case("input_pulldown") =>
        {
            Ok("input_pulldown")
        }
        ScriptValue::Ident(value) | ScriptValue::Text(value)
            if value.eq_ignore_ascii_case("output") =>
        {
            Ok("output")
        }
        _ => Err(script_error(
            line,
            "pin(mode) должен быть input, input_pullup, input_pulldown или output",
        )),
    }
}

fn pin_mode_needs_output(mode: &str) -> bool {
    mode == "output"
}

fn validate_esp32_gpio(pin: u8, line: usize) -> Result<(), String> {
    if !is_esp32_gpio(pin) {
        return Err(script_error(
            line,
            format!("GPIO {pin} нет на классическом ESP32"),
        ));
    }

    if let Some(reason) = reserved_esp32_pin_reason(pin) {
        return Err(script_error(
            line,
            format!("GPIO {pin} нельзя использовать: {reason}"),
        ));
    }

    Ok(())
}

fn ensure_output_capable_pin(pin: u8, line: usize, feature: &str) -> Result<(), String> {
    validate_esp32_gpio(pin, line)?;
    if is_input_only_pin(pin) {
        return Err(script_error(
            line,
            format!(
                "{feature}: GPIO {pin} только вход, для выхода/PWM используй GPIO 2, 4, 5, 12..19, 21..23, 25..27 или 32..33"
            ),
        ));
    }

    Ok(())
}

fn ensure_pwm_capable_pin(pin: u8, line: usize) -> Result<(), String> {
    ensure_output_capable_pin(pin, line, "pwm")
}

fn ensure_adc_capable_pin(pin: u8, line: usize, resources: &BoardResources) -> Result<(), String> {
    validate_esp32_gpio(pin, line)?;
    if !is_adc_pin(pin) {
        return Err(script_error(
            line,
            format!(
                "GPIO {pin} не поддерживает ADC на ESP32; используй ADC1 GPIO32..39 или ADC2 GPIO0,2,4,12..15,25..27"
            ),
        ));
    }

    if resources.wifi_enabled && is_adc2_pin(pin) {
        return Err(script_error(
            line,
            format!("ADC2 GPIO{pin} нельзя использовать вместе с Wi-Fi; используй ADC1 GPIO32..39"),
        ));
    }

    Ok(())
}

fn ensure_distinct_pins(pins: &[Option<u8>], line: usize, owner: &str) -> Result<(), String> {
    for (index, pin) in pins.iter().enumerate() {
        let Some(pin) = pin else {
            continue;
        };

        if pins[index + 1..].iter().flatten().any(|other| other == pin) {
            return Err(script_error(
                line,
                format!("{owner}: GPIO {pin} указан больше одного раза"),
            ));
        }
    }

    Ok(())
}

fn is_esp32_gpio(pin: u8) -> bool {
    pin <= 39 && !matches!(pin, 20 | 24 | 28 | 29 | 30 | 31)
}

fn reserved_esp32_pin_reason(pin: u8) -> Option<&'static str> {
    match pin {
        1 | 3 => Some("занят UART0 и serial monitor"),
        6..=11 => Some("занят SPI flash-памятью"),
        _ => None,
    }
}

fn is_input_only_pin(pin: u8) -> bool {
    matches!(pin, 34..=39)
}

fn is_adc_pin(pin: u8) -> bool {
    matches!(pin, 32..=39 | 0 | 2 | 4 | 12..=15 | 25..=27)
}

fn is_adc2_pin(pin: u8) -> bool {
    matches!(pin, 0 | 2 | 4 | 12..=15 | 25..=27)
}

fn is_raw_uart_call(args: &[ScriptArg]) -> bool {
    args.len() == 1
        && matches!(
            args[0].name.as_deref(),
            None | Some("text") | Some("command")
        )
}

fn quote_command_arg(value: &str) -> String {
    let mut quoted = String::from("\"");
    for ch in value.chars() {
        match ch {
            '"' => quoted.push_str("\\\""),
            '\\' => quoted.push_str("\\\\"),
            _ => quoted.push(ch),
        }
    }
    quoted.push('"');
    quoted
}

fn script_type(name: &str, line: usize) -> Result<ScriptType, String> {
    optional_script_type(name).ok_or_else(|| {
        script_error(
            line,
            format!("неизвестный тип `{name}`, доступны number, bool, text, ms, hz, volt, pin"),
        )
    })
}

fn optional_script_type(name: &str) -> Option<ScriptType> {
    match name {
        "number" => Some(ScriptType::Number),
        "bool" => Some(ScriptType::Bool),
        "text" | "string" => Some(ScriptType::Text),
        "ms" => Some(ScriptType::Ms),
        "hz" => Some(ScriptType::Hz),
        "volt" => Some(ScriptType::Volt),
        "pin" => Some(ScriptType::Pin),
        _ => None,
    }
}

fn validate_value_type(
    value: &ScriptValue,
    expected: ScriptType,
    line: usize,
    context: impl Into<String>,
) -> Result<(), String> {
    if value_matches_type(value, expected) {
        return Ok(());
    }

    Err(script_error(
        line,
        format!(
            "{} ожидает тип {}, получено {}",
            context.into(),
            script_type_name(expected),
            value_type_name(value)
        ),
    ))
}

fn value_matches_type(value: &ScriptValue, expected: ScriptType) -> bool {
    matches!(
        (value, expected),
        (ScriptValue::Number(_), ScriptType::Number)
            | (ScriptValue::Bool(_), ScriptType::Bool)
            | (ScriptValue::Text(_), ScriptType::Text)
            | (ScriptValue::DurationMs(_), ScriptType::Ms)
            | (ScriptValue::FrequencyHz(_), ScriptType::Hz)
            | (ScriptValue::Millivolts(_), ScriptType::Volt)
            | (ScriptValue::Pin(_), ScriptType::Pin)
    )
}

fn script_type_name(ty: ScriptType) -> &'static str {
    match ty {
        ScriptType::Number => "number",
        ScriptType::Bool => "bool",
        ScriptType::Text => "text",
        ScriptType::Ms => "ms",
        ScriptType::Hz => "hz",
        ScriptType::Volt => "volt",
        ScriptType::Pin => "pin",
    }
}

fn value_type_name(value: &ScriptValue) -> &'static str {
    match value {
        ScriptValue::Ident(_) => "identifier",
        ScriptValue::Number(_) => "number",
        ScriptValue::Bool(_) => "bool",
        ScriptValue::Text(_) => "text",
        ScriptValue::DurationMs(_) => "ms",
        ScriptValue::FrequencyHz(_) => "hz",
        ScriptValue::Millivolts(_) => "volt",
        ScriptValue::Pin(_) => "pin",
        ScriptValue::BoardNumberExpr { .. } => "number expression",
    }
}

fn value_as_number(value: &ScriptValue, line: usize) -> Result<u64, String> {
    match value {
        ScriptValue::Number(value) => Ok(*value),
        _ => Err(script_error(line, "ожидалось число")),
    }
}

fn value_as_state(value: &ScriptValue, line: usize) -> Result<&'static str, String> {
    match value {
        ScriptValue::Bool(true) => Ok("on"),
        ScriptValue::Bool(false) => Ok("off"),
        ScriptValue::Ident(value) | ScriptValue::Text(value)
            if value.eq_ignore_ascii_case("on") =>
        {
            Ok("on")
        }
        ScriptValue::Ident(value) | ScriptValue::Text(value)
            if value.eq_ignore_ascii_case("off") =>
        {
            Ok("off")
        }
        ScriptValue::Ident(value) | ScriptValue::Text(value)
            if value.eq_ignore_ascii_case("toggle") =>
        {
            Ok("toggle")
        }
        _ => Err(script_error(
            line,
            "ожидалось состояние on/off/toggle или true/false",
        )),
    }
}

fn value_as_text(value: &ScriptValue) -> String {
    match value {
        ScriptValue::Ident(value) | ScriptValue::Text(value) => value.clone(),
        ScriptValue::Number(value) => value.to_string(),
        ScriptValue::Bool(true) => "true".to_owned(),
        ScriptValue::Bool(false) => "false".to_owned(),
        ScriptValue::DurationMs(value) => format!("{value}ms"),
        ScriptValue::FrequencyHz(value) => format!("{value}hz"),
        ScriptValue::Millivolts(value) => format!("{value}mV"),
        ScriptValue::Pin(value) => format!("GPIO{value}"),
        ScriptValue::BoardNumberExpr { left, op, right } => {
            format!("{} {op} {}", value_as_text(left), value_as_text(right))
        }
    }
}

fn value_as_command_text(
    value: &ScriptValue,
    line: usize,
    context: &str,
) -> Result<String, String> {
    let text = value_as_text(value);
    validate_command_text(&text, line, context)?;
    Ok(text)
}

fn validate_command_text(value: &str, line: usize, context: &str) -> Result<(), String> {
    if let Some(ch) = value.chars().find(|ch| matches!(ch, ';' | '{' | '}')) {
        return Err(script_error(
            line,
            format!(
                "{context} не может содержать `{ch}`: `;`, `{{` и `}}` являются разделителями команд прошивки"
            ),
        ));
    }

    Ok(())
}

fn value_as_feature_name(value: &ScriptValue, line: usize) -> Result<String, String> {
    let feature = match value {
        ScriptValue::Ident(value) | ScriptValue::Text(value) => value.clone(),
        _ => {
            return Err(script_error(
                line,
                "requires(feature) ожидает имя feature, например pwm_real",
            ));
        }
    };

    validate_feature_name(&feature, line)?;
    Ok(feature)
}

fn value_as_feature_list(value: &ScriptValue, line: usize) -> Result<Vec<String>, String> {
    let text = match value {
        ScriptValue::Ident(value) | ScriptValue::Text(value) => value,
        _ => {
            return Err(script_error(
                line,
                "requires(features) ожидает строку или имя feature",
            ));
        }
    };

    let mut features = Vec::new();
    for feature in text.split(|ch: char| ch == ',' || ch.is_whitespace()) {
        let feature = feature.trim();
        if feature.is_empty() {
            continue;
        }
        validate_feature_name(feature, line)?;
        features.push(feature.to_owned());
    }

    if features.is_empty() {
        return Err(script_error(
            line,
            "requires(features) должен содержать хотя бы одну feature",
        ));
    }

    Ok(features)
}

fn validate_feature_name(feature: &str, line: usize) -> Result<(), String> {
    if !is_valid_feature_name(feature) {
        return Err(script_error(
            line,
            "имя feature: ASCII буквы/цифры/`_`/`-`/`.`, максимум 64 символа",
        ));
    }

    Ok(())
}

fn push_command(
    steps: &mut Vec<ScriptStep>,
    line: usize,
    command: impl Into<String>,
) -> Result<(), String> {
    let command = command.into();
    let command = command.trim();

    if command.is_empty() {
        return Err(script_error(line, "UART-команда пустая"));
    }

    if command.contains('\n') || command.contains('\r') {
        return Err(script_error(
            line,
            "UART-команда должна быть в одной строке",
        ));
    }

    if command.len() > 255 {
        return Err(script_error(
            line,
            "UART-команда длиннее 255 байт, плата ее не примет одной строкой",
        ));
    }

    steps.push(ScriptStep::Send {
        command: command.to_owned(),
        line,
    });
    Ok(())
}

fn steps_as_board_script(
    steps: &[ScriptStep],
    line: usize,
    context: &str,
) -> Result<String, String> {
    let mut commands = Vec::new();

    for step in steps {
        match step {
            ScriptStep::Send { command, .. } => commands.push(command.clone()),
            ScriptStep::Requires { .. } => {
                return Err(script_error(
                    line,
                    format!(
                        "`requires(...)` нельзя использовать внутри `{context}`, это проверка совместимости на стороне приложения"
                    ),
                ));
            }
            ScriptStep::Wait { .. } => {
                return Err(script_error(
                    line,
                    format!(
                        "`wait(...)` выполняется на ПК и нельзя использовать внутри `{context}`; используй `sleep(duration: ms(...))`"
                    ),
                ));
            }
            ScriptStep::Expect { .. } => {
                return Err(script_error(
                    line,
                    format!(
                        "`expect(...)` нельзя использовать внутри `{context}`, этот блок выполняет сама плата"
                    ),
                ));
            }
            ScriptStep::IfContains { .. } => {
                return Err(script_error(
                    line,
                    format!(
                        "`if contains(...)` выполняется на ПК и нельзя использовать внутри `{context}`; используй условие платы вроде `if led == on`"
                    ),
                ));
            }
        }
    }

    Ok(commands.join("; "))
}

fn expanded_step_count(steps: &[ScriptStep]) -> usize {
    steps
        .iter()
        .map(|step| match step {
            ScriptStep::Send { .. }
            | ScriptStep::Requires { .. }
            | ScriptStep::Wait { .. }
            | ScriptStep::Expect { .. } => 1,
            ScriptStep::IfContains {
                then_steps,
                else_steps,
                ..
            } => 1 + expanded_step_count(then_steps) + expanded_step_count(else_steps),
        })
        .sum()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SourceDeclKind {
    Let,
    Function,
}

struct SourceDecl {
    name: String,
    line: usize,
    kind: SourceDeclKind,
}

fn lint_source_text(script: &str, lints: &mut Vec<ScriptLint>) {
    let mut declarations = Vec::new();
    let mut token_lines = Vec::new();

    for (index, source_line) in script.lines().enumerate() {
        let line = index + 1;
        let code = code_without_line_comment(source_line);
        let tokens = ident_tokens(&code);

        if let Some(name) = source_declaration_name(&tokens, "let") {
            declarations.push(SourceDecl {
                name: name.to_owned(),
                line,
                kind: SourceDeclKind::Let,
            });
        } else if let Some(name) = source_declaration_name(&tokens, "fn") {
            declarations.push(SourceDecl {
                name: name.to_owned(),
                line,
                kind: SourceDeclKind::Function,
            });
        }

        if is_cmd_call(&code) {
            push_lint(
                lints,
                line,
                "cmd(...) обходит проверки типов и пинов; предпочитай typed-команды".to_owned(),
            );
        }

        token_lines.push(tokens);
    }

    for declaration in declarations {
        let occurrences = token_lines
            .iter()
            .flatten()
            .filter(|token| token.as_str() == declaration.name)
            .count();

        if occurrences <= 1 {
            let what = match declaration.kind {
                SourceDeclKind::Let => "переменная",
                SourceDeclKind::Function => "функция",
            };
            push_lint(
                lints,
                declaration.line,
                format!(
                    "{what} `{}` объявлена, но не используется",
                    declaration.name
                ),
            );
        }
    }
}

fn source_declaration_name<'a>(tokens: &'a [String], keyword: &str) -> Option<&'a str> {
    if tokens.first().is_some_and(|token| token == keyword) {
        tokens.get(1).map(String::as_str)
    } else {
        None
    }
}

fn is_cmd_call(code: &str) -> bool {
    let trimmed = code.trim_start();
    let Some(rest) = trimmed.strip_prefix("cmd") else {
        return false;
    };

    rest.trim_start().starts_with('(')
}

fn code_without_line_comment(line: &str) -> String {
    let mut in_string = false;
    let mut escaped = false;

    for (index, ch) in line.char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }

        if ch == '"' {
            in_string = true;
        } else if ch == '#' || line[index..].starts_with("//") {
            return line[..index].to_owned();
        }
    }

    line.to_owned()
}

fn ident_tokens(code: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut in_string = false;
    let mut escaped = false;
    let mut chars = code.char_indices().peekable();

    while let Some((index, ch)) = chars.next() {
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }

        if ch == '"' {
            in_string = true;
            continue;
        }

        if !is_ident_start(ch) {
            continue;
        }

        let start = index;
        let mut end = index + ch.len_utf8();
        while let Some((next_index, next_ch)) = chars.peek().copied() {
            if !is_ident_continue(next_ch) {
                break;
            }
            chars.next();
            end = next_index + next_ch.len_utf8();
        }

        tokens.push(code[start..end].to_owned());
    }

    tokens
}

fn lint_compiled_steps(steps: &[ScriptStep], lints: &mut Vec<ScriptLint>) {
    let mut initialized = HashSet::new();
    let mut warned_reads = HashSet::new();
    lint_compiled_steps_inner(steps, &mut initialized, &mut warned_reads, lints);
}

fn lint_compiled_steps_inner(
    steps: &[ScriptStep],
    initialized: &mut HashSet<String>,
    warned_reads: &mut HashSet<String>,
    lints: &mut Vec<ScriptLint>,
) {
    for step in steps {
        match step {
            ScriptStep::Send { command, line } => {
                lint_board_command(command, *line, initialized, warned_reads, lints);
            }
            ScriptStep::Requires { .. } => {}
            ScriptStep::IfContains {
                then_steps,
                else_steps,
                ..
            } => {
                let mut then_initialized = initialized.clone();
                lint_compiled_steps_inner(then_steps, &mut then_initialized, warned_reads, lints);

                let mut else_initialized = initialized.clone();
                lint_compiled_steps_inner(else_steps, &mut else_initialized, warned_reads, lints);
            }
            ScriptStep::Wait { .. } | ScriptStep::Expect { .. } => {}
        }
    }
}

fn lint_board_command(
    command: &str,
    line: usize,
    initialized: &mut HashSet<String>,
    warned_reads: &mut HashSet<String>,
    lints: &mut Vec<ScriptLint>,
) {
    let command = command.trim();
    lint_board_block_limit(command, line, lints);

    if let Some(rest) = command.strip_prefix("let ") {
        if let Some((name, value)) = rest.split_once('=') {
            lint_board_expression(value, line, initialized, warned_reads, lints);
            initialized.insert(name.trim().to_owned());
        }
    } else if let Some(rest) = command.strip_prefix("if ") {
        if let Some((condition, _)) = rest.split_once('{') {
            lint_board_expression(condition, line, initialized, warned_reads, lints);
        }
    } else if let Some(rest) = command.strip_prefix("repeat ") {
        let count = rest.split_once('{').map(|(count, _)| count).unwrap_or(rest);
        lint_board_expression(count, line, initialized, warned_reads, lints);
    }

    for body in top_level_brace_bodies(command) {
        for nested in split_board_commands(body) {
            lint_board_command(nested, line, initialized, warned_reads, lints);
        }
    }
}

fn lint_board_block_limit(command: &str, line: usize, lints: &mut Vec<ScriptLint>) {
    let Some(body) = top_level_brace_bodies(command).into_iter().next() else {
        return;
    };

    let (name, limit) = if command.starts_with("timer ") && command.contains(" do {") {
        ("timer do-блок", 128)
    } else if command.starts_with("on pin ") && command.contains(" do {") {
        ("on.pin do-блок", 128)
    } else if command.starts_with("save ") {
        ("save-блок", 256)
    } else {
        return;
    };

    if body.len() >= limit * 7 / 8 {
        push_lint(
            lints,
            line,
            format!("{name} {} из {limit} байт, близко к лимиту", body.len()),
        );
    }
}

fn lint_board_expression(
    expression: &str,
    line: usize,
    initialized: &HashSet<String>,
    warned_reads: &mut HashSet<String>,
    lints: &mut Vec<ScriptLint>,
) {
    for token in ident_tokens(expression) {
        if !is_possible_board_variable_reference(&token) || initialized.contains(&token) {
            continue;
        }

        if warned_reads.insert(token.clone()) {
            push_lint(
                lints,
                line,
                format!("board-переменная `{token}` читается до первого board.var(...)"),
            );
        }
    }
}

fn is_possible_board_variable_reference(token: &str) -> bool {
    is_valid_board_name(token, 16)
        && !matches!(
            token,
            "let"
                | "if"
                | "else"
                | "repeat"
                | "pin"
                | "led"
                | "heartbeat"
                | "wifi"
                | "on"
                | "off"
                | "high"
                | "low"
                | "true"
                | "false"
                | "do"
                | "every"
                | "after"
                | "stop"
                | "mode"
                | "input"
                | "input_pullup"
                | "input_pulldown"
                | "output"
                | "rising"
                | "falling"
                | "change"
                | "debounce"
        )
}

fn top_level_brace_bodies(command: &str) -> Vec<&str> {
    let mut bodies = Vec::new();
    let mut start = None;
    let mut depth = 0_usize;

    for (index, byte) in command.bytes().enumerate() {
        match byte {
            b'{' => {
                if depth == 0 {
                    start = Some(index + 1);
                }
                depth += 1;
            }
            b'}' => {
                if depth == 0 {
                    continue;
                }
                depth -= 1;
                if depth == 0
                    && let Some(start) = start.take()
                {
                    bodies.push(command[start..index].trim());
                }
            }
            _ => {}
        }
    }

    bodies
}

fn split_board_commands(script: &str) -> Vec<&str> {
    let mut commands = Vec::new();
    let mut start = 0_usize;
    let mut depth = 0_usize;

    for (index, byte) in script.bytes().enumerate() {
        match byte {
            b'{' => depth += 1,
            b'}' => depth = depth.saturating_sub(1),
            b';' if depth == 0 => {
                let command = script[start..index].trim();
                if !command.is_empty() {
                    commands.push(command);
                }
                start = index + 1;
            }
            _ => {}
        }
    }

    let command = script[start..].trim();
    if !command.is_empty() {
        commands.push(command);
    }

    commands
}

fn push_lint(lints: &mut Vec<ScriptLint>, line: usize, message: String) {
    lints.push(ScriptLint { line, message });
}

struct ScriptFormatter<'a> {
    source: &'a str,
    pos: usize,
    indent: usize,
    output: String,
    current: String,
    after_close_brace: bool,
}

impl<'a> ScriptFormatter<'a> {
    fn new(source: &'a str) -> Self {
        Self {
            source,
            pos: 0,
            indent: 0,
            output: String::new(),
            current: String::new(),
            after_close_brace: false,
        }
    }

    fn format(mut self) -> String {
        while let Some(ch) = self.peek_char() {
            if ch.is_whitespace() {
                self.next_char();
                continue;
            }

            if self.starts_with("//") || ch == '#' {
                let comment = self.take_line_comment();
                self.append_comment(&comment);
                continue;
            }

            if ch == '"' {
                let string = self.take_string();
                self.append_token(&string);
                continue;
            }

            if is_ident_start(ch) {
                let ident = self.take_while(is_ident_continue);
                self.append_identifier(&ident);
                continue;
            }

            if ch.is_ascii_digit() {
                let number = self.take_while(|value| value.is_ascii_digit());
                self.append_token(&number);
                continue;
            }

            if let Some(op) = self.take_compare_operator() {
                self.append_operator(op);
                continue;
            }

            match ch {
                '(' => {
                    self.next_char();
                    self.append_open_paren();
                }
                ')' => {
                    self.next_char();
                    self.append_close_paren();
                }
                '{' => {
                    self.next_char();
                    self.append_open_brace();
                }
                '}' => {
                    self.next_char();
                    self.append_close_brace();
                }
                ';' => {
                    self.next_char();
                    self.append_semicolon();
                }
                ',' => {
                    self.next_char();
                    self.append_comma();
                }
                ':' => {
                    self.next_char();
                    self.append_colon();
                }
                '.' => {
                    self.next_char();
                    self.append_dot();
                }
                '=' | '+' | '-' | '*' | '/' | '%' | '<' | '>' | '!' => {
                    self.next_char();
                    self.append_operator(ch.encode_utf8(&mut [0; 4]));
                }
                _ => {
                    self.next_char();
                    self.append_token(ch.encode_utf8(&mut [0; 4]));
                }
            }
        }

        self.flush_line();
        while self.output.ends_with('\n') {
            self.output.pop();
        }
        self.output
    }

    fn append_identifier(&mut self, ident: &str) {
        if self.after_close_brace && ident == "else" {
            self.current.push_str(" else");
            self.after_close_brace = false;
        } else {
            self.append_token(ident);
        }
    }

    fn append_token(&mut self, token: &str) {
        if self.after_close_brace {
            self.flush_line();
        }
        self.ensure_indent();
        if self.needs_space_before(token) {
            self.current.push(' ');
        }
        self.current.push_str(token);
        self.after_close_brace = false;
    }

    fn append_comment(&mut self, comment: &str) {
        self.ensure_indent();
        if self.current.trim_end().len() > self.indent_width() && !self.current.ends_with(' ') {
            self.current.push(' ');
        }
        self.current.push_str(comment.trim_end());
        self.flush_line();
    }

    fn append_open_paren(&mut self) {
        if self.after_close_brace {
            self.flush_line();
        }
        self.ensure_indent();
        trim_trailing_spaces(&mut self.current);
        self.current.push('(');
        self.after_close_brace = false;
    }

    fn append_close_paren(&mut self) {
        self.ensure_indent();
        trim_trailing_spaces(&mut self.current);
        self.current.push(')');
        self.after_close_brace = false;
    }

    fn append_open_brace(&mut self) {
        if self.after_close_brace {
            self.flush_line();
        }
        self.ensure_indent();
        trim_trailing_spaces(&mut self.current);
        if self.current.trim().is_empty() {
            self.current.push('{');
        } else {
            self.current.push(' ');
            self.current.push('{');
        }
        self.flush_line();
        self.indent += 1;
    }

    fn append_close_brace(&mut self) {
        self.flush_line();
        self.indent = self.indent.saturating_sub(1);
        self.ensure_indent();
        self.current.push('}');
        self.after_close_brace = true;
    }

    fn append_semicolon(&mut self) {
        self.ensure_indent();
        trim_trailing_spaces(&mut self.current);
        self.current.push(';');
        self.flush_line();
    }

    fn append_comma(&mut self) {
        self.ensure_indent();
        trim_trailing_spaces(&mut self.current);
        self.current.push_str(", ");
        self.after_close_brace = false;
    }

    fn append_colon(&mut self) {
        self.ensure_indent();
        trim_trailing_spaces(&mut self.current);
        self.current.push_str(": ");
        self.after_close_brace = false;
    }

    fn append_dot(&mut self) {
        self.ensure_indent();
        trim_trailing_spaces(&mut self.current);
        self.current.push('.');
        self.after_close_brace = false;
    }

    fn append_operator(&mut self, op: &str) {
        if self.after_close_brace {
            self.flush_line();
        }
        self.ensure_indent();
        trim_trailing_spaces(&mut self.current);
        if self.current.trim_end().len() > self.indent_width() {
            self.current.push(' ');
        }
        self.current.push_str(op);
        self.current.push(' ');
        self.after_close_brace = false;
    }

    fn ensure_indent(&mut self) {
        if self.current.is_empty() {
            for _ in 0..self.indent {
                self.current.push_str("    ");
            }
        }
    }

    fn flush_line(&mut self) {
        trim_trailing_spaces(&mut self.current);
        if !self.current.trim().is_empty() {
            self.output.push_str(&self.current);
            self.output.push('\n');
        }
        self.current.clear();
        self.after_close_brace = false;
    }

    fn needs_space_before(&self, token: &str) -> bool {
        let Some(last) = self.current.chars().rev().find(|ch| !ch.is_whitespace()) else {
            return false;
        };
        let Some(first) = token.chars().next() else {
            return false;
        };

        if matches!(last, '(' | '.' | ':' | ',') {
            return false;
        }

        (last.is_ascii_alphanumeric() || matches!(last, '_' | ')' | '"'))
            && (first.is_ascii_alphanumeric() || matches!(first, '_' | '"'))
    }

    fn indent_width(&self) -> usize {
        self.indent * 4
    }

    fn take_compare_operator(&mut self) -> Option<&'static str> {
        for op in ["==", "!=", "<=", ">="] {
            if self.starts_with(op) {
                self.pos += op.len();
                return Some(op);
            }
        }

        None
    }

    fn take_line_comment(&mut self) -> String {
        let start = self.pos;
        while let Some(ch) = self.peek_char() {
            if ch == '\n' || ch == '\r' {
                break;
            }
            self.next_char();
        }
        self.source[start..self.pos].to_owned()
    }

    fn take_string(&mut self) -> String {
        let start = self.pos;
        self.next_char();
        while let Some(ch) = self.next_char() {
            if ch == '\\' {
                self.next_char();
            } else if ch == '"' {
                break;
            }
        }
        self.source[start..self.pos].to_owned()
    }

    fn take_while(&mut self, predicate: impl Fn(char) -> bool) -> String {
        let start = self.pos;
        while self.peek_char().is_some_and(&predicate) {
            self.next_char();
        }
        self.source[start..self.pos].to_owned()
    }

    fn starts_with(&self, value: &str) -> bool {
        self.source[self.pos..].starts_with(value)
    }

    fn peek_char(&self) -> Option<char> {
        self.source[self.pos..].chars().next()
    }

    fn next_char(&mut self) -> Option<char> {
        let ch = self.peek_char()?;
        self.pos += ch.len_utf8();
        Some(ch)
    }
}

fn trim_trailing_spaces(value: &mut String) {
    while value.ends_with(' ') || value.ends_with('\t') {
        value.pop();
    }
}

fn is_ident_start(ch: char) -> bool {
    ch == '_' || ch.is_ascii_alphabetic()
}

fn is_ident_continue(ch: char) -> bool {
    ch == '_' || ch.is_ascii_alphanumeric()
}

fn is_valid_board_name(name: &str, max_len: usize) -> bool {
    if name.is_empty() || name.len() > max_len {
        return false;
    }

    for (index, byte) in name.bytes().enumerate() {
        let valid = if index == 0 {
            byte.is_ascii_alphabetic() || byte == b'_'
        } else {
            byte.is_ascii_alphanumeric() || byte == b'_'
        };
        if !valid {
            return false;
        }
    }

    true
}

fn is_valid_feature_name(name: &str) -> bool {
    if name.is_empty() || name.len() > 64 {
        return false;
    }

    name.bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
}

fn matches_ignore_ascii_case(value: &str, expected: &[&str]) -> bool {
    expected
        .iter()
        .any(|expected| value.eq_ignore_ascii_case(expected))
}

fn is_unit_constructor(name: &str) -> bool {
    matches!(name, "ms" | "hz" | "volt" | "pin")
}

fn is_reserved_variable_name(name: &str) -> bool {
    matches!(
        name,
        "let" | "fn" | "if" | "else" | "contains" | "true" | "false" | "on" | "off" | "toggle"
    )
}

fn is_reserved_board_variable_name(name: &str) -> bool {
    matches!(
        name,
        "let"
            | "fn"
            | "if"
            | "else"
            | "contains"
            | "repeat"
            | "true"
            | "false"
            | "on"
            | "off"
            | "toggle"
            | "led"
            | "pin"
            | "heartbeat"
            | "wifi"
    )
}

fn is_reserved_function_name(name: &str) -> bool {
    matches!(
        name,
        "let"
            | "fn"
            | "if"
            | "else"
            | "repeat"
            | "contains"
            | "requires"
            | "status"
            | "ping"
            | "help"
            | "caps"
            | "vars"
            | "programs"
            | "save"
            | "run"
            | "delete"
            | "autorun"
            | "boot"
            | "led"
            | "blink"
            | "heartbeat"
            | "wait"
            | "expect"
            | "echo"
            | "cmd"
            | "uart"
            | "pin"
            | "pwm"
            | "adc"
            | "i2c"
            | "spi"
            | "wifi"
            | "timer"
            | "every"
            | "after"
            | "on"
            | "sleep"
            | "ms"
            | "hz"
            | "volt"
    )
}

fn script_error(line: usize, message: impl Into<String>) -> String {
    format!("строка {line}: {}", message.into())
}

fn script_error_at(line: usize, column: usize, message: impl Into<String>) -> String {
    format!("строка {line}, колонка {column}: {}", message.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compiles_variables_repeat_and_if() {
        let steps = compile_script(
            r#"
            let ms = 100;
            let count = 2;
            fn pulse(times: 2, ms: 50) {
                repeat(times: times) {
                    led(state: toggle);
                    wait(ms: ms);
                }
            }
            status();
            expect(text: "ok status", timeout_ms: 1000);
            if contains(text: "led=off") {
                pulse(times: count, ms: ms);
            } else {
                echo(text: "skip");
            }
            "#,
        )
        .unwrap();

        assert!(matches!(steps[0], ScriptStep::Send { .. }));
        assert!(matches!(steps[1], ScriptStep::Expect { .. }));
        let ScriptStep::IfContains {
            then_steps,
            else_steps,
            ..
        } = &steps[2]
        else {
            panic!("expected if contains step");
        };

        assert_eq!(then_steps.len(), 4);
        assert_eq!(else_steps.len(), 1);
    }

    #[test]
    fn compiles_requires_feature_directives() {
        let steps = compile_script(
            r#"
            requires(feature: pwm_real);
            requires(features: "timer_do,on_pin_debounce");
            status();
            "#,
        )
        .unwrap();

        assert!(matches!(
            &steps[0],
            ScriptStep::Requires { features, .. }
                if features.len() == 1 && features[0] == "pwm_real"
        ));
        assert!(matches!(
            &steps[1],
            ScriptStep::Requires { features, .. }
                if features.len() == 2
                    && features[0] == "timer_do"
                    && features[1] == "on_pin_debounce"
        ));
        assert!(matches!(&steps[2], ScriptStep::Send { command, .. } if command == "status"));
    }

    #[test]
    fn rejects_invalid_requires_feature() {
        let error = compile_script("requires(feature: \"bad feature\");")
            .err()
            .unwrap();

        assert!(error.contains("имя feature"));
    }

    #[test]
    fn rejects_requires_inside_board_blocks() {
        let error = compile_script(
            r#"
            timer(id: 0, every: ms(1000)) {
                requires(feature: timer_do);
                led.toggle();
            }
            "#,
        )
        .err()
        .unwrap();

        assert!(error.contains("requires"));
        assert!(error.contains("timer"));
    }

    #[test]
    fn compiles_hardware_primitives_and_units() {
        let steps = compile_script(
            r#"
            pin(id: pin(4), mode: output);
            pin.write(id: pin(4), state: on);
            pwm(pin: pin(5), freq: hz(1000), duty: 512);
            adc(pin: pin(34), max: volt(3.3));
            i2c(sda: pin(21), scl: pin(22), speed: hz(400000));
            spi(sck: pin(18), miso: pin(19), mosi: pin(23), cs: pin(15), speed: hz(1000000));
            uart(tx: pin(17), rx: pin(16), baud: hz(115200));
            timer(id: 0, every: ms(1000));
            sleep(duration: ms(1000));
            "#,
        )
        .unwrap();

        assert_eq!(steps.len(), 9);
        assert!(
            matches!(&steps[0], ScriptStep::Send { command, .. } if command == "pin 4 mode output")
        );
        assert!(matches!(&steps[8], ScriptStep::Send { command, .. } if command == "sleep 1000"));
    }

    #[test]
    fn rejects_wrong_safe_type() {
        let error = compile_script("pwm(pin: pin(5), freq: ms(1000), duty: 512);")
            .err()
            .unwrap();
        assert!(error.contains("hz(...)"));
    }

    #[test]
    fn rejects_command_delimiters_in_typed_text_args() {
        let error = compile_script("echo(text: \"ok; led on\");").err().unwrap();
        assert!(error.contains("echo(text)"));
        assert!(error.contains("разделителями команд"));

        let error = compile_script("wifi(ssid: \"net{name}\");").err().unwrap();
        assert!(error.contains("wifi(ssid)"));
        assert!(error.contains("разделителями команд"));
    }

    #[test]
    fn compiles_typed_let_and_function_params() {
        let steps = compile_script(
            r#"
            let pulses: number = 2;
            let delay: ms = ms(25);

            fn pulse(times: number = 1, delay: ms = ms(50)) {
                repeat(times: times) {
                    wait(ms: delay);
                }
            }

            pulse(times: pulses, delay: delay);
            "#,
        )
        .unwrap();

        assert_eq!(steps.len(), 2);
        assert!(matches!(steps[0], ScriptStep::Wait { ms: 25, .. }));
        assert!(matches!(steps[1], ScriptStep::Wait { ms: 25, .. }));
    }

    #[test]
    fn rejects_typed_let_mismatch() {
        let error = compile_script("let delay: ms = hz(1000);").err().unwrap();

        assert!(error.contains("let `delay`"));
        assert!(error.contains("ms"));
        assert!(error.contains("hz"));
    }

    #[test]
    fn rejects_typed_function_default_mismatch() {
        let error = compile_script("fn pulse(delay: ms = hz(1000)) {}")
            .err()
            .unwrap();

        assert!(error.contains("параметр `delay`"));
        assert!(error.contains("ms"));
        assert!(error.contains("hz"));
    }

    #[test]
    fn rejects_typed_function_arg_mismatch() {
        let error = compile_script(
            r#"
            fn pulse(delay: ms = ms(50)) {
                wait(ms: delay);
            }

            pulse(delay: hz(1000));
            "#,
        )
        .err()
        .unwrap();

        assert!(error.contains("аргумент `delay`"));
        assert!(error.contains("ms"));
        assert!(error.contains("hz"));
    }

    #[test]
    fn formats_blocks_spacing_and_else() {
        let formatted = format_script(
            "fn pulse(times:number=2){repeat(times:times){led.toggle();sleep(duration:ms(50));}} status(); if led==off{led.on();}else{led.off();}",
        );

        assert_eq!(
            formatted,
            r#"fn pulse(times: number = 2) {
    repeat(times: times) {
        led.toggle();
        sleep(duration: ms(50));
    }
}
status();
if led == off {
    led.on();
} else {
    led.off();
}"#
        );
        assert_eq!(format_script(&formatted), formatted);
    }

    #[test]
    fn formats_without_touching_strings_and_comments() {
        let formatted = format_script(
            r#"echo(text:"a;b { c } // not comment"); // inline
# top
requires(features:"timer_do,on_pin_debounce");"#,
        );

        assert_eq!(
            formatted,
            r#"echo(text: "a;b { c } // not comment");
// inline
# top
requires(features: "timer_do,on_pin_debounce");"#
        );
    }

    #[test]
    fn lint_warns_about_unused_symbols_and_raw_cmd() {
        let script = r#"
            let unused: number = 1;

            fn helper(delay: ms = ms(10)) {
                wait(ms: delay);
            }

            cmd(text: "status");
            "#;
        let steps = compile_script(script).unwrap();
        let warnings = lint_script(script, &steps);
        let messages: Vec<_> = warnings
            .iter()
            .map(|warning| warning.message.as_str())
            .collect();

        assert!(messages.iter().any(|message| message.contains("unused")));
        assert!(messages.iter().any(|message| message.contains("helper")));
        assert!(messages.iter().any(|message| message.contains("cmd(...)")));
    }

    #[test]
    fn lint_warns_about_board_variable_read_before_init() {
        let script = r#"
            timer(id: 0, every: ms(1000)) {
                if counter >= 3 {
                    led.off();
                }
            }
            "#;
        let steps = compile_script(script).unwrap();
        let warnings = lint_script(script, &steps);

        assert!(
            warnings
                .iter()
                .any(|warning| warning.message.contains("counter")
                    && warning.message.contains("board-переменная"))
        );
    }

    #[test]
    fn lint_does_not_warn_about_initialized_board_variable() {
        let script = r#"
            board.var(name: counter, value: 0);
            timer(id: 0, every: ms(1000)) {
                if counter >= 3 {
                    led.off();
                }
            }
            "#;
        let steps = compile_script(script).unwrap();
        let warnings = lint_script(script, &steps);

        assert!(
            !warnings
                .iter()
                .any(|warning| warning.message.contains("counter"))
        );
    }

    #[test]
    fn lint_warns_about_board_block_close_to_limit() {
        let body = "led.toggle();\n".repeat(10);
        let script = format!("timer(id: 0, every: ms(1000)) {{\n{body}}}");
        let steps = compile_script(&script).unwrap();
        let warnings = lint_script(&script, &steps);

        assert!(
            warnings
                .iter()
                .any(|warning| warning.message.contains("timer do-блок"))
        );
    }

    #[test]
    fn rejects_reserved_and_input_only_pins() {
        let error = compile_script("pin(id: pin(6), mode: output);")
            .err()
            .unwrap();
        assert!(error.contains("SPI flash"));

        let error = compile_script("pwm(pin: pin(34), freq: hz(1000), duty: 512);")
            .err()
            .unwrap();
        assert!(error.contains("только вход"));
    }

    #[test]
    fn rejects_i2c_spi_pin_conflict() {
        let error = compile_script(
            r#"
            i2c(sda: pin(21), scl: pin(22), speed: hz(400000));
            spi(sck: pin(21), miso: pin(19), mosi: pin(23), speed: hz(1000000));
            "#,
        )
        .err()
        .unwrap();

        assert!(error.contains("GPIO 21 уже занят"));
    }

    #[test]
    fn rejects_adc2_wifi_conflict() {
        let error = compile_script(
            r#"
            adc(pin: pin(25));
            wifi(enabled: true);
            "#,
        )
        .err()
        .unwrap();

        assert!(error.contains("ADC2"));
    }

    #[test]
    fn compiles_board_programs_events_and_board_if() {
        let steps = compile_script(
            r#"
            save(name: quick_blink) {
                led.toggle();
                sleep(duration: ms(100));
            }
            run(name: quick_blink);
            autorun(name: quick_blink);
            programs();
            vars();
            timer(id: 0, every: ms(1000)) {
                led.toggle();
            }
            on.pin(id: pin(0), trigger: falling, debounce: ms(30)) {
                led.toggle();
            }
            if led == off {
                led.on();
            } else {
                led.off();
            }
            timer.stop(id: 0);
            on.pin.off(id: pin(0));
            autorun.off();
            delete(name: quick_blink);
            boot();
            "#,
        )
        .unwrap();

        let commands: Vec<_> = steps
            .iter()
            .map(|step| match step {
                ScriptStep::Send { command, .. } => command.as_str(),
                _ => panic!("expected send step"),
            })
            .collect();

        assert_eq!(commands[0], "save quick_blink { led toggle; sleep 100 }");
        assert_eq!(commands[1], "run quick_blink");
        assert_eq!(commands[2], "autorun quick_blink");
        assert_eq!(commands[3], "programs");
        assert_eq!(commands[4], "vars");
        assert_eq!(commands[5], "timer 0 every 1000 do { led toggle }");
        assert_eq!(
            commands[6],
            "on pin 0 falling debounce 30 do { led toggle }"
        );
        assert_eq!(commands[7], "if led == off { led on } else { led off }");
        assert_eq!(commands[8], "timer 0 stop");
        assert_eq!(commands[9], "on pin 0 off");
        assert_eq!(commands[10], "autorun off");
        assert_eq!(commands[11], "delete quick_blink");
        assert_eq!(commands[12], "boot");
    }

    #[test]
    fn compiles_every_and_after_timer_sugar() {
        let steps = compile_script(
            r#"
            every(id: 0, ms: ms(1000)) {
                led.toggle();
            }

            after(id: 1, delay: ms(5000)) {
                led.off();
            }

            every(interval: ms(250)) {
                led.on();
            }
            "#,
        )
        .unwrap();

        let commands: Vec<_> = steps
            .iter()
            .map(|step| match step {
                ScriptStep::Send { command, .. } => command.as_str(),
                _ => panic!("expected send step"),
            })
            .collect();

        assert_eq!(commands[0], "timer 0 every 1000 do { led toggle }");
        assert_eq!(commands[1], "timer 1 after 5000 do { led off }");
        assert_eq!(commands[2], "timer 0 every 250 do { led on }");
    }

    #[test]
    fn compiles_on_button_sugar() {
        let steps = compile_script(
            r#"
            on.button(id: pin(0)) {
                led.toggle();
            }

            on.button(pin: pin(4), trigger: rising, debounce: ms(0)) {
                led.off();
            }
            "#,
        )
        .unwrap();

        let commands: Vec<_> = steps
            .iter()
            .map(|step| match step {
                ScriptStep::Send { command, .. } => command.as_str(),
                _ => panic!("expected send step"),
            })
            .collect();

        assert_eq!(commands[0], "pin 0 mode input_pullup");
        assert_eq!(
            commands[1],
            "on pin 0 falling debounce 30 do { led toggle }"
        );
        assert_eq!(commands[2], "pin 4 mode input_pullup");
        assert_eq!(commands[3], "on pin 4 rising do { led off }");
    }

    #[test]
    fn compiles_on_boot_sugar() {
        let steps = compile_script(
            r#"
            on.boot() {
                led.on();
                sleep(duration: ms(100));
            }

            on.boot(name: startup) {
                led.toggle();
            }
            "#,
        )
        .unwrap();

        let commands: Vec<_> = steps
            .iter()
            .map(|step| match step {
                ScriptStep::Send { command, .. } => command.as_str(),
                _ => panic!("expected send step"),
            })
            .collect();

        assert_eq!(commands[0], "save boot { led on; sleep 100 }");
        assert_eq!(commands[1], "autorun boot");
        assert_eq!(commands[2], "save startup { led toggle }");
        assert_eq!(commands[3], "autorun startup");
    }

    #[test]
    fn rejects_host_only_steps_inside_on_boot() {
        let error = compile_script(
            r#"
            on.boot() {
                wait(ms: ms(100));
            }
            "#,
        )
        .err()
        .unwrap();

        assert!(error.contains("wait"));
        assert!(error.contains("on.boot"));
    }

    #[test]
    fn rejects_every_without_block() {
        let error = compile_script("every(id: 0, ms: ms(1000));").err().unwrap();

        assert!(error.contains("блок"));
    }

    #[test]
    fn rejects_on_button_without_block() {
        let error = compile_script("on.button(id: pin(0));").err().unwrap();

        assert!(error.contains("блок"));
    }

    #[test]
    fn compiles_board_conditions_with_pin_and_numbers() {
        let steps = compile_script(
            r#"
            let button = pin(0);
            if pin(id: button) == on {
                led.off();
            }
            if counter >= 3 {
                led.on();
            }
            "#,
        )
        .unwrap();

        assert!(
            matches!(&steps[0], ScriptStep::Send { command, .. } if command == "if pin 0 == on { led off }")
        );
        assert!(
            matches!(&steps[1], ScriptStep::Send { command, .. } if command == "if counter >= 3 { led on }")
        );
    }

    #[test]
    fn compiles_board_variables_and_arithmetic() {
        let steps = compile_script(
            r#"
            let start = 0;
            board.var(name: counter, value: start);
            board.var(name: counter, value: counter + 1);
            timer(id: 0, every: ms(1000)) {
                board.var(name: counter, value: counter + 1);
                if counter >= 3 {
                    led.off();
                }
            }
            "#,
        )
        .unwrap();

        let commands: Vec<_> = steps
            .iter()
            .map(|step| match step {
                ScriptStep::Send { command, .. } => command.as_str(),
                _ => panic!("expected send step"),
            })
            .collect();

        assert_eq!(commands[0], "let counter = 0");
        assert_eq!(commands[1], "let counter = counter + 1");
        assert_eq!(
            commands[2],
            "timer 0 every 1000 do { let counter = counter + 1; if counter >= 3 { led off } }"
        );
    }

    #[test]
    fn compiles_board_repeat_without_host_expansion() {
        let steps = compile_script(
            r#"
            board.var(name: pulses, value: 3);
            timer(id: 0, every: ms(1000)) {
                repeat(times: pulses) {
                    led.toggle();
                    sleep(duration: ms(50));
                }
            }
            if led == off {
                repeat(times: 2) {
                    led.toggle();
                }
            }
            "#,
        )
        .unwrap();

        let commands: Vec<_> = steps
            .iter()
            .map(|step| match step {
                ScriptStep::Send { command, .. } => command.as_str(),
                _ => panic!("expected send step"),
            })
            .collect();

        assert_eq!(commands[0], "let pulses = 3");
        assert_eq!(
            commands[1],
            "timer 0 every 1000 do { repeat pulses { led toggle; sleep 50 } }"
        );
        assert_eq!(commands[2], "if led == off { repeat 2 { led toggle } }");
    }

    #[test]
    fn compiles_function_repeat_in_board_context_as_board_repeat() {
        let steps = compile_script(
            r#"
            fn pulse(times: 2) {
                repeat(times: times) {
                    led.toggle();
                }
            }

            timer(id: 0, every: ms(1000)) {
                pulse(times: 3);
            }
            "#,
        )
        .unwrap();

        assert!(matches!(
            &steps[0],
            ScriptStep::Send { command, .. }
                if command == "timer 0 every 1000 do { repeat 3 { led toggle } }"
        ));
    }

    #[test]
    fn rejects_zero_board_repeat() {
        let error = compile_script(
            r#"
            timer(id: 0, every: ms(1000)) {
                repeat(times: 0) {
                    led.toggle();
                }
            }
            "#,
        )
        .err()
        .unwrap();

        assert!(error.contains("1..1000"));
    }

    #[test]
    fn rejects_host_only_steps_inside_board_blocks() {
        let error = compile_script(
            r#"
            timer(id: 0, every: ms(1000)) {
                expect(text: "ok", timeout_ms: ms(1000));
            }
            "#,
        )
        .err()
        .unwrap();

        assert!(error.contains("expect"));
        assert!(error.contains("timer"));
    }

    #[test]
    fn rejects_wrong_on_pin_debounce_unit() {
        let error = compile_script(
            r#"
            on.pin(id: pin(0), trigger: falling, debounce: hz(30)) {
                led.toggle();
            }
            "#,
        )
        .err()
        .unwrap();

        assert!(error.contains("ms(...)"));
    }

    #[test]
    fn parse_errors_include_column() {
        let error = compile_script("status(;").err().unwrap();

        assert!(error.contains("строка 1"));
        assert!(error.contains("колонка 8"));
    }

    #[test]
    fn compiled_steps_keep_source_lines() {
        let steps = compile_script(
            r#"
            fn pulse() {
                led.on();
            }

            status();
            wait(ms: ms(10));
            if contains(text: "ok") {
                pulse();
            }
            "#,
        )
        .unwrap();

        assert_eq!(steps[0].line(), 6);
        assert_eq!(steps[1].line(), 7);

        let ScriptStep::IfContains {
            line, then_steps, ..
        } = &steps[2]
        else {
            panic!("expected if contains step");
        };
        assert_eq!(*line, 8);
        assert_eq!(then_steps[0].line(), 3);
    }
}
