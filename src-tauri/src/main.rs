//! 应用入口：全部逻辑在 `glasssuite` 库 crate（`lib.rs`），这里只负责起进程。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    let args: Vec<String> = std::env::args().collect();
    // CLI 子命令在单实例锁之前分发：`emit` 是短命客户端，不是第二实例
    // （OC-20261113-001 P2：应用主动上报通知的主路径）。
    if args.len() >= 2 && args[1] == "emit" {
        std::process::exit(glasssuite::cli::emit_main(&args));
    }
    glasssuite::run();
}
