// src/main.rs
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
mod app;
mod backend;
mod communication;
mod logging;
use crate::app::PolarimeterApp;
use crate::backend::backend_loop;
// (已修改) 导入新的通信枚举
use crate::communication::{Command, Update}; 
use egui::{Context, FontData, FontDefinitions, FontFamily};
use crossbeam_channel::unbounded;
use anyhow::Result; // <--- 引入我们的 Layer
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{fmt,EnvFilter};

use std::thread;

fn setup_chinese_fonts(ctx: &Context) -> Result<()> {
    let mut fonts = FontDefinitions::default();
    
    // Try to load Chinese fonts based on platform
    // let chinese_font_data = load_chinese_font()?;
    
    // Insert the Chinese font
    fonts.font_data.insert(
            "chinese".to_owned(),
            egui::FontData::from_static(include_bytes!("../SourceHanSansSC-Regular.otf")),
        );
    
    // Configure font families
    fonts.families.entry(FontFamily::Proportional).or_default()
        .insert(0, "chinese".to_owned());
    fonts.families.entry(FontFamily::Monospace).or_default()
        .insert(0, "chinese".to_owned());
    
    // Apply the font configuration
    ctx.set_fonts(fonts);
    
    Ok(())
}
// fn load_chinese_font() -> Result<FontData> {
//     #[cfg(target_os = "windows")]
//     {
//         load_windows_chinese_font()
//     }
    
//     #[cfg(target_os = "macos")]
//     {
//         load_macos_chinese_font()
//     }
    
//     #[cfg(target_os = "linux")]
//     {
//         load_linux_chinese_font()
//     }
    
//     #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
//     {
//         Err(FontError::UnsupportedPlatform)
//     }
// }

// #[cfg(target_os = "windows")]
// fn load_windows_chinese_font() -> Result<FontData> {
//     // List of common Chinese font paths on Windows
//     let font_paths = [
//         r"C:\Windows\Fonts\msyh.ttc",      // Microsoft YaHei
//         r"C:\Windows\Fonts\msyhbd.ttc",    // Microsoft YaHei Bold
//         r"C:\Windows\Fonts\simsun.ttc",    // SimSun
//         r"C:\Windows\Fonts\simhei.ttf",    // SimHei
//         r"C:\Windows\Fonts\simkai.ttf",    // KaiTi
//         r"C:\Windows\Fonts\simfang.ttf",   // FangSong
//         r"C:\Windows\Fonts\msjh.ttc",      // Microsoft JhengHei (Traditional Chinese)
//         r"C:\Windows\Fonts\msjhbd.ttc",    // Microsoft JhengHei Bold
//         r"C:\Windows\Fonts\kaiu.ttf",      // DFKai-SB (Traditional Chinese)
//         r"C:\Windows\Fonts\mingliu.ttc",   // MingLiU (Traditional Chinese)
//     ];
    
//     for font_path in &font_paths {
//         if let Ok(font_data) = std::fs::read(font_path) {
//             return Ok(FontData::from_owned(font_data));
//         }
//     }
    
//     Err(anyhow::anyhow!("你连中文字体都没有？"))
// }

// #[cfg(target_os = "macos")]
// fn load_macos_chinese_font() -> Result<FontData> {
//     let font_paths = [
//         "SourceHanSansSC-Regular.otf",
//         "/System/Library/Fonts/PingFang.ttc",           // PingFang SC
//         "/System/Library/Fonts/STHeiti Light.ttc",      // STHeiti
//         "/System/Library/Fonts/STHeiti Medium.ttc",
//         "/System/Library/Fonts/Hiragino Sans GB.ttc",   // Hiragino Sans GB
//         "/Library/Fonts/Arial Unicode.ttf",             // Arial Unicode MS
//         "/System/Library/Fonts/Apple LiGothic Medium.ttf", // Apple LiGothic (Traditional)
//     ];
    
//     for font_path in &font_paths {
//         if let Ok(font_data) = std::fs::read(font_path) {
//             tracing::info!("使用字体：{}",font_path);
//             return Ok(FontData::from_owned(font_data));
//         }
//     }
    
//     Err(anyhow::anyhow!("你连中文字体都没有？"))
// }

// #[cfg(target_os = "linux")]
// fn load_linux_chinese_font() -> Result<FontData, FontError> {
//     // Common Chinese font paths on Linux distributions
//     let font_paths = [
//         "/usr/share/fonts/truetype/droid/DroidSansFallbackFull.ttf",
//         "/usr/share/fonts/truetype/arphic/uming.ttc",
//         "/usr/share/fonts/truetype/arphic/ukai.ttc",
//         "/usr/share/fonts/truetype/wqy/wqy-microhei.ttc",
//         "/usr/share/fonts/truetype/wqy/wqy-zenhei.ttc",
//         "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
//         "/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf",
//         // Ubuntu/Debian paths
//         "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
//         // CentOS/RHEL paths
//         "/usr/share/fonts/google-droid/DroidSansFallbackFull.ttf",
//         // Arch Linux paths
//         "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
//     ];
    
//     for font_path in &font_paths {
//         if let Ok(font_data) = std::fs::read(font_path) {
//             return Ok(FontData::from_owned(font_data));
//         }
//     }
    
//     Err(anyhow::anyhow!("你连中文字体都没有？"))
// }
fn main() -> eframe::Result<()> {
    // 设置日志

    // (已修改) 创建使用新枚举类型的通道
    let (cmd_tx, cmd_rx) = unbounded::<Command>();
    let (update_tx, update_rx) = unbounded::<Update>();
    let egui_layer = logging::EguiTracingLayer::new(update_tx.clone()); // 克隆一个 sender 给日志系统

    tracing_subscriber::registry()
        .with(
            // 我们同时保留了标准的终端日志输出
            fmt::layer().with_writer(std::io::stdout),
        )
        .with(
             // 添加我们的自定义 egui layer
            egui_layer,
        )
         .with(
            // 添加一个过滤器，可以通过 RUST_LOG 环境变量控制日志级别
            // 例如 `RUST_LOG=info,my_app=debug`
            EnvFilter::new("info")
        )
        .init(); // 设置为全局默认订阅者
    // 在一个新线程中启动后端
    let backend_handle = thread::spawn(move || {
        backend_loop(cmd_rx, update_tx);
    });

    // 在主线程中运行 eframe (egui)
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1350.0, 780.0]),
        ..Default::default()
    };
    
    eframe::run_native(
        "旋光仪控制软件 v1.5.5",
        options,
        // 将后端线程的 handle 传递给 App
        Box::new(|cc| {
            setup_chinese_fonts(&cc.egui_ctx).expect("加载中文字体失败");
            Box::new(PolarimeterApp::new(cmd_tx, update_rx, Some(backend_handle)))
        }),
    )
}