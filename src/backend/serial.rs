use super::{Arc, BackendState, Mutex};
use crate::{backend::{CancellationToken,measurement::cmd}, communication::{DeviceUpdate, GeneralUpdate, Update}};
use anyhow::Result;
use crossbeam_channel::Sender;
use serialport;
use std::time::Duration;
use tracing::{error, info};
use std::io::{BufRead,BufReader};
use std::sync::atomic::Ordering;

pub fn get_available_ports(token: CancellationToken) -> Vec<String> {
    // 1. 获取原始的串口信息列表
    let ports = match serialport::available_ports() {
        Ok(ports) => ports,
        Err(e) => {
            error!("[后端] 无法获取串口列表: {}", e);
            return vec![];
        }
    };
    // return ports.into_iter().map(|p| p.port_name).collect();
    let mut responsive_ports = Vec::new();
    let mut other_ports = Vec::new();

    // 2. 遍历并探测每一个端口
    for p in ports {
        if token.load(Ordering::Relaxed){
            return vec![];
        }
        let port_name = p.port_name;
        let mut is_target_device = false;

        // 尝试打开端口并进行通信测试
        // 使用 if let 链式结构，任何一步失败都会跳过后续操作
        
        let lower_port_name = port_name.to_lowercase();
        if !lower_port_name.contains("debug") && !lower_port_name.contains("bluetooth") {
        
            is_target_device=true;
            
        }
        
        
        // 3. 根据探测结果将端口名分类
        if is_target_device {
            responsive_ports.push(port_name);
        } else {
            other_ports.push(port_name);
        }
    }

    // 4. 合并列表，响应成功的排在前面
    info!("串口列表刷新完成");
    responsive_ports.extend(other_ports);
    responsive_ports
}

pub fn connect(
    state: &Arc<Mutex<BackendState>>,
    port_name: String,
    baud_rate: u32,
    tx: &Sender<Update>,
) -> Result<()> {
    info!("尝试连接到串口 {} @ {} 波特率", port_name, baud_rate);

    // 先断开任何现有连接
    let mut s = state.lock();
    s.devices.serial_port = None;

    s.devices.serial_port = serialport::new(&port_name, baud_rate)
        .timeout(Duration::from_millis(5000))
        .open()
        .map(|port| Some(Arc::new(Mutex::new(port))))
        .unwrap_or_else(|e| {
            error!("打开失败：{}", e);
            None
        });
    if s.devices.serial_port.is_none() {
        return Err(anyhow::anyhow!("连接失败"))
    }
    tx.send(Update::Device(DeviceUpdate::SerialConnectionStatus(true)))?;
    info!("连接成功");
    Ok(())
    
}

pub fn disconnect(state: &Arc<Mutex<BackendState>>) -> Result<()> {
    let mut s = state.lock();
    if s.devices.serial_port.is_some() {
        s.devices.serial_port = None; // Drop 会自动关闭端口
        // info!("串口已断开");
    }
    Ok(())
}

pub fn test(state: &Arc<Mutex<BackendState>>,
    tx: &Sender<Update>,)-> Result<()>{
    let mut s= state.lock();
    if s.devices.serial_port.is_none() {
        return Err(anyhow::anyhow!("未连接串口"))
    }
    let port=s.devices.serial_port.as_mut().unwrap().clone();
    drop(s);
    if cmd(port,77 as u8).is_ok(){//cmd(port,51).is_ok()||
        info!("测试成功");
        
    }else{
        info!("测试失败");
    }
    Ok(())
}


