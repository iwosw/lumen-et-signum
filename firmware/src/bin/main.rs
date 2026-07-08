#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]
#![deny(clippy::large_stack_frames)]

use core::str;

use embedded_storage::{ReadStorage, Storage};
use esp_hal::Blocking;
use esp_hal::analog::adc::{Adc, AdcConfig, Attenuation};
use esp_hal::clock::CpuClock;
use esp_hal::gpio::{
    AnyPin, DriveMode, Flex, InputConfig, Level, NoPin, OutputConfig, OutputSignal, Pull,
};
use esp_hal::ledc::channel::{self, ChannelHW, ChannelIFace};
use esp_hal::ledc::timer::{self, TimerIFace};
use esp_hal::ledc::{LSGlobalClkSource, Ledc, LowSpeed};
use esp_hal::main;
use esp_hal::rtc_cntl::SocResetReason;
use esp_hal::system::{reset_reason, software_reset};
use esp_hal::time::{Duration, Instant, Rate};
use esp_hal::uart::{Config as UartConfig, Uart};
use esp_storage::FlashStorage;
use esp32_rust_fw::protocol::{
    ArithmeticError, ArithmeticOp, BracedBlockError, CompareOp, DoBlockError, MAX_PROGRAM_NAME_LEN,
    MAX_VARIABLE_NAME_LEN, PinEventTrigger, checked_u64_binary_op, compare_bool, compare_u64,
    find_matching_brace, is_adc_pin, is_adc2_pin, is_esp32_gpio, is_input_only_pin,
    is_valid_program_name, is_valid_variable_name, parse_arithmetic_op, parse_braced_block_body,
    parse_compare_op, parse_do_block_body, parse_level_value, parse_pin_event_trigger,
    parse_u64_value, pin_event_trigger_name, pins_are_distinct, take_token,
};

const GPIO_COUNT: usize = 40;
const LED_PIN: u8 = 2;
const MAX_REPEAT_COUNT: u64 = 1_000;
const MAX_SCRIPT_DEPTH: u8 = 4;
const MAX_SCRIPT_STEPS: u16 = 2_048;
const MAX_SCRIPT_SLEEP_BUDGET_MS: u64 = 60_000;
const MAX_TIMER_SCRIPT_LEN: usize = 128;
const MAX_PIN_EVENTS: usize = 4;
const MAX_PIN_EVENT_SCRIPT_LEN: usize = 128;
const MAX_PIN_EVENT_DEBOUNCE_MS: u64 = 60_000;
const MAX_VARIABLES: usize = 16;
const MAX_PROGRAMS: usize = 4;
const MAX_PROGRAM_SCRIPT_LEN: usize = 256;
const MAX_PWM_CHANNELS: usize = 4;
const ADC_READ_MAX_ATTEMPTS: u8 = 16;
const ADC_MAX_SAMPLES: u8 = 64;
const UART_WRITE_MAX_STALLS: usize = 1024;
const PERSIST_SECTOR_SIZE: usize = 4096;
const PERSIST_SLOT_COUNT: usize = 2;
const PERSIST_AREA_LEN: usize = PERSIST_SECTOR_SIZE * PERSIST_SLOT_COUNT;
const PERSIST_MAGIC: [u8; 4] = *b"ERFW";
const PERSIST_VERSION: u8 = 1;
const PERSIST_HEADER_LEN: usize = 16;
const PERSIST_CHECKSUM_OFFSET: usize = 8;
const PERSIST_PAYLOAD_LEN_OFFSET: usize = 12;
const PERSIST_SEQUENCE_OFFSET: usize = 14;
const PERSIST_AUTORUN_LEN: usize = MAX_PROGRAM_NAME_LEN;
const PERSIST_PROGRAM_RECORD_LEN: usize = 4 + MAX_PROGRAM_NAME_LEN + MAX_PROGRAM_SCRIPT_LEN;
const PERSIST_IMAGE_LEN: usize =
    PERSIST_HEADER_LEN + PERSIST_AUTORUN_LEN + MAX_PROGRAMS * PERSIST_PROGRAM_RECORD_LEN;
const FIRMWARE_NAME: &str = "esp32-rust-fw";
const FIRMWARE_VERSION: &str = env!("CARGO_PKG_VERSION");
const CAPS_PROTOCOL_VERSION: &str = "1";
// Keep driver-unimplemented commands out of caps until their handlers stop returning *_driver_unimplemented.
const CAPS_FEATURES: &str = "status,ping,help,caps,vars,programs,save,run,delete,autorun,persist,persist_slots,persist_clear,safe_boot,reboot,reset_reason,boot,led,blink,heartbeat,echo,pin,pwm,pwm_real,adc,adc_samples,on_pin,on_pin_debounce,timer,timer_do,sleep,repeat,board_if,let,script_budget,script_done";

#[derive(Clone, Copy, PartialEq, Eq)]
enum PinOwner {
    Free,
    Led,
    Pin,
    Pwm,
    Adc,
    Event,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PinMode {
    Unused,
    Input,
    InputPullup,
    InputPulldown,
    Output,
}

#[derive(Clone, Copy)]
struct TimerState {
    enabled: bool,
    repeat: bool,
    duration_ms: u64,
    next_ms: u64,
    script_len: usize,
    script: [u8; MAX_TIMER_SCRIPT_LEN],
}

#[derive(Clone, Copy)]
struct PinEventState {
    enabled: bool,
    pin: u8,
    trigger: PinEventTrigger,
    last_level: bool,
    debounce_ms: u64,
    debounce_active: bool,
    debounce_level: bool,
    debounce_since_ms: u64,
    script_len: usize,
    script: [u8; MAX_PIN_EVENT_SCRIPT_LEN],
}

#[derive(Clone, Copy)]
struct VariableState {
    used: bool,
    name_len: usize,
    name: [u8; MAX_VARIABLE_NAME_LEN],
    value: u64,
}

#[derive(Clone, Copy)]
struct ProgramState {
    used: bool,
    name_len: usize,
    name: [u8; MAX_PROGRAM_NAME_LEN],
    script_len: usize,
    script: [u8; MAX_PROGRAM_SCRIPT_LEN],
}

#[derive(Clone, Copy)]
struct PwmChannelState {
    enabled: bool,
    pin: u8,
    freq: u64,
    duty: u16,
}

impl VariableState {
    const fn new() -> Self {
        Self {
            used: false,
            name_len: 0,
            name: [0; MAX_VARIABLE_NAME_LEN],
            value: 0,
        }
    }
}

impl ProgramState {
    const fn new() -> Self {
        Self {
            used: false,
            name_len: 0,
            name: [0; MAX_PROGRAM_NAME_LEN],
            script_len: 0,
            script: [0; MAX_PROGRAM_SCRIPT_LEN],
        }
    }

    fn reset(&mut self) {
        self.used = false;
        self.name_len = 0;
        self.name.fill(0);
        self.script_len = 0;
        self.script.fill(0);
    }
}

impl PwmChannelState {
    const fn new() -> Self {
        Self {
            enabled: false,
            pin: 0,
            freq: 0,
            duty: 0,
        }
    }
}

impl PinEventState {
    const fn new() -> Self {
        Self {
            enabled: false,
            pin: 0,
            trigger: PinEventTrigger::Change,
            last_level: false,
            debounce_ms: 0,
            debounce_active: false,
            debounce_level: false,
            debounce_since_ms: 0,
            script_len: 0,
            script: [0; MAX_PIN_EVENT_SCRIPT_LEN],
        }
    }

    fn reset(&mut self) {
        self.enabled = false;
        self.pin = 0;
        self.trigger = PinEventTrigger::Change;
        self.last_level = false;
        self.debounce_ms = 0;
        self.debounce_active = false;
        self.debounce_level = false;
        self.debounce_since_ms = 0;
        self.script_len = 0;
        self.script.fill(0);
    }
}

impl TimerState {
    const fn new() -> Self {
        Self {
            enabled: false,
            repeat: false,
            duration_ms: 0,
            next_ms: 0,
            script_len: 0,
            script: [0; MAX_TIMER_SCRIPT_LEN],
        }
    }

    fn reset(&mut self) {
        self.enabled = false;
        self.repeat = false;
        self.duration_ms = 0;
        self.next_ms = 0;
        self.script_len = 0;
        self.script.fill(0);
    }
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop {}
}

// This creates a default app-descriptor required by the esp-idf bootloader.
// For more information see: <https://docs.espressif.com/projects/esp-idf/en/stable/esp32/api-reference/system/app_image_format.html#application-description>
esp_bootloader_esp_idf::esp_app_desc!();

struct DeviceState {
    boot_time: Instant,
    led_on: bool,
    blink_ms: u32,
    heartbeat: bool,
    commands: u32,
    unknown_commands: u32,
    line_overflows: u32,
    pin_owners: [PinOwner; GPIO_COUNT],
    pin_modes: [PinMode; GPIO_COUNT],
    pin_levels: [bool; GPIO_COUNT],
    i2c_configured: bool,
    spi_configured: bool,
    aux_uart_configured: bool,
    wifi_enabled: bool,
    adc2_in_use: bool,
    safe_boot: bool,
    script_running: bool,
    timer_script_running: bool,
    pin_event_running: bool,
    autorun_enabled: bool,
    autorun_name_len: usize,
    autorun_name: [u8; MAX_PROGRAM_NAME_LEN],
    pwm_channels: [PwmChannelState; MAX_PWM_CHANNELS],
    variables: [VariableState; MAX_VARIABLES],
    programs: [ProgramState; MAX_PROGRAMS],
    timers: [TimerState; 4],
    pin_events: [PinEventState; MAX_PIN_EVENTS],
    timer_events: u32,
    pin_events_fired: u32,
    program_runs: u32,
    boot_runs: u32,
    persist_loaded: bool,
    persist_slot: u8,
    persist_sequence: u16,
    persist_saves: u32,
    persist_errors: u32,
}

struct RuntimeContext<'a, 'd> {
    uart: &'a mut Uart<'d, Blocking>,
    led: &'a mut Flex<'d>,
    ledc: &'a Ledc<'d>,
    flash: &'a mut FlashStorage<'d>,
    persist_buf: &'a mut [u8; PERSIST_SECTOR_SIZE],
    state: &'a mut DeviceState,
    last_heartbeat: &'a mut Instant,
    last_blink: &'a mut Instant,
}

struct ScriptBudget {
    steps_remaining: u16,
    sleep_remaining_ms: u64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum StateChange {
    Unchanged,
    Changed,
}

#[derive(Clone, Copy)]
enum PersistMutation<'a> {
    SaveProgram {
        slot: usize,
        name: &'a str,
        body: &'a str,
    },
    DeleteProgram {
        name: &'a str,
    },
    SetAutorun {
        name: &'a str,
    },
    ClearAutorun,
    ClearPrograms,
}

#[derive(Clone, Copy)]
struct ProgramSave<'a> {
    change: StateChange,
    slot: usize,
    name: &'a str,
    body: &'a str,
}

impl StateChange {
    const fn changed(self) -> bool {
        matches!(self, Self::Changed)
    }
}

impl ScriptBudget {
    const fn new() -> Self {
        Self {
            steps_remaining: MAX_SCRIPT_STEPS,
            sleep_remaining_ms: MAX_SCRIPT_SLEEP_BUDGET_MS,
        }
    }
}

enum PersistLoadResult {
    Loaded(PersistSlotInfo),
    Empty,
    Invalid,
    StorageError,
}

enum PersistSlotScanError {
    Invalid,
    StorageError,
}

enum PersistImageStatus {
    Valid(u16),
    Empty,
    Invalid,
}

#[derive(Clone, Copy)]
struct PersistSlotInfo {
    slot: usize,
    sequence: u16,
}

fn persistent_slot_offset(flash: &FlashStorage<'_>, slot: usize) -> Option<u32> {
    if slot >= PERSIST_SLOT_COUNT {
        return None;
    }

    let capacity = flash.capacity();
    if capacity < PERSIST_AREA_LEN {
        None
    } else {
        Some((capacity - PERSIST_AREA_LEN + slot * PERSIST_SECTOR_SIZE) as u32)
    }
}

fn select_persistent_slot(
    flash: &mut FlashStorage<'_>,
    buffer: &mut [u8; PERSIST_SECTOR_SIZE],
) -> Result<Option<PersistSlotInfo>, PersistSlotScanError> {
    let mut best: Option<PersistSlotInfo> = None;
    let mut saw_invalid = false;

    for slot in 0..PERSIST_SLOT_COUNT {
        let Some(offset) = persistent_slot_offset(flash, slot) else {
            return Err(PersistSlotScanError::StorageError);
        };
        if flash
            .read(offset, &mut buffer[..PERSIST_IMAGE_LEN])
            .is_err()
        {
            return Err(PersistSlotScanError::StorageError);
        }

        match inspect_persistent_image(&buffer[..PERSIST_IMAGE_LEN]) {
            PersistImageStatus::Valid(sequence) => {
                let slot_info = PersistSlotInfo { slot, sequence };
                if best.is_none_or(|current| {
                    persistent_sequence_is_newer(slot_info.sequence, current.sequence)
                }) {
                    best = Some(slot_info);
                }
            }
            PersistImageStatus::Empty => {}
            PersistImageStatus::Invalid => saw_invalid = true,
        }
    }

    if best.is_some() || !saw_invalid {
        Ok(best)
    } else {
        Err(PersistSlotScanError::Invalid)
    }
}

fn persistent_sequence_is_newer(candidate: u16, current: u16) -> bool {
    let distance = candidate.wrapping_sub(current);
    distance != 0 && distance < 0x8000
}

fn load_persistent_state(
    flash: &mut FlashStorage<'_>,
    buffer: &mut [u8; PERSIST_SECTOR_SIZE],
    state: &mut DeviceState,
) -> PersistLoadResult {
    let slot = match select_persistent_slot(flash, buffer) {
        Ok(Some(slot)) => slot,
        Ok(None) => return PersistLoadResult::Empty,
        Err(PersistSlotScanError::Invalid) => return PersistLoadResult::Invalid,
        Err(PersistSlotScanError::StorageError) => return PersistLoadResult::StorageError,
    };
    let Some(offset) = persistent_slot_offset(flash, slot.slot) else {
        return PersistLoadResult::StorageError;
    };
    if flash
        .read(offset, &mut buffer[..PERSIST_IMAGE_LEN])
        .is_err()
    {
        return PersistLoadResult::StorageError;
    }

    match decode_persistent_state(&buffer[..PERSIST_IMAGE_LEN], state) {
        PersistLoadResult::Loaded(_) => PersistLoadResult::Loaded(slot),
        result => result,
    }
}

fn save_persistent_state(
    flash: &mut FlashStorage<'_>,
    buffer: &mut [u8; PERSIST_SECTOR_SIZE],
    state: &DeviceState,
    mutation: PersistMutation<'_>,
) -> Option<PersistSlotInfo> {
    let best = match select_persistent_slot(flash, buffer) {
        Ok(best) => best,
        Err(PersistSlotScanError::Invalid) => None,
        Err(PersistSlotScanError::StorageError) => return None,
    };
    let slot = best.map_or(0, |slot| (slot.slot + 1) % PERSIST_SLOT_COUNT);
    let sequence = best.map_or(1, |slot| slot.sequence.wrapping_add(1));
    let offset = persistent_slot_offset(flash, slot)?;

    encode_persistent_state(buffer, state, sequence, mutation);
    flash
        .write(offset, &buffer[..])
        .is_ok()
        .then_some(PersistSlotInfo { slot, sequence })
}

fn persist_state(ctx: &mut RuntimeContext<'_, '_>, mutation: PersistMutation<'_>) -> bool {
    if let Some(slot) = save_persistent_state(ctx.flash, ctx.persist_buf, ctx.state, mutation) {
        ctx.state.persist_saves = ctx.state.persist_saves.saturating_add(1);
        ctx.state.persist_loaded = true;
        ctx.state.persist_slot = slot.slot as u8;
        ctx.state.persist_sequence = slot.sequence;
        true
    } else {
        ctx.state.persist_errors = ctx.state.persist_errors.saturating_add(1);
        write_line(ctx.uart, "err persist_failed");
        false
    }
}

fn encode_persistent_state(
    buffer: &mut [u8; PERSIST_SECTOR_SIZE],
    state: &DeviceState,
    sequence: u16,
    mutation: PersistMutation<'_>,
) {
    let mut autorun_enabled = state.autorun_enabled;
    let mut autorun_name_len = state.autorun_name_len;
    let mut autorun_name = state.autorun_name;

    match mutation {
        PersistMutation::SetAutorun { name } => {
            autorun_enabled = true;
            autorun_name_len = name.len();
            autorun_name.fill(0);
            autorun_name[..name.len()].copy_from_slice(name.as_bytes());
        }
        PersistMutation::ClearAutorun | PersistMutation::ClearPrograms => {
            autorun_enabled = false;
            autorun_name_len = 0;
            autorun_name.fill(0);
        }
        PersistMutation::DeleteProgram { name } if autorun_name_matches(state, name) => {
            autorun_enabled = false;
            autorun_name_len = 0;
            autorun_name.fill(0);
        }
        _ => {}
    }

    buffer.fill(0xFF);
    buffer[..4].copy_from_slice(&PERSIST_MAGIC);
    buffer[4] = PERSIST_VERSION;
    buffer[5] = u8::from(autorun_enabled);
    buffer[6] = autorun_name_len as u8;
    buffer[7] = 0;
    write_u32_le(buffer, PERSIST_CHECKSUM_OFFSET, 0);
    write_u16_le(
        buffer,
        PERSIST_PAYLOAD_LEN_OFFSET,
        (PERSIST_IMAGE_LEN - PERSIST_HEADER_LEN) as u16,
    );
    write_u16_le(buffer, PERSIST_SEQUENCE_OFFSET, sequence);

    let mut cursor = PERSIST_HEADER_LEN;
    buffer[cursor..cursor + MAX_PROGRAM_NAME_LEN].copy_from_slice(&autorun_name);
    cursor += MAX_PROGRAM_NAME_LEN;

    for (slot, program) in state.programs.iter().enumerate() {
        match mutation {
            PersistMutation::ClearPrograms => {
                encode_persistent_program_record(buffer, &mut cursor, false, &[], &[]);
            }
            PersistMutation::SaveProgram {
                slot: save_slot,
                name,
                body,
            } if slot == save_slot => encode_persistent_program_record(
                buffer,
                &mut cursor,
                true,
                name.as_bytes(),
                body.as_bytes(),
            ),
            PersistMutation::DeleteProgram { name } if program_name_matches(program, name) => {
                encode_persistent_program_record(buffer, &mut cursor, false, &[], &[]);
            }
            _ => {
                let name = if program.used {
                    &program.name[..program.name_len]
                } else {
                    &[]
                };
                let script = if program.used {
                    &program.script[..program.script_len]
                } else {
                    &[]
                };
                encode_persistent_program_record(buffer, &mut cursor, program.used, name, script);
            }
        }
    }

    let checksum = persistent_checksum(&buffer[..PERSIST_IMAGE_LEN]);
    write_u32_le(buffer, PERSIST_CHECKSUM_OFFSET, checksum);
}

fn encode_persistent_program_record(
    buffer: &mut [u8; PERSIST_SECTOR_SIZE],
    cursor: &mut usize,
    used: bool,
    name: &[u8],
    script: &[u8],
) {
    buffer[*cursor] = u8::from(used);
    buffer[*cursor + 1] = name.len() as u8;
    write_u16_le(buffer, *cursor + 2, script.len() as u16);
    *cursor += 4;

    let name_target = &mut buffer[*cursor..*cursor + MAX_PROGRAM_NAME_LEN];
    name_target.fill(0);
    name_target[..name.len()].copy_from_slice(name);
    *cursor += MAX_PROGRAM_NAME_LEN;

    let script_target = &mut buffer[*cursor..*cursor + MAX_PROGRAM_SCRIPT_LEN];
    script_target.fill(0);
    script_target[..script.len()].copy_from_slice(script);
    *cursor += MAX_PROGRAM_SCRIPT_LEN;
}

fn inspect_persistent_image(buffer: &[u8]) -> PersistImageStatus {
    if buffer[..4].iter().all(|byte| *byte == 0xFF) {
        return PersistImageStatus::Empty;
    }
    if buffer[..4] != PERSIST_MAGIC || buffer[4] != PERSIST_VERSION {
        return PersistImageStatus::Invalid;
    }
    if read_u16_le(buffer, PERSIST_PAYLOAD_LEN_OFFSET) as usize
        != PERSIST_IMAGE_LEN - PERSIST_HEADER_LEN
    {
        return PersistImageStatus::Invalid;
    }
    if read_u32_le(buffer, PERSIST_CHECKSUM_OFFSET) != persistent_checksum(buffer) {
        return PersistImageStatus::Invalid;
    }

    let autorun_enabled = buffer[5] != 0;
    let autorun_name_len = buffer[6] as usize;
    if autorun_name_len > MAX_PROGRAM_NAME_LEN {
        return PersistImageStatus::Invalid;
    }

    let mut cursor = PERSIST_HEADER_LEN;
    let autorun_name = &buffer[cursor..cursor + MAX_PROGRAM_NAME_LEN];
    cursor += MAX_PROGRAM_NAME_LEN;
    if autorun_enabled && str::from_utf8(&autorun_name[..autorun_name_len]).is_err() {
        return PersistImageStatus::Invalid;
    }

    for _ in 0..MAX_PROGRAMS {
        let used = buffer[cursor] != 0;
        let name_len = buffer[cursor + 1] as usize;
        let script_len = read_u16_le(buffer, cursor + 2) as usize;
        cursor += 4;
        let name = &buffer[cursor..cursor + MAX_PROGRAM_NAME_LEN];
        cursor += MAX_PROGRAM_NAME_LEN;
        let script = &buffer[cursor..cursor + MAX_PROGRAM_SCRIPT_LEN];
        cursor += MAX_PROGRAM_SCRIPT_LEN;

        if !used {
            continue;
        }
        if name_len > MAX_PROGRAM_NAME_LEN || script_len > MAX_PROGRAM_SCRIPT_LEN {
            return PersistImageStatus::Invalid;
        }
        let Ok(name_text) = str::from_utf8(&name[..name_len]) else {
            return PersistImageStatus::Invalid;
        };
        if !is_valid_program_name(name_text) || str::from_utf8(&script[..script_len]).is_err() {
            return PersistImageStatus::Invalid;
        }
    }

    PersistImageStatus::Valid(read_u16_le(buffer, PERSIST_SEQUENCE_OFFSET))
}

fn decode_persistent_state(buffer: &[u8], state: &mut DeviceState) -> PersistLoadResult {
    match inspect_persistent_image(buffer) {
        PersistImageStatus::Valid(_) => {}
        PersistImageStatus::Empty => return PersistLoadResult::Empty,
        PersistImageStatus::Invalid => return PersistLoadResult::Invalid,
    }

    let autorun_enabled = buffer[5] != 0;
    let autorun_name_len = buffer[6] as usize;

    let mut cursor = PERSIST_HEADER_LEN;
    let autorun_name = &buffer[cursor..cursor + MAX_PROGRAM_NAME_LEN];
    cursor += MAX_PROGRAM_NAME_LEN;

    for program in state.programs.iter_mut() {
        program.reset();
    }

    for slot in 0..MAX_PROGRAMS {
        let used = buffer[cursor] != 0;
        let name_len = buffer[cursor + 1] as usize;
        let script_len = read_u16_le(buffer, cursor + 2) as usize;
        cursor += 4;
        let name = &buffer[cursor..cursor + MAX_PROGRAM_NAME_LEN];
        cursor += MAX_PROGRAM_NAME_LEN;
        let script = &buffer[cursor..cursor + MAX_PROGRAM_SCRIPT_LEN];
        cursor += MAX_PROGRAM_SCRIPT_LEN;

        if !used {
            continue;
        }
        if name_len > MAX_PROGRAM_NAME_LEN || script_len > MAX_PROGRAM_SCRIPT_LEN {
            return PersistLoadResult::Invalid;
        }
        let Ok(name_text) = str::from_utf8(&name[..name_len]) else {
            return PersistLoadResult::Invalid;
        };
        if !is_valid_program_name(name_text) || str::from_utf8(&script[..script_len]).is_err() {
            return PersistLoadResult::Invalid;
        }

        let program = &mut state.programs[slot];
        program.used = true;
        program.name_len = name_len;
        program.name[..name_len].copy_from_slice(&name[..name_len]);
        program.script_len = script_len;
        program.script[..script_len].copy_from_slice(&script[..script_len]);
    }

    if autorun_enabled {
        let Ok(name) = str::from_utf8(&autorun_name[..autorun_name_len]) else {
            return PersistLoadResult::Invalid;
        };
        if is_valid_program_name(name) && program_exists(state, name) {
            set_autorun(state, name);
        } else {
            clear_autorun(state);
        }
    } else {
        clear_autorun(state);
    }

    PersistLoadResult::Loaded(PersistSlotInfo {
        slot: 0,
        sequence: read_u16_le(buffer, PERSIST_SEQUENCE_OFFSET),
    })
}

fn persistent_checksum(buffer: &[u8]) -> u32 {
    let mut checksum = 0x811C_9DC5_u32;
    for (index, byte) in buffer.iter().enumerate() {
        if (PERSIST_CHECKSUM_OFFSET..PERSIST_CHECKSUM_OFFSET + 4).contains(&index) {
            continue;
        }
        checksum ^= *byte as u32;
        checksum = checksum.wrapping_mul(0x0100_0193);
    }
    checksum
}

fn read_u16_le(buffer: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([buffer[offset], buffer[offset + 1]])
}

fn read_u32_le(buffer: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        buffer[offset],
        buffer[offset + 1],
        buffer[offset + 2],
        buffer[offset + 3],
    ])
}

fn write_u16_le(buffer: &mut [u8], offset: usize, value: u16) {
    buffer[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn write_u32_le(buffer: &mut [u8], offset: usize, value: u32) {
    buffer[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

#[allow(
    clippy::large_stack_frames,
    reason = "it's not unusual to allocate larger buffers etc. in main"
)]
#[main]
fn main() -> ! {
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);

    let mut led = Flex::new(peripherals.GPIO2);
    configure_flex_output(&mut led, false);
    let safe_boot = {
        let mut safe_boot_pin = Flex::new(peripherals.GPIO0);
        configure_flex_input(&mut safe_boot_pin, PinMode::InputPullup);
        !safe_boot_pin.is_high()
    };
    let mut uart = Uart::new(peripherals.UART0, UartConfig::default())
        .unwrap()
        .with_rx(peripherals.GPIO3)
        .with_tx(peripherals.GPIO1);
    let mut ledc = Ledc::new(peripherals.LEDC);
    ledc.set_global_slow_clock(LSGlobalClkSource::APBClk);
    let mut flash = FlashStorage::new(peripherals.FLASH);
    let mut persist_buf = [0_u8; PERSIST_SECTOR_SIZE];

    let mut state = DeviceState {
        boot_time: Instant::now(),
        led_on: false,
        blink_ms: 0,
        heartbeat: true,
        commands: 0,
        unknown_commands: 0,
        line_overflows: 0,
        pin_owners: [PinOwner::Free; GPIO_COUNT],
        pin_modes: [PinMode::Unused; GPIO_COUNT],
        pin_levels: [false; GPIO_COUNT],
        i2c_configured: false,
        spi_configured: false,
        aux_uart_configured: false,
        wifi_enabled: false,
        adc2_in_use: false,
        safe_boot,
        script_running: false,
        timer_script_running: false,
        pin_event_running: false,
        autorun_enabled: false,
        autorun_name_len: 0,
        autorun_name: [0; MAX_PROGRAM_NAME_LEN],
        pwm_channels: [PwmChannelState::new(); MAX_PWM_CHANNELS],
        variables: [VariableState::new(); MAX_VARIABLES],
        programs: [ProgramState::new(); MAX_PROGRAMS],
        timers: [TimerState::new(); 4],
        pin_events: [PinEventState::new(); MAX_PIN_EVENTS],
        timer_events: 0,
        pin_events_fired: 0,
        program_runs: 0,
        boot_runs: 0,
        persist_loaded: false,
        persist_slot: 0,
        persist_sequence: 0,
        persist_saves: 0,
        persist_errors: 0,
    };
    state.pin_owners[LED_PIN as usize] = PinOwner::Led;
    state.pin_modes[LED_PIN as usize] = PinMode::Output;
    match load_persistent_state(&mut flash, &mut persist_buf, &mut state) {
        PersistLoadResult::Loaded(slot) => {
            state.persist_loaded = true;
            state.persist_slot = slot.slot as u8;
            state.persist_sequence = slot.sequence;
        }
        PersistLoadResult::Empty => {}
        PersistLoadResult::Invalid | PersistLoadResult::StorageError => {
            state.persist_errors = state.persist_errors.saturating_add(1);
        }
    }
    let mut line = [0_u8; 256];
    let mut line_len = 0_usize;
    let mut rx = [0_u8; 32];
    let mut last_heartbeat = Instant::now();
    let mut last_blink = Instant::now();

    write_str(&mut uart, "ready ");
    write_str(&mut uart, FIRMWARE_NAME);
    write_str(&mut uart, " v");
    write_line(&mut uart, FIRMWARE_VERSION);
    write_line(&mut uart, "type 'help' for commands");
    if state.persist_loaded {
        write_line(&mut uart, "storage loaded=persist");
    }
    if state.safe_boot {
        write_line(
            &mut uart,
            "boot safe_mode=on reason=gpio0_low autorun=skipped",
        );
    }
    if state.autorun_enabled && !state.safe_boot {
        let mut ctx = RuntimeContext {
            uart: &mut uart,
            led: &mut led,
            ledc: &ledc,
            flash: &mut flash,
            persist_buf: &mut persist_buf,
            state: &mut state,
            last_heartbeat: &mut last_heartbeat,
            last_blink: &mut last_blink,
        };
        let mut budget = ScriptBudget::new();
        ctx.state.script_running = true;
        handle_boot_command(&mut ctx, 0, &mut budget);
        ctx.state.script_running = false;
    }

    loop {
        if uart.read_ready() {
            match uart.read(&mut rx) {
                Ok(count) => {
                    for byte in rx[..count].iter().copied() {
                        match byte {
                            b'\r' | b'\n' => {
                                if line_len > 0 {
                                    let mut ctx = RuntimeContext {
                                        uart: &mut uart,
                                        led: &mut led,
                                        ledc: &ledc,
                                        flash: &mut flash,
                                        persist_buf: &mut persist_buf,
                                        state: &mut state,
                                        last_heartbeat: &mut last_heartbeat,
                                        last_blink: &mut last_blink,
                                    };
                                    handle_line(&line[..line_len], &mut ctx);
                                    line_len = 0;
                                }
                            }
                            8 | 127 => {
                                line_len = line_len.saturating_sub(1);
                            }
                            byte if byte.is_ascii_control() => {}
                            byte => {
                                if line_len < line.len() {
                                    line[line_len] = byte;
                                    line_len += 1;
                                } else {
                                    state.line_overflows = state.line_overflows.saturating_add(1);
                                    line_len = 0;
                                    write_line(&mut uart, "err line_too_long max=255");
                                }
                            }
                        }
                    }
                }
                Err(_) => write_line(&mut uart, "err uart_rx"),
            }
        }

        let mut ctx = RuntimeContext {
            uart: &mut uart,
            led: &mut led,
            ledc: &ledc,
            flash: &mut flash,
            persist_buf: &mut persist_buf,
            state: &mut state,
            last_heartbeat: &mut last_heartbeat,
            last_blink: &mut last_blink,
        };
        run_background_tasks(&mut ctx);
    }
}

fn handle_line(raw: &[u8], ctx: &mut RuntimeContext<'_, '_>) {
    let Ok(command) = str::from_utf8(raw) else {
        write_line(ctx.uart, "err command_not_utf8");
        return;
    };

    let command = command.trim();
    if command.is_empty() {
        return;
    }

    let _ = run_script(command, ctx, 0);
}

fn run_script(script: &str, ctx: &mut RuntimeContext<'_, '_>, script_depth: u8) -> bool {
    let mut budget = ScriptBudget::new();
    let was_running = ctx.state.script_running;
    ctx.state.script_running = true;
    let result = execute_script(script, ctx, script_depth, &mut budget);
    ctx.state.script_running = was_running;
    result
}

fn execute_script(
    script: &str,
    ctx: &mut RuntimeContext<'_, '_>,
    script_depth: u8,
    budget: &mut ScriptBudget,
) -> bool {
    if script_depth > MAX_SCRIPT_DEPTH {
        write_line(ctx.uart, "err script_too_deep max=4");
        return false;
    }

    let mut start = 0_usize;
    let mut brace_depth = 0_usize;

    for (index, byte) in script.bytes().enumerate() {
        match byte {
            b'{' => brace_depth += 1,
            b'}' => {
                if brace_depth == 0 {
                    write_line(ctx.uart, "err script_unexpected_closing_brace");
                    return false;
                }
                brace_depth -= 1;
            }
            b';' if brace_depth == 0 => {
                if !execute_segment(&script[start..index], ctx, script_depth, budget) {
                    return false;
                }
                start = index + 1;
            }
            _ => {}
        }
    }

    if brace_depth != 0 {
        write_line(ctx.uart, "err script_missing_closing_brace");
        return false;
    }

    execute_segment(&script[start..], ctx, script_depth, budget)
}

fn execute_segment(
    segment: &str,
    ctx: &mut RuntimeContext<'_, '_>,
    script_depth: u8,
    budget: &mut ScriptBudget,
) -> bool {
    let command = segment.trim();
    if command.is_empty() {
        return true;
    }

    if !consume_script_step(ctx.uart, budget) {
        return false;
    }

    if command == "repeat" || command.starts_with("repeat ") {
        return execute_repeat(command, ctx, script_depth, budget);
    }

    if command == "if" || command.starts_with("if ") {
        return execute_if(command, ctx, script_depth, budget);
    }

    execute_command(command, ctx, script_depth, budget)
}

fn consume_script_step(uart: &mut Uart<'_, Blocking>, budget: &mut ScriptBudget) -> bool {
    let Some(remaining) = budget.steps_remaining.checked_sub(1) else {
        write_str(uart, "err script_step_limit max=");
        write_u64(uart, MAX_SCRIPT_STEPS as u64);
        write_line(uart, "");
        return false;
    };

    budget.steps_remaining = remaining;
    true
}

fn execute_repeat(
    command: &str,
    ctx: &mut RuntimeContext<'_, '_>,
    script_depth: u8,
    budget: &mut ScriptBudget,
) -> bool {
    if script_depth >= MAX_SCRIPT_DEPTH {
        write_line(ctx.uart, "err script_too_deep max=4");
        return false;
    }

    let Some((count_text, rest)) = take_token(&command["repeat".len()..]) else {
        write_line(ctx.uart, "err repeat_expected_count");
        return false;
    };

    let Some(count) = parse_u64_arg(count_text, ctx.state) else {
        write_line(ctx.uart, "err repeat_count_must_be_number_or_var");
        return false;
    };
    if count == 0 || count > MAX_REPEAT_COUNT {
        write_line(ctx.uart, "err repeat_count_range 1..1000");
        return false;
    }

    let block = rest.trim_start();
    if !block.starts_with('{') {
        write_line(ctx.uart, "err repeat_expected_open_brace");
        return false;
    }

    let Some(close_index) = find_matching_brace(block) else {
        write_line(ctx.uart, "err repeat_missing_closing_brace");
        return false;
    };

    if !block[close_index + 1..].trim().is_empty() {
        write_line(
            ctx.uart,
            "err repeat_trailing_text use_semicolon_between_commands",
        );
        return false;
    }

    let body = &block[1..close_index];
    for _ in 0..count {
        if !execute_script(body, ctx, script_depth + 1, budget) {
            return false;
        }
        run_background_tasks(ctx);
    }

    true
}

fn execute_if(
    command: &str,
    ctx: &mut RuntimeContext<'_, '_>,
    script_depth: u8,
    budget: &mut ScriptBudget,
) -> bool {
    if script_depth >= MAX_SCRIPT_DEPTH {
        write_line(ctx.uart, "err script_too_deep max=4");
        return false;
    }

    let rest = command["if".len()..].trim_start();
    let Some(open_index) = rest.bytes().position(|byte| byte == b'{') else {
        write_line(ctx.uart, "err if_expected_open_brace");
        return false;
    };

    let condition = rest[..open_index].trim();
    if condition.is_empty() {
        write_line(ctx.uart, "err if_expected_condition");
        return false;
    }

    let block = rest[open_index..].trim_start();
    let Some(close_index) = find_matching_brace(block) else {
        write_line(ctx.uart, "err if_missing_closing_brace");
        return false;
    };

    let then_body = &block[1..close_index];
    let trailing = block[close_index + 1..].trim_start();
    let else_body = if trailing.is_empty() {
        None
    } else {
        let Some(after_else) = trailing.strip_prefix("else") else {
            write_line(ctx.uart, "err if_trailing_text use_else_or_semicolon");
            return false;
        };

        if after_else
            .as_bytes()
            .first()
            .is_some_and(|byte| !byte.is_ascii_whitespace() && *byte != b'{')
        {
            write_line(ctx.uart, "err if_expected_else_block");
            return false;
        }

        let else_block = after_else.trim_start();
        if !else_block.starts_with('{') {
            write_line(ctx.uart, "err if_else_expected_open_brace");
            return false;
        }

        let Some(else_close_index) = find_matching_brace(else_block) else {
            write_line(ctx.uart, "err if_else_missing_closing_brace");
            return false;
        };
        if !else_block[else_close_index + 1..].trim().is_empty() {
            write_line(
                ctx.uart,
                "err if_else_trailing_text use_semicolon_between_commands",
            );
            return false;
        }

        Some(&else_block[1..else_close_index])
    };

    let Some(condition_met) = eval_condition(condition, ctx) else {
        return false;
    };

    if condition_met {
        execute_script(then_body, ctx, script_depth + 1, budget)
    } else if let Some(else_body) = else_body {
        execute_script(else_body, ctx, script_depth + 1, budget)
    } else {
        true
    }
}

fn eval_condition(condition: &str, ctx: &mut RuntimeContext<'_, '_>) -> Option<bool> {
    let Some((left, rest)) = take_token(condition) else {
        write_line(ctx.uart, "err if_expected_condition");
        return None;
    };

    match left {
        "led" => eval_bool_condition(ctx.state.led_on, rest, ctx.uart),
        "heartbeat" => eval_bool_condition(ctx.state.heartbeat, rest, ctx.uart),
        "wifi" => eval_bool_condition(ctx.state.wifi_enabled, rest, ctx.uart),
        "pin" => eval_pin_condition(rest, ctx),
        _ => eval_number_condition(left, rest, ctx.uart, ctx.state),
    }
}

fn eval_bool_condition(current: bool, rest: &str, uart: &mut Uart<'_, Blocking>) -> Option<bool> {
    let Some((op_text, rest)) = take_token(rest) else {
        write_line(uart, "err if_expected_operator");
        return None;
    };
    let Some(op) = parse_compare_op(op_text) else {
        write_line(uart, "err if_operator_expected ==|!=|<|<=|>|>=");
        return None;
    };
    if !matches!(op, CompareOp::Eq | CompareOp::NotEq) {
        write_line(uart, "err if_bool_operator_expected ==|!=");
        return None;
    }

    let Some((expected_text, trailing)) = take_token(rest) else {
        write_line(uart, "err if_expected_level");
        return None;
    };
    if !trailing.trim().is_empty() {
        write_line(uart, "err if_condition_too_many_args");
        return None;
    }

    let expected = parse_level(expected_text, uart)?;
    Some(compare_bool(current, op, expected))
}

fn eval_pin_condition(rest: &str, ctx: &mut RuntimeContext<'_, '_>) -> Option<bool> {
    let Some((pin_text, rest)) = take_token(rest) else {
        write_line(ctx.uart, "err if_pin_expected_gpio");
        return None;
    };
    let Some(pin) = parse_u8_arg(pin_text, ctx.state) else {
        write_line(ctx.uart, "err if_pin_expected_number_or_var");
        return None;
    };

    let current = read_pin_level(pin, ctx.uart, ctx.led, ctx.state)?;
    eval_bool_condition(current, rest, ctx.uart)
}

fn eval_number_condition(
    left: &str,
    rest: &str,
    uart: &mut Uart<'_, Blocking>,
    state: &DeviceState,
) -> Option<bool> {
    let Some(left_value) = parse_u64_arg(left, state) else {
        write_line(uart, "err if_left_expected_number_or_var");
        return None;
    };

    let Some((op_text, rest)) = take_token(rest) else {
        write_line(uart, "err if_expected_operator");
        return None;
    };
    let Some(op) = parse_compare_op(op_text) else {
        write_line(uart, "err if_operator_expected ==|!=|<|<=|>|>=");
        return None;
    };

    let Some((right_text, trailing)) = take_token(rest) else {
        write_line(uart, "err if_right_expected_number_or_var");
        return None;
    };
    if !trailing.trim().is_empty() {
        write_line(uart, "err if_condition_too_many_args");
        return None;
    }

    let Some(right_value) = parse_u64_arg(right_text, state) else {
        write_line(uart, "err if_right_expected_number_or_var");
        return None;
    };

    Some(compare_u64(left_value, op, right_value))
}

fn parse_do_block<'a>(
    value: &'a str,
    uart: &mut Uart<'_, Blocking>,
    context: &str,
) -> Option<&'a str> {
    match parse_do_block_body(value) {
        Ok(body) => Some(body),
        Err(error) => {
            write_str(uart, "err ");
            write_str(uart, context);
            write_line(
                uart,
                match error {
                    DoBlockError::ExpectedDoBlock => "_expected_do_block",
                    DoBlockError::ExpectedOpenBrace => "_do_expected_open_brace",
                    DoBlockError::MissingClosingBrace => "_do_missing_closing_brace",
                    DoBlockError::TrailingText => "_do_trailing_text",
                },
            );
            None
        }
    }
}

fn parse_braced_block<'a>(
    value: &'a str,
    uart: &mut Uart<'_, Blocking>,
    context: &str,
) -> Option<&'a str> {
    match parse_braced_block_body(value) {
        Ok(body) => Some(body),
        Err(error) => {
            write_str(uart, "err ");
            write_str(uart, context);
            write_line(
                uart,
                match error {
                    BracedBlockError::ExpectedOpenBrace => "_expected_open_brace",
                    BracedBlockError::MissingClosingBrace => "_missing_closing_brace",
                    BracedBlockError::TrailingText => "_trailing_text",
                },
            );
            None
        }
    }
}

fn execute_command(
    command: &str,
    ctx: &mut RuntimeContext<'_, '_>,
    script_depth: u8,
    budget: &mut ScriptBudget,
) -> bool {
    ctx.state.commands = ctx.state.commands.saturating_add(1);

    if let Some(result) = handle_exact_command(command, ctx) {
        return result;
    }

    if let Some(result) = handle_prefixed_command(command, ctx, script_depth, budget) {
        return result;
    }

    ctx.state.unknown_commands = ctx.state.unknown_commands.saturating_add(1);
    write_str(ctx.uart, "err unknown_command '");
    write_str(ctx.uart, command);
    write_line(ctx.uart, "' try help");
    false
}

fn handle_exact_command(command: &str, ctx: &mut RuntimeContext<'_, '_>) -> Option<bool> {
    match command {
        "help" | "?" => {
            write_help(ctx.uart);
            Some(true)
        }
        "ping" => {
            write_line(ctx.uart, "ok pong");
            Some(true)
        }
        "caps" => {
            write_caps(ctx.uart);
            Some(true)
        }
        "reboot" | "reset" => {
            write_line(ctx.uart, "ok rebooting");
            software_reset();
        }
        "status" => {
            write_status(ctx.uart, ctx.state);
            Some(true)
        }
        "vars" => {
            write_variables(ctx.uart, ctx.state);
            Some(true)
        }
        "programs" => {
            write_programs(ctx.uart, ctx.state);
            Some(true)
        }
        "autorun" => {
            write_autorun(ctx.uart, ctx.state);
            Some(true)
        }
        "led on" => {
            disable_pwm_pin(LED_PIN, ctx.led, ctx.state);
            ctx.state.blink_ms = 0;
            ctx.state.led_on = true;
            ctx.state.pin_levels[LED_PIN as usize] = true;
            ctx.state.pin_owners[LED_PIN as usize] = PinOwner::Led;
            ctx.state.pin_modes[LED_PIN as usize] = PinMode::Output;
            configure_flex_output(ctx.led, true);
            ctx.led.set_high();
            write_line(ctx.uart, "ok led=on blink_ms=0");
            Some(true)
        }
        "led off" => {
            disable_pwm_pin(LED_PIN, ctx.led, ctx.state);
            ctx.state.blink_ms = 0;
            ctx.state.led_on = false;
            ctx.state.pin_levels[LED_PIN as usize] = false;
            ctx.state.pin_owners[LED_PIN as usize] = PinOwner::Led;
            ctx.state.pin_modes[LED_PIN as usize] = PinMode::Output;
            configure_flex_output(ctx.led, false);
            ctx.led.set_low();
            write_line(ctx.uart, "ok led=off blink_ms=0");
            Some(true)
        }
        "led toggle" => {
            disable_pwm_pin(LED_PIN, ctx.led, ctx.state);
            ctx.state.blink_ms = 0;
            let next_led_on = !ctx.state.led_on;
            ctx.state.pin_owners[LED_PIN as usize] = PinOwner::Led;
            ctx.state.pin_modes[LED_PIN as usize] = PinMode::Output;
            configure_flex_output(ctx.led, ctx.state.led_on);
            ctx.led.toggle();
            ctx.state.led_on = next_led_on;
            ctx.state.pin_levels[LED_PIN as usize] = ctx.state.led_on;
            write_str(ctx.uart, "ok led=");
            write_line(ctx.uart, if ctx.state.led_on { "on" } else { "off" });
            Some(true)
        }
        "blink off" | "led blink off" => {
            ctx.state.blink_ms = 0;
            write_line(ctx.uart, "ok blink_ms=0");
            Some(true)
        }
        "heartbeat on" => {
            ctx.state.heartbeat = true;
            write_line(ctx.uart, "ok heartbeat=on");
            Some(true)
        }
        "heartbeat off" => {
            ctx.state.heartbeat = false;
            write_line(ctx.uart, "ok heartbeat=off");
            Some(true)
        }
        _ => None,
    }
}

fn handle_prefixed_command(
    command: &str,
    ctx: &mut RuntimeContext<'_, '_>,
    script_depth: u8,
    budget: &mut ScriptBudget,
) -> Option<bool> {
    if let Some(result) = handle_program_command(command, ctx, script_depth, budget) {
        return Some(result);
    }
    if let Some(result) = handle_gpio_command(command, ctx) {
        return Some(result);
    }
    if let Some(result) = handle_bus_command(command, ctx) {
        return Some(result);
    }
    if let Some(result) = handle_event_command(command, ctx, budget) {
        return Some(result);
    }

    None
}

fn handle_program_command(
    command: &str,
    ctx: &mut RuntimeContext<'_, '_>,
    script_depth: u8,
    budget: &mut ScriptBudget,
) -> Option<bool> {
    if command == "let" {
        write_line(ctx.uart, "err let_expected_assignment");
        Some(false)
    } else if let Some(rest) = command.strip_prefix("let ") {
        Some(handle_let_command(rest, ctx.uart, ctx.state))
    } else if command == "save" {
        write_line(ctx.uart, "err save_expected_name_block");
        Some(false)
    } else if let Some(rest) = command.strip_prefix("save ") {
        Some(handle_save_program_command(rest, ctx))
    } else if command == "run" {
        write_line(ctx.uart, "err run_expected_name");
        Some(false)
    } else if let Some(rest) = command.strip_prefix("run ") {
        Some(handle_run_command(rest, ctx, script_depth, budget))
    } else if let Some(rest) = command.strip_prefix("autorun ") {
        Some(handle_autorun_program_command(rest, ctx))
    } else if command == "persist" {
        write_line(ctx.uart, "err persist_expected_clear");
        Some(false)
    } else if let Some(rest) = command.strip_prefix("persist ") {
        Some(handle_persist_command(rest, ctx))
    } else if command == "boot" {
        Some(handle_boot_command(ctx, script_depth, budget))
    } else if command == "delete" {
        write_line(ctx.uart, "err delete_expected_name");
        Some(false)
    } else if let Some(rest) = command.strip_prefix("delete ") {
        Some(handle_delete_program_command(rest, ctx))
    } else if let Some(rest) = command.strip_prefix("echo ") {
        write_str(ctx.uart, "ok echo ");
        write_line(ctx.uart, rest);
        Some(true)
    } else {
        None
    }
}

fn handle_gpio_command(command: &str, ctx: &mut RuntimeContext<'_, '_>) -> Option<bool> {
    if let Some(rest) = command.strip_prefix("led blink ") {
        Some(set_blink(rest, ctx.uart, ctx.led, ctx.state))
    } else if let Some(rest) = command.strip_prefix("blink ") {
        Some(set_blink(rest, ctx.uart, ctx.led, ctx.state))
    } else if let Some(rest) = command.strip_prefix("pin ") {
        Some(handle_pin_command(rest, ctx.uart, ctx.led, ctx.state))
    } else if let Some(rest) = command.strip_prefix("pwm ") {
        Some(handle_pwm_command(
            rest, ctx.uart, ctx.led, ctx.ledc, ctx.state,
        ))
    } else if let Some(rest) = command.strip_prefix("adc ") {
        Some(handle_adc_command(rest, ctx.uart, ctx.state))
    } else {
        None
    }
}

fn handle_bus_command(command: &str, ctx: &mut RuntimeContext<'_, '_>) -> Option<bool> {
    if let Some(rest) = command.strip_prefix("i2c ") {
        Some(handle_i2c_command(rest, ctx.uart, ctx.state))
    } else if let Some(rest) = command.strip_prefix("spi ") {
        Some(handle_spi_command(rest, ctx.uart, ctx.state))
    } else if let Some(rest) = command.strip_prefix("uart ") {
        Some(handle_aux_uart_command(rest, ctx.uart, ctx.state))
    } else if let Some(rest) = command.strip_prefix("wifi ") {
        Some(handle_wifi_command(rest, ctx.uart, ctx.state))
    } else {
        None
    }
}

fn handle_event_command(
    command: &str,
    ctx: &mut RuntimeContext<'_, '_>,
    budget: &mut ScriptBudget,
) -> Option<bool> {
    if let Some(rest) = command.strip_prefix("on ") {
        Some(handle_on_command(rest, ctx.uart, ctx.led, ctx.state))
    } else if let Some(rest) = command.strip_prefix("timer ") {
        Some(handle_timer_command(rest, ctx.uart, ctx.state))
    } else {
        command
            .strip_prefix("sleep ")
            .map(|rest| handle_sleep_command(rest, ctx, budget))
    }
}

fn handle_save_program_command(rest: &str, ctx: &mut RuntimeContext<'_, '_>) -> bool {
    let Some(save) = handle_save_command(rest, ctx.uart, ctx.state) else {
        return false;
    };

    if save.change.changed()
        && !persist_state(
            ctx,
            PersistMutation::SaveProgram {
                slot: save.slot,
                name: save.name,
                body: save.body,
            },
        )
    {
        return false;
    }

    if save.change.changed() {
        set_program_at_slot(ctx.state, save.slot, save.name, save.body);
    }
    write_str(ctx.uart, "ok save ");
    write_str(ctx.uart, save.name);
    write_str(ctx.uart, " bytes=");
    write_u64(ctx.uart, save.body.len() as u64);
    write_str(ctx.uart, " changed=");
    write_u64(ctx.uart, if save.change.changed() { 1 } else { 0 });
    write_line(ctx.uart, "");
    true
}

fn handle_autorun_program_command(rest: &str, ctx: &mut RuntimeContext<'_, '_>) -> bool {
    let Some((change, name, mutation)) = handle_autorun_command(rest, ctx.uart, ctx.state) else {
        return false;
    };

    if change.changed() && !persist_state(ctx, mutation) {
        return false;
    }

    if change.changed() {
        apply_autorun_mutation(ctx.state, mutation);
    }
    write_str(ctx.uart, "ok autorun=");
    write_str(ctx.uart, name);
    write_str(ctx.uart, " changed=");
    write_u64(ctx.uart, if change.changed() { 1 } else { 0 });
    write_line(ctx.uart, "");
    true
}

fn handle_persist_command(rest: &str, ctx: &mut RuntimeContext<'_, '_>) -> bool {
    if rest.trim() != "clear" {
        write_line(ctx.uart, "err persist_expected_clear");
        return false;
    }

    let programs = count_programs(ctx.state);
    let autorun_was_enabled = ctx.state.autorun_enabled || ctx.state.autorun_name_len != 0;
    let changed = programs > 0 || autorun_was_enabled;
    if changed && !persist_state(ctx, PersistMutation::ClearPrograms) {
        return false;
    }

    if changed {
        clear_programs_and_autorun(ctx.state);
    }
    write_str(ctx.uart, "ok persist_clear programs=");
    write_u64(ctx.uart, programs);
    write_str(ctx.uart, " autorun=");
    write_u64(ctx.uart, if autorun_was_enabled { 1 } else { 0 });
    write_str(ctx.uart, " changed=");
    write_u64(ctx.uart, if changed { 1 } else { 0 });
    write_line(ctx.uart, "");
    true
}

fn handle_delete_program_command(rest: &str, ctx: &mut RuntimeContext<'_, '_>) -> bool {
    let Some((change, name, removed)) = handle_delete_command(rest, ctx.uart, ctx.state) else {
        return false;
    };

    if change.changed() && !persist_state(ctx, PersistMutation::DeleteProgram { name }) {
        return false;
    }

    if change.changed() {
        delete_program(ctx.state, name);
        if autorun_name_matches(ctx.state, name) {
            clear_autorun(ctx.state);
        }
    }
    write_str(ctx.uart, "ok delete ");
    write_str(ctx.uart, name);
    write_str(ctx.uart, " removed=");
    write_u64(ctx.uart, if removed { 1 } else { 0 });
    write_line(ctx.uart, "");
    true
}

fn handle_let_command(rest: &str, uart: &mut Uart<'_, Blocking>, state: &mut DeviceState) -> bool {
    let rest = rest.trim();
    let Some(eq_index) = rest.bytes().position(|byte| byte == b'=') else {
        write_line(uart, "err let_expected_equals");
        return false;
    };

    let name = rest[..eq_index].trim();
    let value_text = rest[eq_index + 1..].trim();
    if !is_valid_variable_name(name) {
        write_line(
            uart,
            "err var_name_expected ascii_letter_or_underscore max=16",
        );
        return false;
    }

    let Some(value) = eval_u64_expression(value_text, uart, state) else {
        return false;
    };

    if !set_variable(state, name, value) {
        write_line(uart, "err vars_full max=16");
        return false;
    }

    write_str(uart, "ok let ");
    write_str(uart, name);
    write_str(uart, "=");
    write_u64(uart, value);
    write_line(uart, "");
    true
}

fn handle_save_command<'a>(
    rest: &'a str,
    uart: &mut Uart<'_, Blocking>,
    state: &mut DeviceState,
) -> Option<ProgramSave<'a>> {
    let Some((name, rest)) = take_token(rest) else {
        write_line(uart, "err save_expected_name");
        return None;
    };
    if !is_valid_program_name(name) {
        write_line(
            uart,
            "err program_name_expected ascii_letter_or_underscore max=16",
        );
        return None;
    }

    let body = parse_braced_block(rest, uart, "save")?;
    if body.len() > MAX_PROGRAM_SCRIPT_LEN {
        write_line(uart, "err program_too_long max=256");
        return None;
    }

    let Some((change, slot)) = plan_program_save(state, name, body) else {
        write_line(uart, "err programs_full max=4");
        return None;
    };

    Some(ProgramSave {
        change,
        slot,
        name,
        body,
    })
}

fn handle_run_command(
    rest: &str,
    ctx: &mut RuntimeContext<'_, '_>,
    script_depth: u8,
    budget: &mut ScriptBudget,
) -> bool {
    if script_depth >= MAX_SCRIPT_DEPTH {
        write_line(ctx.uart, "err script_too_deep max=4");
        return false;
    }

    let Some((name, trailing)) = take_token(rest) else {
        write_line(ctx.uart, "err run_expected_name");
        return false;
    };
    if !trailing.trim().is_empty() {
        write_line(ctx.uart, "err run_too_many_args");
        return false;
    }

    run_program_by_name(name, ctx, script_depth, budget)
}

fn handle_autorun_command<'a>(
    rest: &'a str,
    uart: &mut Uart<'_, Blocking>,
    state: &mut DeviceState,
) -> Option<(StateChange, &'a str, PersistMutation<'a>)> {
    let Some((name, trailing)) = take_token(rest) else {
        write_line(uart, "err autorun_expected_name_or_off");
        return None;
    };
    if !trailing.trim().is_empty() {
        write_line(uart, "err autorun_too_many_args");
        return None;
    }

    if name == "off" {
        let change = if state.autorun_enabled || state.autorun_name_len != 0 {
            StateChange::Changed
        } else {
            StateChange::Unchanged
        };
        return Some((change, "off", PersistMutation::ClearAutorun));
    }

    if !is_valid_program_name(name) {
        write_line(uart, "err autorun_name_expected_program_name");
        return None;
    }

    if !program_exists(state, name) {
        write_line(uart, "err program_not_found");
        return None;
    }

    let change = if autorun_name_matches(state, name) {
        StateChange::Unchanged
    } else {
        StateChange::Changed
    };
    Some((change, name, PersistMutation::SetAutorun { name }))
}

fn handle_boot_command(
    ctx: &mut RuntimeContext<'_, '_>,
    script_depth: u8,
    budget: &mut ScriptBudget,
) -> bool {
    if !ctx.state.autorun_enabled {
        write_line(ctx.uart, "event boot autorun=off");
        write_line(ctx.uart, "ok boot_done autorun=off");
        return true;
    }

    let mut name = [0_u8; MAX_PROGRAM_NAME_LEN];
    let name_len = ctx.state.autorun_name_len;
    name[..name_len].copy_from_slice(&ctx.state.autorun_name[..name_len]);
    let Ok(name) = str::from_utf8(&name[..name_len]) else {
        write_line(ctx.uart, "err autorun_name_not_utf8");
        return false;
    };

    ctx.state.boot_runs = ctx.state.boot_runs.saturating_add(1);
    write_str(ctx.uart, "event boot autorun=");
    write_line(ctx.uart, name);
    let ok = execute_program_by_name(name, ctx, script_depth, budget);
    if ok {
        write_str(ctx.uart, "ok boot_done autorun=");
        write_line(ctx.uart, name);
    }
    ok
}

fn run_program_by_name(
    name: &str,
    ctx: &mut RuntimeContext<'_, '_>,
    script_depth: u8,
    budget: &mut ScriptBudget,
) -> bool {
    write_str(ctx.uart, "event run name=");
    write_line(ctx.uart, name);
    let ok = execute_program_by_name(name, ctx, script_depth, budget);
    if ok {
        write_str(ctx.uart, "ok run_done name=");
        write_line(ctx.uart, name);
    }
    ok
}

fn execute_program_by_name(
    name: &str,
    ctx: &mut RuntimeContext<'_, '_>,
    script_depth: u8,
    budget: &mut ScriptBudget,
) -> bool {
    if script_depth >= MAX_SCRIPT_DEPTH {
        write_line(ctx.uart, "err script_too_deep max=4");
        return false;
    }

    let Some(program) = get_program(ctx.state, name) else {
        write_line(ctx.uart, "err program_not_found");
        return false;
    };
    let Ok(script) = str::from_utf8(&program.script[..program.script_len]) else {
        write_line(ctx.uart, "err program_not_utf8");
        return false;
    };

    ctx.state.program_runs = ctx.state.program_runs.saturating_add(1);
    execute_script(script, ctx, script_depth + 1, budget)
}

fn handle_delete_command<'a>(
    rest: &'a str,
    uart: &mut Uart<'_, Blocking>,
    state: &mut DeviceState,
) -> Option<(StateChange, &'a str, bool)> {
    let Some((name, trailing)) = take_token(rest) else {
        write_line(uart, "err delete_expected_name");
        return None;
    };
    if !trailing.trim().is_empty() {
        write_line(uart, "err delete_too_many_args");
        return None;
    }

    if program_exists(state, name) {
        Some((StateChange::Changed, name, true))
    } else {
        Some((StateChange::Unchanged, name, false))
    }
}

fn program_name_matches(program: &ProgramState, name: &str) -> bool {
    program.used
        && program.name_len == name.len()
        && &program.name[..program.name_len] == name.as_bytes()
}

fn get_program(state: &DeviceState, name: &str) -> Option<ProgramState> {
    state
        .programs
        .iter()
        .find(|program| program_name_matches(program, name))
        .copied()
}

fn program_exists(state: &DeviceState, name: &str) -> bool {
    state
        .programs
        .iter()
        .any(|program| program_name_matches(program, name))
}

fn plan_program_save(state: &DeviceState, name: &str, body: &str) -> Option<(StateChange, usize)> {
    let slot = state
        .programs
        .iter()
        .position(|program| program_name_matches(program, name))
        .or_else(|| state.programs.iter().position(|program| !program.used))?;

    let current = &state.programs[slot];
    if current.used
        && current.script_len == body.len()
        && &current.script[..current.script_len] == body.as_bytes()
    {
        return Some((StateChange::Unchanged, slot));
    }

    Some((StateChange::Changed, slot))
}

fn set_program_at_slot(state: &mut DeviceState, slot: usize, name: &str, body: &str) {
    let mut program = ProgramState::new();
    program.used = true;
    program.name_len = name.len();
    program.name[..name.len()].copy_from_slice(name.as_bytes());
    program.script_len = body.len();
    program.script[..body.len()].copy_from_slice(body.as_bytes());
    state.programs[slot] = program;
}

fn delete_program(state: &mut DeviceState, name: &str) -> bool {
    let Some(index) = state
        .programs
        .iter()
        .position(|program| program_name_matches(program, name))
    else {
        return false;
    };

    state.programs[index] = ProgramState::new();
    true
}

fn clear_programs_and_autorun(state: &mut DeviceState) {
    for program in state.programs.iter_mut() {
        program.reset();
    }
    clear_autorun(state);
}

fn apply_autorun_mutation(state: &mut DeviceState, mutation: PersistMutation<'_>) {
    match mutation {
        PersistMutation::SetAutorun { name } => {
            set_autorun(state, name);
        }
        PersistMutation::ClearAutorun => {
            clear_autorun(state);
        }
        _ => {}
    }
}

fn set_autorun(state: &mut DeviceState, name: &str) -> StateChange {
    if autorun_name_matches(state, name) {
        return StateChange::Unchanged;
    }

    state.autorun_enabled = true;
    state.autorun_name_len = name.len();
    state.autorun_name.fill(0);
    state.autorun_name[..name.len()].copy_from_slice(name.as_bytes());
    StateChange::Changed
}

fn clear_autorun(state: &mut DeviceState) -> StateChange {
    if !state.autorun_enabled && state.autorun_name_len == 0 {
        return StateChange::Unchanged;
    }

    state.autorun_enabled = false;
    state.autorun_name_len = 0;
    state.autorun_name = [0; MAX_PROGRAM_NAME_LEN];
    StateChange::Changed
}

fn autorun_name_matches(state: &DeviceState, name: &str) -> bool {
    state.autorun_enabled
        && state.autorun_name_len == name.len()
        && &state.autorun_name[..state.autorun_name_len] == name.as_bytes()
}

fn eval_u64_expression(
    expression: &str,
    uart: &mut Uart<'_, Blocking>,
    state: &DeviceState,
) -> Option<u64> {
    let Some((left_text, rest)) = take_token(expression) else {
        write_line(uart, "err let_expected_value");
        return None;
    };
    let Some(left) = parse_u64_arg(left_text, state) else {
        write_line(uart, "err let_left_expected_number_or_var");
        return None;
    };

    let rest = rest.trim_start();
    if rest.is_empty() {
        return Some(left);
    }

    let Some((op_text, rest)) = take_token(rest) else {
        write_line(uart, "err let_expected_operator");
        return None;
    };
    let Some(op) = parse_arithmetic_op(op_text) else {
        write_line(uart, "err let_operator_expected +|-|*|/|%");
        return None;
    };

    let Some((right_text, trailing)) = take_token(rest) else {
        write_line(uart, "err let_right_expected_number_or_var");
        return None;
    };
    if !trailing.trim().is_empty() {
        write_line(uart, "err let_expression_too_many_args");
        return None;
    }
    let Some(right) = parse_u64_arg(right_text, state) else {
        write_line(uart, "err let_right_expected_number_or_var");
        return None;
    };

    eval_u64_binary_op(left, op, right, uart)
}

fn eval_u64_binary_op(
    left: u64,
    op: ArithmeticOp,
    right: u64,
    uart: &mut Uart<'_, Blocking>,
) -> Option<u64> {
    match checked_u64_binary_op(left, op, right) {
        Ok(value) => Some(value),
        Err(ArithmeticError::DivideByZero) => {
            write_line(uart, "err let_divide_by_zero");
            None
        }
        Err(ArithmeticError::RemainderByZero) => {
            write_line(uart, "err let_remainder_by_zero");
            None
        }
        Err(ArithmeticError::Overflow) => {
            write_line(uart, "err let_arithmetic_overflow");
            None
        }
    }
}

fn variable_name_matches(variable: &VariableState, name: &str) -> bool {
    variable.used
        && variable.name_len == name.len()
        && &variable.name[..variable.name_len] == name.as_bytes()
}

fn get_variable(state: &DeviceState, name: &str) -> Option<u64> {
    state
        .variables
        .iter()
        .find(|variable| variable_name_matches(variable, name))
        .map(|variable| variable.value)
}

fn set_variable(state: &mut DeviceState, name: &str, value: u64) -> bool {
    if let Some(variable) = state
        .variables
        .iter_mut()
        .find(|variable| variable_name_matches(variable, name))
    {
        variable.value = value;
        return true;
    }

    let Some(variable) = state.variables.iter_mut().find(|variable| !variable.used) else {
        return false;
    };

    variable.used = true;
    variable.name_len = name.len();
    variable.name[..name.len()].copy_from_slice(name.as_bytes());
    variable.value = value;
    true
}

fn set_blink(
    value: &str,
    uart: &mut Uart<'_, Blocking>,
    led: &mut Flex<'_>,
    state: &mut DeviceState,
) -> bool {
    let Some(ms) = parse_u32_arg(value.trim(), state) else {
        write_line(uart, "err blink_ms_must_be_number_or_var");
        return false;
    };

    if !(50..=60_000).contains(&ms) {
        write_line(uart, "err blink_ms_range 50..60000");
        return false;
    }

    disable_pwm_pin(LED_PIN, led, state);
    state.blink_ms = ms;
    state.pin_owners[LED_PIN as usize] = PinOwner::Led;
    state.pin_modes[LED_PIN as usize] = PinMode::Output;
    write_str(uart, "ok blink_ms=");
    write_u64(uart, ms as u64);
    write_line(uart, "");
    true
}

fn handle_pin_command(
    rest: &str,
    uart: &mut Uart<'_, Blocking>,
    led: &mut Flex<'_>,
    state: &mut DeviceState,
) -> bool {
    let mut parts = rest.split_whitespace();
    let Some(pin) = parse_next_u8(parts.next(), uart, "pin", state) else {
        return false;
    };
    let Some(action) = parts.next() else {
        write_line(uart, "err pin_expected_action mode|write|toggle|read");
        return false;
    };

    match action {
        "mode" => {
            let Some(mode_text) = parts.next() else {
                write_line(uart, "err pin_mode_expected");
                return false;
            };
            if parts.next().is_some() {
                write_line(uart, "err pin_too_many_args");
                return false;
            }

            let Some(mode) = parse_pin_mode(mode_text, uart) else {
                return false;
            };
            if !prepare_gpio_pin(pin, mode, uart, state) {
                return false;
            }
            if pin == LED_PIN {
                disable_pwm_pin(pin, led, state);
            }

            if mode == PinMode::Output {
                with_runtime_pin(pin, led, |gpio| {
                    configure_flex_output(gpio, state.pin_levels[pin as usize]);
                });
            } else {
                with_runtime_pin(pin, led, |gpio| configure_flex_input(gpio, mode));
            }

            state.pin_modes[pin as usize] = mode;
            write_str(uart, "ok pin=");
            write_u64(uart, pin as u64);
            write_str(uart, " mode=");
            write_line(uart, pin_mode_name(mode));
            true
        }
        "write" => {
            let Some(level_text) = parts.next() else {
                write_line(uart, "err pin_write_expected_state");
                return false;
            };
            if parts.next().is_some() {
                write_line(uart, "err pin_too_many_args");
                return false;
            }

            let Some(high) = parse_level(level_text, uart) else {
                return false;
            };
            if !validate_output_gpio(pin, uart) || !claim_pin(state, pin, PinOwner::Pin, uart) {
                return false;
            }
            if pin == LED_PIN {
                disable_pwm_pin(pin, led, state);
            }

            with_runtime_pin(pin, led, |pin| configure_flex_output(pin, high));
            state.pin_modes[pin as usize] = PinMode::Output;
            state.pin_levels[pin as usize] = high;
            if pin == LED_PIN {
                state.blink_ms = 0;
                state.led_on = high;
            }

            write_str(uart, "ok pin=");
            write_u64(uart, pin as u64);
            write_str(uart, " level=");
            write_line(uart, if high { "on" } else { "off" });
            true
        }
        "toggle" => {
            if parts.next().is_some() {
                write_line(uart, "err pin_too_many_args");
                return false;
            }
            if !validate_output_gpio(pin, uart) || !claim_pin(state, pin, PinOwner::Pin, uart) {
                return false;
            }
            if pin == LED_PIN {
                disable_pwm_pin(pin, led, state);
            }

            let next = !state.pin_levels[pin as usize];
            with_runtime_pin(pin, led, |pin| {
                configure_flex_output(pin, !next);
                pin.toggle();
            });
            state.pin_modes[pin as usize] = PinMode::Output;
            state.pin_levels[pin as usize] = next;
            if pin == LED_PIN {
                state.blink_ms = 0;
                state.led_on = next;
            }

            write_str(uart, "ok pin=");
            write_u64(uart, pin as u64);
            write_str(uart, " level=");
            write_line(uart, if next { "on" } else { "off" });
            true
        }
        "read" => {
            if parts.next().is_some() {
                write_line(uart, "err pin_too_many_args");
                return false;
            }
            let Some(high) = read_pin_level(pin, uart, led, state) else {
                return false;
            };

            write_str(uart, "ok pin=");
            write_u64(uart, pin as u64);
            write_str(uart, " level=");
            write_line(uart, if high { "on" } else { "off" });
            true
        }
        _ => {
            write_line(uart, "err pin_unknown_action");
            false
        }
    }
}

fn read_pin_level(
    pin: u8,
    uart: &mut Uart<'_, Blocking>,
    led: &mut Flex<'_>,
    state: &mut DeviceState,
) -> Option<bool> {
    read_pin_level_for_owner(pin, PinOwner::Pin, uart, led, state)
}

fn read_pin_level_for_owner(
    pin: u8,
    owner: PinOwner,
    uart: &mut Uart<'_, Blocking>,
    led: &mut Flex<'_>,
    state: &mut DeviceState,
) -> Option<bool> {
    if !validate_gpio(pin, uart) || !claim_pin(state, pin, owner, uart) {
        return None;
    }
    if pin == LED_PIN {
        disable_pwm_pin(pin, led, state);
    }

    Some(if state.pin_modes[pin as usize] == PinMode::Output {
        state.pin_levels[pin as usize]
    } else {
        let mode = match state.pin_modes[pin as usize] {
            PinMode::InputPullup | PinMode::InputPulldown => state.pin_modes[pin as usize],
            _ => PinMode::Input,
        };
        state.pin_modes[pin as usize] = mode;
        with_runtime_pin(pin, led, |pin| {
            configure_flex_input(pin, mode);
            pin.is_high()
        })
    })
}

fn handle_pwm_command(
    rest: &str,
    uart: &mut Uart<'_, Blocking>,
    led: &mut Flex<'_>,
    ledc: &Ledc<'_>,
    state: &mut DeviceState,
) -> bool {
    let mut parts = rest.split_whitespace();
    let Some(pin) = parse_next_u8(parts.next(), uart, "pwm_pin", state) else {
        return false;
    };

    if matches!(parts.clone().next(), Some("stop")) {
        return handle_pwm_stop(pin, parts, uart, led, state);
    }

    handle_pwm_start(pin, parts, uart, led, ledc, state)
}

fn handle_pwm_stop(
    pin: u8,
    mut parts: core::str::SplitWhitespace<'_>,
    uart: &mut Uart<'_, Blocking>,
    led: &mut Flex<'_>,
    state: &mut DeviceState,
) -> bool {
    parts.next();
    if parts.next().is_some() {
        write_line(uart, "err pwm_stop_too_many_args");
        return false;
    }
    if !validate_gpio(pin, uart) {
        return false;
    }
    disable_pwm_pin(pin, led, state);
    release_pin_if_owner(state, pin, PinOwner::Pwm);
    write_str(uart, "ok pwm pin=");
    write_u64(uart, pin as u64);
    write_line(uart, " stopped");
    true
}

fn handle_pwm_start(
    pin: u8,
    parts: core::str::SplitWhitespace<'_>,
    uart: &mut Uart<'_, Blocking>,
    led: &mut Flex<'_>,
    ledc: &Ledc<'_>,
    state: &mut DeviceState,
) -> bool {
    let mut freq = None;
    let mut duty = None;
    for token in parts {
        if let Some(value) = token.strip_prefix("freq=") {
            freq = parse_u64_arg(value, state);
        } else if let Some(value) = token.strip_prefix("duty=") {
            duty = parse_u64_arg(value, state);
        } else {
            write_line(uart, "err pwm_expected_freq_duty");
            return false;
        }
    }

    let Some(freq) = freq else {
        write_line(uart, "err pwm_missing_freq");
        return false;
    };
    let Some(duty) = duty else {
        write_line(uart, "err pwm_missing_duty");
        return false;
    };
    if !(1..=1_000_000).contains(&freq) {
        write_line(uart, "err pwm_freq_range 1..1000000");
        return false;
    }
    if duty > 1023 {
        write_line(uart, "err pwm_duty_range 0..1023");
        return false;
    }
    if !validate_output_gpio(pin, uart) || !claim_pin(state, pin, PinOwner::Pwm, uart) {
        return false;
    }

    if pin == LED_PIN {
        state.blink_ms = 0;
    }

    let Some(channel_index) = assign_pwm_channel(state, pin) else {
        release_pin_if_owner(state, pin, PinOwner::Pwm);
        write_str(uart, "err pwm_channels_full max=");
        write_u64(uart, MAX_PWM_CHANNELS as u64);
        write_line(uart, "");
        return false;
    };

    disable_pwm_pin(pin, led, state);

    if !configure_pwm_driver(channel_index, pin, freq, duty as u16, led, ledc, uart) {
        clear_pwm_channel(state, channel_index);
        release_pin_if_owner(state, pin, PinOwner::Pwm);
        return false;
    }

    state.pwm_channels[channel_index] = PwmChannelState {
        enabled: true,
        pin,
        freq,
        duty: duty as u16,
    };

    write_str(uart, "ok pwm pin=");
    write_u64(uart, pin as u64);
    write_str(uart, " freq=");
    write_u64(uart, freq);
    write_str(uart, " duty=");
    write_u64(uart, duty);
    write_str(uart, " channel=");
    write_u64(uart, channel_index as u64);
    write_line(uart, " driver=ledc");
    true
}

fn assign_pwm_channel(state: &DeviceState, pin: u8) -> Option<usize> {
    state
        .pwm_channels
        .iter()
        .position(|channel| channel.enabled && channel.pin == pin)
        .or_else(|| {
            state
                .pwm_channels
                .iter()
                .position(|channel| !channel.enabled)
        })
}

fn clear_pwm_channel(state: &mut DeviceState, channel_index: usize) {
    state.pwm_channels[channel_index] = PwmChannelState::new();
}

fn disable_pwm_pin(pin: u8, led: &mut Flex<'_>, state: &mut DeviceState) -> bool {
    let Some(channel_index) = state
        .pwm_channels
        .iter()
        .position(|channel| channel.enabled && channel.pin == pin)
    else {
        return false;
    };

    if pin == LED_PIN {
        pwm_output_signal(channel_index).disconnect_from(&*led);
    } else {
        let gpio = unsafe { AnyPin::steal(pin) };
        pwm_output_signal(channel_index).disconnect_from(&gpio);
    }
    clear_pwm_channel(state, channel_index);
    state.pin_levels[pin as usize] = false;
    state.pin_modes[pin as usize] = PinMode::Unused;
    with_runtime_pin(pin, led, |gpio| configure_flex_output(gpio, false));
    true
}

fn configure_pwm_driver(
    channel_index: usize,
    pin: u8,
    freq: u64,
    duty: u16,
    led: &mut Flex<'_>,
    ledc: &Ledc<'_>,
    uart: &mut Uart<'_, Blocking>,
) -> bool {
    let Some(freq) = u32::try_from(freq).ok() else {
        write_line(uart, "err pwm_freq_range 1..1000000");
        return false;
    };

    let mut timer = ledc.timer::<LowSpeed>(pwm_timer_number(channel_index));
    if timer
        .configure(timer::config::Config {
            duty: timer::config::Duty::Duty10Bit,
            clock_source: timer::LSClockSource::APBClk,
            frequency: Rate::from_hz(freq),
        })
        .is_err()
    {
        write_line(uart, "err pwm_timer_config_failed");
        return false;
    }

    if pin == LED_PIN {
        let mut channel = ledc.channel::<LowSpeed>(pwm_channel_number(channel_index), NoPin);
        if channel
            .configure(channel::config::Config {
                timer: &timer,
                duty_pct: 0,
                drive_mode: DriveMode::PushPull,
            })
            .is_err()
        {
            write_line(uart, "err pwm_channel_config_failed");
            return false;
        }

        channel.set_duty_hw(duty as u32);
        configure_flex_output(led, false);
        pwm_output_signal(channel_index).connect_to(&*led);
    } else {
        let gpio = unsafe { AnyPin::steal(pin) };
        let mut channel = ledc.channel::<LowSpeed>(pwm_channel_number(channel_index), gpio);
        if channel
            .configure(channel::config::Config {
                timer: &timer,
                duty_pct: 0,
                drive_mode: DriveMode::PushPull,
            })
            .is_err()
        {
            write_line(uart, "err pwm_channel_config_failed");
            return false;
        }

        channel.set_duty_hw(duty as u32);
    }
    true
}

fn pwm_timer_number(channel_index: usize) -> timer::Number {
    match channel_index {
        0 => timer::Number::Timer0,
        1 => timer::Number::Timer1,
        2 => timer::Number::Timer2,
        _ => timer::Number::Timer3,
    }
}

fn pwm_channel_number(channel_index: usize) -> channel::Number {
    match channel_index {
        0 => channel::Number::Channel0,
        1 => channel::Number::Channel1,
        2 => channel::Number::Channel2,
        _ => channel::Number::Channel3,
    }
}

fn pwm_output_signal(channel_index: usize) -> OutputSignal {
    match channel_index {
        0 => OutputSignal::LEDC_LS_SIG0,
        1 => OutputSignal::LEDC_LS_SIG1,
        2 => OutputSignal::LEDC_LS_SIG2,
        _ => OutputSignal::LEDC_LS_SIG3,
    }
}

fn handle_adc_command(rest: &str, uart: &mut Uart<'_, Blocking>, state: &mut DeviceState) -> bool {
    let mut parts = rest.split_whitespace();
    if parts.next() != Some("read") {
        write_line(uart, "err adc_expected_read");
        return false;
    }

    let Some(pin) = parse_next_u8(parts.next(), uart, "adc_pin", state) else {
        return false;
    };

    let mut max_mv = None;
    let mut samples = 1_u8;
    for token in parts {
        if let Some(value) = token.strip_prefix("max_mv=") {
            let Some(value) = parse_u64_arg(value, state) else {
                write_line(uart, "err number_or_var_expected adc_max_mv");
                return false;
            };
            if !(1..=3900).contains(&value) {
                write_line(uart, "err adc_max_mv_range 1..3900");
                return false;
            }
            max_mv = Some(value);
        } else if let Some(value) = token.strip_prefix("samples=") {
            let Some(value) = parse_u64_arg(value, state) else {
                write_line(uart, "err number_or_var_expected adc_samples");
                return false;
            };
            if !(1..=ADC_MAX_SAMPLES as u64).contains(&value) {
                write_line(uart, "err adc_samples_range 1..64");
                return false;
            }
            samples = value as u8;
        } else {
            write_line(uart, "err adc_expected_max_mv_or_samples");
            return false;
        }
    }

    if !validate_adc_gpio(pin, uart, state) || !claim_pin(state, pin, PinOwner::Adc, uart) {
        return false;
    }
    let adc2_pin = is_adc2_pin(pin);
    if adc2_pin {
        state.adc2_in_use = true;
    }

    let Some(raw) = read_adc_raw(pin, samples, uart) else {
        if adc2_pin {
            state.adc2_in_use = false;
        }
        release_pin_if_owner(state, pin, PinOwner::Adc);
        return false;
    };
    if adc2_pin {
        state.adc2_in_use = false;
    }
    release_pin_if_owner(state, pin, PinOwner::Adc);

    write_str(uart, "ok adc pin=");
    write_u64(uart, pin as u64);
    write_str(uart, " samples=");
    write_u64(uart, samples as u64);
    if let Some(max_mv) = max_mv {
        write_str(uart, " max_mv=");
        write_u64(uart, max_mv);
        write_str(uart, " mv=");
        write_u64(uart, scale_adc_raw_to_mv(raw, max_mv));
    }
    write_str(uart, " raw=");
    write_u64(uart, raw as u64);
    write_line(uart, " driver=adc");
    true
}

fn read_adc_raw(pin: u8, samples: u8, uart: &mut Uart<'_, Blocking>) -> Option<u16> {
    if matches!(pin, 32..=39) {
        read_adc1_raw(pin, samples, uart)
    } else {
        read_adc2_raw(pin, samples, uart)
    }
}

macro_rules! define_adc_reader {
    ($name:ident, $adc:ident, $gpio:ident) => {
        fn $name(samples: u8, uart: &mut Uart<'_, Blocking>) -> Option<u16> {
            let adc_peripheral = unsafe { esp_hal::peripherals::$adc::steal() };
            let gpio = unsafe { esp_hal::peripherals::$gpio::steal() };
            let mut config = AdcConfig::new();
            let mut adc_pin = config.enable_pin(gpio, Attenuation::_11dB);
            let mut adc = Adc::new(adc_peripheral, config);
            let mut attempts = 0_u8;
            let mut read_samples = 0_u8;
            let mut sum = 0_u32;

            while read_samples < samples {
                match adc.read_oneshot(&mut adc_pin) {
                    Ok(raw) => {
                        sum += raw as u32;
                        read_samples += 1;
                        attempts = 0;
                    }
                    Err(_) => {
                        attempts += 1;
                        if attempts >= ADC_READ_MAX_ATTEMPTS {
                            write_line(uart, "err adc_read_failed");
                            break;
                        }
                    }
                }
            }

            (read_samples == samples)
                .then_some(((sum + samples as u32 / 2) / samples as u32) as u16)
        }
    };
}

define_adc_reader!(read_adc1_gpio32_raw, ADC1, GPIO32);
define_adc_reader!(read_adc1_gpio33_raw, ADC1, GPIO33);
define_adc_reader!(read_adc1_gpio34_raw, ADC1, GPIO34);
define_adc_reader!(read_adc1_gpio35_raw, ADC1, GPIO35);
define_adc_reader!(read_adc1_gpio36_raw, ADC1, GPIO36);
define_adc_reader!(read_adc1_gpio37_raw, ADC1, GPIO37);
define_adc_reader!(read_adc1_gpio38_raw, ADC1, GPIO38);
define_adc_reader!(read_adc1_gpio39_raw, ADC1, GPIO39);
define_adc_reader!(read_adc2_gpio0_raw, ADC2, GPIO0);
define_adc_reader!(read_adc2_gpio2_raw, ADC2, GPIO2);
define_adc_reader!(read_adc2_gpio4_raw, ADC2, GPIO4);
define_adc_reader!(read_adc2_gpio12_raw, ADC2, GPIO12);
define_adc_reader!(read_adc2_gpio13_raw, ADC2, GPIO13);
define_adc_reader!(read_adc2_gpio14_raw, ADC2, GPIO14);
define_adc_reader!(read_adc2_gpio15_raw, ADC2, GPIO15);
define_adc_reader!(read_adc2_gpio25_raw, ADC2, GPIO25);
define_adc_reader!(read_adc2_gpio26_raw, ADC2, GPIO26);
define_adc_reader!(read_adc2_gpio27_raw, ADC2, GPIO27);

fn read_adc1_raw(pin: u8, samples: u8, uart: &mut Uart<'_, Blocking>) -> Option<u16> {
    match pin {
        32 => read_adc1_gpio32_raw(samples, uart),
        33 => read_adc1_gpio33_raw(samples, uart),
        34 => read_adc1_gpio34_raw(samples, uart),
        35 => read_adc1_gpio35_raw(samples, uart),
        36 => read_adc1_gpio36_raw(samples, uart),
        37 => read_adc1_gpio37_raw(samples, uart),
        38 => read_adc1_gpio38_raw(samples, uart),
        39 => read_adc1_gpio39_raw(samples, uart),
        _ => {
            write_line(uart, "err adc_unsupported_pin");
            None
        }
    }
}

fn read_adc2_raw(pin: u8, samples: u8, uart: &mut Uart<'_, Blocking>) -> Option<u16> {
    match pin {
        0 => read_adc2_gpio0_raw(samples, uart),
        2 => read_adc2_gpio2_raw(samples, uart),
        4 => read_adc2_gpio4_raw(samples, uart),
        12 => read_adc2_gpio12_raw(samples, uart),
        13 => read_adc2_gpio13_raw(samples, uart),
        14 => read_adc2_gpio14_raw(samples, uart),
        15 => read_adc2_gpio15_raw(samples, uart),
        25 => read_adc2_gpio25_raw(samples, uart),
        26 => read_adc2_gpio26_raw(samples, uart),
        27 => read_adc2_gpio27_raw(samples, uart),
        _ => {
            write_line(uart, "err adc_unsupported_pin");
            None
        }
    }
}

fn scale_adc_raw_to_mv(raw: u16, max_mv: u64) -> u64 {
    ((raw as u64).saturating_mul(max_mv).saturating_add(2047)) / 4095
}

fn handle_i2c_command(rest: &str, uart: &mut Uart<'_, Blocking>, state: &mut DeviceState) -> bool {
    if state.i2c_configured {
        write_line(uart, "err i2c_already_configured");
        return false;
    }

    let mut sda = None;
    let mut scl = None;
    let mut speed = None;
    for token in rest.split_whitespace() {
        if let Some(value) = token.strip_prefix("sda=") {
            sda = parse_u8_arg(value, state);
        } else if let Some(value) = token.strip_prefix("scl=") {
            scl = parse_u8_arg(value, state);
        } else if let Some(value) = token.strip_prefix("speed=") {
            speed = parse_u64_arg(value, state);
        } else {
            write_line(uart, "err i2c_expected_sda_scl_speed");
            return false;
        }
    }

    let Some(sda) = sda else {
        write_line(uart, "err i2c_missing_sda");
        return false;
    };
    let Some(scl) = scl else {
        write_line(uart, "err i2c_missing_scl");
        return false;
    };
    let Some(speed) = speed else {
        write_line(uart, "err i2c_missing_speed");
        return false;
    };
    if sda == scl {
        write_line(uart, "err i2c_same_sda_scl");
        return false;
    }
    if !(1..=1_000_000).contains(&speed) {
        write_line(uart, "err i2c_speed_range 1..1000000");
        return false;
    }
    if !validate_output_gpio(sda, uart) || !validate_output_gpio(scl, uart) {
        return false;
    }
    write_line(uart, "err i2c_driver_unimplemented");
    false
}

fn handle_spi_command(rest: &str, uart: &mut Uart<'_, Blocking>, state: &mut DeviceState) -> bool {
    if state.spi_configured {
        write_line(uart, "err spi_already_configured");
        return false;
    }

    let mut sck = None;
    let mut miso = None;
    let mut mosi = None;
    let mut cs = None;
    let mut speed = None;
    for token in rest.split_whitespace() {
        if let Some(value) = token.strip_prefix("sck=") {
            sck = parse_u8_arg(value, state);
        } else if let Some(value) = token.strip_prefix("miso=") {
            miso = parse_u8_arg(value, state);
        } else if let Some(value) = token.strip_prefix("mosi=") {
            mosi = parse_u8_arg(value, state);
        } else if let Some(value) = token.strip_prefix("cs=") {
            cs = parse_u8_arg(value, state);
        } else if let Some(value) = token.strip_prefix("speed=") {
            speed = parse_u64_arg(value, state);
        } else {
            write_line(uart, "err spi_expected_pins_speed");
            return false;
        }
    }

    let Some(sck) = sck else {
        write_line(uart, "err spi_missing_sck");
        return false;
    };
    let Some(miso) = miso else {
        write_line(uart, "err spi_missing_miso");
        return false;
    };
    let Some(mosi) = mosi else {
        write_line(uart, "err spi_missing_mosi");
        return false;
    };
    let Some(speed) = speed else {
        write_line(uart, "err spi_missing_speed");
        return false;
    };
    if !pins_are_distinct(&[Some(sck), Some(miso), Some(mosi), cs]) {
        write_line(uart, "err spi_duplicate_pin");
        return false;
    }
    if !(1..=80_000_000).contains(&speed) {
        write_line(uart, "err spi_speed_range 1..80000000");
        return false;
    }
    if !validate_output_gpio(sck, uart)
        || !validate_gpio(miso, uart)
        || !validate_output_gpio(mosi, uart)
        || cs.is_some_and(|pin| !validate_output_gpio(pin, uart))
    {
        return false;
    }
    write_line(uart, "err spi_driver_unimplemented");
    false
}

fn handle_aux_uart_command(
    rest: &str,
    uart: &mut Uart<'_, Blocking>,
    state: &mut DeviceState,
) -> bool {
    if state.aux_uart_configured {
        write_line(uart, "err aux_uart_already_configured");
        return false;
    }

    let mut tx = None;
    let mut rx = None;
    let mut baud = None;
    for token in rest.split_whitespace() {
        if let Some(value) = token.strip_prefix("tx=") {
            tx = parse_u8_arg(value, state);
        } else if let Some(value) = token.strip_prefix("rx=") {
            rx = parse_u8_arg(value, state);
        } else if let Some(value) = token.strip_prefix("baud=") {
            baud = parse_u64_arg(value, state);
        } else {
            write_line(uart, "err uart_expected_tx_rx_baud");
            return false;
        }
    }

    let Some(tx) = tx else {
        write_line(uart, "err uart_missing_tx");
        return false;
    };
    let Some(rx) = rx else {
        write_line(uart, "err uart_missing_rx");
        return false;
    };
    let Some(baud) = baud else {
        write_line(uart, "err uart_missing_baud");
        return false;
    };
    if tx == rx {
        write_line(uart, "err uart_same_tx_rx");
        return false;
    }
    if !(300..=5_000_000).contains(&baud) {
        write_line(uart, "err uart_baud_range 300..5000000");
        return false;
    }
    if !validate_output_gpio(tx, uart) || !validate_gpio(rx, uart) {
        return false;
    }
    write_line(uart, "err uart_driver_unimplemented");
    false
}

fn handle_wifi_command(rest: &str, uart: &mut Uart<'_, Blocking>, state: &mut DeviceState) -> bool {
    match rest.trim() {
        "on" => {
            write_line(uart, "err wifi_driver_unimplemented");
            false
        }
        "off" => {
            state.wifi_enabled = false;
            write_line(uart, "ok wifi=off");
            true
        }
        value if value.starts_with("connect ") => {
            write_line(uart, "err wifi_driver_unimplemented");
            false
        }
        _ => {
            write_line(uart, "err wifi_expected_on_off_connect");
            false
        }
    }
}

fn handle_on_command(
    rest: &str,
    uart: &mut Uart<'_, Blocking>,
    led: &mut Flex<'_>,
    state: &mut DeviceState,
) -> bool {
    let Some((target, rest)) = take_token(rest) else {
        write_line(uart, "err on_expected_target pin");
        return false;
    };

    match target {
        "pin" => handle_on_pin_command(rest, uart, led, state),
        _ => {
            write_line(uart, "err on_expected_target pin");
            false
        }
    }
}

fn handle_on_pin_command(
    rest: &str,
    uart: &mut Uart<'_, Blocking>,
    led: &mut Flex<'_>,
    state: &mut DeviceState,
) -> bool {
    let Some((pin_text, rest)) = take_token(rest) else {
        write_line(uart, "err on_pin_expected_gpio");
        return false;
    };
    let Some(pin) = parse_u8_arg(pin_text, state) else {
        write_line(uart, "err on_pin_expected_number_or_var");
        return false;
    };

    let Some((action, rest)) = take_token(rest) else {
        write_line(uart, "err on_pin_expected_rising_falling_change_off");
        return false;
    };

    if action == "off" {
        if !rest.trim().is_empty() {
            write_line(uart, "err on_pin_off_too_many_args");
            return false;
        }

        let removed = disable_pin_event(state, pin);
        write_str(uart, "ok pin_event pin=");
        write_u64(uart, pin as u64);
        write_str(uart, " off removed=");
        write_u64(uart, if removed { 1 } else { 0 });
        write_line(uart, "");
        return true;
    }

    let Some(trigger) = parse_pin_event_trigger(action) else {
        write_line(uart, "err on_pin_expected_rising_falling_change_off");
        return false;
    };
    let Some((debounce_ms, rest)) = parse_optional_pin_event_debounce(rest, uart, state) else {
        return false;
    };
    let Some(body) = parse_do_block(rest, uart, "on_pin") else {
        return false;
    };
    if body.len() > MAX_PIN_EVENT_SCRIPT_LEN {
        write_line(uart, "err pin_event_script_too_long max=128");
        return false;
    }

    let Some(slot) = find_pin_event_slot(state, pin) else {
        write_line(uart, "err pin_events_full max=4");
        return false;
    };
    let Some(last_level) = read_pin_level_for_owner(pin, PinOwner::Event, uart, led, state) else {
        return false;
    };

    let event = &mut state.pin_events[slot];
    event.enabled = true;
    event.pin = pin;
    event.trigger = trigger;
    event.last_level = last_level;
    event.debounce_ms = debounce_ms;
    event.debounce_active = false;
    event.debounce_level = last_level;
    event.debounce_since_ms = 0;
    event.script_len = body.len();
    event.script.fill(0);
    event.script[..body.len()].copy_from_slice(body.as_bytes());

    write_str(uart, "ok pin_event id=");
    write_u64(uart, slot as u64);
    write_str(uart, " pin=");
    write_u64(uart, pin as u64);
    write_str(uart, " trigger=");
    write_str(uart, pin_event_trigger_name(trigger));
    write_str(uart, " debounce_ms=");
    write_u64(uart, debounce_ms);
    write_str(uart, " level=");
    write_line(uart, if last_level { "on" } else { "off" });
    true
}

fn parse_optional_pin_event_debounce<'a>(
    rest: &'a str,
    uart: &mut Uart<'_, Blocking>,
    state: &DeviceState,
) -> Option<(u64, &'a str)> {
    let rest = rest.trim_start();
    let Some(after_debounce) = rest.strip_prefix("debounce") else {
        return Some((0, rest));
    };

    if after_debounce
        .as_bytes()
        .first()
        .is_some_and(|byte| !byte.is_ascii_whitespace())
    {
        return Some((0, rest));
    }

    let Some((ms_text, rest)) = take_token(after_debounce) else {
        write_line(uart, "err on_pin_debounce_expected_ms");
        return None;
    };
    let Some(debounce_ms) = parse_u64_arg(ms_text, state) else {
        write_line(uart, "err number_or_var_expected on_pin_debounce_ms");
        return None;
    };
    if debounce_ms > MAX_PIN_EVENT_DEBOUNCE_MS {
        write_line(uart, "err on_pin_debounce_ms_range 0..60000");
        return None;
    }

    Some((debounce_ms, rest))
}

fn find_pin_event_slot(state: &DeviceState, pin: u8) -> Option<usize> {
    state
        .pin_events
        .iter()
        .position(|event| event.enabled && event.pin == pin)
        .or_else(|| state.pin_events.iter().position(|event| !event.enabled))
}

fn disable_pin_event(state: &mut DeviceState, pin: u8) -> bool {
    let mut removed = false;
    for event in state.pin_events.iter_mut() {
        if event.enabled && event.pin == pin {
            event.reset();
            removed = true;
        }
    }
    if removed {
        release_pin_if_owner(state, pin, PinOwner::Event);
    }

    removed
}

fn handle_timer_command(
    rest: &str,
    uart: &mut Uart<'_, Blocking>,
    state: &mut DeviceState,
) -> bool {
    let Some((id_text, rest)) = take_token(rest) else {
        write_line(uart, "err missing_timer_id");
        return false;
    };
    let Some(id) = parse_u8_arg(id_text, state) else {
        write_line(uart, "err number_or_var_expected timer_id");
        return false;
    };
    if id as usize >= state.timers.len() {
        write_line(uart, "err timer_id_range 0..3");
        return false;
    }

    let Some((action, rest)) = take_token(rest) else {
        write_line(uart, "err timer_expected_every_after_stop");
        return false;
    };

    if action == "stop" {
        if !rest.trim().is_empty() {
            write_line(uart, "err timer_stop_too_many_args");
            return false;
        }

        state.timers[id as usize].reset();
        write_str(uart, "ok timer=");
        write_u64(uart, id as u64);
        write_line(uart, " stopped");
        return true;
    }

    let repeat = match action {
        "every" => true,
        "after" => false,
        _ => {
            write_line(uart, "err timer_expected_every_after_stop");
            return false;
        }
    };

    let Some((duration_text, rest)) = take_token(rest) else {
        write_line(uart, "err missing_timer_ms");
        return false;
    };
    let Some(duration_ms) = parse_u64_arg(duration_text, state) else {
        write_line(uart, "err number_or_var_expected timer_ms");
        return false;
    };

    let mut body = "";
    if !rest.trim().is_empty() {
        let Some(parsed_body) = parse_do_block(rest, uart, "timer") else {
            return false;
        };
        if parsed_body.len() > MAX_TIMER_SCRIPT_LEN {
            write_line(uart, "err timer_script_too_long max=128");
            return false;
        }

        body = parsed_body;
    }

    if !(1..=86_400_000).contains(&duration_ms) {
        write_line(uart, "err timer_ms_range 1..86400000");
        return false;
    }

    let now_ms = state.boot_time.elapsed().as_millis();
    let timer = &mut state.timers[id as usize];
    timer.enabled = true;
    timer.repeat = repeat;
    timer.duration_ms = duration_ms;
    timer.next_ms = now_ms.saturating_add(duration_ms);
    timer.script_len = body.len();
    timer.script.fill(0);
    timer.script[..body.len()].copy_from_slice(body.as_bytes());

    write_str(uart, "ok timer=");
    write_u64(uart, id as u64);
    write_str(uart, if repeat { " every_ms=" } else { " after_ms=" });
    write_u64(uart, duration_ms);
    if !body.is_empty() {
        write_str(uart, " do=on");
    }
    write_line(uart, "");
    true
}

fn handle_sleep_command(
    rest: &str,
    ctx: &mut RuntimeContext<'_, '_>,
    budget: &mut ScriptBudget,
) -> bool {
    let Some(duration_ms) = parse_u64_arg(rest.trim(), ctx.state) else {
        write_line(ctx.uart, "err sleep_ms_must_be_number_or_var");
        return false;
    };
    if !(1..=86_400_000).contains(&duration_ms) {
        write_line(ctx.uart, "err sleep_ms_range 1..86400000");
        return false;
    }
    let Some(remaining_sleep_ms) = budget.sleep_remaining_ms.checked_sub(duration_ms) else {
        write_str(ctx.uart, "err sleep_budget_exceeded max_total_ms=");
        write_u64(ctx.uart, MAX_SCRIPT_SLEEP_BUDGET_MS);
        write_str(ctx.uart, " remaining_ms=");
        write_u64(ctx.uart, budget.sleep_remaining_ms);
        write_line(ctx.uart, " use_timer_for_long_waits");
        return false;
    };
    budget.sleep_remaining_ms = remaining_sleep_ms;

    let started = Instant::now();
    let duration = Duration::from_millis(duration_ms);
    while started.elapsed() < duration {
        run_background_tasks(ctx);
    }

    write_str(ctx.uart, "ok sleep_ms=");
    write_u64(ctx.uart, duration_ms);
    write_line(ctx.uart, "");
    true
}

fn configure_flex_output(pin: &mut Flex<'_>, high: bool) {
    pin.set_level(if high { Level::High } else { Level::Low });
    pin.apply_output_config(&OutputConfig::default());
    pin.set_input_enable(false);
    pin.set_output_enable(true);
}

fn configure_flex_input(pin: &mut Flex<'_>, mode: PinMode) {
    let pull = match mode {
        PinMode::InputPullup => Pull::Up,
        PinMode::InputPulldown => Pull::Down,
        _ => Pull::None,
    };

    pin.set_output_enable(false);
    pin.apply_input_config(&InputConfig::default().with_pull(pull));
    pin.set_input_enable(true);
}

fn with_runtime_pin<R>(pin: u8, led: &mut Flex<'_>, action: impl FnOnce(&mut Flex<'_>) -> R) -> R {
    if pin == LED_PIN {
        action(led)
    } else {
        let mut gpio = Flex::new(unsafe { AnyPin::steal(pin) });
        action(&mut gpio)
    }
}

fn prepare_gpio_pin(
    pin: u8,
    mode: PinMode,
    uart: &mut Uart<'_, Blocking>,
    state: &mut DeviceState,
) -> bool {
    if mode == PinMode::Output && !validate_output_gpio(pin, uart) {
        return false;
    }

    if mode != PinMode::Output && !validate_gpio(pin, uart) {
        return false;
    }

    claim_pin(state, pin, PinOwner::Pin, uart)
}

fn validate_gpio(pin: u8, uart: &mut Uart<'_, Blocking>) -> bool {
    if !is_esp32_gpio(pin) {
        write_str(uart, "err gpio_invalid pin=");
        write_u64(uart, pin as u64);
        write_line(uart, " chip=esp32");
        return false;
    }

    match pin {
        1 | 3 => {
            write_str(uart, "err gpio_reserved pin=");
            write_u64(uart, pin as u64);
            write_line(uart, " reason=uart0_serial_monitor");
            false
        }
        6..=11 => {
            write_str(uart, "err gpio_reserved pin=");
            write_u64(uart, pin as u64);
            write_line(uart, " reason=spi_flash");
            false
        }
        _ => true,
    }
}

fn validate_output_gpio(pin: u8, uart: &mut Uart<'_, Blocking>) -> bool {
    if !validate_gpio(pin, uart) {
        return false;
    }

    if is_input_only_pin(pin) {
        write_str(uart, "err gpio_input_only pin=");
        write_u64(uart, pin as u64);
        write_line(uart, " use=34..39_as_input_only");
        return false;
    }

    true
}

fn validate_adc_gpio(pin: u8, uart: &mut Uart<'_, Blocking>, state: &DeviceState) -> bool {
    if !validate_gpio(pin, uart) {
        return false;
    }

    if !is_adc_pin(pin) {
        write_str(uart, "err adc_unsupported_pin pin=");
        write_u64(uart, pin as u64);
        write_line(uart, " use=adc1_gpio32_39_or_adc2_gpio0_2_4_12_15_25_27");
        return false;
    }

    if state.wifi_enabled && is_adc2_pin(pin) {
        write_str(uart, "err adc2_wifi_conflict pin=");
        write_u64(uart, pin as u64);
        write_line(uart, " use=adc1_gpio32_39");
        return false;
    }

    true
}

fn claim_pin(
    state: &mut DeviceState,
    pin: u8,
    owner: PinOwner,
    uart: &mut Uart<'_, Blocking>,
) -> bool {
    if !can_claim_pin(state, pin, owner, uart) {
        return false;
    }

    claim_pin_unchecked(state, pin, owner);
    true
}

fn can_claim_pin(
    state: &DeviceState,
    pin: u8,
    owner: PinOwner,
    uart: &mut Uart<'_, Blocking>,
) -> bool {
    let current = state.pin_owners[pin as usize];
    if current == PinOwner::Free
        || current == owner
        || (current == PinOwner::Pin
            && owner == PinOwner::Event
            && matches!(
                state.pin_modes[pin as usize],
                PinMode::Input | PinMode::InputPullup | PinMode::InputPulldown
            ))
        || (pin == LED_PIN
            && matches!(current, PinOwner::Led | PinOwner::Pin | PinOwner::Pwm)
            && matches!(owner, PinOwner::Led | PinOwner::Pin | PinOwner::Pwm))
    {
        return true;
    }

    write_str(uart, "err gpio_busy pin=");
    write_u64(uart, pin as u64);
    write_str(uart, " owner=");
    write_pin_owner(uart, current);
    write_str(uart, " requested=");
    write_pin_owner(uart, owner);
    write_line(uart, "");
    false
}

fn claim_pin_unchecked(state: &mut DeviceState, pin: u8, owner: PinOwner) {
    state.pin_owners[pin as usize] = owner;
}

fn release_pin_if_owner(state: &mut DeviceState, pin: u8, owner: PinOwner) {
    if state.pin_owners[pin as usize] == owner {
        state.pin_owners[pin as usize] = PinOwner::Free;
        state.pin_modes[pin as usize] = PinMode::Unused;
        state.pin_levels[pin as usize] = false;
    }
}

fn run_background_tasks(ctx: &mut RuntimeContext<'_, '_>) {
    if ctx.state.blink_ms > 0
        && ctx.last_blink.elapsed() >= Duration::from_millis(ctx.state.blink_ms as u64)
    {
        ctx.led.toggle();
        ctx.state.led_on = !ctx.state.led_on;
        ctx.state.pin_levels[LED_PIN as usize] = ctx.state.led_on;
        *ctx.last_blink = Instant::now();
    }

    if ctx.state.heartbeat && ctx.last_heartbeat.elapsed() >= Duration::from_secs(5) {
        write_str(ctx.uart, "event heartbeat uptime_ms=");
        write_u64(ctx.uart, ctx.state.boot_time.elapsed().as_millis());
        write_str(ctx.uart, " led=");
        write_str(ctx.uart, if ctx.state.led_on { "on" } else { "off" });
        write_str(ctx.uart, " blink_ms=");
        write_u64(ctx.uart, ctx.state.blink_ms as u64);
        write_line(ctx.uart, "");
        *ctx.last_heartbeat = Instant::now();
    }

    poll_timers(ctx);
    poll_pin_events(ctx);
}

fn poll_timers(ctx: &mut RuntimeContext<'_, '_>) {
    if ctx.state.script_running || ctx.state.timer_script_running {
        return;
    }

    for id in 0..ctx.state.timers.len() {
        let now_ms = ctx.state.boot_time.elapsed().as_millis();
        let timer = ctx.state.timers[id];
        if !timer.enabled || now_ms < timer.next_ms {
            continue;
        }

        ctx.state.timer_events = ctx.state.timer_events.saturating_add(1);
        write_str(ctx.uart, "event timer id=");
        write_u64(ctx.uart, id as u64);
        write_str(ctx.uart, " uptime_ms=");
        write_u64(ctx.uart, now_ms);
        write_line(ctx.uart, "");

        if timer.repeat {
            ctx.state.timers[id].next_ms = now_ms.saturating_add(timer.duration_ms);
        } else {
            ctx.state.timers[id].enabled = false;
        }

        if timer.script_len == 0 {
            continue;
        }

        let Ok(script) = str::from_utf8(&timer.script[..timer.script_len]) else {
            write_str(ctx.uart, "err timer_script_not_utf8 id=");
            write_u64(ctx.uart, id as u64);
            write_line(ctx.uart, "");
            continue;
        };

        ctx.state.timer_script_running = true;
        let _ = run_script(script, ctx, 0);
        ctx.state.timer_script_running = false;
    }
}

fn poll_pin_events(ctx: &mut RuntimeContext<'_, '_>) {
    if ctx.state.script_running || ctx.state.pin_event_running {
        return;
    }

    for id in 0..ctx.state.pin_events.len() {
        let now_ms = ctx.state.boot_time.elapsed().as_millis();
        let event = ctx.state.pin_events[id];
        if !event.enabled {
            continue;
        }

        let Some(level) =
            read_pin_level_for_owner(event.pin, PinOwner::Event, ctx.uart, ctx.led, ctx.state)
        else {
            ctx.state.pin_events[id].reset();
            release_pin_if_owner(ctx.state, event.pin, PinOwner::Event);
            write_str(ctx.uart, "err pin_event_disabled id=");
            write_u64(ctx.uart, id as u64);
            write_line(ctx.uart, "");
            continue;
        };

        if event.debounce_ms == 0 {
            if level == event.last_level {
                continue;
            }

            ctx.state.pin_events[id].last_level = level;
            if !pin_event_matches(event.trigger, event.last_level, level) {
                continue;
            }
        } else {
            if level == event.last_level {
                ctx.state.pin_events[id].debounce_active = false;
                continue;
            }

            if !event.debounce_active || event.debounce_level != level {
                ctx.state.pin_events[id].debounce_active = true;
                ctx.state.pin_events[id].debounce_level = level;
                ctx.state.pin_events[id].debounce_since_ms = now_ms;
                continue;
            }

            if now_ms.saturating_sub(event.debounce_since_ms) < event.debounce_ms {
                continue;
            }

            ctx.state.pin_events[id].debounce_active = false;
            ctx.state.pin_events[id].last_level = level;
            if !pin_event_matches(event.trigger, event.last_level, level) {
                continue;
            }
        }

        ctx.state.pin_events_fired = ctx.state.pin_events_fired.saturating_add(1);
        write_str(ctx.uart, "event pin id=");
        write_u64(ctx.uart, id as u64);
        write_str(ctx.uart, " pin=");
        write_u64(ctx.uart, event.pin as u64);
        write_str(ctx.uart, " trigger=");
        write_str(ctx.uart, pin_event_trigger_name(event.trigger));
        write_str(ctx.uart, " level=");
        write_str(ctx.uart, if level { "on" } else { "off" });
        write_str(ctx.uart, " uptime_ms=");
        write_u64(ctx.uart, now_ms);
        write_line(ctx.uart, "");

        let Ok(script) = str::from_utf8(&event.script[..event.script_len]) else {
            write_str(ctx.uart, "err pin_event_script_not_utf8 id=");
            write_u64(ctx.uart, id as u64);
            write_line(ctx.uart, "");
            continue;
        };

        ctx.state.pin_event_running = true;
        let _ = run_script(script, ctx, 0);
        ctx.state.pin_event_running = false;
    }
}

fn pin_event_matches(trigger: PinEventTrigger, previous: bool, current: bool) -> bool {
    match trigger {
        PinEventTrigger::Rising => !previous && current,
        PinEventTrigger::Falling => previous && !current,
        PinEventTrigger::Change => previous != current,
    }
}

fn parse_pin_mode(value: &str, uart: &mut Uart<'_, Blocking>) -> Option<PinMode> {
    match value {
        "input" => Some(PinMode::Input),
        "input_pullup" => Some(PinMode::InputPullup),
        "input_pulldown" => Some(PinMode::InputPulldown),
        "output" => Some(PinMode::Output),
        _ => {
            write_line(
                uart,
                "err pin_mode_expected input|input_pullup|input_pulldown|output",
            );
            None
        }
    }
}

fn parse_level(value: &str, uart: &mut Uart<'_, Blocking>) -> Option<bool> {
    if let Some(level) = parse_level_value(value) {
        return Some(level);
    }

    write_line(uart, "err level_expected on|off|high|low|true|false");
    None
}

fn parse_next_u8(
    value: Option<&str>,
    uart: &mut Uart<'_, Blocking>,
    name: &str,
    state: &DeviceState,
) -> Option<u8> {
    let Some(value) = value else {
        write_str(uart, "err missing_");
        write_line(uart, name);
        return None;
    };

    parse_u8_arg(value, state).or_else(|| {
        write_str(uart, "err number_or_var_expected ");
        write_line(uart, name);
        None
    })
}

fn parse_u8_arg(value: &str, state: &DeviceState) -> Option<u8> {
    let value = parse_u64_arg(value, state)?;
    if value <= u8::MAX as u64 {
        Some(value as u8)
    } else {
        None
    }
}

fn parse_u32_arg(value: &str, state: &DeviceState) -> Option<u32> {
    let value = parse_u64_arg(value, state)?;
    if value <= u32::MAX as u64 {
        Some(value as u32)
    } else {
        None
    }
}

fn parse_u64_arg(value: &str, state: &DeviceState) -> Option<u64> {
    parse_u64_value(value).or_else(|| get_variable(state, value))
}

fn pin_mode_name(mode: PinMode) -> &'static str {
    match mode {
        PinMode::Unused => "unused",
        PinMode::Input => "input",
        PinMode::InputPullup => "input_pullup",
        PinMode::InputPulldown => "input_pulldown",
        PinMode::Output => "output",
    }
}

fn reset_reason_name(reason: Option<SocResetReason>) -> &'static str {
    match reason {
        Some(SocResetReason::ChipPowerOn) => "chip_power_on",
        Some(SocResetReason::CoreSw) => "core_sw",
        Some(SocResetReason::CoreDeepSleep) => "core_deep_sleep",
        Some(SocResetReason::CoreSdio) => "core_sdio",
        Some(SocResetReason::CoreMwdt0) => "core_mwdt0",
        Some(SocResetReason::CoreMwdt1) => "core_mwdt1",
        Some(SocResetReason::CoreRtcWdt) => "core_rtc_wdt",
        Some(SocResetReason::CpuMwdt0) => "cpu_mwdt0",
        Some(SocResetReason::Cpu0Sw) => "cpu0_sw",
        Some(SocResetReason::Cpu0RtcWdt) => "cpu0_rtc_wdt",
        Some(SocResetReason::Cpu1Cpu0) => "cpu1_cpu0",
        Some(SocResetReason::SysBrownOut) => "sys_brown_out",
        Some(SocResetReason::SysRtcWdt) => "sys_rtc_wdt",
        None => "unknown",
    }
}

fn write_pin_owner(uart: &mut Uart<'_, Blocking>, owner: PinOwner) {
    write_str(
        uart,
        match owner {
            PinOwner::Free => "free",
            PinOwner::Led => "led",
            PinOwner::Pin => "pin",
            PinOwner::Pwm => "pwm",
            PinOwner::Adc => "adc",
            PinOwner::Event => "event",
        },
    );
}

fn count_owner(state: &DeviceState, owner: PinOwner) -> u64 {
    state
        .pin_owners
        .iter()
        .filter(|pin_owner| **pin_owner == owner)
        .count() as u64
}

fn count_variables(state: &DeviceState) -> u64 {
    state
        .variables
        .iter()
        .filter(|variable| variable.used)
        .count() as u64
}

fn count_programs(state: &DeviceState) -> u64 {
    state.programs.iter().filter(|program| program.used).count() as u64
}

fn count_enabled_timers(state: &DeviceState) -> u64 {
    state.timers.iter().filter(|timer| timer.enabled).count() as u64
}

fn count_timer_scripts(state: &DeviceState) -> u64 {
    state
        .timers
        .iter()
        .filter(|timer| timer.enabled && timer.script_len > 0)
        .count() as u64
}

fn count_pin_events(state: &DeviceState) -> u64 {
    state
        .pin_events
        .iter()
        .filter(|event| event.enabled)
        .count() as u64
}

fn count_pwm_channels(state: &DeviceState) -> u64 {
    state
        .pwm_channels
        .iter()
        .filter(|channel| channel.enabled)
        .count() as u64
}

fn write_variables(uart: &mut Uart<'_, Blocking>, state: &DeviceState) {
    write_str(uart, "ok vars count=");
    write_u64(uart, count_variables(state));
    write_line(uart, "");

    for variable in state.variables.iter().filter(|variable| variable.used) {
        let Ok(name) = str::from_utf8(&variable.name[..variable.name_len]) else {
            continue;
        };

        write_str(uart, "  ");
        write_str(uart, name);
        write_str(uart, "=");
        write_u64(uart, variable.value);
        write_line(uart, "");
    }
}

fn write_programs(uart: &mut Uart<'_, Blocking>, state: &DeviceState) {
    write_str(uart, "ok programs count=");
    write_u64(uart, count_programs(state));
    write_line(uart, "");

    for program in state.programs.iter().filter(|program| program.used) {
        let Ok(name) = str::from_utf8(&program.name[..program.name_len]) else {
            continue;
        };

        write_str(uart, "  ");
        write_str(uart, name);
        write_str(uart, " bytes=");
        write_u64(uart, program.script_len as u64);
        if autorun_name_matches(state, name) {
            write_str(uart, " autorun=on");
        }
        write_line(uart, "");
    }
}

fn write_autorun(uart: &mut Uart<'_, Blocking>, state: &DeviceState) {
    write_str(uart, "ok autorun=");
    write_autorun_value(uart, state);
    write_line(uart, "");
}

fn write_autorun_value(uart: &mut Uart<'_, Blocking>, state: &DeviceState) {
    if !state.autorun_enabled {
        write_str(uart, "off");
        return;
    }

    let Ok(name) = str::from_utf8(&state.autorun_name[..state.autorun_name_len]) else {
        write_str(uart, "invalid");
        return;
    };
    write_str(uart, name);
}

fn write_help(uart: &mut Uart<'_, Blocking>) {
    write_line(uart, "ok commands:");
    write_help_script(uart);
    write_help_programs(uart);
    write_help_io(uart);
    write_help_examples(uart);
}

fn write_help_script(uart: &mut Uart<'_, Blocking>) {
    write_line(uart, "  <command>; <command>; ...");
    write_str(uart, "  script budget: steps=");
    write_u64(uart, MAX_SCRIPT_STEPS as u64);
    write_str(uart, " sleep_total_ms=");
    write_u64(uart, MAX_SCRIPT_SLEEP_BUDGET_MS);
    write_line(uart, " (use timers for long waits)");
    write_line(uart, "  repeat <1..1000|var> { <commands> }");
    write_line(
        uart,
        "  if <condition> { <commands> } [else { <commands> }]",
    );
    write_line(uart, "  conditions: led|heartbeat|wifi ==|!= on|off");
    write_line(uart, "  conditions: pin <gpio|var> ==|!= on|off");
    write_line(
        uart,
        "  conditions: <number|var> ==|!=|<|<=|>|>= <number|var>",
    );
    write_line(uart, "  let <name> = <number|var> [op <number|var>]");
    write_line(uart, "  let ops: + - * / %");
}

fn write_help_programs(uart: &mut Uart<'_, Blocking>) {
    write_line(uart, "  vars");
    write_line(uart, "  save <name> { <commands> } (persistent)");
    write_line(uart, "  run <name>");
    write_line(uart, "  programs");
    write_line(uart, "  delete <name> (persistent)");
    write_line(
        uart,
        "  autorun <name> | autorun off | autorun (persistent)",
    );
    write_line(uart, "  persist clear (clears saved programs and autorun)");
    write_line(uart, "  hold GPIO0 low on reset to skip autorun");
    write_line(uart, "  boot");
}

fn write_help_io(uart: &mut Uart<'_, Blocking>) {
    write_line(
        uart,
        "  on pin <gpio|var> rising|falling|change [debounce <ms|var>] do { <commands> }",
    );
    write_line(uart, "  on pin <gpio|var> off");
    write_line(uart, "  help | ?");
    write_line(uart, "  ping");
    write_line(uart, "  caps");
    write_line(uart, "  status");
    write_line(uart, "  reboot | reset");
    write_line(uart, "  led on | led off | led toggle");
    write_line(uart, "  led blink <ms> | blink <ms> | blink off");
    write_line(uart, "  heartbeat on | heartbeat off");
    write_line(uart, "  echo <text>");
    write_line(
        uart,
        "  pin <gpio> mode input|input_pullup|input_pulldown|output",
    );
    write_line(
        uart,
        "  pin <gpio> write on|off | pin <gpio> toggle | pin <gpio> read",
    );
    write_line(
        uart,
        "  pwm <gpio> freq=<hz> duty=<0..1023> | pwm <gpio> stop",
    );
    write_line(uart, "  adc read <gpio> [max_mv=<mv>] [samples=<1..64>]");
    write_line(
        uart,
        "  i2c sda=<gpio> scl=<gpio> speed=<hz> (driver unavailable)",
    );
    write_line(
        uart,
        "  spi sck=<gpio> miso=<gpio> mosi=<gpio> [cs=<gpio>] speed=<hz> (driver unavailable)",
    );
    write_line(
        uart,
        "  uart tx=<gpio> rx=<gpio> baud=<hz> (driver unavailable)",
    );
    write_line(
        uart,
        "  wifi on | wifi off | wifi connect ssid=... password=... (driver unavailable)",
    );
    write_line(uart, "  timer <0..3> every|after <ms> [do { <commands> }]");
    write_line(uart, "  timer <0..3> stop");
    write_line(uart, "  sleep <ms|var> (uses script sleep budget)");
}

fn write_help_examples(uart: &mut Uart<'_, Blocking>) {
    write_line(
        uart,
        "  example: repeat 5 { led toggle; sleep 200 }; led off",
    );
    write_line(uart, "  example: if led == on { led off } else { led on }");
    write_line(uart, "  example: if pin button == on { led on }");
    write_line(
        uart,
        "  example: on pin button falling debounce 30 do { led toggle }",
    );
    write_line(
        uart,
        "  example: let presses = 0; let presses = presses + 1",
    );
    write_line(
        uart,
        "  example: save blink { repeat 3 { led toggle; sleep 100 } }",
    );
    write_line(uart, "  example: run blink");
    write_line(uart, "  example: autorun blink; boot");
    write_line(uart, "  example: let lamp = 2; pin lamp write on");
    write_line(uart, "  example: timer 0 every 1000 do { led toggle }");
}

fn write_caps(uart: &mut Uart<'_, Blocking>) {
    write_str(uart, "ok caps name=");
    write_str(uart, FIRMWARE_NAME);
    write_str(uart, " version=");
    write_str(uart, FIRMWARE_VERSION);
    write_str(uart, " protocol=");
    write_str(uart, CAPS_PROTOCOL_VERSION);
    write_str(uart, " features=");
    write_line(uart, CAPS_FEATURES);
}

fn write_status(uart: &mut Uart<'_, Blocking>, state: &DeviceState) {
    write_status_runtime(uart, state);
    write_status_programs(uart, state);
    write_status_gpio(uart, state);
    write_status_buses(uart, state);
    write_status_events(uart, state);
    write_status_pwm(uart, state);
    write_str(uart, " protocol=");
    write_str(uart, CAPS_PROTOCOL_VERSION);
    write_line(uart, "");
}

fn write_status_runtime(uart: &mut Uart<'_, Blocking>, state: &DeviceState) {
    write_str(uart, "ok status uptime_ms=");
    write_u64(uart, state.boot_time.elapsed().as_millis());
    write_str(uart, " led=");
    write_str(uart, if state.led_on { "on" } else { "off" });
    write_str(uart, " blink_ms=");
    write_u64(uart, state.blink_ms as u64);
    write_str(uart, " heartbeat=");
    write_str(uart, if state.heartbeat { "on" } else { "off" });
    write_str(uart, " safe_boot=");
    write_str(uart, if state.safe_boot { "on" } else { "off" });
    write_str(uart, " script_running=");
    write_str(uart, if state.script_running { "yes" } else { "no" });
    write_str(uart, " reset_reason=");
    write_str(uart, reset_reason_name(reset_reason()));
    write_str(uart, " commands=");
    write_u64(uart, state.commands as u64);
    write_str(uart, " unknown=");
    write_u64(uart, state.unknown_commands as u64);
    write_str(uart, " overflows=");
    write_u64(uart, state.line_overflows as u64);
    write_str(uart, " script_step_limit=");
    write_u64(uart, MAX_SCRIPT_STEPS as u64);
    write_str(uart, " sleep_budget_ms=");
    write_u64(uart, MAX_SCRIPT_SLEEP_BUDGET_MS);
}

fn write_status_programs(uart: &mut Uart<'_, Blocking>, state: &DeviceState) {
    write_str(uart, " vars=");
    write_u64(uart, count_variables(state));
    write_str(uart, " programs=");
    write_u64(uart, count_programs(state));
    write_str(uart, " program_runs=");
    write_u64(uart, state.program_runs as u64);
    write_str(uart, " autorun=");
    write_autorun_value(uart, state);
    write_str(uart, " boot_runs=");
    write_u64(uart, state.boot_runs as u64);
    write_str(uart, " persist_loaded=");
    write_str(uart, if state.persist_loaded { "yes" } else { "no" });
    write_str(uart, " persist_slots=");
    write_u64(uart, PERSIST_SLOT_COUNT as u64);
    write_str(uart, " persist_slot=");
    write_u64(uart, state.persist_slot as u64);
    write_str(uart, " persist_seq=");
    write_u64(uart, state.persist_sequence as u64);
    write_str(uart, " persist_saves=");
    write_u64(uart, state.persist_saves as u64);
    write_str(uart, " persist_errors=");
    write_u64(uart, state.persist_errors as u64);
}

fn write_status_gpio(uart: &mut Uart<'_, Blocking>, state: &DeviceState) {
    write_str(uart, " gpio_pin=");
    write_u64(uart, count_owner(state, PinOwner::Pin));
    write_str(uart, " gpio_pwm=");
    write_u64(uart, count_owner(state, PinOwner::Pwm));
    write_str(uart, " gpio_adc=");
    write_u64(uart, count_owner(state, PinOwner::Adc));
    write_str(uart, " gpio_event=");
    write_u64(uart, count_owner(state, PinOwner::Event));
}

fn write_status_buses(uart: &mut Uart<'_, Blocking>, state: &DeviceState) {
    write_str(uart, " i2c=");
    write_str(uart, if state.i2c_configured { "on" } else { "off" });
    write_str(uart, " spi=");
    write_str(uart, if state.spi_configured { "on" } else { "off" });
    write_str(uart, " aux_uart=");
    write_str(
        uart,
        if state.aux_uart_configured {
            "on"
        } else {
            "off"
        },
    );
    write_str(uart, " wifi=");
    write_str(uart, if state.wifi_enabled { "on" } else { "off" });
}

fn write_status_events(uart: &mut Uart<'_, Blocking>, state: &DeviceState) {
    write_str(uart, " timers=");
    write_u64(uart, count_enabled_timers(state));
    write_str(uart, " timer_scripts=");
    write_u64(uart, count_timer_scripts(state));
    write_str(uart, " timer_events=");
    write_u64(uart, state.timer_events as u64);
    write_str(uart, " pin_events=");
    write_u64(uart, count_pin_events(state));
    write_str(uart, " pin_events_fired=");
    write_u64(uart, state.pin_events_fired as u64);
}

fn write_status_pwm(uart: &mut Uart<'_, Blocking>, state: &DeviceState) {
    write_str(uart, " pwm_channels=");
    write_u64(uart, count_pwm_channels(state));
    for (index, channel) in state
        .pwm_channels
        .iter()
        .enumerate()
        .filter(|(_, channel)| channel.enabled)
    {
        write_str(uart, " pwm");
        write_u64(uart, index as u64);
        write_str(uart, "=pin");
        write_u64(uart, channel.pin as u64);
        write_str(uart, ":freq");
        write_u64(uart, channel.freq);
        write_str(uart, ":duty");
        write_u64(uart, channel.duty as u64);
    }
}

fn write_line(uart: &mut Uart<'_, Blocking>, value: &str) {
    write_str(uart, value);
    write_str(uart, "\r\n");
}

fn write_str(uart: &mut Uart<'_, Blocking>, value: &str) {
    let mut bytes = value.as_bytes();
    let mut stalls = 0;

    while !bytes.is_empty() {
        match uart.write(bytes) {
            Ok(0) | Err(_) => {
                stalls += 1;
                if stalls >= UART_WRITE_MAX_STALLS {
                    break;
                }
            }
            Ok(written) => {
                bytes = &bytes[written.min(bytes.len())..];
                stalls = 0;
            }
        }
    }

    let _ = uart.flush();
}

fn write_u64(uart: &mut Uart<'_, Blocking>, mut value: u64) {
    let mut buf = [0_u8; 20];
    let mut index = buf.len();

    if value == 0 {
        write_str(uart, "0");
        return;
    }

    while value > 0 {
        index -= 1;
        buf[index] = b'0' + (value % 10) as u8;
        value /= 10;
    }

    if let Ok(text) = str::from_utf8(&buf[index..]) {
        write_str(uart, text);
    }
}
