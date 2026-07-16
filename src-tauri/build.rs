fn main() {
    // Rust 质量门禁不应依赖已经生成的前端产物；正式打包仍会先执行前端构建。
    let frontend_dist = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../dist");
    std::fs::create_dir_all(&frontend_dist).unwrap_or_else(|error| {
        panic!(
            "无法为 Tauri 上下文准备前端产物目录 {}：{error}",
            frontend_dist.display()
        )
    });
    tauri_build::build()
}
