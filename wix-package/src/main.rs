//! 生成 WiX 变量文件并调用 `wix build` 产出 MSI。
//!
//! 环境变量：
//! - `Q05_ARCH`：`x64` 或 `arm64`，默认 `x64`；
//! - `Q05_EXE`：要打包的 exe 路径，默认 `target/release/litmon-q05.exe`；
//! - `Q05_VERSION`：版本号，默认取本 crate 的 `CARGO_PKG_VERSION`；
//! - `Q05_OUT`：MSI 输出目录，默认 `dist`。

use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

const APP_NAME: &str = "litmon-q05";
const APP_DESCRIPTION: &str = "Oray Q0.5 第三方 Windows 客户端";

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let workspace = manifest_dir.parent().expect("workspace root").to_path_buf();
    let wix_dir = manifest_dir.join("wix");
    let wxi_path = wix_dir.join("var.wxi");

    let arch = env::var("Q05_ARCH").unwrap_or_else(|_| "x64".to_string());
    assert!(
        matches!(arch.as_str(), "x64" | "arm64"),
        "unsupported arch: {arch}"
    );
    let version = env::var("Q05_VERSION").unwrap_or_else(|_| env!("CARGO_PKG_VERSION").to_string());
    let exe = env::var_os("Q05_EXE").map(PathBuf::from).unwrap_or_else(|| {
        workspace
            .join("target")
            .join("release")
            .join("litmon-q05.exe")
    });
    assert!(exe.exists(), "exe not found: {}", exe.display());

    let license = workspace.join("LICENSE");
    let readme = workspace.join("README.md");
    let out_dir = env::var_os("Q05_OUT")
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace.join("dist"));
    fs::create_dir_all(&out_dir).expect("create output dir");

    let upgrade_code =
        uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_X500, APP_NAME.as_bytes()).to_string();
    let values = [
        ("AppName", APP_NAME.to_string()),
        ("Manufacturer", "littrick".to_string()),
        ("Version", version.clone()),
        ("UpgradeCode", upgrade_code),
        ("AppDes", APP_DESCRIPTION.to_string()),
        ("AppExe", exe.display().to_string()),
        ("LicenseFile", license.display().to_string()),
        ("ReadmeFile", readme.display().to_string()),
        ("Platform", arch.clone()),
    ];
    let mut wxi = String::from("<Include xmlns=\"http://wixtoolset.org/schemas/v4/wxs\">\n");
    for (key, value) in values {
        wxi.push_str(&format!("  <?define {key}=\"{value}\" ?>\n"));
    }
    wxi.push_str("</Include>\n");
    fs::write(&wxi_path, wxi).expect("write var.wxi");

    let msi_name = format!("litmon-q05-{version}-windows-{arch}.msi");
    let msi_path = out_dir.join(msi_name);

    let status = Command::new("wix")
        .args([
            "build",
            "package.wxs",
            "directories.wxs",
            "ui.wxs",
            "zh-CN.wxl",
            "-ext",
            "WixToolset.UI.wixext",
            "-culture",
            "zh-CN",
            "-arch",
            &arch,
            "-o",
            msi_path.to_str().expect("msi path"),
        ])
        .current_dir(&wix_dir)
        .status()
        .expect("failed to run wix; install with: dotnet tool install --global wix");
    assert!(status.success(), "wix build failed: {status}");
    assert!(
        msi_path.exists(),
        "msi not generated: {}",
        msi_path.display()
    );
    println!("MSI: {}", msi_path.display());
}
