//! 应用入口：全部逻辑在 `glasssuite` 库 crate（`lib.rs`），这里只负责起进程。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    glasssuite::run();
}
