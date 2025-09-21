use super::model::predict_from_frame;
use super::{Arc, BackendState, CancellationToken, Mutex};
use crate::communication::*;
use anyhow::{anyhow, Result};
use crossbeam_channel::Sender;
use rust_xlsxwriter::{Format, Workbook, XlsxError};
use std::io::{self, BufRead, BufReader};
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::thread;
use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};
use tracing::{error, info};

mod file_saver {
    use super::*;

    pub fn save_static_results(path: &PathBuf, results: &[StaticResult]) -> Result<(), XlsxError> {
        let mut workbook = Workbook::new();
        let worksheet = workbook.add_worksheet();
        worksheet.write_row(0, 0, ["index", "steps", "angle"])?;
        for (i, result) in results.iter().enumerate() {
            worksheet.write(i as u32 + 1, 0, result.index as i32)?;
            worksheet.write(i as u32 + 1, 1, result.steps as i32)?;
            worksheet.write(i as u32 + 1, 2, result.angle as f64)?;
        }
        workbook.save(path)?;
        Ok(())
    }

    pub fn save_dynamic_results(
        path: &PathBuf,
        results: &[DynamicResult],
        params: &DynamicExpParams,
    ) -> Result<(), XlsxError> {
        let mut workbook = Workbook::new();
        let worksheet = workbook.add_worksheet();
        worksheet.write_row(0, 0, ["index", "time", "steps", "angle"])?;
        for (i, result) in results.iter().enumerate() {
            worksheet.write_number(i as u32 + 1, 0, result.index as i32)?;
            worksheet.write_number(i as u32 + 1, 1, result.time)?;
            worksheet.write_number(i as u32 + 1, 2, result.steps as i32)?;
            worksheet.write_number(i as u32 + 1, 3, result.angle as f64)?;
        }
        // --- 2. 在旁边写入实验参数信息 (新增代码) ---
        // 定义参数写入的起始列 (E列留空作为分隔)
        let param_key_col = 5; // F列
        let param_value_col = 6; // G列

        // 创建一个加粗格式用于标签
        let bold_format = Format::new().set_bold();

        // 写入每一项参数，格式为 "标签: 值"
        worksheet.write_string_with_format(0, param_key_col, "实验参数", &bold_format)?;

        worksheet.write_string(2, param_key_col, "实验温度 (°C)")?;
        worksheet.write_number(2, param_value_col, params.temperature)?;

        worksheet.write_string(3, param_key_col, "蔗糖浓度")?;
        worksheet.write_number(3, param_value_col, params.sucrose_conc)?;

        worksheet.write_string(4, param_key_col, "盐酸浓度")?;
        worksheet.write_number(4, param_value_col, params.hcl_conc)?;

        worksheet.write_string(5, param_key_col, "初始旋光角")?;
        worksheet.write_number(5, param_value_col, params.pre_rotation_angle)?;

        worksheet.write_string(6, param_key_col, "步进角")?;
        worksheet.write_number(6, param_value_col, params.step_angle)?;

        worksheet.write_string(7, param_key_col, "采样点数")?;
        worksheet.write_number(7, param_value_col, params.sample_points)?;

        // // --- 3. (可选但推荐) 调整列宽以获得更好的可读性 ---
        // worksheet.set_column_width(0, 3, 12)?; // A-D列宽度
        // worksheet.set_column_width(param_key_col, param_key_col, 15)?; // F列宽度
        // worksheet.set_column_width(param_value_col, param_value_col, 15)?; // G列宽度

        workbook.save(path)?;
        Ok(())
    }
}

pub fn cmd(port_arc: Arc<Mutex<Box<dyn serialport::SerialPort>>>, data: u8) -> Result<()> {
    let mut port = port_arc.lock();
    port.write_all(&[data])?;
    // thread::sleep(Duration::from_millis(10)); // 对应 python code 的 0.01s delay
    // port.write_all(&[100])?; // Stop command
    // info!("ok");
    let mut reader = BufReader::new(&mut *port);
    // wait_for_arduino_signal(port)?;

    let mut response_buffer = String::new();

    // read_line 会阻塞，直到它从串口读取到换行符（0x0A）为止
    match reader.read_line(&mut response_buffer) {
        Ok(_) => {
            if response_buffer.trim() != "1" {
                return Err(anyhow!("回复异常"));
            }
        }
        Err(ref e) if e.kind() == io::ErrorKind::TimedOut => {
            // 如果发生超时，read_line 会返回错误
            return Err(anyhow!("超时"));
        }
        Err(_e) => {
            // 其他读取错误
            return Err(anyhow!("未知错误"));
        }
    }
    // info!("转起来了");
    Ok(())
}

/// `precision_rotate` 的 Rust 实现
pub fn precision_rotate(
    // port: &mut dyn serialport::SerialPort,
    state: &Arc<Mutex<BackendState>>,
    tx: &Sender<Update>,
    steps: i32,
) -> Result<()> {
    let mut steps = steps;
    let mut mul = 1;
    if { state.lock().rotation_direction_need_reverse } {
        steps = -steps;
        mul = -1;
    }
    info!("旋转 {} 步", steps);

    let commands = if steps > 0 {
        vec![62, 60, 58, 56, 64, 66, 68] // 正转指令
    } else {
        steps = -steps;
        mul = mul * -1;
        vec![63, 61, 59, 57, 65, 67, 69] // 反转指令
    };

    let divisors = [3730, 746, 373, 75, 37, 7, 1];

    for i in 0..divisors.len() {
        let num_rotations = steps / divisors[i];
        steps %= divisors[i];
        for _ in 0..num_rotations {
            let mut s = state.lock();
            if s.devices.serial_port.is_none() {
                tx.send(Update::Device(DeviceUpdate::SerialConnectionStatus(false)))?;
                s.measurement.current_steps = None;
                tx.send(Update::Measurement(MeasurementUpdate::CurrentSteps(
                    s.measurement.current_steps,
                )))?;
                return Err(anyhow!("执行失败，请重新连接串口并找零点：串口断开"));
            }
            let port = s.devices.serial_port.as_mut().unwrap().clone();
            drop(s);
            let res = cmd(port, commands[i]);
            if let Err(e) = &res {
                let mut s = state.lock();
                s.devices.serial_port = None;
                tx.send(Update::Device(DeviceUpdate::SerialConnectionStatus(false)))?;
                s.measurement.current_steps = None;
                tx.send(Update::Measurement(MeasurementUpdate::CurrentSteps(
                    s.measurement.current_steps,
                )))?;
                //需要实现串口更新
                error!("执行失败，请重新连接串口并找零点（{}）", e);
                return Err(anyhow!("执行失败，请重新连接串口并找零点（{}）", e));
            } else {
                let mut s = state.lock();
                // info!("金杰活了");
                s.measurement.current_steps =
                    s.measurement.current_steps.map(|s| s + divisors[i] * mul);
                tx.send(Update::Measurement(MeasurementUpdate::CurrentSteps(
                    s.measurement.current_steps,
                )))?;
            }
        }
    }
    info!("旋转完成");
    Ok(())
}

pub fn precision_rotate_to(
    // port: &mut dyn serialport::SerialPort,
    state: &Arc<Mutex<BackendState>>,
    tx: &Sender<Update>,
    steps: i32,
) -> Result<()> {
    // let mut angle = angle;
    let mut steps = steps;
    // if need_reverse {
    //     steps = -steps;
    // }
    // let mut steps = (angle * 746.0).round() as i32;
    {
        if let Some(ss) = { state.lock().measurement.current_steps } {
            steps = steps - ss;
        } else {
            return Err(anyhow!("没有定义零点"));
        }
    }
    precision_rotate(state, tx, steps)?;
    Ok(())
}

enum MoveMode {
    StepForward,
    ResetForward,
    StepBackward,
    ResetBackward,
}

fn step_move(state: &Arc<Mutex<BackendState>>, tx: &Sender<Update>, mode: MoveMode) -> Result<()> {
    // let mut s = state.lock();
    let mut s = state.lock();
    if s.devices.serial_port.is_none() {
        tx.send(Update::Device(DeviceUpdate::SerialConnectionStatus(false)))?;
        s.measurement.current_steps = None;
        tx.send(Update::Measurement(MeasurementUpdate::CurrentSteps(
            s.measurement.current_steps,
        )))?;
        return Err(anyhow!("执行失败，请重新连接串口并找零点：串口断开"));
    }
    let port = s.devices.serial_port.as_mut().unwrap().clone();
    let need_reverse = s.rotation_direction_need_reverse;
    drop(s);
    let (command, steps) = {
        if !need_reverse {
            match mode {
                MoveMode::StepForward => (51, 6),
                MoveMode::StepBackward => (53, -6),
                MoveMode::ResetForward => (114, -12),
                MoveMode::ResetBackward => (55, 12),
            }
        } else {
            match mode {
                MoveMode::StepBackward => (51, -6),
                MoveMode::StepForward => (53, 6),
                MoveMode::ResetBackward => (114, 12),
                MoveMode::ResetForward => (55, -12),
            }
        }
    };
    let res = cmd(port, command);
    if let Err(e) = &res {
        let mut s = state.lock();
        s.devices.serial_port = None;
        tx.send(Update::Device(DeviceUpdate::SerialConnectionStatus(false)))?;
        s.measurement.current_steps = None;
        tx.send(Update::Measurement(MeasurementUpdate::CurrentSteps(
            s.measurement.current_steps,
        )))?;
        error!("请重新连接串口并找零点：{}", e);
        return Err(anyhow!("请重新连接串口并找零点：{}", e));
    }
    let mut s = state.lock();
    s.measurement.current_steps = s.measurement.current_steps.map(|s| s + steps);
    tx.send(Update::Measurement(MeasurementUpdate::CurrentSteps(
        s.measurement.current_steps,
    )))?;
    Ok(())
}

pub fn static_measurement(
    state: &Arc<Mutex<BackendState>>,
    tx: &Sender<Update>,
    token: CancellationToken,
    find_zero: bool,
    times: i32,
) -> Result<()> {
    // if state.lock().training.fitted_model.is_none() || state.lock().devices.camera_manager.is_none() || state.lock().devices.serial_port.is_none()
    // {
    //     tx.send(Update::Measurement(MeasurementUpdate::StaticStatus("设备未就绪".to_string())))?;
    //     return;
    // }

    // 检查先决条件
    {
        let mut s = state.lock();
        if s.training.fitted_model.is_none()
            || s.devices.camera_manager.is_none()
            || s.devices.serial_port.is_none()
        {
            tx.send(Update::General(GeneralUpdate::Error(format!(
                "设备或模型未就绪"
            ))))?;
            tx.send(Update::Measurement(MeasurementUpdate::StaticRunning(false)))?;
            return Err(anyhow!("设备或模型未就绪"));
        }
        if s.measurement.dynamic_task_token.is_some() || s.measurement.static_task_token.is_some() {
            tx.send(Update::General(GeneralUpdate::Error(format!(
                "已经有测量任务在进行"
            ))))?;
            tx.send(Update::Measurement(MeasurementUpdate::StaticRunning(false)))?;
            return Err(anyhow!("已经有测量任务在进行"));
        }
        s.measurement.static_task_token = Some(token.clone());
        tx.send(Update::Measurement(MeasurementUpdate::StaticRunning(true)))?;
        info!("开始静态测量");
    }
    let result = (|| -> Result<()> {
        for i in 0..times {
            // 在每次循环开始时检查是否已请求中断
            if token.load(Ordering::Relaxed) {
                tx.send(Update::Measurement(MeasurementUpdate::StaticStatus(
                    "测试被用户中断".to_string(),
                )))?;
                return Err(anyhow!("测试被用户中断"));
            }
            let mut predictions: VecDeque<usize> = VecDeque::from(vec![2; 5]);
            let timeout = Duration::from_secs(90);
            let start_time = Instant::now();
            let mut first = 2;
            let mut result1: Option<i32> = None;
            let mut result2: Option<i32> = None;
            let (model, isama) = {
                let mut s = state.lock();
                if find_zero {
                    s.measurement.current_steps = Some(0); //临时值
                }
                (
                    s.training.fitted_model.as_ref().unwrap().clone(),
                    s.rotation_direction_is_ama,
                    // s.rotation_direction_need_reverse,
                )
            };
            let mut first_first=2;
            loop {
                let mut s = state.lock();
                if start_time.elapsed() > timeout || token.load(Ordering::Relaxed) {
                    // tx.send(Update::Measurement(MeasurementUpdate::StaticStatus(
                    //     "不正常终止".to_string(),
                    // )))?;
                    tx.send(Update::Measurement(MeasurementUpdate::StaticStatus(
                        format!("测试中断"),
                    )))?;
                    return Err(anyhow!("测试中断"));
                }
                if s.devices.camera_manager.is_none() {
                    s.devices.camera_manager = None;
                    tx.send(Update::Device(DeviceUpdate::CameraConnectionStatus(false)))?;
                    info!("相机异常");
                    return Err(anyhow!("相机异常"));
                }
                let frame = {
                    s.devices
                        .camera_manager
                        .as_ref()
                        .unwrap()
                        .latest_frame
                        .lock()
                        .clone()
                };
                let frame = match frame {
                    Some(f) => f,
                    None => {
                        s.devices.camera_manager = None;
                        tx.send(Update::Device(DeviceUpdate::CameraConnectionStatus(false)))?;
                        info!("相机异常");
                        return Err(anyhow!("相机异常"));
                    }
                };

                let guard2 = s.devices.camera_settings.lock();
                let circle = {
                    if guard2.lock_circle {
                        guard2.locked_circle
                    } else {
                        None
                    }
                };
                let min_radius = guard2.min_radius;
                let max_radius = guard2.max_radius;
                drop(guard2);
                drop(s);
                let prediction =
                    match predict_from_frame(&frame, &model, min_radius, max_radius, circle) {
                        Ok(p) => p,
                        Err(_) => continue,
                    };
                let prediction = prediction ^ (isama as usize);

                predictions.pop_front();
                predictions.push_back(prediction);
                // info!("预测结果：{:?}", predictions);
                let mut should_break = false;
                let mut pp = predictions.clone();
                let pred_slice = pp.make_contiguous();
                if first == 2 {
                    first = prediction;
                }
                if first_first==2{
                    first_first = prediction;
                    if prediction ==0{
                        precision_rotate(state, tx, 746)?;
                    }else{
                        precision_rotate(state, tx, -746)?;
                    }
                }
                // thread::sleep(Duration::from_millis(500));(- = 1 0)

                if predictions.iter().filter(|&x| *x == 1).count()>=3&&first==0 {
                        step_move(state, tx, MoveMode::ResetBackward)?;
                    if result1.is_none() {
                        result1 = Some(state.lock().measurement.current_steps.unwrap());
                        first = 2;
                        predictions = VecDeque::from(vec![2; 5]);
                        precision_rotate(state, tx, -700)?;
                    } else {
                        result2 = Some(state.lock().measurement.current_steps.unwrap());
                        should_break = true;
                    }
                    thread::sleep(Duration::from_millis(150));
                } else if predictions.iter().filter(|&x| *x == 0).count()>=3&&first==1 {
                        step_move(state, tx, MoveMode::ResetForward)?;
                    if result1.is_none() {
                        result1 = Some(state.lock().measurement.current_steps.unwrap());
                        first = 2;
                        predictions = VecDeque::from(vec![2; 5]);
                        precision_rotate(state, tx, 700)?;
                    } else {
                        result2 = Some(state.lock().measurement.current_steps.unwrap());
                        should_break = true;
                    }
                    thread::sleep(Duration::from_millis(150));
                } else if first == 1 {
                        step_move(state, tx, MoveMode::StepForward)?;

                    // should_break=true;
                    thread::sleep(Duration::from_millis(5));
                } else {
                        step_move(state, tx, MoveMode::StepBackward)?;
                    // should_break=true;
                    thread::sleep(Duration::from_millis(5));
                }
                if !find_zero {
                    tx.send(Update::Measurement(MeasurementUpdate::CurrentSteps(
                        state.lock().measurement.current_steps,
                    )))?;
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
            if result1.is_some() && result2.is_some() {
                let st = { state.lock().measurement.current_steps.unwrap() };
                precision_rotate(
                    state,
                    tx,
                    ((((result1.unwrap() + result2.unwrap()) as f64) / 2.0).round() as i32) - st,
                )?;
                if !find_zero {
                    let mut s = state.lock();
                    let result = StaticResult {
                        index: s.measurement.static_results.len() + 1,
                        steps: s.measurement.current_steps.unwrap(),
                        angle: s.measurement.current_steps.unwrap() as f32 / 746.0,
                    };
                    s.measurement.static_results.push(result);

                    tx.send(Update::Measurement(MeasurementUpdate::StaticResults(
                        s.measurement.static_results.clone(),
                    )))?;
                }
            } else {
                return Err(anyhow!("双向逼近失败"));
            }
        }
        Ok(())
    })();
    let mut s = state.lock();
    if let Err(e) = &result {
        if find_zero {
            s.measurement.current_steps = None;
        }
        info!("静态测量失败：{}", e);
    } else {
        if find_zero {
            s.measurement.current_steps = Some(0);
        }

        info!("静态测量完成");
    }
    tx.send(Update::Measurement(MeasurementUpdate::CurrentSteps(
        s.measurement.current_steps,
    )))?;
    s.measurement.static_task_token = None;
    tx.send(Update::Measurement(MeasurementUpdate::StaticRunning(false)))?;
    // tx.send(Update::Measurement(MeasurementUpdate::StaticStatus(
    //     "测量完成".to_string(),
    // )))?;
    result
}

pub fn pre_rotation(
    state: &Arc<Mutex<BackendState>>,
    tx: &Sender<Update>,
    token: CancellationToken,
) -> Result<()> {
    // 检查先决条件
    let result = (|| {
        {
            let s = state.lock();
            if s.training.fitted_model.is_none()
                || s.devices.camera_manager.is_none()
                || s.devices.serial_port.is_none()
            {
                return Err(anyhow!("设备或模型未就绪"));
            }
        }

        let mut predictions: VecDeque<usize> = VecDeque::from(vec![2; 5]);
        let timeout = Duration::from_secs(90);
        let start_time = Instant::now();
        let mut first = 2;
        let (model, isama) = {
            let s = state.lock();
            (
                s.training.fitted_model.as_ref().unwrap().clone(),
                s.rotation_direction_is_ama,
                // s.rotation_direction_need_reverse,
            )
        };
        loop {
            let s = state.lock();
            if start_time.elapsed() > timeout || token.load(Ordering::Relaxed) {
                return Err(anyhow!("超时或被终止"));
            }
            if s.devices.camera_manager.is_none() {
                tx.send(Update::Measurement(MeasurementUpdate::DynamicStatus(
                    format!("相机异常"),
                )))?;
                tx.send(Update::Device(DeviceUpdate::CameraConnectionStatus(false)))?;
                return Err(anyhow!("相机异常"));
            }
            let frame = {
                s.devices
                    .camera_manager
                    .as_ref()
                    .unwrap()
                    .latest_frame
                    .lock()
                    .clone()
            };
            let frame = match frame {
                Some(f) => f,
                None => {
                    tx.send(Update::Measurement(MeasurementUpdate::DynamicStatus(
                        format!("相机异常"),
                    )))?;
                    tx.send(Update::Device(DeviceUpdate::CameraConnectionStatus(false)))?;
                    tx.send(Update::Measurement(MeasurementUpdate::CurrentSteps(
                        s.measurement.current_steps,
                    )))?;
                    return Err(anyhow!("相机异常"));
                }
            };

            let guard2 = s.devices.camera_settings.lock();
            let circle = {
                if guard2.lock_circle {
                    guard2.locked_circle
                } else {
                    None
                }
            };
            let min_radius = guard2.min_radius;
            let max_radius = guard2.max_radius;
            drop(guard2);
            drop(s);
            let prediction =
                match predict_from_frame(&frame, &model, min_radius, max_radius, circle) {
                    Ok(p) => p,
                    Err(_) => continue,
                };
            let prediction = prediction ^ (isama as usize);

            predictions.pop_front();
            predictions.push_back(prediction);
            // info!("预测结果：{:?}", predictions);
            let mut should_break = false;
            tx.send(Update::Measurement(MeasurementUpdate::DynamicStatus(
                format!("预旋转中: {:?}", predictions),
            )))?;
            let mut pp = predictions.clone();
            let pred_slice = pp.make_contiguous();
            if first == 2 {
                first = prediction;
            }
            // thread::sleep(Duration::from_millis(500));(- = 1 0)

            if predictions.iter().filter(|&x| *x == 1).count()>=3&&first==0 {
                    step_move(state, tx, MoveMode::ResetBackward)?;
                should_break = true;
                thread::sleep(Duration::from_millis(150));
            } else if predictions.iter().filter(|&x| *x == 0).count()>=3&&first==1 {
                    step_move(state, tx, MoveMode::ResetForward)?;
                should_break = true;
                thread::sleep(Duration::from_millis(150));
            } else if first == 1 {
                    step_move(state, tx, MoveMode::StepForward)?;
                thread::sleep(Duration::from_millis(5));
            } else {
                    step_move(state, tx, MoveMode::StepBackward)?;
                thread::sleep(Duration::from_millis(5));
            }
            tx.send(Update::Measurement(MeasurementUpdate::CurrentSteps(
                state.lock().measurement.current_steps,
            )))?;
            if should_break {
                return Ok(());
            }
            if pred_slice == [0, 0, 0, 0, 0] {
                first = 0;
            }
            if pred_slice == [1, 1, 1, 1, 1] {
                first = 1;
            }
        }
    })();
    {
        tx.send(Update::Measurement(MeasurementUpdate::CurrentSteps(
            state.lock().measurement.current_steps,
        )))?;
    }
    if result.is_err() {
        token.load(Ordering::Relaxed);
    }
    // tx.send(Update::Measurement(MeasurementUpdate::StaticStatus(
    //     "测量完成".to_string(),
    // )))?;
    result
}

pub fn run_dynamic_experiment_loop(
    state: &Arc<Mutex<BackendState>>,
    tx: &Sender<Update>,
    token: CancellationToken,
) -> Result<()> {
    let (isama, model) = {
        let mut s = state.lock();
        if s.training.fitted_model.is_none()
            || s.devices.camera_manager.is_none()
            || s.devices.serial_port.is_none()
        {
            tx.send(Update::General(GeneralUpdate::Error(format!(
                "设备或模型未就绪"
            ))))?;
            tx.send(Update::Measurement(MeasurementUpdate::DynamicRunning(
                false,
            )))?;
            return Err(anyhow!("设备或模型未就绪"));
        }

        if s.measurement.current_steps.is_none() {
            tx.send(Update::General(GeneralUpdate::Error(format!("未归零"))))?;
            tx.send(Update::Measurement(MeasurementUpdate::DynamicRunning(
                false,
            )))?;
            return Err(anyhow!("未归零"));
        }

        if s.measurement.dynamic_time.is_none() {
            tx.send(Update::General(GeneralUpdate::Error(format!(
                "请先开始计时"
            ))))?;
            tx.send(Update::Measurement(MeasurementUpdate::DynamicRunning(
                false,
            )))?;
            return Err(anyhow!("请先开始计时"));
        }

        if s.measurement.dynamic_task_token.is_some() || s.measurement.static_task_token.is_some() {
            tx.send(Update::General(GeneralUpdate::Error(format!(
                "已经有测量任务在运行"
            ))))?;
            tx.send(Update::Measurement(MeasurementUpdate::DynamicRunning(
                false,
            )))?;
            return Err(anyhow!("已经有测量任务在运行"));
        }

        //过五关斩六将，开始！
        s.measurement.dynamic_task_token = Some(token.clone());
        tx.send(Update::Measurement(MeasurementUpdate::DynamicRunning(true)))?;
        info!("动态追踪启动");
        (
            s.rotation_direction_is_ama,
            // s.rotation_direction_need_reverse,
            s.training.fitted_model.as_ref().unwrap().clone(),
        )
    };
    let result = (|| -> Result<()> {
        info!("动态追踪：开始预旋转");
        pre_rotation(state, tx, token.clone())?;
        
        let params={
            state.lock().measurement.dynamic_params.clone()
        };
        precision_rotate(state, tx, (params.step_angle * 746.0).round() as i32)?;
        info!("动态追踪：预旋转完成");

        let timeout = Duration::from_secs(2000);
        let mut predictions: VecDeque<usize> = VecDeque::from(vec![2; 5]);
        let mut first=2;
        loop {
            let mut s = state.lock();
            if token.load(Ordering::Relaxed)
                || s.measurement.dynamic_results.len() >= s.measurement.dynamic_params.sample_points as usize
                || s.measurement.dynamic_time.unwrap().elapsed() > timeout
            {
                // s.measurement.current_static_steps = None;
                return Ok(());
            }
            if s.devices.camera_manager.is_none() {
                tx.send(Update::Measurement(MeasurementUpdate::CurrentSteps(
                    s.measurement.current_steps,
                )))?;
                s.devices.camera_manager = None;
                tx.send(Update::Device(DeviceUpdate::CameraConnectionStatus(false)))?;
                return Err(anyhow!("相机异常"));
            }
            let frame = {
                s.devices
                    .camera_manager
                    .as_ref()
                    .unwrap()
                    .latest_frame
                    .lock()
                    .clone()
            };
            let frame = match frame {
                Some(f) => f,
                None => {
                    tx.send(Update::Measurement(MeasurementUpdate::CurrentSteps(
                        s.measurement.current_steps,
                    )))?;
                    s.devices.camera_manager = None;
                    tx.send(Update::Device(DeviceUpdate::CameraConnectionStatus(false)))?;
                    return Err(anyhow!("相机异常"));
                }
            };
            let guard2 = s.devices.camera_settings.lock();
            let circle = {
                if guard2.lock_circle {
                    guard2.locked_circle
                } else {
                    None
                }
            };
            let min_radius = guard2.min_radius;
            let max_radius = guard2.max_radius;
            drop(guard2);
            drop(s);
            let prediction =
                match predict_from_frame(&frame, &model, min_radius, max_radius, circle) {
                    Ok(p) => p,
                    Err(_) => continue,
                };
            let prediction = prediction ^ (isama as usize);
            if first==2{
                first=prediction;
            }
            predictions.pop_front();
            predictions.push_back(prediction);
            // let mut should_break = false;
            let pred_slice = predictions.make_contiguous();

            // let isama=s.rotation_direction_is_ama;
            // drop(s);
            // thread::sleep(Duration::from_millis(500));(- = 1 0)
            let mut triggered = false;
            if predictions.iter().filter(|&x| *x == 1).count()>=3&&first==0 {
                triggered = true;
            } else if predictions.iter().filter(|&x| *x == 0).count()>=3&&first==1 {
                triggered = true;
            }
            if triggered {
                // let elapsed_time =
                let params={
                    let mut s = state.lock();
                    let result = crate::communication::DynamicResult {
                        index: s.measurement.dynamic_results.len() + 1,
                        time: s.measurement.dynamic_time.unwrap().elapsed().as_secs_f64(),
                        steps: s.measurement.current_steps.unwrap(),
                        angle: s.measurement.current_steps.unwrap() as f32 / 746.0,
                    };
                    s.measurement.dynamic_results.push(result);
                    tx.send(Update::Measurement(MeasurementUpdate::DynamicResults(
                        s.measurement.dynamic_results.clone(),
                    )))?;
                    info!("已测量第 {} 个点", s.measurement.dynamic_results.len());
                    s.measurement.dynamic_params.clone()  
                };
                save_dynamic_results(state, tx, params.clone())?;
                precision_rotate(state, tx, (params.step_angle * 746.0).round() as i32)?;
                predictions = VecDeque::from(vec![2; 5]);
                thread::sleep(Duration::from_millis(100));
                first=2;
            }

            thread::sleep(Duration::from_millis(50));
        }
    })();
    let mut s = state.lock();
    tx.send(Update::Measurement(MeasurementUpdate::DynamicResults(
        s.measurement.dynamic_results.clone(),
    )))?;
    s.measurement.dynamic_task_token = None;
    tx.send(Update::Measurement(MeasurementUpdate::CurrentSteps(
        s.measurement.current_steps,
    )))?;
    tx.send(Update::Measurement(MeasurementUpdate::DynamicRunning(
        false,
    )))?;
    if let Err(e) = &result {
        tracing::warn!(
            "终止原因：{}",
            e
        );
    } 
    {
        info!(
            "测量完成，共测量 {} 个点",
            s.measurement.dynamic_results.len()
        );
        drop(s);
        precision_rotate_to(state, tx, 0)?;
    }
    result
}

pub fn return_to_zero(state: &Arc<Mutex<BackendState>>, tx: &Sender<Update>) -> Result<()> {
    info!("请求返回零点");
    // let mut s = state.lock();
    if let Some(steps) = {
        let s = state.lock();

        s.measurement.current_steps
    } {
        precision_rotate(&state, tx, -steps)?;
    }
    Ok(())
}

pub fn save_static(
    state: &Arc<Mutex<BackendState>>,
    save_path: PathBuf,
    tx: &Sender<Update>,
) -> Result<()> {
    let results = state.lock().measurement.static_results.clone();
    if results.is_empty() {
        error!("静态测量结果为空");
        return Ok(());
    }
    if file_saver::save_static_results(&save_path, &results).is_err() {
        error!("静态测量保存失败");
    }
    tx.send(Update::Measurement(MeasurementUpdate::StaticStatus(
        "保存成功".to_string(),
    )))?;
    info!("静态测量结果保存成功");
    Ok(())
}
pub fn save_dynamic_results(
    state: &Arc<Mutex<BackendState>>,
    tx: &Sender<Update>,
    params: DynamicExpParams,
) -> Result<()> {
    let s = state.lock();
    let results = s.measurement.dynamic_results.clone();
    if results.is_empty() {
        error!("动态测量结果为空");
        return Ok(());
    }
    if file_saver::save_dynamic_results(&params.path, &results, &params).is_err() {
        error!("动态测量保存失败");
    }
    info!("动态测量结果保存成功");
    Ok(())
}
