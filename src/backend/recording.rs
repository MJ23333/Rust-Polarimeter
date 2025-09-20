// src/backend/recording.rs

use super::{Arc, BackendState, CancellationToken, Mutex};
use crate::communication::{RecordingStatus, RecordingUpdate, Update};
use anyhow::Result;
use crossbeam_channel::Sender;
use opencv::{prelude::*, videoio};
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};
use tracing::info;

const TARGET_FPS: f32 = 30.0;
const FRAME_INTERVAL: Duration = Duration::from_micros((1_000_000.0 / TARGET_FPS) as u64);

/// The main video recording loop, runs in its own thread.
pub fn record_video_loop(
    state: &Arc<Mutex<BackendState>>,
    update_tx: &Sender<Update>,
    save_path: PathBuf,
    mode: String, // "MAM" or "AMA"
    num: i32,
    token: CancellationToken,
) -> Result<()> {
    let state_guard = state.lock();
    let (serial_port_present, camera_present) = {
        (
            state_guard.devices.serial_port.is_some(),
            state_guard.devices.camera_manager.is_some(),
        )
    };
    if !camera_present {
        return Err(anyhow::anyhow!("相机未连接"));
    }
    if !serial_port_present {
        return Err(anyhow::anyhow!("设备未连接"));
    }
    let dataset_folder_name = if mode == "MAM" { "dataset0" } else { "dataset1" };
    let target_dir = save_path.join(dataset_folder_name);
    if target_dir.exists() {
        info!("目标文件夹 {:?} 已存在，正在清空...", &target_dir);
        // Recursively delete the directory and all its contents.
        // The `?` operator will handle potential errors (e.g., permission denied).
        std::fs::remove_dir_all(&target_dir)?;
        info!("旧文件夹已成功删除。");
    }
    std::fs::create_dir_all(&target_dir)?;
    info!("处理后的帧将保存到: {:?}", target_dir);

    update_tx.send(Update::Recording(RecordingUpdate::StatusUpdate(
        RecordingStatus::Started,
    )))?;
    info!("录制开始: {:?}, 模式: {}", save_path, mode);
    let state_clone = Arc::clone(state);
    let tx_clone = update_tx.clone();
    let rotation_handle = std::thread::spawn(move || {
        // let num=3000;
        // Execute the blocking rotation function in the new thread.
        let result = (|| -> Result<()> {
            if mode=="MAM"{
                crate::backend::measurement::precision_rotate(&state_clone, &tx_clone,num)?;
                crate::backend::measurement::precision_rotate(&state_clone, &tx_clone,-num)?;
            }else{
                crate::backend::measurement::precision_rotate(&state_clone, &tx_clone,-num)?;
                crate::backend::measurement::precision_rotate(&state_clone, &tx_clone,num)?;
            
            }
            Ok(())
        })();
        if let Err(e)=result{
            tracing::error!("旋转失败：{}",e);
        }
        // result
    });
    info!("旋转操作已在后台线程启动。");
    let mut saved_frame_count = 0;
    let start_time = Instant::now();
    let mut last_frame_time = Instant::now();
    drop(state_guard);
    loop {
        if token.load(Ordering::Relaxed) {
            break;
        }
        if rotation_handle.is_finished() {
            info!("旋转操作完成，自动结束录制。");
            break;
        }
        let now = Instant::now();
        if now.duration_since(last_frame_time) < FRAME_INTERVAL {
            std::thread::sleep(Duration::from_millis(5));
            continue;
        }
        last_frame_time = now;
        let state_guard = state.lock();
        let frame = state_guard
            .devices
            .camera_manager
            .as_ref()
            .unwrap()
            .latest_frame
            .lock()
            .clone();
        let settings = state_guard.devices.camera_settings.lock().clone();
        drop(state_guard);
        if let Some(frame) = frame {
            let circle = if settings.lock_circle {
                settings.locked_circle
            } else {
                None
            };

            // Call your existing ML processing function
            match crate::backend::model::process_frame_for_ml(&frame, settings.min_radius, settings.max_radius, circle) {
                Ok(processed_pixels) => {
                    saved_frame_count += 1;
                    let filename = format!("frame_{:05}.png", saved_frame_count);
                    let file_path = target_dir.join(filename);

                    // Save the processed 20x20 grayscale pixels as a PNG
                    if let Err(e) = image::save_buffer(
                        &file_path,
                        &processed_pixels,
                        20,
                        20,
                        image::ColorType::L8,
                    ) {
                        tracing::error!("保存PNG帧失败 {:?}: {}", file_path, e);
                    }
                }
                Err(e) => {
                    tracing::warn!("处理帧失败，跳过: {}", e);
                }
            }
        } else {
            state.lock().devices.camera_manager = None;
            update_tx.send(Update::Device(crate::communication::DeviceUpdate::CameraConnectionStatus(false)))?;
            break;
        }

        let elapsed = start_time.elapsed().as_secs_f32();
        update_tx.send(Update::Recording(RecordingUpdate::StatusUpdate(
            RecordingStatus::InProgress {
                elapsed_seconds: elapsed,
            },
        )))?;
    }

    // 保存总步数以备“倒带”

    info!("录制正常结束，共 {} 帧",saved_frame_count);
    if let Err(e) = rotation_handle.join() {
        tracing::error!("旋转线程 panic: {:?}", e);
    }
    // 无论成功还是失败，都发送 Finished 信号
    let _ = update_tx.send(Update::Recording(RecordingUpdate::StatusUpdate(
        RecordingStatus::Finished,
    )));
    // 清理 token
    state.lock().recording.cancellation_token = None;
    Ok(())
}

// 在 `src/backend/serial.rs` 中，您需要一个类似于 `rotate_motor` 的函数，但它接受步数
// src/backend/serial.rs (示意)
// pub fn precision_rotate_steps(state: &Arc<Mutex<BackendState>>, steps: i32) -> BackendResult<()> {
//    ...
// }
