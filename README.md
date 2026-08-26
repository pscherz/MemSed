# MemSed

MEMory Search and EDit for Linux.

MemSed is a Rust/WebUI application for finding, changing, and locking values
in another process's memory.

## Requirements

- Linux with `/proc`
- An installed supported web browser (Firefox, Chrome, Chromium(-based), Safari, Webview)
- Permission to access the target process

## Building

Install Rust, `curl`, and `unzip`, then run:

```console
cargo test
cargo build --release --bin memsed-webui
```

The executable is `target/release/memsed-webui`. The WebUI build downloads its
small native library on the first feature build.

## Usage

```console
./target/release/memsed-webui
```

Select a process, attach to it, enter a value and type, and run **First
Search**. Change the value in the target process, then use **Next Search**,
**Higher**, or **Lower** to narrow the results. Add an address to the
scratchpad to apply a new value or lock it.

Process-memory access may require elevated permissions. WebUI uses the
installed browser as the application window.

## Project structure

- `src/lib.rs`: Linux process, memory, region, and search backend
- `src/bin/memsed-webui.rs`: WebUI bindings and application state
- `ui/`: embedded process-selection and memory-operation pages

## License

See [LICENSE](LICENSE).
