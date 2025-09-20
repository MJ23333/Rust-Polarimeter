use super::{Arc, BackendState, CancellationToken, Mutex};
use crate::communication::*;
use anyhow::Result;
use calamine::{DataType, Reader};
use crossbeam_channel::Sender;
use std::sync::atomic::Ordering;
use tracing::info;

fn send_status<S: Into<String>>(tx: &Sender<Update>, msg: S) -> Result<()> {
    tx.send(Update::General(GeneralUpdate::StatusMessage(msg.into())))?;
    Ok(())
}

pub fn handle_general(
    cmd: GeneralCommand,
    _state: Arc<Mutex<BackendState>>,
    _tx: &Sender<Update>,
    _token: CancellationToken,
) -> Result<()> {
    match cmd {
        GeneralCommand::Shutdown => {
            info!("收到关闭指令 (逻辑待实现)");
        }
    }
    Ok(())
}

pub fn handle_device(
    cmd: DeviceCommand,
    state: Arc<Mutex<BackendState>>,
    tx: &Sender<Update>,
    token: CancellationToken,
) -> Result<()> {
    match cmd {
        DeviceCommand::RefreshSerialPorts => {
            let ports = super::serial::get_available_ports(token);
            tx.send(Update::Device(DeviceUpdate::SerialPortsList(ports)))?;
        }
        DeviceCommand::ConnectSerial { port, baud_rate } => {
            super::serial::connect(&state, port, baud_rate, &tx)?;
        }
        DeviceCommand::DisconnectSerial => {
            super::serial::disconnect(&state)?;
            tx.send(Update::Device(DeviceUpdate::SerialConnectionStatus(false)))?;
            info!("串口已断开");
        }
        DeviceCommand::TestSerial => {
            super::serial::test(&state, &tx)?;
        }
        DeviceCommand::RotateMotor { steps } => {
            // let reverse={state.lock().rotation_direction_need_reverse};
            super::measurement::precision_rotate(&state, tx, steps)?;
        }
        DeviceCommand::RotateTo { steps } => {
            // super::serial::rotate_motor(&state, angle)?;
            // let reverse={state.lock().rotation_direction_need_reverse};
            super::measurement::precision_rotate_to(&state, tx, steps)?;
        }
        DeviceCommand::SetRotationDirection(is_ama) => {
            state.lock().rotation_direction_is_ama = is_ama;
            let dir = if is_ama { "AMA" } else { "MAM" };
            info!("旋光仪模式已设置为 {}", dir);
        }
        DeviceCommand::SetRotationReverse(is_ama) => {
            state.lock().rotation_direction_need_reverse = is_ama;
        }
        DeviceCommand::StartRecording {
            mode,
            save_path,
            num,
        } => {
            // --- 这是命令处理线程，它现在将成为录制线程 ---

            // 1. 检查是否已有录制任务
            // let mut state_guard = state.lock();
            {
                if state.lock().recording.cancellation_token.is_some() {
                    return Ok(());
                }
            }

            // 2. 创建一个专门用于停止本次录制的取消令牌

            // 3. 将令牌存入共享状态，以便 StopRecording 命令可以找到它
            state.lock().recording.cancellation_token = Some(token.clone());
            state.lock().recording.steps_moved = 0;

            // 必须在调用阻塞函数前释放锁，否则 StopRecording 命令将永远无法获取锁
            // drop(state_guard);

            // 4. 直接、阻塞地调用录制循环。
            //    这个 command-thread 会在这里暂停，直到录制结束或被取消。
            super::recording::record_video_loop(&state, &tx, save_path, mode, num, token)?;
        }
        DeviceCommand::StopRecording => {
            // let mut state_guard = state.lock();
            // send_status(&tx, "正在停止录制...")?;
            info!("正在停止录制");
            // info!("{:?}", &state.lock().recording.cancellation_token);
            if let Some(token) = &state.lock().recording.cancellation_token {
                // 设置标志，通知录制线程退出循环
                token.store(true, Ordering::Relaxed);
                // 执行“倒带”逻辑
                // let steps_to_rewind = state_guard.recording.steps_moved;
                // if steps_to_rewind != 0 {
                //     info!("录制停止，电机倒带 {} 步", -steps_to_rewind);
                //     if let Err(e) = super::serial::precision_rotate_steps(&state, -steps_to_rewind) {
                //         error!("电机倒带失败: {}", e);
                //     }
                // }
            } else {
                info!("没有录制任务，何谈停止？");
            }
        }
        DeviceCommand::FindZeroPoint => {
            super::measurement::static_measurement(&state, &tx, token, true,1)?;
        }
        DeviceCommand::ReturnToZero => {
            // send_status(&tx, "正在返回零点...")?;
            if state.lock().measurement.static_task_token.is_none()
                && state.lock().measurement.dynamic_task_token.is_none()
            {
                super::measurement::return_to_zero(&state, &tx)?;
            } else {
                tx.send(Update::General(GeneralUpdate::Error(format!(
                    "请先停止测量任务"
                ))))?;
            }

            // send_status(&tx, "已返回零点")?;
        }
        _ => info!("收到未实现的 DeviceCommand"),
    }
    Ok(())
}

pub fn handle_camera(
    cmd: CameraCommand,
    state: Arc<Mutex<BackendState>>,
    tx: &Sender<Update>,
    _token: CancellationToken,
) -> Result<()> {
    match cmd {
        CameraCommand::Connect { index } => {
            info!("正在连接相机 {}...", index);
            super::camera::connect_camera(&state, index, tx)?;
        }
        CameraCommand::Disconnect => {
            info!("正在断开相机...");
            super::camera::disconnect_camera(&state)?;
            tx.send(Update::Device(DeviceUpdate::CameraConnectionStatus(false)))?;
        }
        CameraCommand::RefreshCameras => {
            super::camera::refresh_cameras(tx)?;
            // tx.send(Update::Device(DeviceUpdate::CameraConnectionStatus(false)))?;
        }
        CameraCommand::SetHoughCircleRadius { min, max } => {
            // --- 实时更新逻辑 ---
            let state_guard = state.lock();
            let mut settings = state_guard.devices.camera_settings.lock();
            settings.min_radius = min as i32;
            settings.max_radius = max as i32;
            // info!("霍夫圆半径已更新为: min={}, max={}", min, max);
        }
        CameraCommand::SetLock(value) => {
            // --- 实时更新逻辑 ---
            let state_guard = state.lock();
            let mut settings = state_guard.devices.camera_settings.lock();
            settings.lock_circle = value;
            info!("圆锁定状态已更新为: {}", value);
        } //_ => info!("收到未实现的 CameraCommand"),
    }
    Ok(())
}

pub fn handle_training(
    cmd: TrainingCommand,
    state: Arc<Mutex<BackendState>>,
    tx: &Sender<Update>,
    token: CancellationToken,
) -> Result<()> {
    match cmd {
        // TrainingCommand::ProcessVideo { video_path, mode } => {
        //     super::model::process_video_for_training(&state, &video_path, &mode, &tx, token)?;
        // }
        TrainingCommand::LoadRecordedDataset { path } => {
            super::model::load_recorded_dataset(&state, &path, &tx)?;
        }
        TrainingCommand::TrainModel { show_roc, show_cm } => {
            super::model::train_model(&state, show_roc, show_cm, &tx)?;
        }
        TrainingCommand::LoadPersistentDataset { path } => {
            super::model::load_persistent_dataset(&state, &path, &tx)?;
        }
        TrainingCommand::ResetModel => {
            super::model::reset_model(&state, &tx)?;
        }
        TrainingCommand::ResetPersistentDataset => {
            state.lock().training.persistent_ama.clear();
            state.lock().training.persistent_mam.clear();
            info!("常驻数据集已重置");
        }
        TrainingCommand::ResetRecordedDataset => {
            state.lock().training.mam_images.clear();
            state.lock().training.ama_images.clear();
            info!("录制数据集已重置");
        }
        // TrainingCommand::LoadModel { path } => {
        //     if let Some(model)=state.lock().training.fitted_model{
        //        let x=bincode::serialize(&model);
        //     }
        // }
        // TrainingCommand::SaveModel { path } =>{

        // }
        _ => info!("收到未实现的 TrainingCommand"),
    }
    Ok(())
}

pub fn handle_static_measure(
    cmd: StaticMeasureCommand,
    state: Arc<Mutex<BackendState>>,
    tx: &Sender<Update>,
    token: CancellationToken,
) -> Result<()> {
    match cmd {
        StaticMeasureCommand::RunSingleMeasurement{time} => {
            if super::measurement::static_measurement(&state, &tx, token, false,time).is_err() {
                state.lock().measurement.static_task_token = None;
                tx.send(Update::Measurement(MeasurementUpdate::StaticRunning(false)))?;
            }
        }
        StaticMeasureCommand::ClearResults => {
            let mut s = state.lock();
            s.measurement.static_results.clear();
            tx.send(Update::Measurement(MeasurementUpdate::StaticResults(
                vec![],
            )))?;
            info!("静态测量结果已清除")
        }
        StaticMeasureCommand::SaveResults { path } => {
            super::measurement::save_static(&state, path, &tx)?;
            info!("静态测量结果已储存")
        }
        StaticMeasureCommand::Stop => {
            if let Some(stoptoken) = &state.lock().measurement.static_task_token {
                stoptoken.store(true, Ordering::Relaxed);
                info!("已发送停止信号");
            } else {
                info!("没有正在运行的静态实验");
            }
        } //_ => info!("收到未实现的 StaticMeasureCommand"),
    }
    Ok(())
}

pub fn handle_dynamic_measure(
    cmd: DynamicMeasureCommand,
    state: Arc<Mutex<BackendState>>,
    tx: &Sender<Update>,
    token: CancellationToken,
) -> Result<()> {
    match cmd {
        DynamicMeasureCommand::Start  => {
            // let token = Arc::new(AtomicBool::new(false));
            // state.lock().measurement.dynamic_task_token = Some(token.clone());
            // 这个函数是阻塞的，但它运行在自己的线程里
            super::measurement::run_dynamic_experiment_loop(&state, &tx, token)?;
        }
        DynamicMeasureCommand::UpdateParams { params }=>{
            state.lock().measurement.dynamic_params=params;
        }
        DynamicMeasureCommand::Stop => {
            if let Some(token) = &state.lock().measurement.dynamic_task_token {
                token.store(true, Ordering::Relaxed);
                info!("已发送停止信号");
            } else {
                info!("没有正在运行的动态实验");
            }
        }
        DynamicMeasureCommand::StartNew => {
            let mut s = state.lock();
            if s.measurement.dynamic_task_token.is_none() {
                s.measurement.dynamic_results.clear();
                s.measurement.dynamic_time = Some(std::time::Instant::now());
                tx.send(Update::Measurement(MeasurementUpdate::DynamicResults(
                    s.measurement.dynamic_results.clone(),
                )))?;
                tx.send(Update::Measurement(MeasurementUpdate::StartTime(
                    s.measurement.dynamic_time.clone(),
                )))?;
                info!("开始新动态试验");
            } else {
                info!("请先关闭动态追踪");
            }
        }
        DynamicMeasureCommand::ClearResults => {
            let mut s = state.lock();
            s.measurement.dynamic_results.clear();
            tx.send(Update::Measurement(MeasurementUpdate::DynamicResults(
                s.measurement.dynamic_results.clone(),
            )))?;
            info!("动态测量结果已清除");
        }
    }
    Ok(())
}

pub fn handle_data_processing(
    cmd: DataProcessingCommand,
    state: Arc<Mutex<BackendState>>,
    tx: &Sender<Update>,
    _token: CancellationToken, // Token can be used for long file loads if needed
) -> Result<()> {
    let mut state_guard = state.lock();

    match cmd {
        DataProcessingCommand::LoadData { path } => {
            info!("正在加载数据");
            let mut workbook: calamine::Xlsx<_> = calamine::open_workbook(path)?;

            if let Some(Ok(range)) = workbook.worksheet_range_at(0) {
                let mut data: Vec<(f64, i32, f64, bool)> = Vec::new();
                for row in range.rows().skip(1) {
                    // 改进后的方式
                    let time_opt = row.get(1).and_then(|c| c.get_float());
                    let steps_opt = row.get(2).and_then(|c| c.get_float());
                    let angle_opt = row.get(3).and_then(|c| c.get_float());
                    // info!("{:?} {:?} {:?}",time_opt,steps_opt,angle_opt);
                    if let (Some(time), Some(steps), Some(angle)) = (time_opt, steps_opt, angle_opt)
                    {
                        data.push((time, steps.round() as i32, angle, false));
                    }
                }
                // Update the state
                state_guard.data_processing.raw_data = Some(data);
                info!("数据加载成功");
            }
        }
        DataProcessingCommand::SetAlphaInf { alpha } => {
            state_guard.data_processing.alpha_inf = alpha;
        }
        DataProcessingCommand::SetRegressionMode { mode } => {
            state_guard.data_processing.regression_mode = mode;
        }
    }

    // After ANY state change, recalculate and push a full update
    super::data::recalculate_and_update(&mut state_guard, &tx)?;

    Ok(())
}
