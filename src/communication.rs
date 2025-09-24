// =======================================================================
// src/communication.rs
// =======================================================================

use std::path::PathBuf;
use std::sync::Arc;
use egui::ColorImage;
use serde::{Deserialize, Serialize};
use tracing::Level;
use chrono::{DateTime, Utc};
//======================================================================
//  命令: Frontend -> Backend
//======================================================================

#[derive(Debug, Clone)]
pub enum Command {
    General(GeneralCommand),
    Device(DeviceCommand),
    Camera(CameraCommand),
    Training(TrainingCommand),
    StaticMeasure(StaticMeasureCommand),
    DynamicMeasure(DynamicMeasureCommand),
    DataProcessing(DataProcessingCommand),
}

#[derive(Debug, Clone)]
pub enum GeneralCommand {
    Shutdown,
}

#[derive(Debug, Clone)]
pub enum DeviceCommand {
    RefreshSerialPorts,
    ConnectSerial { port: String, baud_rate: u32 },
    DisconnectSerial,
    TestSerial,
    SetRotationDirection(bool), // true for AMA, false for MAM
    SetRotationReverse(bool),
    RotateMotor { steps:i32 },
    RotateTo { steps:i32 },
    FindZeroPoint,
    ReturnToZero,
    StartRecording { mode: String, save_path: PathBuf ,num:i32},
    StopRecording,
}

#[derive(Debug, Clone)]
pub enum CameraCommand {
    RefreshCameras,
    Connect { index: usize },
    Disconnect,
    SetHoughCircleRadius { min: u32, max: u32 },
    SetLock(bool),
    Exposure(f64),
}

#[derive(Debug, Clone)]
pub enum TrainingCommand {
    LoadRecordedDataset { path: PathBuf},
    TrainModel { show_roc: bool, show_cm: bool },
    SaveModel { path: PathBuf },
    LoadModel { path: PathBuf },
    ExportDataset { path: PathBuf },
    ResetModel,
    LoadPersistentDataset { path: PathBuf },
    ResetPersistentDataset,
    ResetRecordedDataset
}

#[derive(Debug, Clone)]
pub enum StaticMeasureCommand {
    RunSingleMeasurement{time: i32},
    SaveResults { path: PathBuf },
    ClearResults,
    Stop,
}

#[derive(Debug, Clone)]
pub enum DynamicMeasureCommand {
    Start,
    UpdateParams{params:DynamicExpParams},
    Stop,
    StartNew,
    ClearResults,
}

#[derive(Debug, Clone)]
pub enum DataProcessingCommand {
    LoadData { path: PathBuf },
    SetAlphaInf { alpha: f64 },
    SetRegressionMode { mode: RegressionMode },
}

#[derive(Clone, Debug)]
pub struct DataProcessingStateUpdate {
    pub raw_data: Arc<Vec<(f64, i32, f64,bool)>>, // time, steps, angle
    pub alpha_inf: f64,
    pub regression_mode: RegressionMode,
    pub regression_formula: String,
    pub plot_scatter_points: Vec<(f64, f64)>, 
    pub plot_line_points: Vec<(f64, f64)>,
}
#[derive(Clone, Debug)]
pub enum RecordingStatus {
    Started,
    InProgress { elapsed_seconds: f32 },
    Finished,
    Error(String),
}

#[derive(Debug, Clone)]
pub struct LogMessage {
    pub level: Level,
    pub message: String,
    pub timestamp: DateTime<Utc>,
    pub target: String,
}

//======================================================================
//  更新: Backend -> Frontend
//======================================================================

#[derive(Clone, Debug)]
pub enum Update {
    General(GeneralUpdate),
    Device(DeviceUpdate),
    Recording(RecordingUpdate),
    Training(TrainingUpdate),
    Measurement(MeasurementUpdate),
    DataProcessing(DataProcessingUpdate),
}

#[derive(Clone, Debug)]
pub enum GeneralUpdate {
    StatusMessage(String),
    Error(String),
    NewLog(LogMessage),
}

#[derive(Clone, Debug)]
pub enum RecordingUpdate {
    StatusUpdate(RecordingStatus),
}

#[derive(Clone, Debug)]
pub enum DeviceUpdate {
    SerialPortsList(Vec<String>),
    SerialConnectionStatus(bool),
    CameraList(Vec<String>),
    CameraConnectionStatus(bool),
    NewCameraFrame(Arc<ColorImage>),
}

#[derive(Clone, Debug)]
pub enum TrainingUpdate {
    VideoProcessingUpdate { mode: String, message: String },
    TrainingStatus(String),
    ModelReady(bool),
    TrainingPlotsReady {
        cm: Option<ConfusionMatrixData>,
        roc: Option<RocCurveData>,
    },
    PersistentDatasetStatus(String),
    MAMDatasetStatus(String),
    AMADatasetStatus(String),
}

#[derive(Clone, Debug)]
pub enum MeasurementUpdate {
    StaticStatus(String),
    StaticRunning(bool),
    StaticResults(Vec<StaticResult>),
    DynamicStatus(String),
    DynamicResults(Vec<DynamicResult>),
    DynamicRunning(bool),
    CurrentSteps(Option<i32>),
    StartTime(Option<std::time::Instant>)
}

#[derive(Clone, Debug)]
pub enum DataProcessingUpdate {
    FullState(DataProcessingStateUpdate),
}

//======================================================================
//  共享数据结构
//======================================================================
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RegressionMode { Linear, Log, Inverse }

#[derive(Debug, Clone)]
pub struct DynamicExpParams {
    pub path: PathBuf,
    pub temperature: f32,
    pub sucrose_conc: f32,
    pub hcl_conc: f32,
    pub pre_rotation_angle: f32,
    pub step_angle: f32,
    pub sample_points: u32,
}

#[derive(Clone, Debug)]
pub struct ConfusionMatrixData {
    pub matrix: [[u32; 2]; 2], // [[TN, FP], [FN, TP]]
    pub accuracy: f32,
}

#[derive(Clone, Debug)]
pub struct RocCurveData {
    pub points: Vec<(f64, f64)>,
    pub auc: f64,
}

#[derive(Clone, Debug)]
pub struct StaticResult {
    pub index: usize,
    pub steps: i32,
    pub angle: f32,
}

#[derive(Clone, Debug)]
pub struct DynamicResult {
    pub index: usize,
    pub time: f64,
    pub steps: i32,
    pub angle: f32,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct TrainedModel {
    pub parameters: ndarray::Array1<f64>,
    pub intercept: f64,
}

pub enum FileDialogResult {
    // 模型训练
    StartRecording(PathBuf),
    RecordedDataset(PathBuf),
    PersistentDataset(PathBuf),
    // 静态测量
    SaveStaticResults(PathBuf),
    // 动态测量
    SaveDynamicExperiment(PathBuf),
    // 数据处理
    LoadDataProcessingFile(PathBuf),
}
