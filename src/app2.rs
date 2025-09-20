// src/app.rs

use crate::communication::*; // Now imports the new hierarchical enums
use crossbeam_channel::{Receiver, Sender};
use egui::{CentralPanel, ComboBox, DragValue, RichText, TopBottomPanel, Ui};
use egui_extras::{Column, TableBuilder};
use egui_plot::{Line, Plot, PlotPoints, Points};
use std::sync::Arc;
use std::thread;

// 注意: Tab 枚举已被移除，因为我们现在使用独立的窗口。

pub struct PolarimeterApp {
    // --- 通信 ---
    cmd_tx: Sender<Command>,
    update_rx: Receiver<Update>,
    backend_handle: Option<thread::JoinHandle<()>>,

    // --- UI 窗口可见性状态 ---
    is_device_control_open: bool,
    is_model_training_open: bool,
    is_static_measurement_open: bool,
    is_dynamic_measurement_open: bool,
    is_data_processing_open: bool,
    is_camera_open: bool,
    is_plots_open: bool,

    // --- 通用 UI 状态 ---
    status_message: String,
    cm_data: Option<ConfusionMatrixData>,
    roc_data: Option<RocCurveData>,

    // --- 窗口 1: 设备控制 ---
    serial_ports: Vec<String>,
    selected_serial_port: String,
    is_serial_connected: bool,
    rotation_direction_is_ama: bool,
    rotation_direction_reverse: bool,
    manual_rotation_angle: f32,
    manual_rotation_to_angle: f32,
    is_recording: bool,
    recording_elapsed_time: f32,
    recording_mode: String, // "MAM" or "AMA"

    // --- 窗口 2: 相机 ---
    camera_list: Vec<String>,
    selected_camera_idx: usize,
    is_camera_connected: bool,
    camera_texture: Option<egui::TextureHandle>,
    camera_image: Option<Arc<egui::ColorImage>>,
    exposure: f32,
    min_radius: u32,
    max_radius: u32,
    camera_lock_circle: bool,

    // --- 窗口 3: 模型训练 ---
    mam_video_path: String,
    ama_video_path: String,
    dataset_path: String,
    mam_video_status: String,
    ama_video_status: String,
    persistent_dataset_status: String,
    training_status: String,
    is_model_ready: bool,
    train_show_roc: bool,
    train_show_cm: bool,

    // --- 窗口 4: 静态测量 ---
    is_static_running: bool,
    static_pre_rotation_angle: f32,
    static_measurement_status: String,
    static_results: Vec<StaticResult>,

    // --- 窗口 5: 动态测量 ---
    dynamic_params: DynamicExpParams,
    dynamic_save_path: String,
    dynamic_measurement_status: String,
    dynamic_results: Vec<DynamicResult>,
    is_dynamic_exp_running: bool,
    current_angle: Option<f32>,
    start_time: Option<std::time::Instant>,

    // --- 窗口 6: 数据处理 ---
    data_import_path: String,
    alpha_inf: f64,
    regression_mode: RegressionMode,
    regression_formula: String,
    raw_plot_data: Arc<Vec<(f64, i32, f64, bool)>>,
    plot_scatter_points: Vec<(f64, f64)>,
    plot_line_points: Vec<(f64, f64)>,
}

impl eframe::App for PolarimeterApp {
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        tracing::info!("前端：正在退出，通知后端关闭...");
        if let Err(e) = self.cmd_tx.send(Command::General(GeneralCommand::Shutdown)) {
            tracing::error!("前端：发送关闭指令失败: {}", e);
        }
        if let Some(handle) = self.backend_handle.take() {
            tracing::info!("前端：等待后端线程完成...");
            if let Err(e) = handle.join() {
                tracing::error!("前端：等待后端线程时发生错误: {:?}", e);
            } else {
                tracing::info!("前端：后端线程已成功关闭。");
            }
        }
    }

    /// 主更新循环，现在负责绘制菜单栏和所有独立的窗口。
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        // 1. 处理来自后端的更新
        self.handle_backend_updates();

        // 2. 加载新的相机图像为纹理
        if let Some(image) = self.camera_image.take() {
            let texture = ctx.load_texture("camera_feed", image, Default::default());
            self.camera_texture = Some(texture);
        }

        // 3. 绘制顶部菜单栏
        TopBottomPanel::top("top_panel").show(ctx, |ui| {
            egui::menu::bar(ui, |ui| {
                // ui.menu_button("文件", |ui| {
                //     if ui.button("退出").clicked() {
                //         frame.close();
                //     }
                // });
                ui.toggle_value(&mut self.is_device_control_open, "设备控制");
                ui.toggle_value(&mut self.is_camera_open, "相机");
                ui.toggle_value(&mut self.is_model_training_open, "模型训练");
                ui.toggle_value(&mut self.is_static_measurement_open, "静态测量");
                ui.toggle_value(&mut self.is_dynamic_measurement_open, "动态测量");
                ui.toggle_value(&mut self.is_data_processing_open, "数据处理");
            });
        });

        // 4. 绘制所有独立的UI窗口
        self.show_device_control_window(ctx);
        self.show_model_training_window(ctx);
        self.show_static_measurement_window(ctx);
        self.show_dynamic_measurement_window(ctx);
        self.show_data_processing_window(ctx);
        self.show_camera_window(ctx);
        self.show_plots_window(ctx); // 训练评估结果窗口

        // 5. 绘制中央面板作为背景
        CentralPanel::default().show(ctx, |ui| {
            ui.centered_and_justified(|ui| {
                ui.heading("旋光仪控制软件 v1.5.4-Rust");
            });
            ui.vertical_centered(|ui| {
                ui.label("使用“窗口”菜单来显示/隐藏不同的功能面板。");
            });
        });

        // 6. 绘制底部状态栏
        TopBottomPanel::bottom("bottom_panel").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(&self.status_message);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label("v1.5.4 Rust 版");
                });
            });
        });

        // 7. 请求重绘以保持UI流畅
        ctx.request_repaint();
    }
}

impl PolarimeterApp {
    pub fn new(
        cmd_tx: Sender<Command>,
        update_rx: Receiver<Update>,
        backend_handle: Option<thread::JoinHandle<()>>,
    ) -> Self {
        // 启动时请求初始数据
        cmd_tx
            .send(Command::Device(DeviceCommand::RefreshSerialPorts))
            .unwrap();
        cmd_tx
            .send(Command::Camera(CameraCommand::RefreshCameras))
            .unwrap();

        Self {
            cmd_tx,
            update_rx,
            backend_handle,

            // 初始化窗口为默认打开状态
            is_device_control_open: true,
            is_model_training_open: false,
            is_static_measurement_open: false,
            is_dynamic_measurement_open: false,
            is_data_processing_open: false,
            is_camera_open: true,
            is_plots_open: false,

            status_message: "欢迎使用!".to_string(),
            serial_ports: vec!["刷新中...".to_string()],
            selected_serial_port: "".to_string(),
            is_serial_connected: false,
            rotation_direction_is_ama: false,
            rotation_direction_reverse: false,
            manual_rotation_angle: 0.0,
            manual_rotation_to_angle: 0.0,
            camera_list: vec!["刷新中...".to_string()],
            is_recording: false,
            recording_elapsed_time: 0.0,
            recording_mode: "MAM".to_string(),
            selected_camera_idx: 0,
            is_camera_connected: false,
            camera_texture: None,
            camera_image: None,
            exposure: -8.0,
            min_radius: 30,
            max_radius: 45,
            camera_lock_circle: false,
            cm_data: None,
            roc_data: None,
            mam_video_path: String::new(),
            ama_video_path: String::new(),
            dataset_path: String::new(),
            mam_video_status: "未处理".to_string(),
            ama_video_status: "未处理".to_string(),
            persistent_dataset_status: "未导入".to_string(),
            training_status: "无可用模型".to_string(),
            is_model_ready: false,
            train_show_roc: true,
            train_show_cm: true,
            is_static_running: false,
            static_pre_rotation_angle: 0.0,
            static_measurement_status: "空闲".to_string(),
            static_results: Vec::new(),
            dynamic_params: DynamicExpParams {
                student_name: "".to_string(),
                student_id: "".to_string(),
                temperature: 25.0,
                sucrose_conc: 0.0,
                hcl_conc: 0.0,
                pre_rotation_angle: 5.0,
                step_angle: -0.5,
                sample_points: 12,
            },
            dynamic_save_path: String::new(),
            dynamic_measurement_status: String::new(),
            dynamic_results: Vec::new(),
            is_dynamic_exp_running: false,
            current_angle: None,
            data_import_path: String::new(),
            alpha_inf: 0.0,
            regression_mode: RegressionMode::Log,
            regression_formula: String::new(),
            raw_plot_data: Arc::new(Vec::new()),
            plot_scatter_points: Vec::new(),
            plot_line_points: Vec::new(),
            start_time: None,
        }
    }

    /// 处理所有来自后端的待处理更新
    fn handle_backend_updates(&mut self) {
        while let Ok(update) = self.update_rx.try_recv() {
            match update {
                Update::General(update) => match update {
                    GeneralUpdate::StatusMessage(msg) => self.status_message = msg,
                    GeneralUpdate::Error(err_msg) => {
                        self.status_message = format!("错误: {}", err_msg);
                    }
                },
                Update::Device(update) => match update {
                    DeviceUpdate::SerialPortsList(ports) => {
                        self.serial_ports = ports;
                        if !self.serial_ports.is_empty() {
                            self.selected_serial_port = self.serial_ports[0].clone();
                        }
                    }
                    DeviceUpdate::SerialConnectionStatus(status) => {
                        self.is_serial_connected = status
                    }
                    DeviceUpdate::CameraList(cameras) => self.camera_list = cameras,
                    DeviceUpdate::CameraConnectionStatus(status) => {
                        self.is_camera_connected = status
                    }
                    DeviceUpdate::NewCameraFrame(img) => self.camera_image = Some(img),
                },
                Update::Recording(update) => match update {
                    RecordingUpdate::StatusUpdate(status) => match status {
                        RecordingStatus::Started => {
                            self.is_recording = true;
                            self.recording_elapsed_time = 0.0;
                            self.status_message = "录制已开始".to_string();
                        }
                        RecordingStatus::InProgress { elapsed_seconds } => {
                            self.is_recording = true;
                            self.recording_elapsed_time = elapsed_seconds;
                        }
                        RecordingStatus::Finished => {
                            self.is_recording = false;
                            self.status_message = "录制已完成".to_string();
                        }
                        RecordingStatus::Error(e) => {
                            self.is_recording = false;
                            self.status_message = format!("录制错误: {}", e);
                        }
                    },
                },
                Update::Training(update) => match update {
                    TrainingUpdate::VideoProcessingUpdate { mode, message } => {
                        if mode == "MAM" {
                            self.mam_video_status = message;
                        } else {
                            self.ama_video_status = message;
                        }
                    }
                    TrainingUpdate::TrainingStatus(msg) => self.training_status = msg,
                    TrainingUpdate::ModelReady(ready) => self.is_model_ready = ready,
                    TrainingUpdate::TrainingPlotsReady { cm, roc } => {
                        if cm.is_some() || roc.is_some() {
                            self.cm_data = cm;
                            self.roc_data = roc;
                            self.is_plots_open = true; // 自动打开评估结果窗口
                        }
                    }
                    TrainingUpdate::PersistentDatasetStatus(msg) => {
                        self.persistent_dataset_status = msg
                    }
                    TrainingUpdate::MAMDatasetStatus(msg) => self.mam_video_status = msg,
                    TrainingUpdate::AMADatasetStatus(msg) => self.ama_video_status = msg,
                },
                Update::Measurement(update) => match update {
                    MeasurementUpdate::StaticStatus(msg) => {self.static_measurement_status = msg.clone();
                    self.status_message = msg;},
                    MeasurementUpdate::StaticResults(results) => self.static_results = results,
                    MeasurementUpdate::DynamicResults(results) => self.dynamic_results = results,
                    MeasurementUpdate::DynamicRunning(running) => {
                        self.is_dynamic_exp_running = running
                    }
                    MeasurementUpdate::StaticRunning(running) => self.is_static_running = running,
                    MeasurementUpdate::CurrentSteps(steps) => {
                        if let Some(steps) = steps {
                            self.current_angle = Some((steps as f32) / 746.0);
                        } else {
                            self.current_angle = None;
                        }
                    }
                    MeasurementUpdate::StartTime(time) => self.start_time = time,
                    MeasurementUpdate::DynamicStatus(msg) => {
                        self.dynamic_measurement_status = msg.clone();
                        self.status_message = msg;
                    }
                },
                Update::DataProcessing(update) => match update {
                    DataProcessingUpdate::FullState(state) => {
                        self.raw_plot_data = state.raw_data;
                        self.alpha_inf = state.alpha_inf;
                        self.regression_mode = state.regression_mode;
                        self.regression_formula = state.regression_formula;

                        // --- NEW: Directly accept pre-calculated plot data ---
                        self.plot_scatter_points = state.plot_scatter_points;
                        self.plot_line_points = state.plot_line_points;
                    }
                },
            }
        }
    }

    // ===================================================================================
    //  UI 窗口绘制函数
    //  每个函数都负责绘制一个可独立开关的窗口。
    // ===================================================================================

    fn show_device_control_window(&mut self, ctx: &egui::Context) {
        let mut is_device_control_open = self.is_device_control_open;
        egui::Window::new("设备控制")
            .open(&mut is_device_control_open)
            
            .vscroll(false)
            .resizable(true)
            .default_width(350.0)
            .show(ctx, |ui| {
                self.ui_device_control(ui);
            });
        self.is_device_control_open = is_device_control_open;
    }

    fn show_model_training_window(&mut self, ctx: &egui::Context) {
        let mut is_model_training_open = self.is_model_training_open;
        egui::Window::new("模型训练")
            .open(&mut is_model_training_open)
            .vscroll(false)
            .resizable(true)
            .default_width(500.0)
            .show(ctx, |ui| {
                self.ui_model_training(ui);
            });
        self.is_model_training_open = is_model_training_open;
    }

    fn show_static_measurement_window(&mut self, ctx: &egui::Context) {
        let mut is_static_measurement_open = self.is_static_measurement_open;
        egui::Window::new("静态测量")
            .open(&mut is_static_measurement_open)
            .vscroll(false)
            .resizable(true)
            .default_width(400.0)
            .show(ctx, |ui| {
                self.ui_static_measurement(ui);
            });
        self.is_static_measurement_open = is_static_measurement_open;
    }

    fn show_dynamic_measurement_window(&mut self, ctx: &egui::Context) {
        let mut is_dynamic_measurement_open = self.is_dynamic_measurement_open;
        egui::Window::new("动态测量")
            .open(&mut is_dynamic_measurement_open)
            .vscroll(false)
            .resizable(true)
            .default_width(550.0)
            .show(ctx, |ui| {
                self.ui_dynamic_measurement(ui);
            });
        self.is_dynamic_measurement_open = is_dynamic_measurement_open;
    }

    fn show_data_processing_window(&mut self, ctx: &egui::Context) {
        let mut is_data_processing_open = self.is_data_processing_open;
        egui::Window::new("数据处理 - 控制与数据")
            .open(&mut is_data_processing_open)
            .resizable(true)
            .vscroll(false) // 允许垂直滚动，避免内容撑满窗口
            .default_width(380.0)
            .show(ctx, |ui| {
                self.ui_data_processing_controls(ui);
            });

        // 第二个窗口：回归图
        egui::Window::new("数据处理 - 回归图")
            .open(&mut is_data_processing_open)
            .resizable(true)
            .default_width(500.0)
            .default_height(400.0)
            .show(ctx, |ui| {
                self.ui_data_processing_plot(ui);
            });

        // 最后，将本地布尔值的状态同步回 self。
        // 这样，关闭任何一个窗口都会导致下一次重绘时两个窗口都不再显示。
        self.is_data_processing_open = is_data_processing_open;
    }

    fn show_camera_window(&mut self, ctx: &egui::Context) {
        let mut is_camera_open = self.is_camera_open;
        egui::Window::new("相机")
            .open(&mut is_camera_open)
            .vscroll(false) // 禁用主滚动条，内部有自己的滚动和布局
            .resizable(true)
            .default_width(600.0)
            .show(ctx, |ui| {
                self.ui_camera_panel(ui);
            });
        self.is_camera_open = is_camera_open;
    }

    fn show_plots_window(&mut self, ctx: &egui::Context) {
        // 这个窗口由后端数据驱动，当有新结果时 is_plots_open 会被设为 true
        egui::Window::new("训练评估结果")
            .open(&mut self.is_plots_open)
            .vscroll(true)
            .resizable(true)
            .default_width(400.0)
            .show(ctx, |ui| {
                if let Some(cm) = &self.cm_data {
                    ui.heading("混淆矩阵 (Confusion Matrix)");
                    ui.label(format!("整体准确度: {:.2}%", cm.accuracy * 100.0));

                    egui::Grid::new("cm_grid").show(ui, |ui| {
                        ui.label("");
                        ui.label("预测为 0 (MAM)");
                        ui.label("预测为 1 (AMA)");
                        ui.end_row();
                        ui.label("实际为 0 (MAM)");
                        ui.label(cm.matrix[0][0].to_string());
                        ui.label(cm.matrix[0][1].to_string());
                        ui.end_row();
                        ui.label("实际为 1 (AMA)");
                        ui.label(cm.matrix[1][0].to_string());
                        ui.label(cm.matrix[1][1].to_string());
                        ui.end_row();
                    });
                    ui.separator();
                }

                if let Some(_roc) = &self.roc_data {
                    ui.heading("ROC 曲线");
                    // ... egui_plot logic ...
                }
            });
    }

    // ===================================================================================
    //  UI 内容绘制函数
    //  这些函数包含每个窗口内部的具体UI组件，与之前的 draw_*_tab 函数内容基本一致。
    // ===================================================================================

    fn ui_camera_panel(&mut self, ui: &mut Ui) {
        // egui::TopBottomPanel::top("camera_top_controls").show_inside(ui, |ui| {
        //     // ui.heading("相机控制");
        //     // ui.separator();

            
        // });

        egui::TopBottomPanel::bottom("camera_bottom_controls").show_inside(ui, |ui| {
            // ui.separator();

            if ui
                .checkbox(&mut self.camera_lock_circle, "锁定圆形位置")
                .changed()
            {
                self.cmd_tx
                    .send(Command::Camera(CameraCommand::SetLock(
                        self.camera_lock_circle,
                    )))
                    .unwrap();
            }

            let min_radius_slider = ui.add(
                egui::Slider::new(&mut self.min_radius, 1..=self.max_radius).text("最小圆半径"),
            );
            let max_radius_slider = ui.add(
                egui::Slider::new(&mut self.max_radius, self.min_radius..=200).text("最大圆半径"),
            );

            if min_radius_slider.changed() || max_radius_slider.changed() {
                self.cmd_tx
                    .send(Command::Camera(CameraCommand::SetHoughCircleRadius {
                        min: self.min_radius,
                        max: self.max_radius,
                    }))
                    .unwrap();
            }
            ui.add_space(5.0);
        });

        egui::CentralPanel::default()
            // .frame(egui::Frame::group(ui.style()))
            .show_inside(ui, |ui| {
                if self.camera_texture.is_some() && self.is_camera_connected {
                    let texture = self.camera_texture.as_ref().unwrap();
                    let img = egui::Image::new(texture)
                        .maintain_aspect_ratio(true)
                        .max_size(ui.available_size());
                    ui.centered_and_justified(|ui| {
                        ui.add(img);
                    });
                } else {
                    ui.centered_and_justified(|ui| {
                        ui.label("[无信号]");
                    });
                }
            });
    }

    fn ui_device_control(&mut self, ui: &mut Ui) {
        // ui.heading("串口与电机");
        // ui.separator();

        ui.horizontal(|ui| {
            ui.label("选择串口:");
            let selected_text = self.selected_serial_port.clone();
            // egui::ComboBox::from_id_source("serial_select")
            //     .selected_text(&selected_text)
            //     .show_ui(ui, |ui| {
            //         for port in &self.serial_ports {
            //             ui.selectable_value(&mut self.selected_serial_port, port.clone(), port);
            //         }
            //     });
            if egui::ComboBox::from_id_source("serial_select")
                .selected_text(selected_text)
                .show_ui(ui, |ui| {
                    for port in &self.serial_ports {
                        if ui
                            .selectable_value(&mut self.selected_serial_port, port.clone(), port)
                            .clicked()
                        {
                            self.cmd_tx
                                .send(Command::Device(DeviceCommand::ConnectSerial {
                                    port: self.selected_serial_port.clone(),
                                    baud_rate: 9600,
                                }))
                                .unwrap();
                        }
                    }
                })
                .response
                .clicked()
            {}
            if ui.button("刷新").clicked() {
                self.cmd_tx
                    .send(Command::Device(DeviceCommand::RefreshSerialPorts))
                    .unwrap();
            };
            if self.is_serial_connected {
                if ui.button("断开").clicked() {
                    self.cmd_tx
                        .send(Command::Device(DeviceCommand::DisconnectSerial))
                        .unwrap();
                }
            } else {
                if ui.button("连接").clicked() && !self.selected_serial_port.is_empty() {
                    self.cmd_tx
                        .send(Command::Device(DeviceCommand::ConnectSerial {
                            port: self.selected_serial_port.clone(),
                            baud_rate: 9600,
                        }))
                        .unwrap();
                }
            }
        });

        ui.add_space(10.0);

        ui.horizontal(|ui| {
                ui.label("选择相机:");
                let selected_text = self
                    .camera_list
                    .get(self.selected_camera_idx)
                    .cloned()
                    .unwrap_or_else(|| "N/A".to_string());
                if egui::ComboBox::from_id_source("camera_select")
                    .selected_text(selected_text)
                    .show_ui(ui, |ui| {
                        for (i, cam) in self.camera_list.iter().enumerate() {
                            if ui
                                .selectable_value(&mut self.selected_camera_idx, i, cam)
                                .clicked()
                            {
                                self.cmd_tx
                                    .send(Command::Camera(CameraCommand::Connect {
                                        index: self.selected_camera_idx,
                                    }))
                                    .unwrap();
                            }
                        }
                    })
                    .response
                    .clicked()
                {}

                if ui.button("刷新").clicked() {
                    self.cmd_tx
                        .send(Command::Camera(CameraCommand::RefreshCameras))
                        .unwrap();
                }
                if self.is_camera_connected {
                    if ui.button("断开").clicked() {
                        self.cmd_tx
                            .send(Command::Camera(CameraCommand::Disconnect))
                            .unwrap();
                        self.camera_texture = None;
                    }
                } else {
                    if ui.button("连接").clicked() {
                        self.cmd_tx
                            .send(Command::Camera(CameraCommand::Connect {
                                index: self.selected_camera_idx,
                            }))
                            .unwrap();
                    }
                }
            });

        ui.add_space(10.0);
        ui.separator();
        ui.horizontal(|ui| {
            ui.label("正值对应:");
            if ui
                .radio_value(&mut self.rotation_direction_is_ama, false, "明暗明 (新)")
                .changed()
                || ui
                    .radio_value(&mut self.rotation_direction_is_ama, true, "暗明暗 (旧)")
                    .changed()
            {
                self.cmd_tx
                    .send(Command::Device(DeviceCommand::SetRotationDirection(
                        self.rotation_direction_is_ama,
                    )))
                    .unwrap();
            }
        });
        ui.horizontal(|ui| {
            ui.label("旋转方向:");
            if ui
                .radio_value(&mut self.rotation_direction_reverse, false, "正")
                .changed()
                || ui
                    .radio_value(&mut self.rotation_direction_reverse, true, "反")
                    .changed()
            {
                self.cmd_tx
                    .send(Command::Device(DeviceCommand::SetRotationReverse(
                        self.rotation_direction_reverse,
                    )))
                    .unwrap();
            }
        });
        ui.separator();

        ui.add_space(10.0);
        ui.add_enabled_ui(self.is_serial_connected, |ui| {
            ui.horizontal(|ui| {
                ui.label("手动旋转");
                ui.add(
                    egui::DragValue::new(&mut self.manual_rotation_angle)
                        .speed(0.1)
                        .suffix("°"),
                );
                if ui.button("旋转").clicked() {
                    self.cmd_tx
                        .send(Command::Device(DeviceCommand::RotateMotor {
                            steps: (self.manual_rotation_angle * 746.0).round() as i32,
                        }))
                        .unwrap();
                    self.manual_rotation_angle=0.0;
                }
            });
            ui.add_enabled_ui(self.current_angle.is_some(), |ui| {
                ui.horizontal(|ui| {
                    ui.label("手动旋转至");
                    ui.add(
                        egui::DragValue::new(&mut self.manual_rotation_to_angle)
                            .speed(0.1)
                            .suffix("°"),
                    );
                    if ui.button("旋转").clicked() {
                        self.cmd_tx
                            .send(Command::Device(DeviceCommand::RotateTo {
                                steps: (self.manual_rotation_to_angle * 746.0).round() as i32,
                            }))
                            .unwrap();
                        self.manual_rotation_to_angle=0.0;
                    }
                });
            });
        });
        ui.add_enabled_ui(
            self.is_model_ready && self.is_camera_connected && self.is_serial_connected,
            |ui| {
                if !self.is_static_running {
                    if ui.button("寻找旋光零点").clicked() {
                        self.cmd_tx
                            .send(Command::Device(DeviceCommand::FindZeroPoint))
                            .unwrap();
                    }
                } else {
                    if ui.button("停止寻找").clicked() {
                        self.cmd_tx
                            .send(Command::StaticMeasure(StaticMeasureCommand::Stop))
                            .unwrap();
                    }
                }
            },
        );
        if let Some(ang) = self.current_angle {
            ui.label(format!("当前角度: {:.2}°", ang));
        } else {
            ui.label(format!("没有有效零点"));
        }
    }

    fn ui_model_training(&mut self, ui: &mut Ui) {
        ui.heading("视频录制");

        let device_ready = self.is_serial_connected && self.is_camera_connected;

        ui.add_enabled_ui(device_ready, |ui| {
            ui.horizontal(|ui| {
                ui.add_enabled_ui(!self.is_recording, |ui| {
                    ComboBox::from_id_source("combo_mode")
                        .selected_text(&self.recording_mode)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(
                                &mut self.recording_mode,
                                "MAM".to_string(),
                                "明暗明 (MAM)",
                            );
                            ui.selectable_value(
                                &mut self.recording_mode,
                                "AMA".to_string(),
                                "暗明暗 (AMA)",
                            );
                        });
                });

                if !self.is_recording {
                    if ui.button("开始录制").clicked() {
                        if let Some(path) = rfd::FileDialog::new()
                            .add_filter("AVI Video", &["avi"])
                            .set_file_name(&format!(
                                "{}_video.avi",
                                self.recording_mode.to_lowercase()
                            ))
                            .save_file()
                        {
                            self.cmd_tx
                                .send(Command::Device(DeviceCommand::StartRecording {
                                    mode: self.recording_mode.clone(),
                                    save_path: path,
                                }))
                                .unwrap();
                        }
                    }
                } else {
                    if ui.button("停止录制").clicked() {
                        self.cmd_tx
                            .send(Command::Device(DeviceCommand::StopRecording))
                            .unwrap();
                    }
                }
            })
        });

        if self.is_recording {
            ui.label(format!("录制中... {:.1}s", self.recording_elapsed_time));
        } else if !device_ready {
            ui.label("请先连接串口和相机以启用录制功能。");
        }

        ui.separator();
        ui.heading("数据与训练");

        // 使用一个 Group 作为外框
        // ui.group(|ui| {
        ui.label("数据源路径");
        // 使用 Grid 来对齐标签、输入框和状态
        egui::Grid::new("model_inputs_grid")
            .num_columns(3)
            .spacing([20.0, 4.0])
            .striped(true)
            .show(ui, |ui| {
                // 第一行: MAM 视频
                ui.label("MAM 视频:");
                ui.horizontal(|ui| {
                    ui.text_edit_singleline(&mut self.mam_video_path);
                    if ui.button("...").clicked() {
                        if let Some(path) = rfd::FileDialog::new().pick_file() {
                            self.mam_video_path = path.to_string_lossy().to_string();
                            self.cmd_tx
                                .send(Command::Training(TrainingCommand::ProcessVideo {
                                    video_path: self.mam_video_path.clone().into(),
                                    mode: "MAM".to_string(),
                                }))
                                .unwrap();
                        }
                    }
                });
                ui.label(&self.mam_video_status);
                ui.end_row();

                // 第二行: AMA 视频
                ui.label("AMA 视频:");
                ui.horizontal(|ui| {
                    ui.text_edit_singleline(&mut self.ama_video_path);
                    if ui.button("...").clicked() {
                        if let Some(path) = rfd::FileDialog::new().pick_file() {
                            self.ama_video_path = path.to_string_lossy().to_string();
                            self.cmd_tx
                                .send(Command::Training(TrainingCommand::ProcessVideo {
                                    video_path: self.ama_video_path.clone().into(),
                                    mode: "AMA".to_string(),
                                }))
                                .unwrap();
                        }
                    }
                });
                ui.label(&self.ama_video_status);
                ui.end_row();

                // 第三行: 常驻数据集
                ui.label("常驻数据集:");
                ui.horizontal(|ui| {
                    ui.text_edit_singleline(&mut self.dataset_path);
                    if ui.button("...").clicked() {
                        if let Some(path) = rfd::FileDialog::new().pick_folder() {
                            self.dataset_path = path.to_string_lossy().to_string();
                            self.cmd_tx
                                .send(Command::Training(TrainingCommand::LoadPersistentDataset {
                                    path: self.dataset_path.clone().into(),
                                }))
                                .unwrap();
                        }
                    }
                });
                ui.label(&self.persistent_dataset_status);
                ui.end_row();
            });

        // ui.add_space(5.0);

        // 将处理按钮放在 Grid 下方
        // ui.horizontal(|ui| {
        //     if ui.button("从此视频获取MAM数据").clicked() {
        //         self.cmd_tx
        //             .send(Command::Training(TrainingCommand::ProcessVideo {
        //                 video_path: self.mam_video_path.clone().into(),
        //                 mode: "MAM".to_string(),
        //             }))
        //             .unwrap();
        //     }
        //     if ui.button("从此视频获取AMA数据").clicked() {
        //         self.cmd_tx
        //             .send(Command::Training(TrainingCommand::ProcessVideo {
        //                 video_path: self.ama_video_path.clone().into(),
        //                 mode: "AMA".to_string(),
        //             }))
        //             .unwrap();
        //     }
        //     if ui.button("导入常驻数据集").clicked() {
        //         self.cmd_tx
        //             .send(Command::Training(TrainingCommand::LoadPersistentDataset {
        //                 path: self.dataset_path.clone().into(),
        //             }))
        //             .unwrap();
        //     }
        // });
        // });

        ui.separator();

        // --- 后续的训练、保存、加载等 UI 保持不变 ---
        ui.horizontal(|ui| {
            // ui.checkbox(&mut self.train_show_roc, "显示 ROC 曲线");
            ui.checkbox(&mut self.train_show_cm, "显示混淆矩阵");
            if ui.button("训练模型").clicked() {
                self.cmd_tx
                    .send(Command::Training(TrainingCommand::TrainModel {
                        show_roc: self.train_show_roc,
                        show_cm: self.train_show_cm,
                    }))
                    .unwrap();
            };
        });

        ui.label(format!("状态: {}", self.training_status));

        // ui.separator();
        // ui.horizontal(|ui| {
        //     // if ui.button("保存模型").clicked() {
        //     //     if let Some(path) = rfd::FileDialog::new()
        //     //         .add_filter("Model", &["joblib"])
        //     //         .save_file()
        //     //     {
        //     //         self.cmd_tx
        //     //             .send(Command::Training(TrainingCommand::SaveModel { path }))
        //     //             .unwrap();
        //     //     }
        //     // }
        //     // if ui.button("加载模型").clicked() {
        //     //     if let Some(path) = rfd::FileDialog::new()
        //     //         .add_filter("Model", &["joblib", "pkl"])
        //     //         .pick_file()
        //     //     {
        //     //         self.cmd_tx
        //     //             .send(Command::Training(TrainingCommand::LoadModel { path }))
        //     //             .unwrap();
        //     //     }
        //     // }
        //     // if ui.button("导出数据集").clicked() {
        //     //     if let Some(path) = rfd::FileDialog::new().pick_folder() {
        //     //         self.cmd_tx
        //     //             .send(Command::Training(TrainingCommand::ExportDataset { path }))
        //     //             .unwrap();
        //     //     }
        //     // }
        //     if ui.button("重置模型与数据").clicked() {
        //         self.cmd_tx
        //             .send(Command::Training(TrainingCommand::ResetModel))
        //             .unwrap();
        //     }
        // });
    }

    fn ui_static_measurement(&mut self, ui: &mut Ui) {
        if let Some(ang) = self.current_angle {
            ui.label(format!("当前角度: {:.2}°", ang));
        } else {
            ui.label(format!("没有有效零点"));
        }
        ui.separator();
        let device_and_model_ready = self.is_camera_connected
            && self.is_serial_connected
            && self.is_model_ready
            && self.current_angle.is_some();
        ui.horizontal(|ui| {
            ui.add_enabled_ui(
                device_and_model_ready && !self.is_dynamic_exp_running,
                |ui| {
                    if !self.is_static_running {
                        if ui.button("运行精细测量").clicked() {
                            self.cmd_tx
                                .send(Command::StaticMeasure(
                                    StaticMeasureCommand::RunSingleMeasurement,
                                ))
                                .unwrap();
                        }
                    } else {
                        if ui.button("停止精细测量").clicked() {
                            self.cmd_tx
                                .send(Command::StaticMeasure(StaticMeasureCommand::Stop))
                                .unwrap();
                        }
                    }
                },
            );
            ui.label(format!("{}", self.static_measurement_status));
        });

        ui.add_space(10.0);
        // ui.add_enabled_ui(self.is_in_measurement_mode, |ui| {
        //     ui.group(|ui| {
        //         ui.label("测量控制");
        //         ui.horizontal(|ui| {
        //             ui.label("预旋转:");
        //             ui.add(
        //                 egui::DragValue::new(&mut self.static_pre_rotation_angle)
        //                     .speed(0.1)
        //                     .suffix("°"),
        //             );
        //             if ui.button("执行").clicked() {
        //                 self.cmd_tx
        //                     .send(Command::StaticMeasure(StaticMeasureCommand::PreRotate {
        //                         angle: self.static_pre_rotation_angle,
        //                     }))
        //                     .unwrap();
        //             }
        //         });
        //         if ui.button("运行精细测量").clicked() {
        //             self.cmd_tx
        //                 .send(Command::StaticMeasure(
        //                     StaticMeasureCommand::RunSingleMeasurement,
        //                 ))
        //                 .unwrap();
        //         }
        //         ui.label(format!("状态: {}", self.static_measurement_status));
        //         if ui.button("回到零点并退出").clicked() {
        //             self.cmd_tx
        //                 .send(Command::StaticMeasure(StaticMeasureCommand::ReturnToZero))
        //                 .unwrap();
        //         }
        //     });
        // });
        ui.add_space(10.0);
        // ui.heading("结果");
        ui.horizontal(|ui| {
            if ui.button("保存结果").clicked() {
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter("Excel", &["xlsx"])
                    .save_file()
                {
                    self.cmd_tx
                        .send(Command::StaticMeasure(StaticMeasureCommand::SaveResults {
                            path,
                        }))
                        .unwrap();
                }
            }
            if ui.button("清除结果").clicked() {
                self.cmd_tx
                    .send(Command::StaticMeasure(StaticMeasureCommand::ClearResults))
                    .unwrap();
            }
        });

        TableBuilder::new(ui)
            .striped(true)
            .resizable(true)
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .column(Column::auto().at_least(100.0))
            .column(Column::auto().at_least(100.0))
            .column(Column::remainder())
            .header(20.0, |mut h| {
                h.col(|ui| {
                    ui.strong("序号");
                });
                h.col(|ui| {
                    ui.strong("步数");
                });
                h.col(|ui| {
                    ui.strong("角度 (°)");
                });
            })
            .body(|mut body| {
                for r in &self.static_results {
                    body.row(20.0, |mut row| {
                        row.col(|ui| {
                            ui.label(r.index.to_string());
                        });
                        row.col(|ui| {
                            ui.label(r.steps.to_string());
                        });
                        row.col(|ui| {
                            ui.label(format!("{:.2}", r.angle));
                        });
                    });
                }
            });
    }

    fn ui_dynamic_measurement(&mut self, ui: &mut Ui) {
        if let Some(ang) = self.current_angle {
            ui.label(format!("当前角度: {:.2}°", ang));
        } else {
            ui.label(format!("没有有效零点"));
        }
        ui.separator();

        ui.add_enabled_ui(!self.is_dynamic_exp_running, |ui| {
            egui::Grid::new("dynamic_params")
                .num_columns(4)
                .spacing([20.0, 8.0])
                .show(ui, |ui| {
                    ui.label("姓名:");
                    ui.text_edit_singleline(&mut self.dynamic_params.student_name);
                    ui.label("学号:");
                    ui.text_edit_singleline(&mut self.dynamic_params.student_id);
                    ui.end_row();
                    ui.label("实验温度 (°C):");
                    ui.add(egui::DragValue::new(&mut self.dynamic_params.temperature));
                    ui.label("蔗糖浓度 (g/mL):");
                    ui.add(egui::DragValue::new(&mut self.dynamic_params.sucrose_conc));
                    ui.end_row();
                    ui.label("盐酸浓度 (mol/L):");
                    ui.add(egui::DragValue::new(&mut self.dynamic_params.hcl_conc));
                    ui.end_row();
                });
            ui.separator();
            ui.horizontal(|ui| {
                ui.label("步进角度(°):");
                ui.add(egui::DragValue::new(&mut self.dynamic_params.step_angle));
                ui.label("采样点数目:");
                ui.add(egui::DragValue::new(&mut self.dynamic_params.sample_points));
            })
        });

        ui.separator();

        ui.horizontal(|ui| {
            if ui
                .add_enabled(!self.is_dynamic_exp_running, egui::Button::new("开始计时"))
                .clicked()
            {
                self.cmd_tx
                    .send(Command::DynamicMeasure(DynamicMeasureCommand::StartNew))
                    .unwrap();
            }
            ui.add_enabled_ui(
                self.is_camera_connected
                    && self.is_serial_connected
                    && self.is_model_ready
                    && !self.is_static_running
                    && self.current_angle.is_some()
                    && self.start_time.is_some(),
                |ui| {
                    if !self.is_dynamic_exp_running {
                        if ui.button("开始跟踪").clicked() {
                            // self.is_dynamic_exp_running = true;
                            // self.dynamic_results.clear();
                            self.cmd_tx
                                .send(Command::DynamicMeasure(DynamicMeasureCommand::Start {
                                    params: self.dynamic_params.clone(),
                                }))
                                .unwrap();
                        }
                    } else {
                        if ui.button("停止跟踪").clicked() {
                            self.cmd_tx
                                .send(Command::DynamicMeasure(DynamicMeasureCommand::Stop))
                                .unwrap();
                        }
                    }
                },
            );
        });
        ui.horizontal(|ui| {
            if let Some(time) = self.start_time {
                ui.label(format!("{:.2} s", time.elapsed().as_secs_f64()));
            }
            ui.label(format!("{}", self.dynamic_measurement_status));
        });

        // ui.label(format!("当前角度: {:.2}°", self.current_angle));
        ui.separator();
        ui.horizontal(|ui| {
            if ui.button("保存结果").clicked() {
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter("Excel", &["xlsx"])
                    .save_file()
                {
                    self.cmd_tx
                        .send(Command::DynamicMeasure(
                            DynamicMeasureCommand::SaveResults { path },
                        ))
                        .unwrap();
                }
            }
            if ui.button("清除结果").clicked() {
                self.cmd_tx
                    .send(Command::DynamicMeasure(DynamicMeasureCommand::ClearResults))
                    .unwrap();
            }
        });
        TableBuilder::new(ui)
            .striped(true)
            .resizable(true)
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .columns(Column::auto().at_least(100.0), 4)
            .header(20.0, |mut h| {
                h.col(|ui| {
                    ui.strong("序号");
                });
                h.col(|ui| {
                    ui.strong("时间 (s)");
                });
                h.col(|ui| {
                    ui.strong("步数");
                });
                h.col(|ui| {
                    ui.strong("角度 (°)");
                });
            })
            .body(|mut body| {
                for r in &self.dynamic_results {
                    body.row(20.0, |mut row| {
                        row.col(|ui| {
                            ui.label(r.index.to_string());
                        });
                        row.col(|ui| {
                            ui.label(format!("{:.2}", r.time));
                        });
                        row.col(|ui| {
                            ui.label(r.steps.to_string());
                        });
                        row.col(|ui| {
                            ui.label(format!("{:.2}", r.angle));
                        });
                    });
                }
            });
    }

    fn ui_data_processing_controls(&mut self, ui: &mut Ui) {
        // 这里是原来 SidePanel 中的所有内容
        if ui.button("加载数据").clicked() {
            if let Some(path) = rfd::FileDialog::new().pick_file() {
                self.cmd_tx
                    .send(Command::DataProcessing(DataProcessingCommand::LoadData {
                        path: path,
                    }))
                    .unwrap();
            }
        }
        ui.separator();
        ui.horizontal(|ui| {
            ui.label("α_∞(°):");
            if ui.add(DragValue::new(&mut self.alpha_inf)).changed() {
                self.cmd_tx
                    .send(Command::DataProcessing(
                        DataProcessingCommand::SetAlphaInf {
                            alpha: self.alpha_inf,
                        },
                    ))
                    .unwrap();
            }

            // --- MODIFIED: Send command on change ---
            let old_mode = self.regression_mode;

            // 2. 正常绘制 ComboBox，selectable_value 会在用户点击时直接修改 self.regression_mode
            ComboBox::from_label("拟合模式")
                .selected_text(format!("{:?}", self.regression_mode))
                .show_ui(ui, |ui| {
                    ui.selectable_value(
                        &mut self.regression_mode,
                        RegressionMode::Linear,
                        "Δα - t",
                    );
                    ui.selectable_value(&mut self.regression_mode, RegressionMode::Log, "lnΔα - t");
                    ui.selectable_value(
                        &mut self.regression_mode,
                        RegressionMode::Inverse,
                        "1/Δα - t",
                    );
                });

            // 3. 在绘制之后，比较新旧值是否不同
            if self.regression_mode != old_mode {
                // 4. 如果值已改变，则发送命令
                self.cmd_tx
                    .send(Command::DataProcessing(
                        DataProcessingCommand::SetRegressionMode {
                            mode: self.regression_mode,
                        },
                    ))
                    .unwrap();
            }
        });
        ui.separator();

        // 数据表格
        TableBuilder::new(ui)
            .striped(true)
            .resizable(true)
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .columns(Column::auto().at_least(80.0), 4)
            .header(20.0, |mut h| {
                h.col(|ui| {
                    ui.strong("时间");
                });
                h.col(|ui| {
                    ui.strong("步数");
                });
                h.col(|ui| {
                    ui.strong("角度");
                });
                h.col(|ui| {
                    ui.strong("α(t)-α(∞)");
                });
            })
            .body(|mut body| {
                for (time, steps, angle, isok) in self.raw_plot_data.iter() {
                    body.row(20.0, |mut row| {
                        if *isok {
                            row.col(|ui| {
                                ui.label(RichText::new(format!("{:.2}", time)));
                            });
                            row.col(|ui| {
                                ui.label(RichText::new(format!("{}", steps)));
                            });
                            row.col(|ui| {
                                ui.label(RichText::new(format!("{:.2}", angle)));
                            });
                            row.col(|ui| {
                                let diff = angle - self.alpha_inf;
                                ui.label(RichText::new(format!("{:.2}", diff)));
                            });
                        } else {
                            // Use red if invalid
                            let text_color = egui::Color32::RED;
                            row.col(|ui| {
                                ui.label(RichText::new(format!("{:.2}", time)).color(text_color));
                            });
                            row.col(|ui| {
                                ui.label(RichText::new(format!("{}", steps)).color(text_color));
                            });
                            row.col(|ui| {
                                ui.label(RichText::new(format!("{:.2}", angle)).color(text_color));
                            });
                            row.col(|ui| {
                                let diff = angle - self.alpha_inf;
                                ui.label(RichText::new(format!("{:.2}", diff)).color(text_color));
                            });
                        };
                    });
                }
            });
    }

    /// 绘制“数据处理 - 回归图”窗口的内容
    fn ui_data_processing_plot(&mut self, ui: &mut Ui) {
        ui.with_layout(egui::Layout::bottom_up(egui::Align::Center), |ui| {
            // 1. 先添加公式标签。
            //    由于是 bottom-up 布局，它会被放置在可用区域的最底部。
            //    Align::Center 会自动处理水平居中。
            ui.label(&self.regression_formula);
            ui.add_space(4.0); // 在公式和图表之间添加一点间距，更美观

            // 2. 然后添加 Plot 组件。
            //    Plot 是一个“可扩张”的组件，它会自动填充上方所有剩余的空间。
            //    这样就完美地限制了它的尺寸，避免了无限扩张。
            let mode = match self.regression_mode {
                RegressionMode::Linear => "a",
                RegressionMode::Inverse => "1/Δα",
                RegressionMode::Log => "lnΔα",
            };
            Plot::new("data_plot")
                .legend(egui_plot::Legend::default())
                .x_axis_label("t")
                .y_axis_label(mode)
                .y_axis_width(3)
                .show(ui, |plot_ui| {
                    // --- REWRITTEN: Plotting logic is now extremely simple ---

                    // 1. Draw the scatter points from backend data

                    if !self.plot_scatter_points.is_empty() {
                        let points = Points::new(PlotPoints::from(
                            self.plot_scatter_points
                                .iter()
                                .map(|&(x, y)| [x, y])
                                .collect::<Vec<[f64; 2]>>(),
                        ))
                        .name("原始数据")
                        .shape(egui_plot::MarkerShape::Cross)
                        .radius(5.0);

                        plot_ui.points(points);
                    }

                    // 2. Draw the regression line from backend data

                    if !self.plot_line_points.is_empty() {
                        let line = Line::new(PlotPoints::from(
                            self.plot_line_points
                                .iter()
                                .map(|&(x, y)| [x, y])
                                .collect::<Vec<[f64; 2]>>(),
                        ))
                        .name("拟合直线");

                        plot_ui.line(line);
                    }
                });
        });
        // ui.label(&self.regression_formula);
    }
}

// --- 辅助小部件 (Helper Widgets) ---
fn file_path_picker(ui: &mut Ui, label: &str, path_str: &mut String, status: &str) {
    ui.horizontal(|ui| {
        ui.label(label);
        // 让文本框占用更多空间
        let response = ui.text_edit_singleline(path_str);
        if response.lost_focus() { /* do something? */ }

        if ui.button("...").clicked() {
            if let Some(path) = rfd::FileDialog::new().pick_file() {
                *path_str = path.to_string_lossy().to_string();
            }
        }
    });
    ui.label(status);
}

fn dir_path_picker(ui: &mut Ui, label: &str, path_str: &mut String, status: &str) {
    ui.horizontal(|ui| {
        ui.label(label);
        ui.text_edit_singleline(path_str);
        if ui.button("...").clicked() {
            if let Some(path) = rfd::FileDialog::new().pick_folder() {
                *path_str = path.to_string_lossy().to_string();
            }
        }
    });
    if !status.is_empty() {
        ui.label(status);
    }
}
fn save_file_picker(
    ui: &mut egui::Ui,
    label: &str,
    path_str: &mut String,
    filter_name: &str,
    filter_extensions: &[&str],
) {
    ui.horizontal(|ui| {
        ui.label(label);
        ui.text_edit_singleline(path_str);
        if ui.button("...").clicked() {
            // 创建一个用于“保存文件”的对话框
            let dialog = rfd::FileDialog::new()
                // 添加文件类型过滤器，这会帮助用户保存为正确的格式
                .add_filter(filter_name, filter_extensions);

            // 打开对话框并等待用户选择
            if let Some(path) = dialog.save_file() {
                // 如果用户确认了一个路径，则更新我们的字符串状态
                *path_str = path.to_string_lossy().to_string();
            }
        }
    });
}
