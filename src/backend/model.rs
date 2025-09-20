use super::{Arc, BackendState, CancellationToken, Mutex, TrainingState};
use crate::communication::*;
use anyhow::{anyhow, Result};
use crossbeam_channel::{ Sender};
use linfa::prelude::*;
use linfa_logistic::{FittedLogisticRegression, LogisticRegression};
use ndarray::{Array1, Array2, ArrayBase, Dim, OwnedRepr};
use opencv::{core, imgproc, prelude::*, videoio};
use rand::thread_rng;
use std::path::{Path, PathBuf};
use tracing::info;

pub fn process_frame_for_ml(
    frame: &Mat,
    min_radius: i32,
    max_radius: i32,
    cir: Option<(i32, i32, i32)>,
) -> Result<Vec<u8>> {
    let mut gray = Mat::default();
    imgproc::cvt_color(
        frame,
        &mut gray,
        imgproc::COLOR_BGR2GRAY,
        0,
        core::AlgorithmHint::ALGO_HINT_DEFAULT,
    )?;

    let (center, radius) = if cir.is_none() {
        let mut circles = core::Vector::<core::Vec3f>::new();
        imgproc::hough_circles(
            &gray,
            &mut circles,
            imgproc::HOUGH_GRADIENT,
            1.0,
            30.0,
            40.0,
            10.0,
            min_radius,
            max_radius,
        )?;

        if circles.is_empty() {
            return Err(anyhow!("找不到圆"));
        }

        let p = circles.get(0)?;
        let center = core::Point::new(p[0] as i32, p[1] as i32);
        let radius = p[2] as i32;
        (center, radius)
    } else {
        let center = core::Point::new(cir.unwrap().0 as i32, cir.unwrap().1 as i32);
        let radius = cir.unwrap().2 as i32;
        (center, radius)
    };
    // 霍夫圆检测以定位区域

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

pub fn predict_from_frame(
    frame: &Mat,
    model: &FittedLogisticRegression<f64, usize>,
    min_radius: i32,
    max_radius: i32,
    cir: Option<(i32, i32, i32)>,
) -> Result<usize> {
    let features_u8 = process_frame_for_ml(frame, min_radius, max_radius, cir)?;
    let features_f64: Vec<f64> = features_u8.iter().map(|&p| p as f64 / 255.0).collect();
    let features_arr = Array1::from(features_f64);

    // (已优化) 不再需要 new_from_raw，直接使用传入的、已存在的模型对象进行预测
    let dataset = DatasetBase::from(features_arr.insert_axis(ndarray::Axis(0)));
    let prediction = model.predict(&dataset);

    Ok(prediction[0])
}

// pub fn process_video_for_training(
//     state: &Arc<Mutex<BackendState>>,
//     video_path: &PathBuf,
//     mode: &str,
//     tx: &Sender<Update>,
//     token: CancellationToken,
// ) -> Result<()> {
//     info!("[后端] 开始处理视频: {:?}, 模式: {}", video_path, mode);
//     tx.send(Update::Training(TrainingUpdate::VideoProcessingUpdate {
//         mode: mode.to_string(),
//         message: "打开视频...".to_string(),
//     }))
//     .unwrap();
//     let guard1 = state.lock();
//     let guard2 = guard1.devices.camera_settings.lock();
//     let circle = {
//         if guard2.lock_circle {
//             guard2.locked_circle
//         } else {
//             None
//         }
//     };
//     let mut cap =
//         match videoio::VideoCapture::from_file(video_path.to_str().unwrap(), videoio::CAP_ANY) {
//             Ok(cap) => cap,
//             Err(_e) => {
//                 tx.send(Update::Training(TrainingUpdate::VideoProcessingUpdate {
//                     mode: mode.to_string(),
//                     message: "错误了".to_string(),
//                 }))
//                 .unwrap();
//                 return Ok(());
//             }
//         };
//     let total_frames = cap.get(videoio::CAP_PROP_FRAME_COUNT).unwrap_or(0.0) as u32;
//     let mut processed_images = Vec::new();
//     let mut frame_count = 0;
//     let min_radius = guard2.min_radius;
//     let max_radius = guard2.max_radius;
//     drop(guard2);
//     drop(guard1);
//     while let Ok(true) = cap.is_opened() {
//         if token.load(std::sync::atomic::Ordering::Relaxed) {
//             break;
//         }
//         let mut frame = Mat::default();
//         if let Ok(true) = cap.read(&mut frame) {
//             if frame.empty() {
//                 break;
//             }
//             frame_count += 1;
//             // info!("yep");
//             if frame_count % 10 == 0 {
//                 // 每10帧更新一次进度
//                 let msg = format!("处理中: {}/{}", frame_count, total_frames);
//                 tx.send(Update::Training(TrainingUpdate::VideoProcessingUpdate {
//                     mode: mode.to_string(),
//                     message: msg,
//                 }))
//                 .unwrap();
//             }
//             if let Ok(processed) = process_frame_for_ml(&frame, min_radius, max_radius, circle) {
//                 processed_images.push(processed);
//             }
//         } else {
//             break;
//         }
//     }

//     if mode == "MAM" {
//         state.lock().training.mam_images = processed_images;
//         info!("man");
//         tx.send(Update::Training(TrainingUpdate::MAMDatasetStatus(
//             "完成".to_string(),
//         )))
//         .unwrap();
//         info!("man");
//     } else {
//         state.lock().training.ama_images = processed_images;
//         tx.send(Update::Training(TrainingUpdate::AMADatasetStatus(
//             "完成".to_string(),
//         )))
//         .unwrap();
//     }
//     tx.send(Update::Training(TrainingUpdate::VideoProcessingUpdate {
//         mode: mode.to_string(),
//         message: format!("完成, 提取了 {} 帧", frame_count),
//     }))
//     .unwrap();
//     Ok(())
// }
pub fn load_recorded_dataset(
    state: &Arc<Mutex<BackendState>>,
    path: &Path,
    tx: &Sender<Update>,
) -> Result<()> {
    info!("开始加载录制数据集: {:?}", path);
    tx.send(Update::Training(TrainingUpdate::MAMDatasetStatus(
        "正在加载".to_string(),
    )))
    .unwrap();
    let mut loaded_mam = 0;
    let mut loaded_ama = 0;

    // 加载 dataset0 (MAM)
    let mam_path = path.join("dataset0");
    let training_state = &mut state.lock().training;
    training_state.mam_images.clear();
    if let Ok(entries) = std::fs::read_dir(mam_path) {
        for entry in entries.flatten() {
            if let Ok(img) = image::open(entry.path()) {
                let luma_img = img.to_luma8();
                // 注意：这里我们假设图片已经是20x20，如果不是，还需要resize
                // let resized = image::imageops::resize(&luma_img, 20, 20, image::imageops::FilterType::Triangle);
                training_state.mam_images.push(luma_img.into_raw());
                loaded_mam += 1;
            }
        }
    }

    // 加载 dataset1 (AMA)
    let ama_path = path.join("dataset1");
    training_state.ama_images.clear();
    if let Ok(entries) = std::fs::read_dir(ama_path) {
        for entry in entries.flatten() {
            if let Ok(img) = image::open(entry.path()) {
                let luma_img = img.to_luma8();
                training_state.ama_images.push(luma_img.into_raw());
                loaded_ama += 1;
            }
        }
    }

    let msg = format!("MAM {}, AMA {}",loaded_mam,loaded_ama);
    info!("录制数据集加载完成：{}", msg);
    tx.send(Update::Training(TrainingUpdate::MAMDatasetStatus(
        msg,
    )))
    .unwrap();
    Ok(())
}

pub fn train_model(
    state: &Arc<Mutex<BackendState>>,
    show_roc: bool,
    show_cm: bool,
    tx: &Sender<Update>,
) -> Result<()> {
    info!("开始训练模型");

    let training_state = &mut state.lock().training;

    let all_mam = [
        &training_state.mam_images[..],
        &training_state.persistent_mam[..],
    ]
    .concat();
    let all_ama = [
        &training_state.ama_images[..],
        &training_state.persistent_ama[..],
    ]
    .concat();
    info!("最终数据量——MAM：{}；AMA：{}",all_mam.len(),all_ama.len());
    if all_mam.is_empty() || all_ama.is_empty() {
        tx.send(Update::Training(TrainingUpdate::TrainingStatus(
            "数据集为空".to_string(),
        )))?;
        tracing::warn!("数据集为空");
        return Ok(());
    }

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
    let (train, valid) = dataset.shuffle(&mut rng).split_with_ratio(0.8);

    info!("正在训练");
    let model: FittedLogisticRegression<f64, usize> =
        LogisticRegression::default().fit(&train).unwrap();

    training_state.fitted_model = Some(model.clone());
    let predictions = model.predict(&valid);
    let cm = predictions.confusion_matrix(valid.targets()).unwrap();
    let accuracy = cm.accuracy();
    let cm = calculate_binary_confusion_matrix(&predictions, valid.targets());
    info!("训练完成，模型准确度: {}", accuracy);

    // 发送图表数据
    tx.send(Update::Training(TrainingUpdate::TrainingPlotsReady {
        cm: if show_cm {
            Some(ConfusionMatrixData {
                matrix: cm,
                accuracy,
            })
        } else {
            None
        },
        roc: if show_roc { None } else { None }, // ROC 计算较复杂，暂留空
    }))
    .unwrap();

    tx.send(Update::Training(TrainingUpdate::ModelReady(true)))?;

    Ok(())
}

pub fn load_persistent_dataset(
    state: &Arc<Mutex<BackendState>>,
    path: &Path,
    tx: &Sender<Update>,
) -> Result<()> {
    info!("开始加载常驻数据集: {:?}", path);
    tx.send(Update::Training(TrainingUpdate::PersistentDatasetStatus(
        "正在加载".to_string(),
    )))
    .unwrap();
    let mut loaded_mam = 0;
    let mut loaded_ama = 0;

    // 加载 dataset0 (MAM)
    let mam_path = path.join("dataset0");
    let training_state = &mut state.lock().training;
    training_state.persistent_mam.clear();
    if let Ok(entries) = std::fs::read_dir(mam_path) {
        for entry in entries.flatten() {
            if let Ok(img) = image::open(entry.path()) {
                let luma_img = img.to_luma8();
                // 注意：这里我们假设图片已经是20x20，如果不是，还需要resize
                // let resized = image::imageops::resize(&luma_img, 20, 20, image::imageops::FilterType::Triangle);
                training_state.persistent_mam.push(luma_img.into_raw());
                loaded_mam += 1;
            }
        }
    }

    // 加载 dataset1 (AMA)
    let ama_path = path.join("dataset1");
    training_state.persistent_ama.clear();
    if let Ok(entries) = std::fs::read_dir(ama_path) {
        for entry in entries.flatten() {
            if let Ok(img) = image::open(entry.path()) {
                let luma_img = img.to_luma8();
                training_state.persistent_ama.push(luma_img.into_raw());
                loaded_ama += 1;
            }
        }
    }

    let msg = format!("MAM {}, AMA {}",loaded_mam,loaded_ama);
    info!("数据集加载完成 {}", msg);
    tx.send(Update::Training(TrainingUpdate::PersistentDatasetStatus(
        msg,
    )))
    .unwrap();
    Ok(())
}

pub fn reset_model(state: &Arc<Mutex<BackendState>>, tx: &Sender<Update>) -> Result<()> {
    let mut s = state.lock();
    s.training = TrainingState::new(); // 重置为新的空状态

    tx.send(Update::Training(TrainingUpdate::ModelReady(false)))?;
    tx.send(Update::Training(TrainingUpdate::TrainingStatus(
        "无可用模型".to_string(),
    )))?;
    // ... (发送其他重置状态的更新) ...

    Ok(())
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
        tracing::error!("错误: 预测和真实标签的长度不一致.");
        return confusion_matrix;
    }

    // 遍历预测结果和真实标签
    let num_samples = predictions.len();
    for i in 0..num_samples {
        let true_label = targets[i];
        let predicted_label = predictions[i];

        // 确保标签是0或1
        if true_label > 1 || predicted_label > 1 {
             tracing::error!("警告: 存在非0或1的标签。此函数只适用于二分类。");
            continue;
        }

        // 更新混淆矩阵。
        // cm[真实标签][预测标签]
        confusion_matrix[true_label][predicted_label] += 1;
    }

    confusion_matrix
}
