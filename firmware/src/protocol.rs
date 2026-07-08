pub const MAX_VARIABLE_NAME_LEN: usize = 16;
pub const MAX_PROGRAM_NAME_LEN: usize = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompareOp {
    Eq,
    NotEq,
    Less,
    LessOrEq,
    Greater,
    GreaterOrEq,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArithmeticOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArithmeticError {
    DivideByZero,
    RemainderByZero,
    Overflow,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PinEventTrigger {
    Rising,
    Falling,
    Change,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DoBlockError {
    ExpectedDoBlock,
    ExpectedOpenBrace,
    MissingClosingBrace,
    TrailingText,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BracedBlockError {
    ExpectedOpenBrace,
    MissingClosingBrace,
    TrailingText,
}

pub fn find_matching_brace(value: &str) -> Option<usize> {
    let mut depth = 0_usize;
    for (index, byte) in value.bytes().enumerate() {
        match byte {
            b'{' => depth += 1,
            b'}' => {
                if depth == 0 {
                    return None;
                }
                depth -= 1;
                if depth == 0 {
                    return Some(index);
                }
            }
            _ => {}
        }
    }

    None
}

pub fn take_token(value: &str) -> Option<(&str, &str)> {
    let value = value.trim_start();
    if value.is_empty() {
        return None;
    }

    let end = value
        .bytes()
        .position(|byte| byte.is_ascii_whitespace())
        .unwrap_or(value.len());
    Some((&value[..end], &value[end..]))
}

pub fn parse_do_block_body(value: &str) -> Result<&str, DoBlockError> {
    let value = value.trim_start();
    let Some(after_do) = value.strip_prefix("do") else {
        return Err(DoBlockError::ExpectedDoBlock);
    };

    if after_do
        .as_bytes()
        .first()
        .is_some_and(|byte| !byte.is_ascii_whitespace() && *byte != b'{')
    {
        return Err(DoBlockError::ExpectedDoBlock);
    }

    let block = after_do.trim_start();
    if !block.starts_with('{') {
        return Err(DoBlockError::ExpectedOpenBrace);
    }

    let Some(close_index) = find_matching_brace(block) else {
        return Err(DoBlockError::MissingClosingBrace);
    };

    if !block[close_index + 1..].trim().is_empty() {
        return Err(DoBlockError::TrailingText);
    }

    Ok(&block[1..close_index])
}

pub fn parse_braced_block_body(value: &str) -> Result<&str, BracedBlockError> {
    let block = value.trim_start();
    if !block.starts_with('{') {
        return Err(BracedBlockError::ExpectedOpenBrace);
    }

    let Some(close_index) = find_matching_brace(block) else {
        return Err(BracedBlockError::MissingClosingBrace);
    };

    if !block[close_index + 1..].trim().is_empty() {
        return Err(BracedBlockError::TrailingText);
    }

    Ok(&block[1..close_index])
}

pub fn parse_compare_op(value: &str) -> Option<CompareOp> {
    match value {
        "==" => Some(CompareOp::Eq),
        "!=" => Some(CompareOp::NotEq),
        "<" => Some(CompareOp::Less),
        "<=" => Some(CompareOp::LessOrEq),
        ">" => Some(CompareOp::Greater),
        ">=" => Some(CompareOp::GreaterOrEq),
        _ => None,
    }
}

pub fn compare_bool(left: bool, op: CompareOp, right: bool) -> bool {
    match op {
        CompareOp::Eq => left == right,
        CompareOp::NotEq => left != right,
        _ => false,
    }
}

pub fn compare_u64(left: u64, op: CompareOp, right: u64) -> bool {
    match op {
        CompareOp::Eq => left == right,
        CompareOp::NotEq => left != right,
        CompareOp::Less => left < right,
        CompareOp::LessOrEq => left <= right,
        CompareOp::Greater => left > right,
        CompareOp::GreaterOrEq => left >= right,
    }
}

pub fn parse_arithmetic_op(value: &str) -> Option<ArithmeticOp> {
    match value {
        "+" => Some(ArithmeticOp::Add),
        "-" => Some(ArithmeticOp::Sub),
        "*" => Some(ArithmeticOp::Mul),
        "/" => Some(ArithmeticOp::Div),
        "%" => Some(ArithmeticOp::Rem),
        _ => None,
    }
}

pub fn checked_u64_binary_op(
    left: u64,
    op: ArithmeticOp,
    right: u64,
) -> Result<u64, ArithmeticError> {
    match op {
        ArithmeticOp::Add => left.checked_add(right).ok_or(ArithmeticError::Overflow),
        ArithmeticOp::Sub => left.checked_sub(right).ok_or(ArithmeticError::Overflow),
        ArithmeticOp::Mul => left.checked_mul(right).ok_or(ArithmeticError::Overflow),
        ArithmeticOp::Div => left.checked_div(right).ok_or(ArithmeticError::DivideByZero),
        ArithmeticOp::Rem => left
            .checked_rem(right)
            .ok_or(ArithmeticError::RemainderByZero),
    }
}

pub fn is_valid_ascii_identifier(name: &str, max_len: usize) -> bool {
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

pub fn is_valid_program_name(name: &str) -> bool {
    is_valid_ascii_identifier(name, MAX_PROGRAM_NAME_LEN)
}

pub fn is_valid_variable_name(name: &str) -> bool {
    is_valid_ascii_identifier(name, MAX_VARIABLE_NAME_LEN)
}

pub fn parse_level_value(value: &str) -> Option<bool> {
    match value {
        "on" | "high" | "true" | "1" => Some(true),
        "off" | "low" | "false" | "0" => Some(false),
        _ => None,
    }
}

pub fn parse_pin_event_trigger(value: &str) -> Option<PinEventTrigger> {
    match value {
        "rising" => Some(PinEventTrigger::Rising),
        "falling" => Some(PinEventTrigger::Falling),
        "change" => Some(PinEventTrigger::Change),
        _ => None,
    }
}

pub fn pin_event_trigger_name(trigger: PinEventTrigger) -> &'static str {
    match trigger {
        PinEventTrigger::Rising => "rising",
        PinEventTrigger::Falling => "falling",
        PinEventTrigger::Change => "change",
    }
}

pub fn parse_u64_value(value: &str) -> Option<u64> {
    let mut result = 0_u64;
    let mut has_digit = false;

    for byte in value.bytes() {
        if !byte.is_ascii_digit() {
            return None;
        }

        has_digit = true;
        result = result.checked_mul(10)?.checked_add((byte - b'0') as u64)?;
    }

    has_digit.then_some(result)
}

pub fn pins_are_distinct(pins: &[Option<u8>]) -> bool {
    let mut index = 0;
    while index < pins.len() {
        let Some(pin) = pins[index] else {
            index += 1;
            continue;
        };

        let mut other = index + 1;
        while other < pins.len() {
            if pins[other] == Some(pin) {
                return false;
            }
            other += 1;
        }
        index += 1;
    }

    true
}

pub fn is_esp32_gpio(pin: u8) -> bool {
    pin <= 39 && !matches!(pin, 20 | 24 | 28 | 29 | 30 | 31)
}

pub fn is_input_only_pin(pin: u8) -> bool {
    matches!(pin, 34..=39)
}

pub fn is_adc_pin(pin: u8) -> bool {
    matches!(pin, 32..=39 | 0 | 2 | 4 | 12..=15 | 25..=27)
}

pub fn is_adc2_pin(pin: u8) -> bool {
    matches!(pin, 0 | 2 | 4 | 12..=15 | 25..=27)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matching_brace_supports_nested_blocks() {
        assert_eq!(find_matching_brace("{ led on }"), Some(9));
        assert_eq!(
            find_matching_brace("{ repeat 2 { led toggle } } trailing"),
            Some(26)
        );
        assert_eq!(find_matching_brace("{ missing"), None);
        assert_eq!(find_matching_brace("}"), None);
    }

    #[test]
    fn matching_brace_handles_deep_nesting_without_saturating() {
        let mut value = std::string::String::new();
        for _ in 0..300 {
            value.push('{');
        }
        for _ in 0..300 {
            value.push('}');
        }

        assert_eq!(find_matching_brace(&value), Some(599));
    }

    #[test]
    fn take_token_trims_leading_space_and_returns_rest() {
        assert_eq!(take_token("  led on"), Some(("led", " on")));
        assert_eq!(take_token("single"), Some(("single", "")));
        assert_eq!(take_token("   "), None);
    }

    #[test]
    fn parses_do_blocks() {
        assert_eq!(parse_do_block_body("do { led on }"), Ok(" led on "));
        assert_eq!(parse_do_block_body("  do{led off}"), Ok("led off"));
        assert_eq!(
            parse_do_block_body("done { led on }"),
            Err(DoBlockError::ExpectedDoBlock)
        );
        assert_eq!(
            parse_do_block_body("do led on"),
            Err(DoBlockError::ExpectedOpenBrace)
        );
        assert_eq!(
            parse_do_block_body("do { led on"),
            Err(DoBlockError::MissingClosingBrace)
        );
        assert_eq!(
            parse_do_block_body("do { led on } extra"),
            Err(DoBlockError::TrailingText)
        );
    }

    #[test]
    fn parses_plain_braced_blocks() {
        assert_eq!(parse_braced_block_body("{ led on }"), Ok(" led on "));
        assert_eq!(
            parse_braced_block_body("led on"),
            Err(BracedBlockError::ExpectedOpenBrace)
        );
        assert_eq!(
            parse_braced_block_body("{ led on"),
            Err(BracedBlockError::MissingClosingBrace)
        );
        assert_eq!(
            parse_braced_block_body("{ led on } trailing"),
            Err(BracedBlockError::TrailingText)
        );
    }

    #[test]
    fn parses_comparison_and_arithmetic_ops() {
        assert_eq!(parse_compare_op("<="), Some(CompareOp::LessOrEq));
        assert_eq!(parse_compare_op("=>"), None);
        assert!(compare_bool(true, CompareOp::Eq, true));
        assert!(compare_u64(2, CompareOp::Less, 3));
        assert_eq!(parse_arithmetic_op("%"), Some(ArithmeticOp::Rem));
        assert_eq!(parse_arithmetic_op("**"), None);
    }

    #[test]
    fn checked_arithmetic_reports_invalid_operations() {
        assert_eq!(checked_u64_binary_op(4, ArithmeticOp::Add, 5), Ok(9));
        assert_eq!(
            checked_u64_binary_op(4, ArithmeticOp::Div, 0),
            Err(ArithmeticError::DivideByZero)
        );
        assert_eq!(
            checked_u64_binary_op(4, ArithmeticOp::Rem, 0),
            Err(ArithmeticError::RemainderByZero)
        );
        assert_eq!(
            checked_u64_binary_op(u64::MAX, ArithmeticOp::Add, 1),
            Err(ArithmeticError::Overflow)
        );
    }

    #[test]
    fn validates_identifiers_for_variables_and_programs() {
        assert!(is_valid_variable_name("presses_1"));
        assert!(is_valid_program_name("_boot"));
        assert!(!is_valid_variable_name("1press"));
        assert!(!is_valid_program_name("has-dash"));
        assert!(!is_valid_variable_name("abcdefghijklmnopq"));
    }

    #[test]
    fn parses_level_and_pin_event_trigger_values() {
        assert_eq!(parse_level_value("high"), Some(true));
        assert_eq!(parse_level_value("0"), Some(false));
        assert_eq!(parse_level_value("enabled"), None);
        assert_eq!(
            parse_pin_event_trigger("falling"),
            Some(PinEventTrigger::Falling)
        );
        assert_eq!(pin_event_trigger_name(PinEventTrigger::Change), "change");
    }

    #[test]
    fn parses_unsigned_decimal_numbers() {
        assert_eq!(parse_u64_value("0"), Some(0));
        assert_eq!(parse_u64_value("18446744073709551615"), Some(u64::MAX));
        assert_eq!(parse_u64_value("18446744073709551616"), None);
        assert_eq!(parse_u64_value(""), None);
        assert_eq!(parse_u64_value("12ms"), None);
    }

    #[test]
    fn validates_board_pin_helpers() {
        assert!(pins_are_distinct(&[Some(18), Some(19), None, Some(5)]));
        assert!(!pins_are_distinct(&[Some(18), Some(19), Some(18)]));
        assert!(is_esp32_gpio(39));
        assert!(!is_esp32_gpio(40));
        assert!(!is_esp32_gpio(24));
        assert!(is_input_only_pin(34));
        assert!(is_adc_pin(32));
        assert!(is_adc2_pin(25));
        assert!(!is_adc2_pin(32));
    }
}
