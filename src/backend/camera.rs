use super::{Arc, BackendState, Mutex};
use crate::communication::{DeviceUpdate, Update};
use anyhow::Result;
use crossbeam_channel::Sender;
use opencv::{core, imgproc, prelude::*, videoio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};
const TARGET_FRAME_DURATION: Duration = Duration::from_millis(33);
use tracing::{error, info};

#[derive(Clone, Debug, Default)]
pub struct CameraSettings {
    pub exposure: f64,
    pub show_circle: bool,
    pub lock_circle: bool,
    pub locked_circle: Option<(i32, i32, i32)>,
    pub min_radius: i32,
    pub max_radius: i32,
}

pub struct CameraManager {
    thread_handle: Option<thread::JoinHandle<()>>,
    stop_signal: Arc<AtomicBool>,
    pub latest_frame: Arc<Mutex<Option<Mat>>>,
}

impl CameraManager {
    pub fn new(
        camera_index: i32,
        update_tx: Sender<Update>,
        settings: Arc<Mutex<CameraSettings>>,
    ) -> Result<Self> {
        let stop_signal = Arc::new(AtomicBool::new(false));
        let thread_stop_signal = stop_signal.clone();
        let latest_frame = Arc::new(Mutex::new(None));

        let thread_handle = {
            let thread_latest_frame = latest_frame.clone();
            thread::spawn(move || {
                let mut cam = match videoio::VideoCapture::new(camera_index, videoio::CAP_ANY) {
                    Ok(cam) => {
                        if !cam.is_opened().unwrap_or(false) {
                            error!("后端：无法打开相机索引 {}", camera_index);
                            let _ = update_tx
                                .send(Update::Device(DeviceUpdate::CameraConnectionStatus(false)));
                            return;
                        }
                        info!("后端：相机 {} 已成功在捕获线程中打开", camera_index);
                        let _ = update_tx
                            .send(Update::Device(DeviceUpdate::CameraConnectionStatus(true)));
                        cam
                    }
                    Err(e) => {
                        error!("后端：创建VideoCapture失败：{}", e);
                        let _ = update_tx
                            .send(Update::Device(DeviceUpdate::CameraConnectionStatus(false)));
                        return;
                    }
                };
                // let mut consecutive_read_errors = 0;
                while !thread_stop_signal.load(Ordering::Relaxed) {
                    let mut frame = Mat::default();
                    let start_time = Instant::now();

                    // if cam.set(videoio::CAP_PROP_AUTO_EXPOSURE, 0.0).is_err() && cam.set(videoio::CAP_PROP_EXPOSURE, settings_guard.exposure).is_err(){
                    //         error!("曝光设置失败");
                    // }
                    if let Ok(true) = cam.read(&mut frame) {
                        // consecutive_read_errors = 0;
                        // if getframe {
                        if frame.empty() {
                            // info!("相机断开4");
                            continue;
                        }
                        let mut processed_frame = frame.clone();

                        *thread_latest_frame.lock() = Some(frame.clone());
                        let (lock_circle, min_radius, max_radius, mut circle) = {
                            let s = settings.lock();
                            (s.lock_circle, s.min_radius, s.max_radius, s.locked_circle)
                        };
                        let res = detect_and_draw_circle(
                            &frame,
                            &mut processed_frame,
                            min_radius,
                            max_radius,
                            circle,
                            lock_circle,
                        );
                        if let Ok(cir) = res {
                            circle = cir;
                            let mut s = settings.lock();
                            s.locked_circle = circle;
                            if let Some(color_image) = mat_to_color_image(processed_frame) {
                                let _ = update_tx.send(Update::Device(
                                    DeviceUpdate::NewCameraFrame(Arc::new(color_image)),
                                ));
                            }
                        }
                    } else {
                        // info!("相机断开3");
                        *thread_latest_frame.lock() = None;
                    }
                    let elapsed = start_time.elapsed();
                    if elapsed < TARGET_FRAME_DURATION {
                        // 只休眠剩余的时间
                        thread::sleep(TARGET_FRAME_DURATION - elapsed);
                    }
                }

                info!("后端：相机捕获线程 {} 已停止", camera_index);
            })
        };

        Ok(Self {
            thread_handle: Some(thread_handle),
            stop_signal,
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

pub fn connect_camera(
    state: &Arc<Mutex<BackendState>>,
    index: usize,
    tx: &Sender<Update>,
) -> Result<()> {
    // 必须显式 drop 旧的 manager，以确保旧线程停止
    let mut state_guard = state.lock();
    state_guard.devices.camera_manager = None;

    // camera_settings 是主状态的一部分，但 camera_manager 不是
    // 这里我们为相机线程创建一个独立的 settings Arc，它在 manager 启动时初始化
    let settings_clone = Arc::clone(&state_guard.devices.camera_settings);

    let manager = CameraManager::new(index as i32, tx.clone(), settings_clone)?;
    state_guard.devices.camera_manager = Some(manager);
    Ok(())
}

pub fn disconnect_camera(state: &Arc<Mutex<BackendState>>) -> Result<()> {
    state.lock().devices.camera_manager = None;
    Ok(())
}
// pub fn set_hough(state: &Arc<Mutex<BackendState>>) -> Result<()> {
//     state.lock().devices.camera_manager = None;
//     Ok(())
// }

pub fn refresh_cameras(update_tx: &Sender<Update>) -> Result<()> {
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
    update_tx
        .send(Update::Device(DeviceUpdate::CameraList(devices)))
        .unwrap();
    Ok(())
}

fn detect_and_draw_circle(
    input: &Mat,
    output: &mut Mat,
    min_radius: i32,
    max_radius: i32,
    cir: Option<(i32, i32, i32)>,
    locked: bool,
) -> Result<Option<(i32, i32, i32)>> {
    if cir.is_some() && locked {
        let circle = cir.unwrap();
        let center = core::Point::new(circle.0, circle.1);
        let radius = circle.2;

        let color = core::Scalar::new(0.0, 0.0, 255.0, 255.0); // Red for locked

        imgproc::circle(output, center, radius, color, 2, imgproc::LINE_AA, 0).unwrap_or(());
        Ok(cir)
    } else {
        let mut gray = Mat::default();
        imgproc::cvt_color(
            input,
            &mut gray,
            imgproc::COLOR_BGR2GRAY,
            0,
            core::AlgorithmHint::ALGO_HINT_DEFAULT,
        )?;

        let mut circles = core::Vector::<core::Vec3f>::new();
        imgproc::hough_circles(
            &gray,
            &mut circles,
            imgproc::HOUGH_GRADIENT,
            1.0,        // dp
            30.0,       // minDist
            40.0,       // param1 (Canny a)
            10.0,       // param2 (Canny b)
            min_radius, // minRadius
            max_radius, // maxRadius
        )?;

        if circles.len() > 0 {
            // 只取第一个检测到的圆
            let circle_params = circles.get(0).unwrap();
            let center = core::Point::new(
                circle_params[0].round() as i32,
                circle_params[1].round() as i32,
            );
            let radius = circle_params[2].round() as i32;

            let color = core::Scalar::new(0.0, 255.0, 0.0, 255.0); // Green for unlocked
            imgproc::circle(output, center, radius, color, 2, imgproc::LINE_AA, 0).unwrap_or(());
            Ok(Some((
                circle_params[0].round() as i32,
                circle_params[1].round() as i32,
                circle_params[2].round() as i32,
            )))
        } else {
            Ok(None)
        }
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
