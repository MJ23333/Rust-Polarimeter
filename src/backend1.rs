// src/backend.rs

use crate::communication::{
    BackendCommand, DynamicExpParams, FrontendUpdate, RegressionMode, Static
DynamicResult};
use crate::communication::{ConfusionMatrixData, RocCurveData, TrainedModel};
use anyhow::{anyhow, Result};
use calamine::{open_workbook, Reader, Xlsx};
use crossbeam_channel::{Receiver, Sender};
use image::{io::Reader as ImageReader, GrayImage};
use linfa::prelude::*;
use linfa::traits::{Fit, Predict};
use linfa_logistic::FittedLogisticRegression;
use linfa_logistic::LogisticRegression;
use ndarray::{Array, Array1, Array2, ArrayView1};
use ndarray::{ArrayBase, Dim, OwnedRepr};
use opencv::core::ALGO_HINT_DEFAULT;
use opencv::{core, imgproc, prelude::*, videoio};
use parking_lot::Mutex;
use rand::thread_rng;
use serde::{Deserialize, Serialize};
use serialport;
use std::collections::VecDeque;
use std::io::{self, BufRead, BufReader};
use std::path::Path;
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::thread;
use std::time::{Duration, Instant};
use tracing::{error, info}; // 使用 anyhow 来简化错误处理
                            // --- 相机管理结构体 ---

#[derive(Clone, Debug)]
struct CameraSettings {
    exposure: f64,
    show_circle: bool,
    lock_circle: bool,
    // 锁定后的圆心和半径
    locked_circle: Option<(i32, i32, i32)>,
    min_radius: i32,
    max_radius: i32,
}

impl Default for CameraSettings {
    fn default() -> Self {
        Self {
            exposure: -8.0,
            show_circle: true,
            lock_circle: false,
            locked_circle: None,
            min_radius: 30,
            max_radius: 45,
        }
    }
}

struct CameraManager {
    thread_handle: Option<thread::JoinHandle<()>>,
    stop_signal: Arc<AtomicBool>,
    settings: Arc<Mutex<CameraSettings>>,
    latest_frame: Arc<Mutex<Option<Mat>>>,
}

impl CameraManager {
    fn new(
        camera_index: i32,
        update_tx: Sender<FrontendUpdate>,
        initial_settings: Arc<Mutex<CameraSettings>>,
    ) -> Result<Self, opencv::Error> {
        let stop_signal = Arc::new(AtomicBool::new(false));
        let thread_stop_signal = stop_signal.clone();
        let thread_settings = initial_settings.clone();
        let latest_frame = Arc::new(Mutex::new(None));
        let thread_handle = {
            let thread_latest_frame = latest_frame.clone();

            thread::spawn(move || {
                // 在新线程中打开相机
                let mut cam = match videoio::VideoCapture::new(camera_index, videoio::CAP_ANY) {
                    Ok(cam) => {
                        if !cam.is_opened().unwrap_or(false) {
                            error!("后端：无法打开相机索引 {}", camera_index);
                            update_tx
                                .send(FrontendUpdate::CameraConnectionStatus(false))
                                .unwrap();
                            return;
                        }
                        update_tx
                            .send(FrontendUpdate::CameraConnectionStatus(true))
                            .unwrap();
                        info!("后端：相机 {} 已成功在捕获线程中打开", camera_index);
                        cam
                    }
                    Err(e) => {
                        error!("后端：创建VideoCapture失败：{}", e);
                        update_tx
                            .send(FrontendUpdate::CameraConnectionStatus(false))
                            .unwrap();
                        return;
                    }
                };

                // 循环捕获
                while !thread_stop_signal.load(Ordering::Relaxed) {
                    let mut frame = Mat::default();
                    if let Ok(true) = cam.read(&mut frame) {
                        if frame.empty() {
                            continue;
                        }

                        let mut processed_frame = frame.clone();
                        let mut settings = thread_settings.lock();
                        *thread_latest_frame.lock() = Some(frame.clone());
                        // 图像处理：霍夫圆检测
                        if settings.show_circle {
                            detect_and_draw_circle(&frame, &mut processed_frame, &mut settings);
                        }

                        // 将 Mat 转换为 egui::ColorImage
                        if let Some(color_image) = mat_to_color_image(processed_frame) {
                            update_tx
                                .send(FrontendUpdate::NewCameraFrame(Arc::new(color_image)))
                                .unwrap();
                        }
                    }
                    thread::sleep(Duration::from_millis(16)); // ~60 FPS
                }
                info!("后端：相机捕获线程 {} 已停止", camera_index);
            })
        };

        Ok(Self {
            thread_handle: Some(thread_handle),
            stop_signal,
            settings: initial_settings,
            latest_frame,
        })
    }
}

impl Drop for CameraManager {
    fn drop(&mut self) {
        info!("后端：正在关闭 CameraManager...");
        self.stop_signal.store(true, Ordering::Relaxed);
        if let Some(handle) = self.thread_handle.take() {
            handle.join().expect("无法 join 相机线程");
        }
        info!("后端：CameraManager 已成功关闭。");
    }
}

/// 后端状态
/// 后端状态 (已更新)
struct BackendState {
    camera_manager: Option<CameraManager>,
    camera_settings: Arc<Mutex<CameraSettings>>,
    // --- 新增串口状态 ---
    serial_port: Option<Box<dyn serialport::SerialPort>>,
    // 对应 Python 中的 `all_reverse_var` (false = 新/MAM, true = 旧/AMA)
    rotation_direction_is_ama: bool,
    mam_images: Vec<Vec<u8>>,
    ama_images: Vec<Vec<u8>>,
    persistent_mam: Vec<Vec<u8>>,
    persistent_ama: Vec<Vec<u8>>,
    fitted_model: Option<FittedLogisticRegression<f64, usize>>,
    // --- 新增静态测量状态 ---
    // 从零点开始累计的总步数
    current_static_steps: i32,
    static_results: Vec<StaticResult>,
    dynamic_results: Vec<DynamicResult>,
}
/// Backend main loop, runs in a separate thread.
pub fn backend_loop(cmd_rx: Receiver<BackendCommand>, update_tx: Sender<FrontendUpdate>) {
    info!("Backend thread started.");
    let mut state = BackendState {
        camera_manager: None,
        camera_settings: Arc::new(Mutex::new(CameraSettings::default())),
        // --- 初始化串口状态 ---
        serial_port: None,
        rotation_direction_is_ama: false,
        mam_images: Vec::new(),
        ama_images: Vec::new(),
        persistent_mam: Vec::new(),
        persistent_ama: Vec::new(),
        fitted_model: None,
        current_static_steps: 0,
        static_results: Vec::new(),
        dynamic_results: Vec::new(),
    };
    while let Ok(command) = cmd_rx.recv() {
        // --- Match every possible command from the frontend ---
        match command {
            BackendCommand::Shutdown => {
                info!("Backend shutting down.");
                shutdown_backend();
                break;
            }
            // --- Device Control ---
            BackendCommand::RefreshSerialPorts => refresh_serial_ports(&update_tx),
            BackendCommand::ConnectSerial { port, baud_rate } => {
                connect_serial(&mut state, port, baud_rate, &update_tx);
            }
            BackendCommand::DisconnectSerial => {
                disconnect_serial(&mut state, &update_tx);
            }
            BackendCommand::SetRotationDirection(is_ama) => {
                state.rotation_direction_is_ama = is_ama;
                info!(
                    "后端：旋转方向设置为 {}",
                    if is_ama { "AMA (旧)" } else { "MAM (新)" }
                );
            }
            BackendCommand::RotateMotor { angle } => {
                rotate_motor(&mut state, angle, &update_tx);
            }
            BackendCommand::StartRecording { mode, save_path } => {
                start_recording(&mode, &save_path, &update_tx)
            }
            BackendCommand::StopRecording => stop_recording(&update_tx),
            // --- Camera Control ---
            BackendCommand::RefreshCameras => refresh_cameras(&update_tx),
            BackendCommand::ConnectCamera { index } => {
                connect_camera(index, &update_tx, &mut state)
            }
            BackendCommand::DisconnectCamera => disconnect_camera(&update_tx, &mut state),
            BackendCommand::SetExposure(value) => set_exposure(value, &update_tx, &mut state),
            BackendCommand::SetLock(value) => set_lock(value, &update_tx, &mut state),
            BackendCommand::SetHoughCircleRadius { min, max } => {
                set_hough_circle_radius(min, max, &update_tx, &mut state)
            }
            // --- Model Training ---
            BackendCommand::ProcessVideoForTraining { video_path, mode } => {
                process_video_for_training(&mut state, &video_path, &mode, &update_tx);
            }
            BackendCommand::LoadPersistentDataset { path } => {
                load_persistent_dataset(&mut state, &path, &update_tx);
            }
            BackendCommand::LoadDataForPlotting { path } => {
                load_data_for_plotting(&path, &update_tx);
            }
            BackendCommand::TrainModel { show_roc, show_cm } => {
                train_model(&mut state, show_roc, show_cm, &update_tx);
            }
            BackendCommand::SaveModel { path } => save_model(&state, &path, &update_tx),
            BackendCommand::LoadModel { path } => load_model(&mut state, &path, &update_tx),
            BackendCommand::ExportDataset { path } => export_dataset(&path, &update_tx),
            BackendCommand::ResetModel => reset_model(&mut state, &update_tx),
            // --- Static Measurement ---
            BackendCommand::StartManualMeasurement => {
                // 进入测量模式时重置步数
                state.current_static_steps = 0;
                update_tx
                    .send(FrontendUpdate::StatusMessage(
                        "已进入静态测量模式".to_string(),
                    ))
                    .unwrap();
            }
            BackendCommand::StaticPreRotate { angle } => {
                let steps = (angle * 746.0).round() as i32;
                rotate_motor(&mut state, angle, &update_tx);
                state.current_static_steps += steps;
            }
            BackendCommand::FindZeroPoint => {
                find_zero_point(&mut state, &update_tx);
            }
            BackendCommand::RunSingleMeasurement => {
                run_single_measurement(&mut state, &update_tx);
            }
            BackendCommand::ReturnToZero => {
                // 计算需要反向旋转的度数
                let angle_to_return = -(state.current_static_steps as f32 / 746.0);
                rotate_motor(&mut state, angle_to_return, &update_tx);
                state.current_static_steps = 0; // 重置步数
            }
            // --- Dynamic Measurement ---
            BackendCommand::StartDynamicExperiment { params, save_path } => {
                // 当收到开始命令时，直接调用并进入阻塞的实验循环函数
                // 这个函数会接管后端线程，直到它自己结束
                run_dynamic_experiment_loop(&mut state, params, save_path, &update_tx, &cmd_rx);
            }
            BackendCommand::StopDynamicExperiment => {
                // 这个命令现在实际上什么也不做
                // 因为停止逻辑已在 run_dynamic_experiment_loop 内部处理
                info!("[后端] 收到终止命令（将被循环忽略，因其在监听通道）");
            }
            BackendCommand::SaveStaticResults { path } => {}
            BackendCommand::ClearStaticResults => {}
            // --- Data Processing ---
            BackendCommand::LoadDataForPlotting { path } => {
                load_data_for_plotting(&path, &update_tx)
            }
            BackendCommand::CalculateRegression { alpha_inf, mode } => {
                calculate_regression(alpha_inf, mode, &update_tx)
            }
        }
        thread::sleep(Duration::from_millis(10)); // Prevent 100% CPU usage
    }
    info!("Backend thread terminated.");
}

// ===============================================================
// ================ Backend Interface Stubs (All NEW stubs added) ================
// ===============================================================
fn refresh_cameras(update_tx: &Sender<FrontendUpdate>) {
    info!("[后端] 正在刷新相机列表...");
    let mut devices = Vec::new();
    // 尝试前10个索引，与Python代码逻辑一致
    for i in 0..10 {
        if let Ok(cam) = videoio::VideoCapture::new(i, videoio::CAP_ANY) {
            if cam.is_opened().unwrap_or(false) {
                devices.push(format!("Camera {}", i));
            } else {
                break;
            }
        }
    }
    info!("[后端] 发现的相机: {:?}", devices);
    update_tx.send(FrontendUpdate::CameraList(devices)).unwrap();
}

fn detect_and_draw_circle(input: &Mat, output: &mut Mat, settings: &mut CameraSettings) {
    let mut gray = Mat::default();
    if !settings.lock_circle {
        if let Ok(()) = imgproc::cvt_color(
            input,
            &mut gray,
            imgproc::COLOR_BGR2GRAY,
            0,
            core::AlgorithmHint::ALGO_HINT_DEFAULT,
        ) {
            let mut circles = core::Vector::<core::Vec3f>::new();
            imgproc::hough_circles(
                &gray,
                &mut circles,
                imgproc::HOUGH_GRADIENT,
                1.0,                 // dp
                30.0,                // minDist
                40.0,                // param1 (Canny a)
                10.0,                // param2 (Canny b)
                settings.min_radius, // minRadius
                settings.max_radius, // maxRadius
            )
            .unwrap_or(());

            if circles.len() > 0 {
                // 只取第一个检测到的圆
                let circle_params = circles.get(0).unwrap();
                let center = core::Point::new(
                    circle_params[0].round() as i32,
                    circle_params[1].round() as i32,
                );
                let radius = circle_params[2].round() as i32;

                let color = if settings.lock_circle {
                    core::Scalar::new(0.0, 0.0, 255.0, 255.0) // Red for locked
                } else {
                    core::Scalar::new(0.0, 255.0, 0.0, 255.0) // Green for unlocked
                };
                settings.locked_circle = Some((
                    circle_params[0].round() as i32,
                    circle_params[1].round() as i32,
                    circle_params[2].round() as i32,
                ));
                imgproc::circle(output, center, radius, color, 2, imgproc::LINE_AA, 0)
                    .unwrap_or(());
            }
        }
    } else {
        let center = core::Point::new(
            settings.locked_circle.unwrap().0,
            settings.locked_circle.unwrap().1,
        );
        let radius = settings.locked_circle.unwrap().2;

        let color = if settings.lock_circle {
            core::Scalar::new(0.0, 0.0, 255.0, 255.0) // Red for locked
        } else {
            core::Scalar::new(0.0, 255.0, 0.0, 255.0) // Green for unlocked
        };

        imgproc::circle(output, center, radius, color, 2, imgproc::LINE_AA, 0).unwrap_or(());
    }
}

fn mat_to_color_image(mat: Mat) -> Option<egui::ColorImage> {
    let mut rgba_mat = Mat::default();
    if imgproc::cvt_color(
        &mat,
        &mut rgba_mat,
        imgproc::COLOR_BGR2RGBA,
        0,
        core::AlgorithmHint::ALGO_HINT_DEFAULT,
    )
    .is_err()
    {
        return None;
    }

    let size = rgba_mat.size().unwrap();
    let width = size.width as usize;
    let height = size.height as usize;

    if let Ok(data) = rgba_mat.data_bytes() {
        let pixels: Vec<egui::Color32> = data
            .chunks_exact(4)
            .map(|p| egui::Color32::from_rgba_unmultiplied(p[0], p[1], p[2], p[3]))
            .collect();

        if pixels.len() == width * height {
            return Some(egui::ColorImage {
                size: [width, height],
                pixels,
            });
        }
    }
    None
}

fn shutdown_backend() {
    info!("[Backend] Executing cleanup...");
}

// --- Device Control ---

fn start_recording(mode: &str, save_path: &PathBuf, update_tx: &Sender<FrontendUpdate>) {
    info!(
        "[Backend] Start recording. Mode: {}, Path: {:?}",
        mode, save_path
    );
}
fn stop_recording(update_tx: &Sender<FrontendUpdate>) {
    info!("[Backend] Stop recording");
}

// --- Camera Control ---
fn connect_camera(index: usize, update_tx: &Sender<FrontendUpdate>, state: &mut BackendState) {
    info!("[Backend] Connecting to camera {}", index);
    state.camera_manager = None;
    info!("后端：请求连接到相机 {}", index);
    match CameraManager::new(
        index as i32,
        update_tx.clone(),
        state.camera_settings.clone(),
    ) {
        Ok(manager) => {
            state.camera_manager = Some(manager);
            update_tx
                .send(FrontendUpdate::CameraConnectionStatus(true))
                .unwrap();
        }
        Err(e) => {
            error!("后端：创建 CameraManager 失败: {}", e);
            update_tx
                .send(FrontendUpdate::CameraConnectionStatus(false))
                .unwrap();
        }
    }
}
fn disconnect_camera(update_tx: &Sender<FrontendUpdate>, state: &mut BackendState) {
    info!("[Backend] Disconnecting camera");
    state.camera_manager = None;
    update_tx
        .send(FrontendUpdate::CameraConnectionStatus(false))
        .unwrap();
}
fn set_exposure(value: f32, update_tx: &Sender<FrontendUpdate>, state: &mut BackendState) {
    state.camera_settings.lock().exposure = value as f64;
    // 注意：OpenCV 的 set property 可能在线程中操作更安全，这里简化
    info!("[Backend] Setting exposure to: {}", value);
}
fn set_lock(value: bool, update_tx: &Sender<FrontendUpdate>, state: &mut BackendState) {
    state.camera_settings.lock().lock_circle = value;
    // 注意：OpenCV 的 set property 可能在线程中操作更安全，这里简化
    info!("[Backend] Setting lock to: {}", value);
}
fn set_hough_circle_radius(
    min: u32,
    max: u32,
    update_tx: &Sender<FrontendUpdate>,
    state: &mut BackendState,
) {
    info!(
        "[Backend] Setting Hough circle radius: min={}, max={}",
        min, max
    );
    let mut settings = state.camera_settings.lock();
    settings.min_radius = min as i32;
    settings.max_radius = max as i32;
}
fn export_dataset(path: &PathBuf, update_tx: &Sender<FrontendUpdate>) {
    info!("[Backend] Exporting dataset to: {:?}", path);
}
fn refresh_serial_ports(update_tx: &Sender<FrontendUpdate>) {
    info!("[后端] 正在刷新串口列表");
    match serialport::available_ports() {
        Ok(ports) => {
            let port_names: Vec<String> = ports.into_iter().map(|p| p.port_name).collect();
            info!("[后端] 发现的串口: {:?}", port_names);
            update_tx
                .send(FrontendUpdate::SerialPortsList(port_names))
                .unwrap();
        }
        Err(e) => {
            error!("[后端] 无法获取串口列表: {}", e);
            update_tx
                .send(FrontendUpdate::SerialPortsList(vec![]))
                .unwrap();
        }
    }
}

fn connect_serial(
    state: &mut BackendState,
    port_name: String,
    baud_rate: u32,
    update_tx: &Sender<FrontendUpdate>,
) {
    info!("[后端] 尝试连接到串口 {} @ {} 波特率", port_name, baud_rate);
    // 先断开任何现有连接
    state.serial_port = None;

    // match serialport::new(&port_name, baud_rate)
    match serialport::new(&port_name, baud_rate)
        .timeout(Duration::from_millis(500)) // 设置超时，对 readline 很重要
        .open()
    {
        Ok(port) => {
            state.serial_port = Some(port);
            update_tx
                .send(FrontendUpdate::SerialConnectionStatus(true))
                .unwrap();
            update_tx
                .send(FrontendUpdate::StatusMessage(format!(
                    "已连接到 {}",
                    port_name
                )))
                .unwrap();
        }
        Err(e) => {
            update_tx
                .send(FrontendUpdate::SerialConnectionStatus(false))
                .unwrap();
            update_tx
                .send(FrontendUpdate::StatusMessage(format!("连接失败: {}", e)))
                .unwrap();
            error!("[后端] 连接串口失败: {}", e);
        }
    }
}

fn disconnect_serial(state: &mut BackendState, update_tx: &Sender<FrontendUpdate>) {
    if state.serial_port.is_some() {
        state.serial_port = None; // Drop 会自动关闭端口
        update_tx
            .send(FrontendUpdate::SerialConnectionStatus(false))
            .unwrap();
        update_tx
            .send(FrontendUpdate::StatusMessage("设备已断开".to_string()))
            .unwrap();
        info!("[后端] 串口已断开");
    }
}

/// 发送单个命令到 Arduino 的辅助函数
fn cmd(port: &mut dyn serialport::SerialPort, data: u8) -> io::Result<()> {
    port.write_all(&[data])?;
    thread::sleep(Duration::from_millis(10)); // 对应 python code 的 0.01s delay
    port.write_all(&[100])?; // Stop command
    Ok(())
}

/// `precision_rotate` 的 Rust 实现
fn precision_rotate(
    port: &mut dyn serialport::SerialPort,
    angle: f32,
    is_ama: bool, // 对应 all_reverse_var
    need_reverse: bool,
) -> io::Result<()> {
    let mut angle = angle;
    let reverse = is_ama && need_reverse;
    if reverse {
        angle = -angle;
    }

    let mut steps = (angle * 746.0).round() as i32;
    info!("[后端] 旋转: angle={}, steps={}", angle, steps);

    let commands = if steps > 0 {
        vec![62, 60, 58, 56, 64, 66, 68] // 正转指令
    } else {
        steps = -steps;
        vec![63, 61, 59, 57, 65, 67, 69] // 反转指令
    };

    let divisors = [3730, 746, 373, 75, 37, 7, 1];

    for i in 0..divisors.len() {
        let num_rotations = steps / divisors[i];
        steps %= divisors[i];
        for _ in 0..num_rotations {
            cmd(port, commands[i])?;
        }
    }
    Ok(())
}

fn rotate_motor(state: &mut BackendState, angle: f32, update_tx: &Sender<FrontendUpdate>) {
    if let Some(port) = &mut state.serial_port {
        update_tx
            .send(FrontendUpdate::StatusMessage(format!(
                "正在旋转 {:.2}°...",
                angle
            )))
            .unwrap();
        // 在 `precision_rotate` 的 Rust 实现中，第二个布尔值 `is_ama` 对应 python 的 `all_reverse_var.get()`，
        // 第三个布尔值 `need_reverse` 在手动旋转时为 `false`
        if let Err(e) =
            precision_rotate(port.as_mut(), angle, state.rotation_direction_is_ama, false)
        {
            error!("[后端] 旋转失败: {}", e);
            update_tx
                .send(FrontendUpdate::StatusMessage(format!("旋转错误: {}", e)))
                .unwrap();
        } else {
            update_tx
                .send(FrontendUpdate::StatusMessage("旋转完成".to_string()))
                .unwrap();
        }
    } else {
        update_tx
            .send(FrontendUpdate::StatusMessage(
                "错误: 设备未连接".to_string(),
            ))
            .unwrap();
    }
}

// --- Model Training ---
fn process_frame_for_ml(frame: &Mat) -> Result<Vec<u8>, opencv::Error> {
    let mut gray = Mat::default();
    imgproc::cvt_color(
        frame,
        &mut gray,
        imgproc::COLOR_BGR2GRAY,
        0,
        core::AlgorithmHint::ALGO_HINT_DEFAULT,
    )?;

    // 霍夫圆检测以定位区域
    let mut circles = core::Vector::<core::Vec3f>::new();
    imgproc::hough_circles(
        &gray,
        &mut circles,
        imgproc::HOUGH_GRADIENT,
        1.0,
        30.0,
        40.0,
        10.0,
        30,
        45,
    )?;

    if circles.is_empty() {
        return Err(opencv::Error::new(core::StsError, "未检测到圆形"));
    }

    let p = circles.get(0)?;
    let center = core::Point::new(p[0] as i32, p[1] as i32);
    let radius = p[2] as i32;

    // 裁剪并缩放
    let rect = core::Rect::new(center.x - radius, center.y - radius, radius * 2, radius * 2);
    let cropped = Mat::roi(&gray, rect)?;
    let mut resized = Mat::default();
    imgproc::resize(
        &cropped,
        &mut resized,
        core::Size::new(20, 20),
        0.0,
        0.0,
        imgproc::INTER_AREA,
    )?;

    // 展平并返回
    let mut flat: Vec<u8> = Vec::with_capacity(400);
    if resized.is_continuous() {
        flat.extend_from_slice(resized.data_bytes()?);
    } else {
        // ... (处理非连续Mat的逻辑) ...
    }
    Ok(flat)
}

fn process_video_for_training(
    state: &mut BackendState,
    video_path: &PathBuf,
    mode: &str,
    update_tx: &Sender<FrontendUpdate>,
) {
    info!("[后端] 开始处理视频: {:?}, 模式: {}", video_path, mode);
    update_tx
        .send(FrontendUpdate::VideoProcessingUpdate {
            mode: mode.to_string(),
            message: "打开视频...".to_string(),
        })
        .unwrap();

    let mut cap =
        match videoio::VideoCapture::from_file(video_path.to_str().unwrap(), videoio::CAP_ANY) {
            Ok(cap) => cap,
            Err(e) => {
                update_tx
                    .send(FrontendUpdate::VideoProcessingUpdate {
                        mode: mode.to_string(),
                        message: format!("错误: {}", e),
                    })
                    .unwrap();
                return;
            }
        };
    let total_frames = cap.get(videoio::CAP_PROP_FRAME_COUNT).unwrap_or(0.0) as u32;
    let mut processed_images = Vec::new();
    let mut frame_count = 0;

    while let Ok(true) = cap.is_opened() {
        let mut frame = Mat::default();
        if let Ok(true) = cap.read(&mut frame) {
            if frame.empty() {
                break;
            }
            frame_count += 1;
            if frame_count % 10 == 0 {
                // 每10帧更新一次进度
                let msg = format!("处理中: {}/{}", frame_count, total_frames);
                update_tx
                    .send(FrontendUpdate::VideoProcessingUpdate {
                        mode: mode.to_string(),
                        message: msg,
                    })
                    .unwrap();
            }
            if let Ok(processed) = process_frame_for_ml(&frame) {
                processed_images.push(processed);
            }
        } else {
            break;
        }
    }

    if mode == "MAM" {
        state.mam_images = processed_images;
        update_tx
            .send(FrontendUpdate::MAMDatasetStatus(format!(
                "完成, 提取了 {} 帧",
                frame_count
            )))
            .unwrap();
    } else {
        state.ama_images = processed_images;
        update_tx
            .send(FrontendUpdate::AMADatasetStatus(format!(
                "完成, 提取了 {} 帧",
                frame_count
            )))
            .unwrap();
    }
    update_tx
        .send(FrontendUpdate::VideoProcessingUpdate {
            mode: mode.to_string(),
            message: format!("完成, 提取了 {} 帧", frame_count),
        })
        .unwrap();
}

fn calculate_binary_confusion_matrix(
    predictions: &ArrayBase<OwnedRepr<usize>, Dim<[usize; 1]>>,
    targets: &ArrayBase<OwnedRepr<usize>, Dim<[usize; 1]>>,
) -> [[u32; 2]; 2] {
    // 初始化一个2x2的混淆矩阵，所有元素为0
    let mut confusion_matrix = [[0u32; 2]; 2];

    // 检查预测和真实标签的长度是否一致
    if predictions.len() != targets.len() {
        // 在实际应用中，你可能需要返回一个 Result 或 panic
        // 为了简化，这里直接返回初始化的矩阵
        eprintln!("错误: 预测和真实标签的长度不一致.");
        return confusion_matrix;
    }

    // 遍历预测结果和真实标签
    let num_samples = predictions.len();
    info!("{}", num_samples);
    for i in 0..num_samples {
        let true_label = targets[i];
        let predicted_label = predictions[i];

        // 确保标签是0或1
        if true_label > 1 || predicted_label > 1 {
            eprintln!("警告: 存在非0或1的标签。此函数只适用于二分类。");
            continue;
        }

        // 更新混淆矩阵。
        // cm[真实标签][预测标签]
        confusion_matrix[true_label][predicted_label] += 1;
    }

    confusion_matrix
}
fn train_model(
    state: &mut BackendState,
    show_roc: bool,
    show_cm: bool,
    update_tx: &Sender<FrontendUpdate>,
) {
    info!("[后端] 开始训练模型");
    update_tx
        .send(FrontendUpdate::TrainingUpdate("准备数据...".to_string()))
        .unwrap();
    let all_mam = [&state.mam_images[..], &state.persistent_mam[..]].concat();
    let all_ama = [&state.ama_images[..], &state.persistent_ama[..]].concat();

    if all_mam.is_empty() || all_ama.is_empty() {
        update_tx
            .send(FrontendUpdate::TrainingUpdate(
                "错误: 数据集为空".to_string(),
            ))
            .unwrap();
        return;
    }

    // 将数据转换为 ndarray
    let mam_records = all_mam.len();
    let ama_records = all_ama.len();
    let records = mam_records + ama_records;
    let features = 400; // 20x20
    let mut data_vec: Vec<f64> = Vec::with_capacity(records * features);
    all_mam
        .iter()
        .for_each(|img| data_vec.extend(img.iter().map(|&p| p as f64 / 255.0)));
    all_ama
        .iter()
        .for_each(|img| data_vec.extend(img.iter().map(|&p| p as f64 / 255.0)));
    let data_array = Array2::from_shape_vec((records, features), data_vec).unwrap();

    let mut labels_vec: Vec<usize> = Vec::with_capacity(records);
    labels_vec.resize(mam_records, 0); // MAM a 0
    labels_vec.extend_from_slice(&vec![1; ama_records]); // AMA a 1
    let labels_array = Array1::from(labels_vec);

    let dataset = Dataset::new(data_array, labels_array);
    let mut rng = thread_rng();
    let shuffled = dataset.shuffle(&mut rng);
    let (train, valid) = shuffled.split_with_ratio(0.8);

    update_tx
        .send(FrontendUpdate::TrainingUpdate("正在训练...".to_string()))
        .unwrap();
    let model: FittedLogisticRegression<f64, usize> =
        LogisticRegression::default().fit(&train).unwrap();
    // 保存模型参数
    state.fitted_model = Some(model.clone());

    update_tx
        .send(FrontendUpdate::TrainingUpdate("评估模型...".to_string()))
        .unwrap();
    let predictions = model.predict(&valid);
    let cm = predictions.confusion_matrix(valid.targets()).unwrap();
    let accuracy = cm.accuracy();
    let cm = calculate_binary_confusion_matrix(&predictions, valid.targets());

    info!("[后端] 模型准确度: {}", accuracy);

    // 发送图表数据
    update_tx
        .send(FrontendUpdate::TrainingPlotsReady {
            cm: if show_cm {
                Some(ConfusionMatrixData {
                    matrix: cm,
                    accuracy,
                })
            } else {
                None
            },
            roc: if show_roc { None } else { None }, // ROC 计算较复杂，暂留空
        })
        .unwrap();

    update_tx
        .send(FrontendUpdate::TrainingUpdate(format!(
            "训练完成, 准确度: {:.2}%",
            accuracy * 100.0
        )))
        .unwrap();
    update_tx.send(FrontendUpdate::ModelReady(true)).unwrap();
}
fn load_persistent_dataset(
    state: &mut BackendState,
    path: &PathBuf,
    update_tx: &Sender<FrontendUpdate>,
) {
    info!("[后端] 开始加载常驻数据集于: {:?}", path);
    update_tx
        .send(FrontendUpdate::StatusMessage(
            "正在加载常驻数据集...".to_string(),
        ))
        .unwrap();
    update_tx
        .send(FrontendUpdate::PersistentDatasetStatus(
            "正在加载...".to_string(),
        ))
        .unwrap();
    let mut loaded_mam = 0;
    let mut loaded_ama = 0;

    // 加载 dataset0 (MAM)
    let mam_path = path.join("dataset0");
    state.persistent_mam.clear();
    if let Ok(entries) = std::fs::read_dir(mam_path) {
        for entry in entries.flatten() {
            if let Ok(img) = image::open(entry.path()) {
                let luma_img = img.to_luma8();
                // 注意：这里我们假设图片已经是20x20，如果不是，还需要resize
                // let resized = image::imageops::resize(&luma_img, 20, 20, image::imageops::FilterType::Triangle);
                state.persistent_mam.push(luma_img.into_raw());
                loaded_mam += 1;
            }
        }
    }

    // 加载 dataset1 (AMA)
    let ama_path = path.join("dataset1");
    state.persistent_ama.clear();
    if let Ok(entries) = std::fs::read_dir(ama_path) {
        for entry in entries.flatten() {
            if let Ok(img) = image::open(entry.path()) {
                let luma_img = img.to_luma8();
                state.persistent_ama.push(luma_img.into_raw());
                loaded_ama += 1;
            }
        }
    }

    let msg = format!("常驻数据集加载完成: MAM={}, AMA={}", loaded_mam, loaded_ama);
    info!("[后端] {}", msg);
    update_tx
        .send(FrontendUpdate::StatusMessage(msg.clone()))
        .unwrap();
    update_tx
        .send(FrontendUpdate::PersistentDatasetStatus(msg))
        .unwrap();
}
fn save_model(state: &BackendState, path: &PathBuf, update_tx: &Sender<FrontendUpdate>) {
    // if let Some(model) = &state.model {
    //     match bincode::serialize(model) {
    //         Ok(bytes) => {
    //             if let Err(e) = std::fs::write(path, bytes) {
    //                 update_tx.send(FrontendUpdate::StatusMessage(format!("保存失败: {}", e))).unwrap();
    //             } else {
    //                 update_tx.send(FrontendUpdate::StatusMessage("模型已保存".to_string())).unwrap();
    //             }
    //         }
    //         Err(e) => update_tx.send(FrontendUpdate::StatusMessage(format!("序列化失败: {}", e))).unwrap(),
    //     }
    // }
}

fn load_model(state: &mut BackendState, path: &PathBuf, update_tx: &Sender<FrontendUpdate>) {
    // match std::fs::read(path) {
    //     Ok(bytes) => match bincode::deserialize::<TrainedModel>(&bytes) {
    //         Ok(model) => {
    //             state.model = Some(model);
    //             update_tx.send(FrontendUpdate::ModelReady(true)).unwrap();
    //             update_tx.send(FrontendUpdate::StatusMessage("模型已加载".to_string())).unwrap();
    //         }
    //         Err(e) => update_tx.send(FrontendUpdate::StatusMessage(format!("反序列化失败: {}", e))).unwrap(),
    //     },
    //     Err(e) => update_tx.send(FrontendUpdate::StatusMessage(format!("读取文件失败: {}", e))).unwrap(),
    // }
}

fn reset_model(state: &mut BackendState, update_tx: &Sender<FrontendUpdate>) {
    state.mam_images.clear();
    state.ama_images.clear();
    state.persistent_mam.clear();
    state.persistent_ama.clear();
    state.fitted_model = None;
    update_tx.send(FrontendUpdate::ModelReady(false)).unwrap();
    update_tx
        .send(FrontendUpdate::TrainingUpdate("无可用模型".to_string()))
        .unwrap();
    update_tx
        .send(FrontendUpdate::VideoProcessingUpdate {
            mode: "MAM".to_string(),
            message: "未处理".to_string(),
        })
        .unwrap();
    update_tx
        .send(FrontendUpdate::VideoProcessingUpdate {
            mode: "AMA".to_string(),
            message: "未处理".to_string(),
        })
        .unwrap();
    update_tx
        .send(FrontendUpdate::StatusMessage(
            "模型和数据已重置".to_string(),
        ))
        .unwrap();
}

// --- Static Measurement ---
/// 辅助函数：从相机获取一帧图像
fn get_latest_frame(state: &BackendState) -> Result<Mat> {
    if let Some(cam_manager) = &state.camera_manager {
        for _ in 0..10 {
            // 尝试几次以确保获取到的是新帧
            if let Some(frame) = cam_manager.latest_frame.lock().clone() {
                return Ok(frame);
            }
            thread::sleep(Duration::from_millis(50));
        }
        Err(anyhow!("无法从相机获取图像帧"))
    } else {
        Err(anyhow!("相机未连接"))
    }
}

/// 辅助函数：对图像帧运行ML预测
fn predict_from_frame(frame: &Mat, model: &FittedLogisticRegression<f64, usize>) -> Result<usize> {
    let features_u8 = process_frame_for_ml(frame)?;
    let features_f64: Vec<f64> = features_u8.iter().map(|&p| p as f64 / 255.0).collect();
    let features_arr = Array1::from(features_f64);

    // (已优化) 不再需要 new_from_raw，直接使用传入的、已存在的模型对象进行预测
    let dataset = DatasetBase::from(features_arr.insert_axis(ndarray::Axis(0)));
    let prediction = model.predict(&dataset);

    Ok(prediction[0])
}

/// 辅助函数：等待 Arduino 的同步信号
fn wait_for_arduino_signal(port: &mut dyn serialport::SerialPort) -> Result<i32> {
    let mut reader = BufReader::new(port);
    let mut line_buffer = String::new();
    reader.read_line(&mut line_buffer)?;
    Ok(line_buffer.trim().parse().unwrap_or(0))
}

/// `find_zero_point` 的完整实现
fn find_zero_point(state: &mut BackendState, update_tx: &Sender<FrontendUpdate>) {
    if state.fitted_model.is_none() || state.camera_manager.is_none() || state.serial_port.is_none()
    {
        update_tx
            .send(FrontendUpdate::StatusMessage(
                "错误: 设备或模型未就绪".to_string(),
            ))
            .unwrap();
        return;
    }

    update_tx
        .send(FrontendUpdate::StaticMeasurementUpdate(
            "开始寻找零点...".to_string(),
        ))
        .unwrap();

    let initial_frame = match get_latest_frame(state) {
        Ok(f) => f,
        Err(_) => return,
    };
    let model = state.fitted_model.as_ref().unwrap();
    // let mut prediction = match predict_from_frame(&initial_frame, model) { Ok(p) => p, Err(_) => return };

    let mut predictions: VecDeque<usize> = VecDeque::from(vec![2; 5]);
    let mut measured_steps = 0;
    let timeout = Duration::from_secs(60);
    let start_time = Instant::now();
    let mut first =2;

    loop {
        if start_time.elapsed() > timeout {
            update_tx
                .send(FrontendUpdate::StaticMeasurementUpdate(
                    "测量超时".to_string(),
                ))
                .unwrap();
            break;
        }
        let frame = match get_latest_frame(state) {
            Ok(f) => f,
            Err(_) => break,
        };
        let model = state.fitted_model.as_ref().unwrap();
        let prediction = match predict_from_frame(&frame, model) {
            Ok(p) => p,
            Err(_) => continue,
        };

        predictions.pop_front();
        predictions.push_back(prediction);

        let mut should_break = false;

        update_tx
            .send(FrontendUpdate::StaticMeasurementUpdate(format!(
                "测量中 ({} steps): {:?}",
                measured_steps, predictions
            )))
            .unwrap();

        let pred_slice = predictions.make_contiguous();
        if first==2{
            first=prediction;
        }

        if (pred_slice == [0, 0, 0, 1, 1]) {
            if let Some(port) = &mut state.serial_port {
                // (已修正) 使用 != 进行逻辑异或
                let is_ama_logic = (prediction == 1) != state.rotation_direction_is_ama;
                let correction_cmd = if is_ama_logic { 114 } else { 55 };

                if cmd(port.as_mut(), correction_cmd).is_err()
                    || wait_for_arduino_signal(port.as_mut()).is_err()
                {
                    break;
                }

                update_tx
                    .send(FrontendUpdate::StaticMeasurementUpdate(
                        "测量完成".to_string(),
                    ))
                    .unwrap();
                state.current_static_steps=0;
                should_break = true;
                thread::sleep(Duration::from_millis(150))
            }
        } else if (pred_slice == [1, 1, 1, 0, 0]) {
            if let Some(port) = &mut state.serial_port {
                // (已修正) 使用 != 进行逻辑异或
                let is_ama_logic = (prediction == 1) != state.rotation_direction_is_ama;
                let correction_cmd = if is_ama_logic { 55 } else { 114 };

                if cmd(port.as_mut(), correction_cmd).is_err()
                    || wait_for_arduino_signal(port.as_mut()).is_err()
                {
                    break;
                }

                update_tx
                    .send(FrontendUpdate::StaticMeasurementUpdate(
                        "测量完成".to_string(),
                    ))
                    .unwrap();

                should_break = true;
                state.current_static_steps=0;
                thread::sleep(Duration::from_millis(150))
            }
        } else {
            if let Some(port) = &mut state.serial_port {
                // (已修正) 使用 != 进行逻辑异或
                let direction_bit = (first == 1) != state.rotation_direction_is_ama;
                let cmd_byte = if direction_bit { 51 } else { 53 };
                if cmd(port.as_mut(), cmd_byte).is_err()
                    || wait_for_arduino_signal(port.as_mut()).is_err()
                {
                    break;
                }
                thread::sleep(Duration::from_millis(10))
            } else {
                break;
            }
        }

        if should_break {
            break;
        }
        if pred_slice == [0, 0, 0, 0, 0] {
            first = 0;
        }
        if pred_slice == [1, 1, 1, 1, 1] {
            first = 1;
        }
    }
    // update_tx.send(FrontendUpdate::StaticMeasurementUpdate("正在寻找零点...".to_string())).unwrap();

    // if let Some(port) = &mut state.serial_port {
    //     if port.write_all(&[52]).is_err() || wait_for_arduino_signal(port.as_mut()).is_err() {
    //         update_tx.send(FrontendUpdate::StatusMessage("错误: 无法与设备通信".to_string())).unwrap();
    //         return;
    //     }
    // } else {
    //     update_tx.send(FrontendUpdate::StatusMessage("错误: 设备未连接".to_string())).unwrap();
    //     return;
    // }
    // let mut predictions: VecDeque<usize> = VecDeque::from(vec![2; 5]);

    // let timeout = Duration::from_secs(30);
    // let start_time = Instant::now();

    // // 3. 开始搜索循环
    // loop {
    //     if start_time.elapsed() > timeout {
    //         update_tx.send(FrontendUpdate::StaticMeasurementUpdate("寻找零点超时".to_string())).unwrap();
    //         break;
    //     }

    //     let frame = match get_latest_frame(state) { Ok(f) => f, Err(_) => break };
    //     let model = state.fitted_model.as_ref().unwrap(); // 我们在开头检查过is_none
    //     let prediction = match predict_from_frame(&frame, model) { Ok(p) => p, Err(_) => continue };

    //     let pred_slice = predictions.make_contiguous();
    //     let mut needs_break = false;
    //     // 2b. 在独立的块中可变借用 state 来操作串口
    //     if let Some(port) = &mut state.serial_port {
    //         if pred_slice == [0, 0, 0, 1, 1] {
    //             // (已修正) 使用 != 进行逻辑异或
    //             let cmd_byte = if (prediction == 1) != state.rotation_direction_is_ama { 55 } else { 114 };
    //             cmd(port.as_mut(), cmd_byte).unwrap_or(());
    //             update_tx.send(FrontendUpdate::StaticMeasurementUpdate("找到零点!".to_string())).unwrap();
    //             needs_break = true;
    //         } else if pred_slice == [1, 1, 1, 0, 0] {
    //             let cmd_byte = if (prediction == 1) != state.rotation_direction_is_ama { 114 } else { 55 };
    //             cmd(port.as_mut(), cmd_byte).unwrap_or(());
    //             update_tx.send(FrontendUpdate::StaticMeasurementUpdate("找到零点!".to_string())).unwrap();
    //             needs_break = true;
    //         }

    //         if needs_break {
    //             break;
    //         }

    //         let direction_bit = if state.rotation_direction_is_ama { prediction } else { 1 - prediction };
    //         let cmd_byte = if direction_bit == 0 { 51 } else { 53 };
    //         if cmd(port.as_mut(), cmd_byte).is_err() || wait_for_arduino_signal(port.as_mut()).is_err() {
    //             break;
    //         }
    //     } else {
    //         break; // 串口断开
    //     }
    // }
}

/// `run_single_measurement` 的完整实现
fn run_single_measurement(state: &mut BackendState, update_tx: &Sender<FrontendUpdate>) {
    if state.fitted_model.is_none() || state.camera_manager.is_none() || state.serial_port.is_none()
    {
        update_tx
            .send(FrontendUpdate::StatusMessage(
                "错误: 设备或模型未就绪".to_string(),
            ))
            .unwrap();
        return;
    }

    update_tx
        .send(FrontendUpdate::StaticMeasurementUpdate(
            "开始精细测量...".to_string(),
        ))
        .unwrap();

    let model = state.fitted_model.as_ref().unwrap();

    let mut predictions: VecDeque<usize> = VecDeque::from(vec![2; 5]);
    let mut measured_steps = 0;
    let timeout = Duration::from_secs(60);
    let start_time = Instant::now();
    let mut first= 2;
    loop {
        if start_time.elapsed() > timeout {
            update_tx
                .send(FrontendUpdate::StaticMeasurementUpdate(
                    "测量超时".to_string(),
                ))
                .unwrap();
            break;
        }
        let frame = match get_latest_frame(state) {
            Ok(f) => f,
            Err(_) => break,
        };
        let model = state.fitted_model.as_ref().unwrap();
        let prediction = match predict_from_frame(&frame, model) {
            Ok(p) => p,
            Err(_) => continue,
        };
        let mut should_break = false;
        
        

        predictions.pop_front();
        predictions.push_back(prediction);
        update_tx
            .send(FrontendUpdate::StaticMeasurementUpdate(format!(
                "测量中 ({} steps): {:?}",
                measured_steps, predictions
            )))
            .unwrap();

        let pred_slice = predictions.make_contiguous();
        if (first==2){
            first=prediction;
        }
        if (pred_slice == [0, 0, 0, 1, 1])
        {
            if let Some(port) = &mut state.serial_port {
                // (已修正) 使用 != 进行逻辑异或
                let (correction_cmd, correction_steps) =
                    if state.rotation_direction_is_ama { (48, -12) } else { (54, 12) };

                if cmd(port.as_mut(), correction_cmd).is_err()
                    || wait_for_arduino_signal(port.as_mut()).is_err()
                {
                    break;
                }
                measured_steps += correction_steps;
                state.current_static_steps+=correction_steps;


                let result = StaticResult {
                    index: state.static_results.len() + 1,
                    steps: measured_steps,
                    angle: measured_steps as f32 / 746.0,
                };
                state.static_results.push(result);
                update_tx.send(FrontendUpdate::StaticResultsUpdated(state.static_results.clone())).unwrap();
                update_tx
                    .send(FrontendUpdate::StaticMeasurementUpdate(
                        "测量完成".to_string(),
                    ))
                    .unwrap();
                should_break = true;
                thread::sleep(Duration::from_millis(150));
            }
        }else if (pred_slice == [1,1,1,0,0])
        {
            if let Some(port) = &mut state.serial_port {
                // (已修正) 使用 != 进行逻辑异或
                let (correction_cmd, correction_steps) =
                    if state.rotation_direction_is_ama { (54, -12) } else { (48, 12) };

                if cmd(port.as_mut(), correction_cmd).is_err()
                    || wait_for_arduino_signal(port.as_mut()).is_err()
                {
                    break;
                }
                measured_steps += correction_steps;
                state.current_static_steps+=correction_steps;


               let result = StaticResult {
                    index: state.static_results.len() + 1,
                    steps: measured_steps,
                    angle: measured_steps as f32 / 746.0,
                };
                state.static_results.push(result);
                update_tx.send(FrontendUpdate::StaticResultsUpdated(state.static_results.clone())).unwrap();
                update_tx
                    .send(FrontendUpdate::StaticMeasurementUpdate(
                        "测量完成".to_string(),
                    ))
                    .unwrap();
                should_break = true;
                thread::sleep(Duration::from_millis(150));
            }
        }else{
            if let Some(port) = &mut state.serial_port {
            // (已修正) 使用 != 进行逻辑异或
            let direction_bit = (first == 1) != state.rotation_direction_is_ama;
            let (cmd_byte, step_change) = if direction_bit { (51, 6) } else { (53, -6) };

            if cmd(port.as_mut(), cmd_byte).is_err()
                || wait_for_arduino_signal(port.as_mut()).is_err()
            {
                break;
            }
            measured_steps += step_change;
            state.current_static_steps+=step_change;
        } else {
            break;
        }
        }

        if should_break {
            break;
        }

        if pred_slice == [0, 0, 0, 0, 0] {
            first = 0;
        }
        if pred_slice == [1, 1, 1, 1, 1] {
            first = 1;
        }
    }
}

// --- Dynamic Measurement ---
fn run_dynamic_experiment_loop(
    state: &mut BackendState,
    params: DynamicExpParams,
    _save_path: PathBuf, // 暂时未使用，但保留以备将来保存
    update_tx: &Sender<FrontendUpdate>,
    cmd_rx: &Receiver<BackendCommand>, // 传入命令接收器以监听停止信号
) {
    // --- 检查先决条件 ---
    if state.fitted_model.is_none() || state.camera_manager.is_none() || state.serial_port.is_none()
    {
        update_tx
            .send(FrontendUpdate::StatusMessage(
                "错误: 设备或模型未就绪".to_string(),
            ))
            .unwrap();
        update_tx
            .send(FrontendUpdate::DynamicExperimentFinished)
            .unwrap();
        return;
    }
    update_tx
        .send(FrontendUpdate::StatusMessage("动态实验已启动".to_string()))
        .unwrap();
    info!("[后端] 进入动态实验循环, 参数: {:?}", params);

    // --- 初始化实验状态 ---
    let mut step_count = 0;
    let mut result_index = 1;
    let mut predictions: VecDeque<usize> = VecDeque::from(vec![2; 5]);
    let start_time = Instant::now();
    let is_ama = state.rotation_direction_is_ama;

    // 1. 初始预旋转
    if let Some(port) = &mut state.serial_port {
        let initial_steps = (params.pre_rotation_angle * 746.0).round() as i32;
        precision_rotate(port.as_mut(), params.pre_rotation_angle, is_ama, false).unwrap_or(());
        step_count += initial_steps;
        update_tx
            .send(FrontendUpdate::CurrentAngleUpdate(
                step_count as f32 / 746.0,
            ))
            .unwrap();
    }

    // 2. 测量主循环
    loop {
        // --- 核心：检查中断信号 ---
        // 使用 try_recv() 非阻塞地检查是否有新命令
        if let Ok(cmd) = cmd_rx.try_recv() {
            // 如果是停止命令，就退出循环
            if matches!(cmd, BackendCommand::StopDynamicExperiment) {
                info!("[动态实验] 收到终止命令，退出循环。");
                update_tx
                    .send(FrontendUpdate::StatusMessage(
                        "动态实验已手动停止".to_string(),
                    ))
                    .unwrap();
                break;
            }
        }

        // 检查是否已采集所有样本点
        if result_index > params.sample_points as usize {
            update_tx
                .send(FrontendUpdate::StatusMessage(
                    "已采集所有样本点，实验自动结束".to_string(),
                ))
                .unwrap();
            break;
        }

        // --- 执行单步测量逻辑 (与之前版本相同) ---
        let frame = match get_latest_frame(state) {
            Ok(f) => f,
            Err(_) => {
                thread::sleep(Duration::from_millis(100));
                continue;
            }
        };
        let model = state.fitted_model.as_ref().unwrap();

        if let Ok(prediction) = predict_from_frame(&frame, model) {
            predictions.pop_front();
            predictions.push_back(prediction);
        }

        let pred_slice = predictions.make_contiguous();
        let step_angle_is_positive = params.step_angle > 0.0;
        let trigger_logic = step_angle_is_positive != is_ama;

        let mut triggered = false;
        if trigger_logic {
            if pred_slice == [0, 1, 1, 1, 1] || pred_slice == [1, 1, 1, 1, 1] {
                triggered = true;
            }
        } else {
            if pred_slice == [1, 0, 0, 0, 0] || pred_slice == [0, 0, 0, 0, 0] {
                triggered = true;
            }
        }

        if triggered {
            let elapsed_time = start_time.elapsed().as_secs_f64();

            let result = crate::communication::DynamicResult {
                index: result_index,
                time: elapsed_time,
                steps: step_count,
                angle: step_count as f32 / 746.0,
            };
            state.dynamic_results.push(result);
            update_tx.send(FrontendUpdate::DynamicResultsUpdated(state.dynamic_results.clone())).unwrap();
            result_index += 1;

            if let Some(port) = &mut state.serial_port {
                let steps_to_rotate = (params.step_angle * 746.0).round() as i32;
                precision_rotate(port.as_mut(), params.step_angle, is_ama, false).unwrap_or(());
                step_count += steps_to_rotate;
                update_tx
                    .send(FrontendUpdate::CurrentAngleUpdate(
                        step_count as f32 / 746.0,
                    ))
                    .unwrap();
            }

            thread::sleep(Duration::from_secs(1));
        }

        thread::sleep(Duration::from_millis(50));
    }

    // 循环结束后发送完成信号
    update_tx
        .send(FrontendUpdate::DynamicExperimentFinished)
        .unwrap();
    info!("[后端] 动态实验循环正常退出。");
}

// --- Data Processing ---
/// 从 .xlsx 文件加载用于绘图的数据
fn load_data_for_plotting(path: &PathBuf, update_tx: &Sender<FrontendUpdate>) {
    info!("[后端] 开始加载绘图数据于: {:?}", path);

    let mut workbook: Xlsx<_> = match open_workbook(path) {
        Ok(book) => book,
        Err(e) => {
            let msg = format!("打开文件失败: {}", e);
            error!("[后端] {}", msg);
            update_tx.send(FrontendUpdate::StatusMessage(msg)).unwrap();
            return;
        }
    };

    // 读取第一个工作表
    if let Some(Ok(range)) = workbook.worksheet_range_at(0) {
        let mut data: Vec<(f64, i32, f64)> = Vec::new();
        // 跳过第一行（表头）
        for row in range.rows().skip(1) {
            // 假设列顺序为: time, steps, angles
            let time = row.get(0).and_then(|c| c.get_float()).unwrap_or(0.0);
            let steps = row.get(1).and_then(|c| c.get_int()).unwrap_or(0) as i32;
            let angle = row.get(2).and_then(|c| c.get_float()).unwrap_or(0.0);
            data.push((time, steps, angle));
        }

        info!("[后端] 成功读取 {} 条数据用于绘图", data.len());
        update_tx
            .send(FrontendUpdate::StatusMessage(format!(
                "数据加载成功, 共 {} 条记录",
                data.len()
            )))
            .unwrap();
        update_tx
            .send(FrontendUpdate::PlottingDataReady(Arc::new(data)))
            .unwrap();
    } else {
        let msg = "无法读取工作表".to_string();
        error!("[后端] {}", msg);
        update_tx.send(FrontendUpdate::StatusMessage(msg)).unwrap();
    }
}
fn calculate_regression(alpha_inf: f64, mode: RegressionMode, update_tx: &Sender<FrontendUpdate>) {
    info!(
        "[Backend] Calculating regression. Alpha_inf: {}, Mode: {:?}",
        alpha_inf, mode
    );
    let result_formula = format!("y = -0.05x + 5.5\nR² = 0.998");
    update_tx
        .send(FrontendUpdate::RegressionResult(result_formula))
        .unwrap();
}
// pub fn run_single_measurement(
//     state: &Arc<Mutex<BackendState>>,
//     tx: &Sender<Update>,
//     token: CancellationToken,
// ) -> Result<()> {
//     // if state.lock().training.fitted_model.is_none() || state.lock().devices.camera_manager.is_none() || state.lock().devices.serial_port.is_none()
//     // {
//     //     tx.send(Update::Measurement(MeasurementUpdate::StaticStatus("设备未就绪".to_string())))?;
//     //     return;
//     // }

//     // 检查先决条件
//     {
//         let s = state.lock();
//         if s.training.fitted_model.is_none()
//             || s.devices.camera_manager.is_none()
//             || s.devices.serial_port.is_none()
//             || s.measurement.current_static_steps.is_none()
//         {
//             return Err(anyhow!("设备或模型未就绪"));
//         }
//     }
//     info!("开始测试...");
//     tx.send(Update::Measurement(MeasurementUpdate::StaticStatus(
//         "开始测试...".to_string(),
//     )))?;
//     // -------------------------------------------------------------------
//     // -- 此处省略了与硬件交互和模型预测的核心循环逻辑 --
//     // info!("正在执行 find_zero_point 逻辑（已省略）...");
//     // // -------------------------------------------------------------------

//     // let mut prediction = match predict_from_frame(&initial_frame, model) { Ok(p) => p, Err(_) => return };
//     let mut predictions: VecDeque<usize> = VecDeque::from(vec![2; 5]);
//     let mut measured_steps = 0;
//     let timeout = Duration::from_secs(60);
//     let start_time = Instant::now();
//     let mut first = 2;
//     let mut result1: Option<i32> = None;
//     let mut result2: Option<i32> = None;

//     loop {
//         let mut s = state.lock();
//         if start_time.elapsed() > timeout || token.load(Ordering::Relaxed) {
//             tx.send(Update::Measurement(MeasurementUpdate::StaticStatus(
//                 "超时".to_string(),
//             )))?;
//             // s.measurement.current_static_steps = None;
//             s.measurement.static_task_token = None;
//             tx.send(Update::Measurement(MeasurementUpdate::CurrentSteps(
//                 state.lock().measurement.current_static_steps,
//             )))?;
//             return Ok(());
//         }
//         info!("ok");
//         let model = s.training.fitted_model.as_ref().unwrap();
//         let frame = s
//             .devices
//             .camera_manager
//             .as_ref()
//             .unwrap()
//             .latest_frame
//             .lock()
//             .clone();
//         let frame = match (frame) {
//             Some(f) => f,
//             None => continue,
//         };
//         let model = s.training.fitted_model.as_ref().unwrap();
//         let guard2 = s.devices.camera_settings.lock();
//         let circle = {
//             if guard2.lock_circle {
//                 guard2.locked_circle
//             } else {
//                 None
//             }
//         };
//         let min_radius = guard2.min_radius;
//         let max_radius = guard2.max_radius;
//         drop(guard2);
//         let prediction = match predict_from_frame(&frame, model, min_radius, max_radius, circle) {
//             Ok(p) => p,
//             Err(_) => continue,
//         };

//         predictions.pop_front();
//         predictions.push_back(prediction);
//         info!("{:?}", predictions);
//         let mut should_break = false;
//         tx.send(Update::Measurement(MeasurementUpdate::StaticStatus(
//             format!("测量中 ({} steps): {:?}", measured_steps, predictions),
//         )))?;
//         let mut pp = predictions.clone();
//         let pred_slice = pp.make_contiguous();
//         if first == 2 {
//             first = prediction;
//         }
//         let isama = s.rotation_direction_is_ama;
//         drop(s);
//         // thread::sleep(Duration::from_millis(500));(- = 1 0)

//         if (pred_slice == [0, 0, 0, 1, 1]) {
//             // (已修正) 使用 != 进行逻辑异或
//             // let is_ama_logic = (prediction == 1) != s.rotation_direction_is_ama;
//             // let correction_cmd = if is_ama_logic { 114 } else { 55 };
//             if !isama {
//                 step_move(state, tx, MoveMode::ResetBackward)?;
//             } else {
//                 step_move(state, tx, MoveMode::ResetForward)?;
//             }
//             if result1.is_none() {
//                 result1 = { Some(state.lock().measurement.current_static_steps.unwrap()) };
//                 first = 2;
//                 predictions = VecDeque::from(vec![2; 5]);
//                 precision_rotate(state, tx, -700, false);
//             } else {
//                 result2 = { Some(state.lock().measurement.current_static_steps.unwrap()) };
//                 should_break = true;
//             }
//             thread::sleep(Duration::from_millis(150));
//         } else if (pred_slice == [1, 1, 1, 0, 0]) {
//             if isama {
//                 step_move(state, tx, MoveMode::ResetBackward)?;
//             } else {
//                 step_move(state, tx, MoveMode::ResetForward)?;
//             }
//             if result1.is_none() {
//                 result1 = Some({ state.lock().measurement.current_static_steps.unwrap() });
//                 first = 2;
//                 predictions = VecDeque::from(vec![2; 5]);
//                 precision_rotate(state, tx, 700, false);
//             } else {
//                 result2 = Some(state.lock().measurement.current_static_steps.unwrap());
//                 should_break = true;
//             }
//             thread::sleep(Duration::from_millis(150));
//         } else if first == 1 {
//             if !isama {
//                 step_move(state, tx, MoveMode::StepForward)?;
//             } else {
//                 step_move(state, tx, MoveMode::StepBackward)?;
//             }

//             // should_break=true;
//             thread::sleep(Duration::from_millis(10));
//         } else {
//             if isama {
//                 step_move(state, tx, MoveMode::StepForward)?;
//             } else {
//                 step_move(state, tx, MoveMode::StepBackward)?;
//             }
//             // should_break=true;
//             thread::sleep(Duration::from_millis(10));
//         }

//         if should_break {
//             break;
//         }
//         if pred_slice == [0, 0, 0, 0, 0] {
//             first = 0;
//         }
//         if pred_slice == [1, 1, 1, 1, 1] {
//             first = 1;
//         }
//     }
//     // state.lock().measurement.current_static_steps = Some(0);
//     if result1.is_some() && result2.is_some() {
//         let st = { state.lock().measurement.current_static_steps.unwrap() };
//         precision_rotate(
//             state,
//             tx,
//             ((((result1.unwrap() + result2.unwrap()) as f64) / 2.0).round() as i32) - st,
//             false,
//         )?;
//     }
//     let mut s = state.lock();
//     tx.send(Update::Measurement(MeasurementUpdate::CurrentSteps(
//         s.measurement.current_static_steps,
//     )))?;
//     info!("寻找零点完成。");
//     let result = StaticResult {
//         index: s.measurement.static_results.len() + 1,
//         steps: s.measurement.current_static_steps.unwrap(),
//         angle: s.measurement.current_static_steps.unwrap() as f32 / 746.0,
//     };
//     s.measurement.static_results.push(result);

//     tx.send(Update::Measurement(MeasurementUpdate::StaticResults(
//         s.measurement.static_results.clone(),
//     )))?;
//     tx.send(Update::Measurement(MeasurementUpdate::StaticStatus(
//         "测量完成".to_string(),
//     )))?;
//     Ok(())
// }

// pub fn pre_rotation(
//     state: &Arc<Mutex<BackendState>>,
//     tx: &Sender<Update>,
//     token: CancellationToken,
// ) -> Result<()> {
//     // if state.lock().training.fitted_model.is_none() || state.lock().devices.camera_manager.is_none() || state.lock().devices.serial_port.is_none()
//     // {
//     //     tx.send(Update::Measurement(MeasurementUpdate::StaticStatus("设备未就绪".to_string())))?;
//     //     return;
//     // }

//     // 检查先决条件
//     {
//         let s = state.lock();
//         if s.training.fitted_model.is_none()
//             || s.devices.camera_manager.is_none()
//             || s.devices.serial_port.is_none()
//             || s.measurement.current_static_steps.is_none()
//         {
//             return Err(anyhow!("设备或模型未就绪"));
//         }
//     }
//     info!("预旋转...");
//     let mut predictions: VecDeque<usize> = VecDeque::from(vec![2; 5]);
//     let mut measured_steps = 0;
//     let timeout = Duration::from_secs(60);
//     let start_time = Instant::now();
//     let mut first = 2;

//     loop {
//         let mut s = state.lock();
//         if start_time.elapsed() > timeout || token.load(Ordering::Relaxed) {
//             tx.send(Update::Measurement(MeasurementUpdate::StaticStatus(
//                 "超时".to_string(),
//             )))?;
//             // s.measurement.current_static_steps = None;
//             s.measurement.static_task_token = None;
//             tx.send(Update::Measurement(MeasurementUpdate::CurrentSteps(
//                 state.lock().measurement.current_static_steps,
//             )))?;
//             return Ok(());
//         }
//         info!("ok");
//         let model = s.training.fitted_model.as_ref().unwrap();
//         let frame = s
//             .devices
//             .camera_manager
//             .as_ref()
//             .unwrap()
//             .latest_frame
//             .lock()
//             .clone();
//         let frame = match (frame) {
//             Some(f) => f,
//             None => continue,
//         };
//         let model = s.training.fitted_model.as_ref().unwrap();
//         let guard2 = s.devices.camera_settings.lock();
//         let circle = {
//             if guard2.lock_circle {
//                 guard2.locked_circle
//             } else {
//                 None
//             }
//         };
//         let min_radius = guard2.min_radius;
//         let max_radius = guard2.max_radius;
//         drop(guard2);
//         let prediction = match predict_from_frame(&frame, model, min_radius, max_radius, circle) {
//             Ok(p) => p,
//             Err(_) => continue,
//         };

//         predictions.pop_front();
//         predictions.push_back(prediction);
//         info!("{:?}", predictions);
//         let mut should_break = false;
//         tx.send(Update::Measurement(MeasurementUpdate::StaticStatus(
//             format!("测量中 ({} steps): {:?}", measured_steps, predictions),
//         )))?;

//         let pred_slice = predictions.make_contiguous();
//         if first == 2 {
//             first = prediction;
//         }
//         let isama = s.rotation_direction_is_ama;
//         drop(s);
//         // thread::sleep(Duration::from_millis(500));(- = 1 0)

//         if (pred_slice == [0, 0, 0, 1, 1]) {
//             // (已修正) 使用 != 进行逻辑异或
//             // let is_ama_logic = (prediction == 1) != s.rotation_direction_is_ama;
//             // let correction_cmd = if is_ama_logic { 114 } else { 55 };
//             if !isama {
//                 step_move(state, tx, MoveMode::ResetBackward)?;
//             } else {
//                 step_move(state, tx, MoveMode::ResetForward)?;
//             }
//             should_break = true;
//             thread::sleep(Duration::from_millis(150));
//         } else if (pred_slice == [1, 1, 1, 0, 0]) {
//             if isama {
//                 step_move(state, tx, MoveMode::ResetBackward)?;
//             } else {
//                 step_move(state, tx, MoveMode::ResetForward)?;
//             }
//             should_break = true;
//             thread::sleep(Duration::from_millis(150));
//         } else if first == 1 {
//             if !isama {
//                 step_move(state, tx, MoveMode::StepForward)?;
//             } else {
//                 step_move(state, tx, MoveMode::StepBackward)?;
//             }

//             // should_break=true;
//             thread::sleep(Duration::from_millis(10));
//         } else {
//             if isama {
//                 step_move(state, tx, MoveMode::StepForward)?;
//             } else {
//                 step_move(state, tx, MoveMode::StepBackward)?;
//             }
//             // should_break=true;
//             thread::sleep(Duration::from_millis(10));
//         }

//         if should_break {
//             break;
//         }
//         if pred_slice == [0, 0, 0, 0, 0] {
//             first = 0;
//         }
//         if pred_slice == [1, 1, 1, 1, 1] {
//             first = 1;
//         }
//     }
//     // state.lock().measurement.current_static_steps = Some(0);
//     let mut s = state.lock();
//     tx.send(Update::Measurement(MeasurementUpdate::CurrentSteps(
//         s.measurement.current_static_steps,
//     )))?;
//     info!("预旋转完成。");

//     tx.send(Update::Measurement(MeasurementUpdate::StaticStatus(
//         "测量完成".to_string(),
//     )))?;
//     Ok(())
// }