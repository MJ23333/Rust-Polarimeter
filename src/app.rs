// src/app.rs

// 假设此模块定义了所有与后端通信所需的 Command 和 Update 枚举
// For standalone compilation, you would need to provide dummy definitions.
use crate::communication::{self, *};
use crossbeam_channel::{Receiver, Sender};
use egui::{
    CentralPanel, Color32, ComboBox, DragValue, Frame, RichText, Stroke, TopBottomPanel, Ui,
};
use egui_extras::{Column, TableBuilder};
use egui_plot::{Line, Plot, PlotPoints, Points};
use egui::{Rect, Pos2, Vec2}; // 新增：导入 Rect, Pos2, Vec2
use std::sync::Arc;
use std::thread;
use std::collections::VecDeque;
use tracing::Level;

// 新增：用于管理左侧主工作区当前显示的标签页
#[derive(PartialEq, Clone, Copy)]
enum Tab {
    Welcome, // 新增欢迎页
    DeviceControl,
    ModelTraining,
    StaticMeasurement,
    DynamicMeasurement,
    DataProcessing,
}

pub struct PolarimeterApp {
    // --- 通信 ---
    cmd_tx: Sender<Command>,
    update_rx: Receiver<Update>,
    backend_handle: Option<thread::JoinHandle<()>>,
    log_buffer: VecDeque<communication::LogMessage>,

    // --- UI 核心状态 ---
    active_tab: Tab, // 当前激活的标签页

    // --- 通用 UI 状态 ---
    status_message: String,
    cm_data: Option<ConfusionMatrixData>,
    roc_data: Option<RocCurveData>,
    is_plots_window_open: bool, // 训练结果评估窗口仍然可以是一个独立的弹出窗口

    // --- 窗口 1: 设备控制 (状态移至监视器, 控制逻辑在标签页) ---
    serial_ports: Vec<String>,
    selected_serial_port: String,
    is_serial_connected: bool,
    rotation_direction_is_ama: bool,
    rotation_direction_reverse: bool,
    manual_rotation_angle: f32,
    manual_rotation_to_angle: f32,
    current_angle: Option<f32>,

    // --- 相机 (状态和控制移至监视器) ---
    camera_list: Vec<String>,
    selected_camera_idx: usize,
    is_camera_connected: bool,
    camera_texture: Option<egui::TextureHandle>,
    camera_image: Option<Arc<egui::ColorImage>>,
    exposure: f32,
    min_radius: u32,
    max_radius: u32,
    camera_lock_circle: bool,
    camera_view_rect: Option<Rect>, // 用 Rect 存储当前视图的范围 (uv-coordinates)
    is_dragging_camera_view: bool,   // 标记是否正在拖动视图

    // --- 录制 (控制在模型训练标签页) ---
    is_recording: bool,
    recording_elapsed_time: f32,
    recording_mode: String, // "MAM" or "AMA"
    recording_angle: f32,

    // --- 窗口 2: 模型训练 ---
    recorded_dataset_path: String,
    ama_video_path: String,
    dataset_path: String,
    mam_video_status: String,
    ama_video_status: String,
    persistent_dataset_status: String,
    training_status: String,
    is_model_ready: bool,
    train_show_roc: bool,
    train_show_cm: bool,

    // --- 窗口 3: 静态测量 ---
    is_static_running: bool,
    static_pre_rotation_angle: f32,
    static_measurement_status: String,
    static_results: Vec<StaticResult>,

    // --- 窗口 4: 动态测量 ---
    dynamic_params: DynamicExpParams,
    dynamic_save_path: String,
    dynamic_measurement_status: String,
    dynamic_results: Vec<DynamicResult>,
    is_dynamic_exp_running: bool,
    start_time: Option<std::time::Instant>,

    // --- 窗口 5: 数据处理 ---
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

    /// 主更新循环，实现新的 "标签页 + 监视器" 布局
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // 1. 优先处理所有后端消息和相机图像更新
        self.handle_backend_updates();
        if let Some(image) = self.camera_image.take() {
            let texture = ctx.load_texture("camera_feed", image, Default::default());
            self.camera_texture = Some(texture);
        }

        // 2. 绘制底部固定的状态栏
        // 2. 绘制贯通顶部的标签栏
        TopBottomPanel::top("main_top_bar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.active_tab, Tab::Welcome, "0. 欢迎");
                ui.selectable_value(&mut self.active_tab, Tab::DeviceControl, "1. 设备控制");
                ui.selectable_value(&mut self.active_tab, Tab::ModelTraining, "2. 模型训练");
                ui.selectable_value(&mut self.active_tab, Tab::StaticMeasurement, "3. 静态测量");
                ui.selectable_value(&mut self.active_tab, Tab::DynamicMeasurement, "4. 动态测量");
                ui.selectable_value(&mut self.active_tab, Tab::DataProcessing, "5. 数据处理");
            });
        });
        TopBottomPanel::bottom("bottom_panel").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label("v2.0.0-Rust (Tabbed Layout)");
                });
            });
        });

        // 3. 根据当前激活的标签页，选择合适的布局
        if self.active_tab == Tab::Welcome {
            // 如果是欢迎页，则只显示一个占满全页的中央面板 (不变)
            CentralPanel::default().show(ctx, |ui| {
                self.draw_welcome_tab(ui);
            });
        } else {
            // 对于其他所有页面，使用固定的 50/50 分栏布局
            let panel_width = ctx.available_rect().width() * 0.5;

            if self.active_tab == Tab::DataProcessing {
                // “数据处理”页的特殊布局
                self.draw_data_processing_layout(ctx, panel_width);
            } else {
                // 标准布局
                egui::SidePanel::right("monitor_panel")
                    .exact_width(panel_width) // 精确设置宽度为50%
                    .resizable(false)       // 禁用拖动
                    .show(ctx, |ui| {
                        self.draw_monitor_panel(ui);
                    });

                CentralPanel::default().show(ctx, |ui| {
                    self.draw_main_workspace(ui);
                });
            }
        }

        // (可选) 独立的模型评估结果窗口
        // self.show_plots_window(ctx);

        // 4. 请求重绘以保持UI流畅 (相机画面等)
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
            log_buffer: VecDeque::with_capacity(100),
            backend_handle,
            active_tab: Tab::Welcome, // 默认打开第一个标签页
            status_message: "欢迎使用!".to_string(),
            is_plots_window_open: false,
            recording_angle: 15.0,
            // ... 其他所有字段的默认值和原先保持一致 ...
            cm_data: None,
            roc_data: None,
            serial_ports: vec!["刷新中...".to_string()],
            selected_serial_port: "".to_string(),
            is_serial_connected: false,
            rotation_direction_is_ama: false,
            rotation_direction_reverse: false,
            manual_rotation_angle: 0.0,
            manual_rotation_to_angle: 0.0,
            current_angle: None,
            camera_list: vec!["刷新中...".to_string()],
            selected_camera_idx: 0,
            is_camera_connected: false,
            camera_texture: None,
            camera_image: None,
            camera_view_rect: None, // 初始为空，连接相机后设置
            is_dragging_camera_view: false,
            exposure: -8.0,
            min_radius: 30,
            max_radius: 45,
            camera_lock_circle: false,
            is_recording: false,
            recording_elapsed_time: 0.0,
            recording_mode: "MAM".to_string(),
            recorded_dataset_path: String::new(),
            ama_video_path: String::new(),
            dataset_path: String::new(),
            mam_video_status: "未导入".to_string(),
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
            start_time: None,
            data_import_path: String::new(),
            alpha_inf: 0.0,
            regression_mode: RegressionMode::Log,
            regression_formula: String::new(),
            raw_plot_data: Arc::new(Vec::new()),
            plot_scatter_points: Vec::new(),
            plot_line_points: Vec::new(),
        }
    }

    /// 处理所有来自后端的待处理更新 (此函数逻辑不变)
    fn handle_backend_updates(&mut self) {
        while let Ok(update) = self.update_rx.try_recv() {
            match update {
                Update::General(update) => match update {
                    GeneralUpdate::StatusMessage(msg) => self.status_message = msg,
                    GeneralUpdate::Error(err_msg) => {
                        self.status_message = format!("错误: {}", err_msg);
                    }
                    GeneralUpdate::NewLog(log_line) => { // <--- 新增的处理分支
                    self.log_buffer.push_back(log_line);
                    // 如果日志超过100条，就从前面移除旧的
                    if self.log_buffer.len() > 100 {
                        self.log_buffer.pop_front();
                    }
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
                            // self.is_plots_window_open = true; // 自动打开评估结果窗口
                        }
                    }
                    TrainingUpdate::PersistentDatasetStatus(msg) => {
                        self.persistent_dataset_status = msg
                    }
                    TrainingUpdate::MAMDatasetStatus(msg) => self.mam_video_status = msg,
                    TrainingUpdate::AMADatasetStatus(msg) => self.ama_video_status = msg,
                },
                Update::Measurement(update) => match update {
                    MeasurementUpdate::StaticStatus(msg) => {
                        self.static_measurement_status = msg.clone();
                        self.status_message = msg;
                    }
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
                        self.plot_scatter_points = state.plot_scatter_points;
                        self.plot_line_points = state.plot_line_points;
                    }
                },
            }
        }
    }

    // ===================================================================================
    //  新布局的绘制函数
    // ===================================================================================

    /// 绘制右侧的监视面板
    fn draw_welcome_tab(&mut self, ui: &mut Ui) {
        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(ui.available_height() * 0.1); // 顶部留白

                let welcome_text = RichText::new(
                    r#"欢迎使用旋光仪控制软件 v2.1.0
本软件专为蔗糖水解反应动力学实验设计，旨在提供一个清晰、高效、一体化的操作与数据分析平台。

请遵循顶部标签页的引导，按以下顺序完成实验：
1.  设备控制: 连接并检查旋光仪电机与相机硬件。
2.  模型训练: (首次使用或更换环境时) 录制视频并训练用于识别旋光状态的 AI 模型。
3.  动态测量: 填入实验参数，开始自动化的数据采集流程。
4.  数据处理: 导入实验数据，进行一级反应动力学拟合与分析。

祝您实验顺利！"#,
                )
                .heading()
                .line_height(Some(32.0));

                ui.label(welcome_text); // 限制文本最大宽度，使其在宽屏上更易读
            });
        });
    }
    fn draw_monitor_panel(&mut self, ui: &mut Ui) {
        // 该函数现在负责管理自己的内部布局，而不是依赖外部滚动条
        // --- 1. 顶部区域：状态清单 (固定高度) ---
        egui::TopBottomPanel::top("monitor_top_panel").frame(egui::Frame::none()).show_inside(ui, |ui| {
            ui.heading("监视与状态");
            ui.separator();
            ui.label(RichText::new("准备清单").strong());
            ui.group(|ui| {
                ui.set_width(ui.available_width()); // 占满宽度
                let serial_status_text = if self.is_serial_connected {
                    RichText::new("✅ 串口电机: 已连接").color(Color32::GREEN)
                } else {
                    RichText::new("❌ 串口电机: 未连接").color(Color32::LIGHT_RED)
                };
                ui.label(serial_status_text);

                let camera_status_text = if self.is_camera_connected {
                    RichText::new("✅ 相机: 已连接").color(Color32::GREEN)
                } else {
                    RichText::new("❌ 相机: 未连接").color(Color32::LIGHT_RED)
                };
                ui.label(camera_status_text);

                let model_status_text = if self.is_model_ready {
                    RichText::new("✅ 识别模型: 已就绪").color(Color32::GREEN)
                } else {
                    RichText::new("❌ 识别模型: 未就绪").color(Color32::LIGHT_RED)
                };
                ui.label(model_status_text);
            });
            ui.add_space(5.0);
        });

        // --- 3. 底部区域：圆圈设定和日志 (固定高度) ---
        // 注意：底部面板的控件需要按“从下到上”的顺序添加
        egui::TopBottomPanel::bottom("monitor_bottom_panel").frame(egui::Frame::none()).show_inside(ui, |ui| {
            // --- 日志 (最底部) ---

            // --- 圆圈设定 (在日志上面) ---
            ui.add_space(5.0);
            ui.label(RichText::new("识别设定").strong());
            ui.group(|ui| {
                ui.set_width(ui.available_width()); // 占满宽度
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
                    egui::Slider::new(&mut self.max_radius, self.min_radius..=200)
                        .text("最大圆半径"),
                );
                if min_radius_slider.changed() || max_radius_slider.changed() {
                    self.cmd_tx
                        .send(Command::Camera(CameraCommand::SetHoughCircleRadius {
                            min: self.min_radius,
                            max: self.max_radius,
                        }))
                        .unwrap();
                }
            });
            ui.add_space(5.0);
            ui.label(RichText::new("日志").strong());
            Frame::group(ui.style()).show(ui, |ui|{
        ui.set_height(120.0); // 可以适当增加高度
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .stick_to_bottom(true) // 自动滚动到底部，显示最新日志
            .show(ui, |ui| {
                // 从后往前迭代，这样最新的日志显示在最下方
                // let log_text = self.log_buffer.iter().cloned().collect::<Vec<_>>().join("\n");
                // ui.label(RichText::new(log_text).monospace().size(12.0));
                for log in &self.log_buffer {
                    draw_log_message(ui,log); 
                }
            });
    });
        });

        // --- 2. 中间区域：相机画面 (自动填充所有剩余空间) ---
        egui::CentralPanel::default()
            .frame(Frame::none()) // 中间区域本身不需要边框
            .show_inside(ui, |ui| {
                ui.label(RichText::new("实时画面").strong());
                // 使用 Frame::canvas 来给相机画面添加边框和背景
                let camera_frame =
                    Frame::canvas(ui.style()).stroke(ui.style().visuals.window_stroke);

                camera_frame.show(ui, |ui| {
                    // 这个 Frame 会自动填充 CentralPanel 的所有空间
                    ui.set_width(ui.available_width());
                    ui.set_height(ui.available_height());

                    if self.camera_texture.is_some() && self.is_camera_connected {
                        let texture = self.camera_texture.as_ref().unwrap();
                        
                        // let img = egui::Image::new(texture)
                        //     .maintain_aspect_ratio(true)
                        //     .max_size(ui.available_size()); // 图像大小适应可用空间
                        // ui.centered_and_justified(|ui| {
                        //     ui.add(img);
                        // });
                        // 步骤 1: 初始化或重置视图矩形
                        // 如果 camera_view_rect 未初始化，则设置为覆盖整个图像 (UV坐标从[0,0]到[1,1])
                        if self.camera_view_rect.is_none() {
                            self.camera_view_rect = Some(Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)));
                        }
                        let mut view_rect = self.camera_view_rect.unwrap();

                        // 步骤 2: 分配UI空间并感知交互
                        let response = ui.allocate_response(ui.available_size(), egui::Sense::drag());
                        let screen_rect = response.rect;

                        // 步骤 3: 处理滚轮缩放
                        if response.hovered() {
                            let mut scroll_delta = 0.0;
                            ui.input(|i| {
                                for event in &i.events {
                                    if let egui::Event::Scroll(delta) = event {
                                        scroll_delta = delta.y;
                                    }
                                }
                            });

                            if scroll_delta != 0.0 {
                                let zoom_factor = if scroll_delta > 0.0 { 0.9 } else { 1.1 };
                                if let Some(pointer_pos) = response.hover_pos() {
                                    // a. 计算指针在UV坐标系中的绝对位置 (这部分是正确的)
                                    let pointer_offset_in_pixels = pointer_pos - screen_rect.min;
                                    let uv_per_pixel = view_rect.size() / screen_rect.size();
                                    let pointer_offset_in_uv = pointer_offset_in_pixels * uv_per_pixel;
                                    let pointer_in_uv = view_rect.min + pointer_offset_in_uv;

                                    // **【核心逻辑修正】**
                                    // b. 计算新的视图尺寸
                                    let new_size = view_rect.size() * zoom_factor;
                                    
                                    // c. 计算从旧视图左上角到鼠标指针的向量，并按比例缩放
                                    let old_min_to_pointer_vec = pointer_in_uv - view_rect.min;
                                    let new_min_to_pointer_vec = old_min_to_pointer_vec * zoom_factor;
                                    
                                    // d. 新的左上角 = 鼠标指针位置 - 新的缩放后向量
                                    // 这确保了鼠标指针 `pointer_in_uv` 在缩放前后，其UV坐标不变
                                    let new_min = pointer_in_uv - new_min_to_pointer_vec;
                                    
                                    // e. 使用新的左上角和尺寸创建视图矩形
                                    view_rect = Rect::from_min_size(new_min, new_size);
                                }
                            }
                        }

                        // 步骤 4: 处理拖动平移
                        if response.dragged() {
                            let drag_delta_in_pixels = response.drag_delta();
                            let uv_per_pixel = view_rect.size() / screen_rect.size();
                            let drag_delta_in_uv = drag_delta_in_pixels * uv_per_pixel;
                            
                            // **【修正】** 使用 .translated() 方法进行平移，兼容所有版本
                            // 注意拖动方向与平移方向相反，所以用负的 delta
                            view_rect = view_rect.translate(-drag_delta_in_uv);
                        }

                        // 步骤 5: 限制 view_rect 的边界
                        let mut new_size = view_rect.size();
                        new_size.x = new_size.x.min(1.0);
                        new_size.y = new_size.y.min(1.0);
                        let min_zoom_level = 0.05;
                        new_size.x = new_size.x.max(min_zoom_level);
                        new_size.y = new_size.y.max(min_zoom_level);
                        view_rect = Rect::from_center_size(view_rect.center(), new_size);
                        
                        let mut offset = Vec2::ZERO;
                        if view_rect.min.x < 0.0 { offset.x = -view_rect.min.x; }
                        if view_rect.min.y < 0.0 { offset.y = -view_rect.min.y; }
                        if view_rect.max.x > 1.0 { offset.x = 1.0 - view_rect.max.x; }
                        if view_rect.max.y > 1.0 { offset.y = 1.0 - view_rect.max.y; }
                        
                        // **【修正】** 使用 .translated() 方法施加边界修正
                        view_rect = view_rect.translate(offset);

                        self.camera_view_rect = Some(view_rect);

                        // 步骤 6: 绘制图像
                        let image = egui::Image::new(texture)
                            .uv(view_rect)
                            .max_size(screen_rect.size())
                            .maintain_aspect_ratio(true);
                        
                        ui.put(screen_rect, image);

                    } else {
                        ui.centered_and_justified(|ui| {
                            ui.label("[无相机信号]");
                        });
                        self.camera_view_rect = None;
                    }
                });
            });
    }

    /// 绘制左侧的主工作区 (标签页导航 + 内容)
    fn draw_main_workspace(&mut self, ui: &mut Ui) {
        // --- 标签页导航栏 ---

        // --- 根据当前标签页绘制对应内容 ---
        match self.active_tab {
            Tab::DeviceControl => self.draw_device_control_tab(ui),
            Tab::ModelTraining => self.draw_model_training_tab(ui),
            Tab::StaticMeasurement => self.draw_static_measurement_tab(ui),
            Tab::DynamicMeasurement => self.draw_dynamic_measurement_tab(ui),
            // Welcome 和 DataProcessing 在此函数外处理，这里无需匹配
            _ => {}
        }
    }

    fn draw_data_processing_layout(&mut self, ctx: &egui::Context, panel_width: f32) {
        // 右侧面板用于绘图
        egui::SidePanel::right("data_processing_plot")
            .exact_width(panel_width) // 精确设置宽度为50%
            .resizable(false)       // 禁用拖动
            .show(ctx, |ui| {
                ui.heading("回归图");
                self.ui_data_processing_plot(ui);
            });

        // 中央区域 (左侧) 用于控制和数据显示
        CentralPanel::default().show(ctx, |ui| {
            // --- 标签页导航栏 (已移除) ---

            ui.heading("数据处理与分析");
            self.ui_data_processing_controls(ui);
        });
    }

    // ===================================================================================
    //  各标签页内容的绘制函数 (由旧的 ui_* 函数改造而来)
    // ===================================================================================

    fn draw_device_control_tab(&mut self, ui: &mut Ui) {
        ui.heading("设备连接与手动控制");
        egui::ScrollArea::vertical().show(ui, |ui| {
            // --- 串口连接 ---
            ui.add_space(5.0);
            ui.label(RichText::new("串口电机连接").strong());
            ui.horizontal(|ui| {
                let selected_text = self.selected_serial_port.clone();
                egui::ComboBox::from_id_source("serial_select")
                    .selected_text(selected_text)
                    .show_ui(ui, |ui| {
                        for port in &self.serial_ports {
                            ui.selectable_value(&mut self.selected_serial_port, port.clone(), port);
                        }
                    });

                if ui.button("刷新").clicked() {
                    self.cmd_tx
                        .send(Command::Device(DeviceCommand::RefreshSerialPorts))
                        .unwrap();
                }

                if self.is_serial_connected {
                    if ui.button("断开").clicked() {
                        self.cmd_tx
                            .send(Command::Device(DeviceCommand::DisconnectSerial))
                            .unwrap();
                    }
                    if ui.button("测试").clicked() {
                        self.cmd_tx
                            .send(Command::Device(DeviceCommand::TestSerial))
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

            // --- 相机连接 ---
            ui.label(RichText::new("相机连接").strong());
            ui.horizontal(|ui| {
                let selected_text = self
                    .camera_list
                    .get(self.selected_camera_idx)
                    .cloned()
                    .unwrap_or_else(|| "N/A".to_string());
                egui::ComboBox::from_id_source("camera_select")
                    .selected_text(selected_text)
                    .show_ui(ui, |ui| {
                        for (i, cam) in self.camera_list.iter().enumerate() {
                            ui.selectable_value(&mut self.selected_camera_idx, i, cam);
                        }
                    });

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

            // --- 电机参数与控制 ---
            ui.add_space(10.0);
            ui.label(RichText::new("电机参数设定").strong());
            ui.horizontal(|ui| {
                ui.label("正值对应:");
                if ui
                    .radio_value(&mut self.rotation_direction_is_ama, false, "明暗明 (MAM)")
                    .changed()
                    || ui
                        .radio_value(&mut self.rotation_direction_is_ama, true, "暗明暗 (AMA)")
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
            ui.add_space(10.0);

            ui.label(RichText::new("手动控制").strong());
            ui.add_enabled_ui(self.is_serial_connected, |ui| {
                ui.horizontal(|ui| {
                    ui.label("手动旋转");
                    ui.add(
                        egui::DragValue::new(&mut self.manual_rotation_angle)
                            .speed(0.1)
                            .suffix("°").clamp_range(-10.0..=10.0),
                    );
                    if ui.button("旋转").clicked() {
                        self.cmd_tx
                            .send(Command::Device(DeviceCommand::RotateMotor {
                                steps: (self.manual_rotation_angle * 746.0).round() as i32,
                            }))
                            .unwrap();
                        self.manual_rotation_angle = 0.0;
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
                            // self.manual_rotation_to_angle = 0.0;
                        }
                    });
                });
            });
            ui.add_space(10.0);

            ui.label(RichText::new("自动零点校准").strong());
            ui.add_enabled_ui(
                self.is_model_ready && self.is_camera_connected && self.is_serial_connected,
                |ui| {
                    if !self.is_static_running {
                        // 借用 is_static_running 状态
                        
                        if ui.button("寻找旋光零点").clicked() {
                            self.cmd_tx
                                .send(Command::Device(DeviceCommand::FindZeroPoint))
                                .unwrap();
                        }
                        ui.label("（请移出零点附近再点击寻找零点）");
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
                ui.label("当前角度: 未知 (请先校准零点)");
            }
        });
    }

    fn draw_model_training_tab(&mut self, ui: &mut Ui) {
        // 此函数内容基本与原 ui_model_training 一致
        ui.heading("分类模型训练");
        ui.label(RichText::new("手动控制").strong());
            ui.add_enabled_ui(self.is_serial_connected, |ui| {
                ui.horizontal(|ui| {
                    ui.label("手动旋转");
                    ui.add(
                        egui::DragValue::new(&mut self.manual_rotation_angle)
                            .speed(0.1)
                            .suffix("°").clamp_range(-10.0..=10.0),
                    );
                    if ui.button("旋转").clicked() {
                        self.cmd_tx
                            .send(Command::Device(DeviceCommand::RotateMotor {
                                steps: (self.manual_rotation_angle * 746.0).round() as i32,
                            }))
                            .unwrap();
                        self.manual_rotation_angle = 0.0;
                    }
                });
            });
        ui.label(RichText::new("视频录制").strong());

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
                ui.label("每次录制旋转：");
                ui.add(
                            egui::DragValue::new(&mut self.recording_angle)
                                .speed(0.1)
                                .suffix("°"),
                );
                if !self.is_recording {
                    if ui.button("开始录制").clicked() {
                        if let Some(path) = rfd::FileDialog::new().pick_folder()
                        {
                            self.cmd_tx
                                .send(Command::Device(DeviceCommand::StartRecording {
                                    mode: self.recording_mode.clone(),
                                    save_path: path,
                                    num: (self.recording_angle*746.0).round() as i32
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
        ui.label(RichText::new("数据集加载").strong());
        // 使用 Grid 来对齐标签、输入框和状态
        egui::Grid::new("model_inputs_grid")
            .num_columns(3)
            .spacing([20.0, 4.0])
            .striped(true)
            .show(ui, |ui| {
                // 第一行: MAM 视频
                ui.label("录制数据集:");
                ui.horizontal(|ui| {
                    ui.text_edit_singleline(&mut self.recorded_dataset_path);
                    if ui.button("...").clicked() {
                        if let Some(path) = rfd::FileDialog::new().pick_folder() {
                            self.recorded_dataset_path = path.to_string_lossy().to_string();
                            self.cmd_tx
                                .send(Command::Training(TrainingCommand::LoadRecordedDataset { 
                                    path: self.recorded_dataset_path.clone().into(),
                                }))
                                .unwrap();
                        }
                    }
                    if ui.button("重置").clicked() {
                        // if let Some(path) = rfd::FileDialog::new().pick_folder() {
                            self.recorded_dataset_path = "".to_string();
                            self.mam_video_status="未导入".to_string();
                            self.cmd_tx
                                .send(Command::Training(TrainingCommand::ResetRecordedDataset))
                                .unwrap();
                        // }
                    }
                });
                ui.label(&self.mam_video_status);
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
                    if ui.button("重置").clicked() {
                        // if let Some(path) = rfd::FileDialog::new().pick_folder() {
                            self.dataset_path = "".to_string();
                            self.persistent_dataset_status="未导入".to_string();
                            self.cmd_tx
                                .send(Command::Training(TrainingCommand::ResetPersistentDataset))
                                .unwrap();
                        // }
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
        ui.label(RichText::new("训练设置").strong());
        // --- 后续的训练、保存、加载等 UI 保持不变 ---
        ui.horizontal(|ui| {
            // ui.checkbox(&mut self.train_show_roc, "显示 ROC 曲线");
            
            if ui.button("训练模型").clicked() {
                self.cmd_tx
                    .send(Command::Training(TrainingCommand::TrainModel {
                        show_roc: self.train_show_roc,
                        show_cm: self.train_show_cm,
                    }))
                    .unwrap();
            };
        });

        // ui.label(format!("状态: {}", self.training_status));
        if let Some(cm) = &self.cm_data {
            ui.add_space(15.0);
            ui.separator();
            ui.add_space(10.0);

            ui.label(RichText::new("训练结果").strong());
            ui.label(format!("整体准确度: {:.2}%", cm.accuracy * 100.0));

            egui::Grid::new("cm_grid_inline").show(ui, |ui| {
                ui.label("");
                ui.label(RichText::new("预测为 MAM").strong());
                ui.label(RichText::new("预测为 AMA").strong());
                ui.end_row();

                ui.label(RichText::new("实际为 MAM").strong());
                ui.label(cm.matrix[0][0].to_string());
                ui.label(cm.matrix[0][1].to_string());
                ui.end_row();

                ui.label(RichText::new("实际为 AMA").strong());
                ui.label(cm.matrix[1][0].to_string());
                ui.label(cm.matrix[1][1].to_string());
                ui.end_row();
            });
        }
    
    }

    fn draw_static_measurement_tab(&mut self, ui: &mut Ui) {
        // 此函数内容基本与原 ui_static_measurement 一致
        ui.heading("样品静态测量");
        ui.label(RichText::new("电机状态").strong());
        if let Some(ang) = self.current_angle {
            ui.label(format!("当前角度: {:.2}°", ang));
        } else {
            ui.label(format!("没有有效零点"));
        }
        ui.separator();
        ui.label(RichText::new("手动控制").strong());
            ui.add_enabled_ui(self.is_serial_connected, |ui| {
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
                            // self.manual_rotation_to_angle = 0.0;
                        }
                    });
                });
            });
        ui.separator();
        ui.label(RichText::new("静态测量设置").strong());
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
        ui.label(RichText::new("测量结果").strong());
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
            // .resizable(true)
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

    fn draw_dynamic_measurement_tab(&mut self, ui: &mut Ui) {
        // 此函数内容基本与原 ui_dynamic_measurement 一致
        ui.heading("动态测量");
        ui.label(RichText::new("电机状态").strong());
        if let Some(ang) = self.current_angle {
            ui.label(format!("当前角度: {:.2}°", ang));
        } else {
            ui.label(format!("没有有效零点"));
        }
        ui.separator();
        ui.label(RichText::new("手动控制").strong());
            ui.add_enabled_ui(self.is_serial_connected, |ui| {
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
                            // self.manual_rotation_to_angle = 0.0;
                        }
                    });
                });
            });
        ui.separator();
        ui.label(RichText::new("参数设置").strong());
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
        ui.label(RichText::new("动态测量控制").strong());
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
        ui.label(RichText::new("测量结果").strong());
        ui.horizontal(|ui| {
            if ui.button("保存结果").clicked() {
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter("Excel", &["xlsx"])
                    .save_file()
                {
                    self.cmd_tx
                        .send(Command::DynamicMeasure(
                            DynamicMeasureCommand::SaveResults { path ,params:self.dynamic_params.clone()},
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
            // .resizable(true)
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

    // ===================================================================================
    //  数据处理页面所需的具体UI函数 (基本不变)
    // ===================================================================================

    fn ui_data_processing_controls(&mut self, ui: &mut Ui) {
        // 此函数内容与原 ui_data_processing_controls 一致
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
        ui.label(RichText::new("数据").strong());
        // 数据表格
        TableBuilder::new(ui)
            .striped(true)
            // .resizable(true)
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
                            let text_color = egui::Color32::LIGHT_RED;
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
    }

    // ===================================================================================
    //  独立的模型评估结果窗口 (基本不变)
    // ===================================================================================

    // fn show_plots_window(&mut self, ctx: &egui::Context) {
    //     // 这个窗口由后端数据驱动，当有新结果时 is_plots_window_open 会被设为 true
    //     egui::Window::new("训练评估结果")
    //         .open(&mut self.is_plots_window_open)
    //         .vscroll(true)
    //         .resizable(true)
    //         .default_width(400.0)
    //         .show(ctx, |ui| {
    //             if let Some(cm) = &self.cm_data {
    //                 ui.heading("混淆矩阵 (Confusion Matrix)");
    //                 ui.label(format!("整体准确度: {:.2}%", cm.accuracy * 100.0));

    //                 egui::Grid::new("cm_grid").show(ui, |ui| {
    //                     ui.label("");
    //                     ui.label("预测为 0 (MAM)");
    //                     ui.label("预测为 1 (AMA)");
    //                     ui.end_row();
    //                     ui.label("实际为 0 (MAM)");
    //                     ui.label(cm.matrix[0][0].to_string());
    //                     ui.label(cm.matrix[0][1].to_string());
    //                     ui.end_row();
    //                     ui.label("实际为 1 (AMA)");
    //                     ui.label(cm.matrix[1][0].to_string());
    //                     ui.label(cm.matrix[1][1].to_string());
    //                     ui.end_row();
    //                 });
    //                 ui.separator();
    //             }

    //             if let Some(_roc) = &self.roc_data {
    //                 ui.heading("ROC 曲线");
    //                 // ... egui_plot logic ...
    //             }
    //         });
    // }
}
/// 这是一个兼容旧版 egui 的辅助函数，
/// 它使用 horizontal 布局来将多个 RichText 放在同一行。
fn draw_log_message(ui: &mut Ui, log: &LogMessage) {
    let (level_str, color) = level_to_style(log.level);

    let layout_response = ui.horizontal_wrapped(|ui| {
        // 让 UI 元素更紧凑一些
        ui.style_mut().spacing.item_spacing.x = 4.0;

        // 1. (可选) 显示时间戳

        // 2. 【新增】显示日志来源 (target)
        // 我们给它一个柔和的、不同于其他元素的颜色，比如青色或灰色

        // 3. 显示日志级别
        ui.label(
            RichText::new(format!("[{}]", level_str))
                .color(color)
                .monospace()
        );

        // 4. 显示日志消息
        ui.label(
            RichText::new(&log.message)
                .monospace()
        );
                ui.label(
            RichText::new(format!("({})",&log.target))
                .color(Color32::from_rgb(100, 160, 180)) // 柔和的青色
                .monospace()
        );
    }).response;

    // 悬停文本依然显示完整的 UTC 时间
    layout_response.on_hover_text(
        format!("Timestamp: {}\nTarget: {}", 
            log.timestamp.format("%Y-%m-%d %H:%M:%S %Z").to_string(),
            log.target
        )
    );
}

// level_to_style 函数保持不变
fn level_to_style(level: Level) -> (&'static str, Color32) {
    match level {
        Level::ERROR => ("ERROR", Color32::from_rgb(255, 80, 80)),
        Level::WARN => ("WARN", Color32::from_rgb(255, 215, 0)),
        Level::INFO => ("INFO", Color32::from_rgb(0, 192, 255)),
        Level::DEBUG => ("DEBUG", Color32::from_rgb(128, 128, 128)),
        Level::TRACE => ("TRACE", Color32::from_rgb(150, 100, 200)),
    }
}
