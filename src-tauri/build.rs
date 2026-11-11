fn main() {
    tauri_build::build();
    embed_test_manifest();
}

/// 为集成测试二进制嵌入 Common-Controls v6 清单。
///
/// tauri::test 编译出的测试 exe 没有应用 manifest，加载器会把 comctl32.dll
/// 绑定到 System32 的 v5（5.82），而 tauri 默认特性链导入的
/// TaskDialogIndirect 只存在于 v6 → 进程初始化即 0xc0000139。
/// 主 exe 由 tauri_build 自带 manifest，此处的 link-arg 只作用于测试目标，
/// 互不交叠。
fn embed_test_manifest() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let manifest = r#"<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <dependency>
    <dependentAssembly>
      <assemblyIdentity type="win32" name="Microsoft.Windows.Common-Controls" version="6.0.0.0" processorArchitecture="*" publicKeyToken="6595b64144ccf1df" language="*" />
    </dependentAssembly>
  </dependency>
</assembly>
"#;
    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR set");
    let path = std::path::Path::new(&out_dir).join("test-common-controls-v6.manifest");
    std::fs::write(&path, manifest).expect("write test manifest");
    println!("cargo:rustc-link-arg-tests=/MANIFEST:EMBED");
    println!(
        "cargo:rustc-link-arg-tests=/MANIFESTINPUT:{}",
        path.display()
    );
    println!("cargo:rerun-if-changed=build.rs");
}
