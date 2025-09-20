use super::{BackendState};
use anyhow::Result;

use crate::communication::*;
use crossbeam_channel::Sender;
use ndarray::{Array1,Axis};
use linfa::traits::{Fit, Predict};
use linfa_linear::{FittedLinearRegression, LinearRegression};
use linfa::prelude::*;
pub fn recalculate_and_update(state: &mut BackendState, tx: &Sender<Update>) -> Result<()> {
    let dp_state = &mut state.data_processing;
    dp_state.plot_scatter_points.clear();
    dp_state.plot_line_points.clear();
    dp_state.regression_formula.clear();
    // If there's no data, clear results and send an update
    let Some(raw_data) = &mut dp_state.raw_data else {
        // 没有数据，发送一个清空的状态
        tx.send(Update::DataProcessing(DataProcessingUpdate::FullState(dp_state.clone().into())))?;
        return Ok(());
    };
    if raw_data.is_empty() {
        tx.send(Update::DataProcessing(DataProcessingUpdate::FullState(dp_state.clone().into())))?;
        return Ok(());
    }

    // --- 1. 计算用于绘图的散点坐标 (y-axis transformation) ---
    dp_state.plot_scatter_points = raw_data.iter_mut().filter_map(|point| {
        let diff = point.2 - dp_state.alpha_inf;
        let y_val = match dp_state.regression_mode {
            RegressionMode::Linear => diff,
            RegressionMode::Log => if diff > 1e-9 { diff.ln() } else { f64::NAN },
            RegressionMode::Inverse => if diff > 1e-9 { 1.0 / diff } else { f64::NAN },
        };
        if y_val.is_finite() { 
            point.3=true;
            Some((point.0, y_val)) 
        } else { 
            point.3=false;
            None 
        }
    }).collect();

    if dp_state.plot_scatter_points.is_empty() {
        // 如果变换后没有有效数据点，也发送清空的状态
        tx.send(Update::DataProcessing(DataProcessingUpdate::FullState(dp_state.clone().into())))?;
        return Ok(());
    }

    // --- 2. 准备 linfa 数据集 ---
    let (x_data, y_data): (Vec<f64>, Vec<f64>) = dp_state.plot_scatter_points.iter().cloned().unzip();
    let x_arr = Array1::from(x_data.clone());
    let y_arr = Array1::from(y_data);
    let dataset = Dataset::new(x_arr.insert_axis(Axis(1)), y_arr);
    let model:FittedLinearRegression<f64> = LinearRegression::new().fit(&dataset)?;
    
    let params = model.params();
    let intercept = model.intercept();
    let predicted_y = model.predict(&dataset);
    let y_true = dataset.targets();
    
    // 计算 SST (Total Sum of Squares)
    let y_mean = y_true.mean().unwrap();
    let sst = y_true.iter().map(|y| (y - y_mean).powi(2)).sum::<f64>();

    // 计算 SSR (Sum of Squared Residuals)
    let ssr = y_true.iter().zip(predicted_y.iter()).map(|(y, y_pred)| (y - y_pred).powi(2)).sum::<f64>();

    // 计算 R²，并处理 SST 为 0 的边缘情况
    let r2 = if sst.abs() < 1e-9 {
        if ssr.abs() < 1e-9 { 1.0 } else { 0.0 }
    } else {
        1.0 - (ssr / sst)
    };
    // Update state with new results
    let sign = if intercept >= 0.0 { "+" } else { "-" };
    dp_state.regression_formula = format!("y = {:.4}x {} {:.4}\nR² = {:.6}", params[0], sign, intercept.abs(), r2);
    
    let x_min = x_data.iter().cloned().fold(f64::INFINITY, f64::min);
    let x_max = x_data.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    let y_min = params[0] * x_min + intercept;
    let y_max = params[0] * x_max + intercept;
    dp_state.plot_line_points = vec![(x_min, y_min), (x_max, y_max)];

    // --- 5. 发送完整的、包含所有绘图数据的状态更新 ---
    tx.send(Update::DataProcessing(DataProcessingUpdate::FullState(dp_state.clone().into())))?;


    Ok(())
}
