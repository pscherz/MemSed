use std::{
    fmt::Write,
    sync::{Mutex, OnceLock},
};

use memsed_core::{
    MemorySearch, MemoryType, ProcessId, ProcessMemory, SearchComparison, inspect_process,
    list_processes,
};
use webui_rs::webui;

const PROCESS_HTML: &str = include_str!("../../ui/process.html");
const MEMORY_HTML: &str = include_str!("../../ui/memory.html");

fn process_html() -> String {
    let uid = std::fs::metadata("/proc/self")
        .map(|metadata| {
            use std::os::unix::fs::MetadataExt;
            metadata.uid()
        })
        .unwrap_or_default();
    PROCESS_HTML.replace("__CURRENT_UID__", &uid.to_string())
}

struct AppState {
    process: Option<ProcessMemory>,
    search: MemorySearch,
    scratchpad: Vec<ScratchpadItem>,
}

struct ScratchpadItem {
    address: u64,
    memory_type: MemoryType,
    value: String,
    active: bool,
}

static STATE: OnceLock<Mutex<AppState>> = OnceLock::new();

fn state() -> &'static Mutex<AppState> {
    STATE.get_or_init(|| {
        Mutex::new(AppState {
            process: None,
            search: MemorySearch::default(),
            scratchpad: Vec::new(),
        })
    })
}

fn escape_json(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}

fn refresh_processes(event: webui::Event) {
    let response = match list_processes() {
        Ok(processes) => {
            let mut json = String::from("[");
            for (index, process) in processes.iter().enumerate() {
                if index != 0 {
                    json.push(',');
                }
                let executable = process
                    .executable
                    .as_deref()
                    .map_or_else(String::new, |path| path.to_string_lossy().into_owned());
                let _ = write!(
                    json,
                    r#"{{"pid":{},"uid":{},"name":"{}","user":"{}","executable":"{}","command":"{}"}}"#,
                    process.pid,
                    process.uid,
                    escape_json(&process.name),
                    escape_json(&process.user),
                    escape_json(&executable),
                    escape_json(&process.command)
                );
            }
            json.push(']');
            json
        }
        Err(error) => format!(r#"{{"error":"{}"}}"#, escape_json(&error.to_string())),
    };
    event.return_string(&response);
}

fn inspect_selected_process(event: webui::Event) {
    let pid = event.get_int() as ProcessId;
    let response = match inspect_process(pid) {
        Ok(process) => format!(
            r#"{{"pid":{},"name":"{}","command":"{}"}}"#,
            process.pid,
            escape_json(&process.name),
            escape_json(&process.command)
        ),
        Err(error) => format!(r#"{{"error":"{}"}}"#, escape_json(&error.to_string())),
    };
    event.return_string(&response);
}

fn attach_process(event: webui::Event) {
    let pid = event.get_int() as ProcessId;
    let response = match ProcessMemory::open(pid) {
        Ok(memory) => {
            let mut app = state().lock().expect("application state lock poisoned");
            app.process = Some(memory);
            app.search.reset();
            app.scratchpad.clear();
            event.show_client(MEMORY_HTML);
            r#"{"ok":true}"#.to_owned()
        }
        Err(error) => format!(r#"{{"error":"{}"}}"#, escape_json(&error.to_string())),
    };
    event.return_string(&response);
}

fn attached_process(event: webui::Event) {
    let app = state().lock().expect("application state lock poisoned");
    let response = match app
        .process
        .as_ref()
        .and_then(|process| inspect_process(process.pid()).ok())
    {
        Some(process) => format!(
            r#"{{"pid":{},"name":"{}"}}"#,
            process.pid,
            escape_json(&process.name)
        ),
        None => r#"{"error":"no process attached"}"#.to_owned(),
    };
    event.return_string(&response);
}

fn parse_type(value: &str) -> Option<MemoryType> {
    match value {
        "u8" => Some(MemoryType::U8),
        "u16" => Some(MemoryType::U16),
        "u32" => Some(MemoryType::U32),
        "u64" => Some(MemoryType::U64),
        "i8" => Some(MemoryType::I8),
        "i16" => Some(MemoryType::I16),
        "i32" => Some(MemoryType::I32),
        "i64" => Some(MemoryType::I64),
        "f32" => Some(MemoryType::F32),
        "f64" => Some(MemoryType::F64),
        _ => None,
    }
}

fn search_result_json(search: &MemorySearch) -> String {
    let mut json = String::from(r#"{"count":"#);
    let _ = write!(json, "{}", search.current_results().len());
    json.push_str(r#","results":["#);
    for (index, result) in search.current_results().iter().take(1_000).enumerate() {
        if index != 0 {
            json.push(',');
        }
        let previous = result.previous.as_deref().unwrap_or("N/A");
        let _ = write!(
            json,
            r#"{{"address":"0x{:016x}","type":"{}","value":"{}","previous":"{}"}}"#,
            result.address,
            result.memory_type.name(),
            escape_json(&result.value),
            escape_json(previous)
        );
    }
    json.push_str("]}");
    json
}

fn scratchpad_json(items: &[ScratchpadItem]) -> String {
    let mut json = String::from("[");
    for (index, item) in items.iter().enumerate() {
        if index != 0 {
            json.push(',');
        }
        let _ = write!(
            json,
            r#"{{"index":{},"address":"0x{:016x}","type":"{}","value":"{}","active":{}}}"#,
            index,
            item.address,
            item.memory_type.name(),
            escape_json(&item.value),
            item.active
        );
    }
    json.push(']');
    json
}

fn encode_value(memory_type: MemoryType, value: &str) -> Result<Vec<u8>, String> {
    let parsed = value.parse::<f64>().map_err(|error| error.to_string())?;
    let bytes = match memory_type {
        MemoryType::U8 => (parsed as u8).to_ne_bytes().to_vec(),
        MemoryType::U16 => (parsed as u16).to_ne_bytes().to_vec(),
        MemoryType::U32 => (parsed as u32).to_ne_bytes().to_vec(),
        MemoryType::U64 => (parsed as u64).to_ne_bytes().to_vec(),
        MemoryType::I8 => (parsed as i8).to_ne_bytes().to_vec(),
        MemoryType::I16 => (parsed as i16).to_ne_bytes().to_vec(),
        MemoryType::I32 => (parsed as i32).to_ne_bytes().to_vec(),
        MemoryType::I64 => (parsed as i64).to_ne_bytes().to_vec(),
        MemoryType::F32 => (parsed as f32).to_ne_bytes().to_vec(),
        MemoryType::F64 => parsed.to_ne_bytes().to_vec(),
    };
    Ok(bytes)
}

fn add_scratchpad(event: webui::Event) {
    let address = match u64::from_str_radix(event.get_string().trim_start_matches("0x"), 16) {
        Ok(address) => address,
        Err(error) => {
            event.return_string(&format!(
                r#"{{"error":"{}"}}"#,
                escape_json(&error.to_string())
            ));
            return;
        }
    };
    let Some(memory_type) = parse_type(&event.get_string_at(1)) else {
        event.return_string(r#"{"error":"unknown memory type"}"#);
        return;
    };
    let value = event.get_string_at(2);
    let mut app = state().lock().expect("application state lock poisoned");
    if app.scratchpad.iter().any(|item| item.address == address) {
        event.return_string(&scratchpad_json(&app.scratchpad));
        return;
    }
    app.scratchpad.push(ScratchpadItem {
        address,
        memory_type,
        value,
        active: false,
    });
    event.return_string(&scratchpad_json(&app.scratchpad));
}

fn update_scratchpad(event: webui::Event) {
    let index = event.get_int() as usize;
    let value = event.get_string_at(1);
    let active = event.get_bool_at(2);
    let mut app = state().lock().expect("application state lock poisoned");
    let Some(item) = app.scratchpad.get(index) else {
        event.return_string(r#"{"error":"scratchpad item not found"}"#);
        return;
    };
    let address = item.address;
    let memory_type = item.memory_type;
    let Some(process) = app.process.as_ref() else {
        event.return_string(r#"{"error":"no process attached"}"#);
        return;
    };
    let Ok(bytes) = encode_value(memory_type, &value) else {
        event.return_string(r#"{"error":"invalid value"}"#);
        return;
    };
    if let Err(error) = process.write(address, &bytes).and_then(|written| {
        (written == bytes.len())
            .then_some(())
            .ok_or_else(|| std::io::Error::other("short process memory write"))
    }) {
        event.return_string(&format!(
            r#"{{"error":"{}"}}"#,
            escape_json(&error.to_string())
        ));
        return;
    }
    let item = app.scratchpad.get_mut(index).expect("item checked above");
    item.value = value.into();
    item.active = active;
    event.return_string(&scratchpad_json(&app.scratchpad));
}

fn remove_scratchpad(event: webui::Event) {
    let index = event.get_int() as usize;
    let mut app = state().lock().expect("application state lock poisoned");
    if index < app.scratchpad.len() {
        app.scratchpad.remove(index);
    }
    event.return_string(&scratchpad_json(&app.scratchpad));
}

fn update_params(event: &webui::Event, search: &mut MemorySearch) -> Result<(), String> {
    let value = event
        .get_string()
        .parse::<f64>()
        .map_err(|error| error.to_string())?;
    let memory_type =
        parse_type(&event.get_string_at(1)).ok_or_else(|| "unknown memory type".to_owned())?;
    let alignment = event.get_int_at(2).max(1) as u64;
    let deviation = event
        .get_string_at(3)
        .parse::<f64>()
        .map_err(|error| error.to_string())?;
    search.params.value = value;
    search.params.memory_type = memory_type;
    search.params.alignment = alignment;
    search.params.deviation = deviation;
    Ok(())
}

fn first_search(event: webui::Event) {
    let mut app = state().lock().expect("application state lock poisoned");
    let Some(process) = app.process.take() else {
        event.return_string(r#"{"error":"no process attached"}"#);
        return;
    };
    let response = if let Err(error) = update_params(&event, &mut app.search) {
        format!(r#"{{"error":"{}"}}"#, escape_json(&error))
    } else {
        match app.search.first(&process) {
            Ok(_) => search_result_json(&app.search),
            Err(error) => format!(r#"{{"error":"{}"}}"#, escape_json(&error.to_string())),
        }
    };
    app.process = Some(process);
    event.return_string(&response);
}

fn next_search(event: webui::Event) {
    let mut app = state().lock().expect("application state lock poisoned");
    let Some(process) = app.process.take() else {
        event.return_string(r#"{"error":"no process attached"}"#);
        return;
    };
    let response = if let Err(error) = update_params(&event, &mut app.search) {
        format!(r#"{{"error":"{}"}}"#, escape_json(&error))
    } else {
        match app
            .search
            .next_with_comparison(&process, SearchComparison::WithinRange)
        {
            Ok(_) => search_result_json(&app.search),
            Err(error) => format!(r#"{{"error":"{}"}}"#, escape_json(&error.to_string())),
        }
    };
    app.process = Some(process);
    event.return_string(&response);
}

fn update_search_results(event: webui::Event) {
    let limit = event.get_int().max(1) as usize;
    let mut app = state().lock().expect("application state lock poisoned");
    let Some(process) = app.process.take() else {
        event.return_string(r#"{"error":"no process attached"}"#);
        return;
    };
    let response = match app.search.refresh_current(&process, limit) {
        Ok(_) => search_result_json(&app.search),
        Err(error) => format!(r#"{{"error":"{}"}}"#, escape_json(&error.to_string())),
    };
    app.process = Some(process);
    event.return_string(&response);
}

fn next_comparison_search(event: webui::Event, comparison: SearchComparison) {
    let mut app = state().lock().expect("application state lock poisoned");
    let Some(process) = app.process.take() else {
        event.return_string(r#"{"error":"no process attached"}"#);
        return;
    };
    let response = match app.search.next_with_comparison(&process, comparison) {
        Ok(_) => search_result_json(&app.search),
        Err(error) => format!(r#"{{"error":"{}"}}"#, escape_json(&error.to_string())),
    };
    app.process = Some(process);
    event.return_string(&response);
}

fn next_higher(event: webui::Event) {
    next_comparison_search(event, SearchComparison::Higher);
}

fn next_lower(event: webui::Event) {
    next_comparison_search(event, SearchComparison::Lower);
}

fn detach_process(event: webui::Event) {
    let mut app = state().lock().expect("application state lock poisoned");
    app.process = None;
    app.search.reset();
    app.scratchpad.clear();
    let html = process_html();
    event.show_client(html);
    event.return_string(r#"{"ok":true}"#);
}

fn reset_search(_event: webui::Event) {
    state()
        .lock()
        .expect("application state lock poisoned")
        .search
        .reset();
}

fn quit_memsed(_event: webui::Event) {
    webui::exit();
}

fn main() {
    let window = webui::Window::new();
    window.bind("refresh_processes", refresh_processes);
    window.bind("inspect_process", inspect_selected_process);
    window.bind("attach_process", attach_process);
    window.bind("attached_process", attached_process);
    window.bind("first_search", first_search);
    window.bind("next_search", next_search);
    window.bind("update_search_results", update_search_results);
    window.bind("next_higher", next_higher);
    window.bind("next_lower", next_lower);
    window.bind("detach_process", detach_process);
    window.bind("reset_search", reset_search);
    window.bind("add_scratchpad", add_scratchpad);
    window.bind("update_scratchpad", update_scratchpad);
    window.bind("remove_scratchpad", remove_scratchpad);
    window.bind("quit_memsed", quit_memsed);
    let html = process_html();
    window.show(html);
    webui::wait();
    webui::clean();
}
