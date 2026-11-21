//! 构建时从 SVG 生成多尺寸 ICO，并以资源 ID 1 嵌入 exe。
//!
//! 仓库只保存 SVG；ICO 写入 `OUT_DIR`，不进入版本库。
//! GPUI 的 Windows 后端按资源 ID 1 加载窗口图标。

use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};

const ICON_SIZES: [u32; 9] = [16, 20, 24, 32, 40, 48, 64, 128, 256];

fn main() {
    println!("cargo:rerun-if-changed=assets/litmon-q05.svg");

    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let ico = out_dir.join("litmon-q05.ico");
    let rc_file = out_dir.join("litmon-q05.rc");
    let res = out_dir.join("litmon-q05.res");

    let svg = fs::read(manifest_dir.join("assets").join("litmon-q05.svg"))
        .expect("读取 assets/litmon-q05.svg 失败");
    let ico_bytes = render_ico(&svg).expect("SVG 渲染为 ICO 失败");
    fs::write(&ico, ico_bytes).expect("写入 ICO 失败");

    let ico_path = ico.display().to_string().replace('\\', "\\\\");
    fs::write(&rc_file, format!("1 ICON \"{ico_path}\"\n")).expect("写入 rc 失败");

    let rc = find_rc().expect("找不到 rc.exe（Windows SDK）");
    let status = Command::new(&rc)
        .current_dir(&out_dir)
        .arg("/nologo")
        .arg("/fo")
        .arg(&res)
        .arg(&rc_file)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .unwrap_or_else(|err| panic!("启动 rc.exe 失败: {err}"));

    assert!(status.success(), "rc.exe 退出码 {status}");
    assert!(res.exists(), "rc.exe 未生成 {}", res.display());

    // MSVC 的 link.exe 可以直接接收 .res 输入。
    println!("cargo:rustc-link-arg-bins={}", res.display());
}

/// 用 resvg 将 SVG 渲染成各尺寸位图，并打包为 ICO（小尺寸使用 BMP 条目）。
fn render_ico(svg: &[u8]) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let tree = resvg::usvg::Tree::from_data(svg, &resvg::usvg::Options::default())?;
    let base = tree.size();
    let mut entries = Vec::new();
    for size in ICON_SIZES {
        let mut pixmap = resvg::tiny_skia::Pixmap::new(size, size)
            .ok_or_else(|| format!("无法创建 {size}x{size} 画布"))?;
        let sx = size as f32 / base.width();
        let sy = size as f32 / base.height();
        resvg::render(
            &tree,
            resvg::tiny_skia::Transform::from_scale(sx, sy),
            &mut pixmap.as_mut(),
        );
        entries.push((size, encode_bmp_entry(&pixmap)));
    }

    let mut ico = Vec::new();
    push_u16(&mut ico, 0);
    push_u16(&mut ico, 1);
    push_u16(&mut ico, entries.len() as u16);
    let mut offset = 6 + 16 * entries.len() as u32;
    for (size, data) in &entries {
        let dim = if *size == 256 { 0 } else { *size as u8 };
        ico.extend_from_slice(&[dim, dim, 0, 0]);
        push_u16(&mut ico, 1);
        push_u16(&mut ico, 32);
        push_u32(&mut ico, data.len() as u32);
        push_u32(&mut ico, offset);
        offset += data.len() as u32;
    }
    for (_, data) in &entries {
        ico.extend_from_slice(data);
    }
    Ok(ico)
}

fn encode_bmp_entry(pixmap: &resvg::tiny_skia::Pixmap) -> Vec<u8> {
    let w = pixmap.width();
    let h = pixmap.height();
    let mut data = Vec::new();
    push_u32(&mut data, 40); // BITMAPINFOHEADER
    push_u32(&mut data, w);
    push_u32(&mut data, h * 2);
    push_u16(&mut data, 1);
    push_u16(&mut data, 32);
    push_u32(&mut data, 0); // BI_RGB
    push_u32(&mut data, w * h * 4);
    push_u32(&mut data, 0);
    push_u32(&mut data, 0);
    push_u32(&mut data, 0);
    push_u32(&mut data, 0);

    // XOR 位图，BGRA，自下而上。
    for y in (0..h).rev() {
        for x in 0..w {
            let pixel = pixmap.pixel(x, y).expect("pixel in bounds").demultiply();
            data.extend_from_slice(&[pixel.blue(), pixel.green(), pixel.red(), pixel.alpha()]);
        }
    }

    // AND mask，自下而上；alpha=0 的像素置位。
    let mask_stride = w.div_ceil(32) * 4;
    for y in (0..h).rev() {
        let mut row = vec![0u8; mask_stride as usize];
        for x in 0..w {
            if pixmap.pixel(x, y).expect("pixel in bounds").alpha() == 0 {
                row[(x / 8) as usize] |= 0x80 >> (x % 8);
            }
        }
        data.extend_from_slice(&row);
    }
    data
}

fn push_u16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn push_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn find_rc() -> Option<PathBuf> {
    if Command::new("rc.exe")
        .arg("/?")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok()
    {
        return Some(PathBuf::from("rc.exe"));
    }

    let mut roots = Vec::new();
    for key in ["ProgramFiles(x86)", "ProgramFiles"] {
        if let Some(root) = env::var_os(key) {
            roots.push(PathBuf::from(root));
        }
    }
    roots.push(PathBuf::from(r"C:\Program Files (x86)"));

    for root in roots {
        let bin = root.join("Windows Kits").join("10").join("bin");
        let Ok(entries) = fs::read_dir(&bin) else {
            continue;
        };
        let mut versions: Vec<PathBuf> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.is_dir())
            .collect();
        versions.sort();
        for version in versions.into_iter().rev() {
            let candidate = version.join("x64").join("rc.exe");
            if candidate.exists() {
                return Some(candidate);
            }
        }
    }
    None
}
