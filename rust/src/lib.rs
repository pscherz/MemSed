//! Dependency-light Linux backend primitives for MemSed.
//!
//! The crate intentionally has no third-party dependencies. The browser UI
//! can be added later without coupling it to `/proc` data structures.

#![cfg(target_os = "linux")]

use std::{
    fs::{self, File},
    io::{self, Read},
    os::unix::{fs::FileExt, fs::MetadataExt},
    path::{Path, PathBuf},
};

pub type ProcessId = i32;
pub type MemoryAddress = u64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Process {
    pub pid: ProcessId,
    pub name: String,
    pub executable: Option<PathBuf>,
    pub command: String,
    pub user: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegionKind {
    File,
    Heap,
    Stack,
    Anonymous,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RegionFlags(u8);

impl RegionFlags {
    pub const READ: Self = Self(1);
    pub const WRITE: Self = Self(2);
    pub const EXECUTE: Self = Self(4);
    pub const SHARED: Self = Self(8);

    pub const fn empty() -> Self {
        Self(0)
    }

    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    const fn insert(&mut self, flag: Self) {
        self.0 |= flag.0;
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryRegion {
    pub start: MemoryAddress,
    pub end: MemoryAddress,
    pub kind: RegionKind,
    pub flags: RegionFlags,
    pub file_path: Option<PathBuf>,
    pub file_offset: u64,
}

impl MemoryRegion {
    pub const fn len(&self) -> u64 {
        self.end - self.start
    }

    pub const fn is_empty(&self) -> bool {
        self.start == self.end
    }
}

#[derive(Debug)]
pub struct ProcessMemory {
    pid: ProcessId,
    file: File,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryType {
    U8,
    U16,
    U32,
    U64,
    I8,
    I16,
    I32,
    I64,
    F32,
    F64,
}

impl MemoryType {
    pub const fn size(self) -> usize {
        match self {
            Self::U8 | Self::I8 => 1,
            Self::U16 | Self::I16 => 2,
            Self::U32 | Self::I32 | Self::F32 => 4,
            Self::U64 | Self::I64 | Self::F64 => 8,
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::U8 => "u8",
            Self::U16 => "u16",
            Self::U32 => "u32",
            Self::U64 => "u64",
            Self::I8 => "i8",
            Self::I16 => "i16",
            Self::I32 => "i32",
            Self::I64 => "i64",
            Self::F32 => "f32",
            Self::F64 => "f64",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct SearchParams {
    pub value: f64,
    pub deviation: f64,
    pub alignment: u64,
    pub memory_type: MemoryType,
    pub region_kinds: [bool; 4],
    pub required_flags: RegionFlags,
}

impl Default for SearchParams {
    fn default() -> Self {
        Self {
            value: 123.0,
            deviation: 0.1,
            alignment: 4,
            memory_type: MemoryType::I32,
            region_kinds: [false, true, true, true],
            required_flags: RegionFlags::READ,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SearchResult {
    pub address: MemoryAddress,
    pub memory_type: MemoryType,
    pub value: String,
    pub previous: Option<String>,
    pub numeric_value: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchComparison {
    WithinRange,
    Higher,
    Lower,
}

#[derive(Debug, Default)]
pub struct MemorySearch {
    pub params: SearchParams,
    pub regions: Vec<MemoryRegion>,
    pub batches: Vec<Vec<SearchResult>>,
}

impl MemorySearch {
    pub fn reset(&mut self) {
        self.regions.clear();
        self.batches.clear();
    }

    pub fn first(&mut self, memory: &ProcessMemory) -> io::Result<usize> {
        self.batches.clear();
        self.regions = memory_regions(
            memory.pid,
            Some(&[
                RegionKind::File,
                RegionKind::Heap,
                RegionKind::Stack,
                RegionKind::Anonymous,
            ]),
            self.params.required_flags,
        )?
        .into_iter()
        .filter(|region| match region.kind {
            RegionKind::File => self.params.region_kinds[0],
            RegionKind::Heap => self.params.region_kinds[1],
            RegionKind::Stack => self.params.region_kinds[2],
            RegionKind::Anonymous => self.params.region_kinds[3],
        })
        .collect();
        let mut results = Vec::new();
        for region in &self.regions {
            self.scan_region(memory, region, None, &mut results)?;
        }
        self.batches.push(results);
        Ok(self.batches[0].len())
    }

    pub fn next(&mut self, memory: &ProcessMemory) -> io::Result<usize> {
        self.next_with_comparison(memory, SearchComparison::WithinRange)
    }

    pub fn next_with_comparison(
        &mut self,
        memory: &ProcessMemory,
        comparison: SearchComparison,
    ) -> io::Result<usize> {
        let previous = self.batches.last().cloned().unwrap_or_default();
        let mut results = Vec::new();
        for item in &previous {
            let mut bytes = vec![0; item.memory_type.size()];
            if memory.read(item.address, &mut bytes)? != bytes.len() {
                continue;
            }
            if let Some(value) = decode_value(item.memory_type, &bytes) {
                let matches = match comparison {
                    SearchComparison::WithinRange => {
                        in_range(value, self.params.value, self.params.deviation)
                    }
                    SearchComparison::Higher => value > item.numeric_value,
                    SearchComparison::Lower => value < item.numeric_value,
                };
                if matches {
                    results.push(SearchResult {
                        address: item.address,
                        memory_type: item.memory_type,
                        value: format_value(item.memory_type, value),
                        previous: Some(item.value.clone()),
                        numeric_value: value,
                    });
                }
            }
        }
        self.batches.push(results);
        Ok(self.batches.last().map_or(0, Vec::len))
    }

    pub fn current_results(&self) -> &[SearchResult] {
        self.batches.last().map_or(&[], Vec::as_slice)
    }

    fn scan_region(
        &self,
        memory: &ProcessMemory,
        region: &MemoryRegion,
        _previous: Option<&[SearchResult]>,
        results: &mut Vec<SearchResult>,
    ) -> io::Result<()> {
        let size = self.params.memory_type.size();
        let alignment = self.params.alignment.max(1);
        let chunk_size = 128 * 1024;
        let mut chunk_start = region.start;
        let mut buffer = vec![0; chunk_size];
        while chunk_start < region.end && results.len() < 100_000 {
            let remaining = region.end - chunk_start;
            let read_len = remaining.min(chunk_size as u64) as usize;
            let read = memory.read(chunk_start, &mut buffer[..read_len])?;
            if read == 0 {
                break;
            }
            let mut offset = 0usize;
            while offset + size <= read && results.len() < 100_000 {
                let address = chunk_start + offset as u64;
                if let Some(value) =
                    decode_value(self.params.memory_type, &buffer[offset..offset + size])
                {
                    if in_range(value, self.params.value, self.params.deviation) {
                        results.push(SearchResult {
                            address,
                            memory_type: self.params.memory_type,
                            value: format_value(self.params.memory_type, value),
                            previous: None,
                            numeric_value: value,
                        });
                    }
                }
                offset += alignment as usize;
            }
            chunk_start += read as u64;
        }
        Ok(())
    }
}

fn decode_value(memory_type: MemoryType, bytes: &[u8]) -> Option<f64> {
    let value = match memory_type {
        MemoryType::U8 => u8::from_ne_bytes(bytes.try_into().ok()?) as f64,
        MemoryType::U16 => u16::from_ne_bytes(bytes.try_into().ok()?) as f64,
        MemoryType::U32 => u32::from_ne_bytes(bytes.try_into().ok()?) as f64,
        MemoryType::U64 => u64::from_ne_bytes(bytes.try_into().ok()?) as f64,
        MemoryType::I8 => i8::from_ne_bytes(bytes.try_into().ok()?) as f64,
        MemoryType::I16 => i16::from_ne_bytes(bytes.try_into().ok()?) as f64,
        MemoryType::I32 => i32::from_ne_bytes(bytes.try_into().ok()?) as f64,
        MemoryType::I64 => i64::from_ne_bytes(bytes.try_into().ok()?) as f64,
        MemoryType::F32 => f32::from_ne_bytes(bytes.try_into().ok()?) as f64,
        MemoryType::F64 => f64::from_ne_bytes(bytes.try_into().ok()?),
    };
    value.is_finite().then_some(value)
}

fn in_range(value: f64, target: f64, deviation: f64) -> bool {
    value >= target - deviation.abs() && value <= target + deviation.abs()
}

fn format_value(memory_type: MemoryType, value: f64) -> String {
    match memory_type {
        MemoryType::F32 | MemoryType::F64 => format!("{value:.6}"),
        _ => format!("{value:.0}"),
    }
}

impl ProcessMemory {
    pub fn open(pid: ProcessId) -> io::Result<Self> {
        let path = proc_path(pid, "mem");
        Ok(Self {
            pid,
            file: File::options().read(true).write(true).open(path)?,
        })
    }

    pub const fn pid(&self) -> ProcessId {
        self.pid
    }

    pub fn read(&self, address: MemoryAddress, buffer: &mut [u8]) -> io::Result<usize> {
        self.file.read_at(buffer, address)
    }

    pub fn write(&self, address: MemoryAddress, buffer: &[u8]) -> io::Result<usize> {
        self.file.write_at(buffer, address)
    }
}

pub fn is_process_alive(pid: ProcessId) -> bool {
    Path::new(&format!("/proc/{pid}")).is_dir()
}

pub fn pause_process(pid: ProcessId) -> io::Result<()> {
    send_signal(pid, 19)
}

pub fn resume_process(pid: ProcessId) -> io::Result<()> {
    send_signal(pid, 18)
}

pub fn list_processes() -> io::Result<Vec<Process>> {
    let mut processes = Vec::new();
    for entry in fs::read_dir("/proc")? {
        let entry = entry?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let Ok(pid) = name.parse::<ProcessId>() else {
            continue;
        };
        if let Ok(process) = inspect_process(pid) {
            processes.push(process);
        }
    }
    processes.sort_by_key(|process| process.pid);
    Ok(processes)
}

pub fn inspect_process(pid: ProcessId) -> io::Result<Process> {
    let proc_dir = PathBuf::from(format!("/proc/{pid}"));
    let name = read_trimmed(proc_dir.join("comm"))?;
    let command = read_cmdline(proc_dir.join("cmdline"))
        .unwrap_or_else(|_| "<cannot read cmdline>".to_owned());
    let executable = fs::read_link(proc_dir.join("exe")).ok();
    let user = fs::metadata(&proc_dir)
        .ok()
        .map(|metadata| metadata.uid().to_string())
        .unwrap_or_default();

    Ok(Process {
        pid,
        name,
        executable,
        command,
        user,
    })
}

pub fn memory_regions(
    pid: ProcessId,
    kinds: Option<&[RegionKind]>,
    required_flags: RegionFlags,
) -> io::Result<Vec<MemoryRegion>> {
    let contents = fs::read_to_string(proc_path(pid, "maps"))?;
    let mut regions = Vec::new();
    for line in contents.lines() {
        let mut fields = line.splitn(6, char::is_whitespace);
        let Some(addresses) = fields.next() else {
            continue;
        };
        let Some(permissions) = fields.next() else {
            continue;
        };
        let Some(offset) = fields.next() else {
            continue;
        };
        let _device = fields.next();
        let _inode = fields.next();
        let path = fields
            .next()
            .map(str::trim)
            .filter(|value| !value.is_empty());

        let Some((start, end)) = addresses.split_once('-') else {
            continue;
        };
        let start = u64::from_str_radix(start, 16)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        let end = u64::from_str_radix(end, 16)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        let file_offset = u64::from_str_radix(offset, 16)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;

        let mut flags = RegionFlags::empty();
        for permission in permissions.chars() {
            match permission {
                'r' => flags.insert(RegionFlags::READ),
                'w' => flags.insert(RegionFlags::WRITE),
                'x' => flags.insert(RegionFlags::EXECUTE),
                's' => flags.insert(RegionFlags::SHARED),
                _ => {}
            }
        }
        if !flags.contains(required_flags) {
            continue;
        }

        let kind = match path {
            Some("[heap]") => RegionKind::Heap,
            Some("[stack]") => RegionKind::Stack,
            Some(_) => RegionKind::File,
            None => RegionKind::Anonymous,
        };
        if kinds.is_some_and(|allowed| !allowed.contains(&kind)) {
            continue;
        }

        regions.push(MemoryRegion {
            start,
            end,
            kind,
            flags,
            file_path: path.map(PathBuf::from),
            file_offset,
        });
    }
    Ok(regions)
}

fn proc_path(pid: ProcessId, file: &str) -> PathBuf {
    PathBuf::from(format!("/proc/{pid}/{file}"))
}

fn read_trimmed(path: impl AsRef<Path>) -> io::Result<String> {
    let mut value = String::new();
    File::open(path)?.read_to_string(&mut value)?;
    Ok(value.trim_end_matches(['\r', '\n']).to_owned())
}

fn read_cmdline(path: impl AsRef<Path>) -> io::Result<String> {
    let mut value = Vec::new();
    File::open(path)?.read_to_end(&mut value)?;
    if value.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "empty cmdline",
        ));
    }
    Ok(String::from_utf8_lossy(&value)
        .replace('\0', " ")
        .trim()
        .to_owned())
}

#[cfg(target_os = "linux")]
fn send_signal(pid: ProcessId, signal: i32) -> io::Result<()> {
    let result = libc_kill(pid, signal);
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(target_os = "linux")]
extern "C" {
    fn kill(pid: ProcessId, signal: i32) -> i32;
}

#[cfg(target_os = "linux")]
fn libc_kill(pid: ProcessId, signal: i32) -> i32 {
    unsafe { kill(pid, signal) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_process_is_inspectable() {
        let process = inspect_process(std::process::id() as ProcessId).unwrap();
        assert_eq!(process.pid, std::process::id() as ProcessId);
        assert!(!process.name.is_empty());
    }

    #[test]
    fn current_process_has_regions() {
        let regions =
            memory_regions(std::process::id() as ProcessId, None, RegionFlags::READ).unwrap();
        assert!(!regions.is_empty());
        assert!(regions
            .iter()
            .all(|region| region.flags.contains(RegionFlags::READ)));
    }
}
