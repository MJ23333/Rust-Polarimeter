mod camera;
mod command;
mod data;
mod measurement;
mod model;
mod recording;
mod serial;

use self::camera::{CameraManager, CameraSettings};
use crate::communication::{
    Command, DataProcessingStateUpdate, DeviceCommand, DeviceUpdate, DynamicExpParams,
    GeneralCommand, GeneralUpdate, MeasurementUpdate, RegressionMode, Update,
};
use crossbeam_channel::{Receiver, Sender};
use parking_lot::Mutex;
use std::thread;
use std::time::Duration;
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};
use tracing::{error, info};
// use self::error::{ BackendError};
use super::communication::{DynamicResult, StaticResult};
use anyhow::Result;
use linfa_logistic::FittedLogisticRegression;

// use std::sync::atomic::{AtomicBool, Ordering}; // 引入 Ordering
use std::thread::JoinHandle;

pub struct BackgroundTask {
    handle: JoinHandle<()>,
    // 每个任务有自己的取消令牌，用于单独取消
    cancellation_token: CancellationToken,
}

pub type CancellationToken = Arc<AtomicBool>;

pub struct DeviceState {
    camera_manager: Option<CameraManager>,
    serial_port: Option<Arc<Mutex<Box<dyn serialport::SerialPort>>>>,
    camera_settings: Arc<Mutex<CameraSettings>>,
}
// --- NEW: State for the recording task ---
pub struct RecordingState {
    pub cancellation_token: Option<CancellationToken>,
    // 用于“倒带”功能，记录录制期间电机转动的总步数
    pub steps_moved: i32,
}

pub struct TrainingState {
    mam_images: Vec<Vec<u8>>,
    ama_images: Vec<Vec<u8>>,
    persistent_mam: Vec<Vec<u8>>,
    persistent_ama: Vec<Vec<u8>>,
    fitted_model: Option<FittedLogisticRegression<f64, usize>>,
}

impl TrainingState {
    fn new() -> Self {
        Self {
            mam_images: Vec::new(),
            ama_images: Vec::new(),
            persistent_mam: Vec::new(),
            persistent_ama: Vec::new(),
            fitted_model: None,
        }
    }
}

pub struct MeasurementState {
    current_steps: Option<i32>,
    static_results: Vec<StaticResult>,
    static_task_token: Option<CancellationToken>,
    dynamic_results: Vec<DynamicResult>,
    dynamic_task_token: Option<CancellationToken>,
    dynamic_time: Option<std::time::Instant>,
    dynamic_params: DynamicExpParams,
}
#[derive(Clone, Debug)]
pub struct DataProcessingState {
    pub raw_data: Option<Vec<(f64, i32, f64, bool)>>, // time, steps, angle
    pub alpha_inf: f64,
    pub regression_mode: RegressionMode,
    // Calculated results are also part of the state
    pub regression_formula: String,
    pub plot_scatter_points: Vec<(f64, f64)>, // --- NEW ---
    pub plot_line_points: Vec<(f64, f64)>,
}

impl DataProcessingState {
    fn new() -> Self {
        Self {
            raw_data: None,
            alpha_inf: 0.0,
            regression_mode: RegressionMode::Log, // Default mode
            regression_formula: String::new(),
            plot_scatter_points: Vec::new(), // --- NEW ---
            plot_line_points: Vec::new(),
        }
    }
}

pub struct BackendState {
    pub devices: DeviceState,
    pub recording: RecordingState,
    pub training: TrainingState,
    pub measurement: MeasurementState,
    pub data_processing: DataProcessingState,
    pub rotation_direction_is_ama: bool,
    pub rotation_direction_need_reverse: bool,
    // --- NEW: 统一的任务管理器 ---
    // --- NEW: 全局关停信号 ---
    pub shutdown_signal: CancellationToken,
}
impl From<DataProcessingState> for DataProcessingStateUpdate {
    fn from(dp_state: DataProcessingState) -> Self {
        Self {
            raw_data: Arc::new(dp_state.raw_data.unwrap_or_default()),
            alpha_inf: dp_state.alpha_inf,
            regression_mode: dp_state.regression_mode,
            regression_formula: dp_state.regression_formula,
            plot_line_points: dp_state.plot_line_points,
            plot_scatter_points: dp_state.plot_scatter_points,
        }
    }
}

impl BackendState {
    fn new() -> Self {
        Self {
            devices: DeviceState {
                camera_manager: None,
                serial_port: None,
                camera_settings: Arc::new(Mutex::new(CameraSettings {
                    exposure: -8.0,
                    lock_circle: false,
                    locked_circle: None,
                    min_radius: 30,
                    max_radius: 45,
                })),
            },
            recording: RecordingState {
                // --- NEW ---
                cancellation_token: None,
                steps_moved: 0,
            },
            training: TrainingState::new(),
            measurement: MeasurementState {
                current_steps: None,
                static_results: Vec::new(),
                static_task_token: None,
                dynamic_results: Vec::new(),
                dynamic_task_token: None,
                dynamic_time: None,
                dynamic_params: DynamicExpParams {
                    path: PathBuf::new(),
                    temperature: 25.0,
                    sucrose_conc: 0.0,
                    hcl_conc: 0.0,
                    pre_rotation_angle: 5.0,
                    step_angle: -0.5,
                    sample_points: 12,
                },
            },
            data_processing: DataProcessingState::new(),
            rotation_direction_is_ama: false,
            rotation_direction_need_reverse: false,
            shutdown_signal: Arc::new(AtomicBool::new(false)),
        }
    }
}

/// 后端主循环 (修正后的最终版)
pub fn backend_loop(cmd_rx: Receiver<Command>, update_tx: Sender<Update>) {
    info!("后端线程已启动");
    let mut active_tasks: Vec<BackgroundTask> = Vec::new();
    let state = Arc::new(Mutex::new(BackendState::new()));
    let global_shutdown_signal = state.lock().shutdown_signal.clone();

    if true {
        // 为监控线程创建专属的取消令牌
        let monitor_token = Arc::new(AtomicBool::new(false));
        let state_for_monitor = Arc::clone(&state);
        let token_for_monitor = monitor_token.clone(); // 克隆 token 以移入线程
        let tx = update_tx.clone();

        info!("正在启动状态监控线程...");
        let monitor_handle = thread::spawn(move || {
            info!("状态监控线程已启动。");
            // 只要未收到取消信号，就持续运行
            let mut times = 1;
            while !token_for_monitor.load(Ordering::Relaxed) {
                {
                    // 使用独立的块来限制 MutexGuard 的生命周期
                    // 在这里获取 state 的锁
                    let mut s = state_for_monitor.lock();
                    if s.devices.serial_port.is_none() {
                        let _ =
                            tx.send(Update::Device(DeviceUpdate::SerialConnectionStatus(false)));
                        s.measurement.current_steps = None;
                        let _ = tx.send(Update::Measurement(MeasurementUpdate::CurrentSteps(
                            s.measurement.current_steps,
                        )));
                        drop(s);
                        // info!("串口断开");
                    } else if times % 10 == 0 {
                        let port = s.devices.serial_port.as_mut().unwrap().clone();
                        drop(s);
                        let _=measurement::cmd(port, 77 as u8);
                    } else {
                        drop(s);
                    }
                    let mut s = state_for_monitor.lock();
                    if s.devices.camera_manager.is_none() {
                        s.devices.camera_manager = None;
                        // info!("相机断开");
                        let _ =
                            tx.send(Update::Device(DeviceUpdate::CameraConnectionStatus(false)));
                    } else {
                        let frame = {
                            s.devices
                                .camera_manager
                                .as_ref()
                                .unwrap()
                                .latest_frame
                                .lock()
                                .clone()
                        };
                        match frame {
                            Some(_) => {}
                            None => {
                                // info!("相机断开");
                                s.devices.camera_manager = None;
                                let _ = tx.send(Update::Device(
                                    DeviceUpdate::CameraConnectionStatus(false),
                                ));
                            }
                        };
                    }

                    // TODO: 在这里执行对 state_guard 中数据的检查逻辑
                    // 例如: if state_guard.measurement.some_field > threshold { ... }
                    // 锁会在这个块的末尾自动释放，这很重要，
                    // 因为我们不应该在持有锁的时候睡眠。
                }
                // info!("OK");
                // 线程休眠一秒
                thread::sleep(Duration::from_secs(1));
                times += 1;
            }
            info!("状态监控线程已关停。");
        });

        // 将监控线程作为一个后台任务加入到任务管理器中
        // 这样它就能和命令产生的其他任务一样，被统一地关停
        active_tasks.push(BackgroundTask {
            handle: monitor_handle,
            cancellation_token: monitor_token,
        });
    }
    // 当主循环退出时，state 的最后一个 Arc 将被销毁，
    // 其内部的 active_tasks 会被 drop，进而 join 所有的 handle。
    while !global_shutdown_signal.load(Ordering::Relaxed) {
        if let Ok(command) = cmd_rx.recv_timeout(Duration::from_millis(200)) {
            // 如果是关停命令，直接在这里处理，然后跳出循环
            if matches!(&command, Command::General(GeneralCommand::Shutdown)) {
                info!("收到关停指令，将触发全局关停信号。");
                global_shutdown_signal.store(true, Ordering::Relaxed);
                continue; // 继续循环，下一次迭代将因为 while 条件不满足而退出
            }

            // 清理已完成的旧任务
            active_tasks.retain(|task| !task.handle.is_finished());

            // 为新任务创建一个独有的取消令牌
            let task_token = Arc::new(AtomicBool::new(false));

            let state_clone = Arc::clone(&state);
            let update_tx_clone = update_tx.clone();
            let token_clone = task_token.clone();

            // 为每个命令创建一个工作线程
            let handle = thread::spawn(move || {
                // 在这个新线程里直接执行命令，并传入它的取消令牌
                let result =
                    dispatch_command(command, state_clone, update_tx_clone.clone(), token_clone);

                // 错误处理...
                if let Err(e) = result {
                    let error_msg = format!("执行命令时出错: {}", e);
                    error!("{}", error_msg);
                    let _ = update_tx_clone.send(Update::General(GeneralUpdate::Error(error_msg)));
                }
            });

            // 将新任务的 handle 和 token 注册到状态中
            active_tasks.push(BackgroundTask {
                handle,
                cancellation_token: task_token,
            });
        }
    }

    // --- 关停流程 (与之前相同) ---
    // --- 开始关停流程 ---
    {
        let mut state_guard = state.lock();

        // 1. 停止并清理 CameraManager (因为它有自己的线程)
        info!("正在关闭相机管理器...");
        state_guard.devices.camera_manager = None;
    }

    // 2. 向所有活动任务发送取消信号
    info!("向 {} 个活动任务发送停止信号...", active_tasks.len());
    for task in &active_tasks {
        task.cancellation_token.store(true, Ordering::Relaxed);
    }

    // 3. 等待所有任务线程结束
    // 我们需要 take 走 handles 来 join 它们，这会清空 active_tasks
    let tasks_to_join = std::mem::take(&mut active_tasks);
    info!("等待 {} 个任务线程结束...", tasks_to_join.len());
    for (i, task) in tasks_to_join.into_iter().enumerate() {
        if let Err(e) = task.handle.join() {
            error!("等待任务 {} 时发生错误: {:?}", i, e);
        } else {
            info!("任务 {} 已成功结束", i);
        }
    }

    info!("后端线程已完全清理并终止");
}

fn dispatch_command(
    command: Command,
    state: Arc<Mutex<BackendState>>,
    update_tx: Sender<Update>,
    token: CancellationToken, // <--- 新增参数
) -> Result<()> {
    match command {
        Command::General(cmd) => command::handle_general(cmd, state, &update_tx, token),
        Command::Device(cmd) => command::handle_device(cmd, state, &update_tx, token),
        Command::Camera(cmd) => command::handle_camera(cmd, state, &update_tx, token),
        Command::Training(cmd) => command::handle_training(cmd, state, &update_tx, token),
        Command::StaticMeasure(cmd) => {
            command::handle_static_measure(cmd, state, &update_tx, token)
        }
        Command::DynamicMeasure(cmd) => {
            command::handle_dynamic_measure(cmd, state, &update_tx, token)
        }
        Command::DataProcessing(cmd) => {
            command::handle_data_processing(cmd, state, &update_tx, token)
        }
    }
}
