// src/tracing_layer.rs

use crate::communication::{GeneralUpdate, Update, LogMessage};
use crossbeam_channel::Sender;
use std::fmt;
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::layer::{Context, Layer};
use chrono::Utc;

pub struct EguiTracingLayer {
    log_tx: Sender<Update>,
}

impl EguiTracingLayer {
    pub fn new(log_tx: Sender<Update>) -> Self {
        Self { log_tx }
    }
}

// 访问者模式保持不变，用于提取消息
struct MessageVisitor {
    message: String,
}

impl Visit for MessageVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        if field.name() == "message" {
            // 移除字符串字面量的引号，让显示更干净
            self.message = format!("{:?}", value).trim_matches('"').to_string();
        }
    }
}

impl<S> Layer<S> for EguiTracingLayer
where
    S: Subscriber,
{
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let mut visitor = MessageVisitor {
            message: String::new(),
        };
        event.record(&mut visitor);

        if visitor.message.is_empty() {
            return;
        }

        // 创建 LogMessage 实例时，填充新增的 target 字段
        let log_message = LogMessage {
            level: *event.metadata().level(),
            message: visitor.message,
            timestamp: Utc::now(),
            // v-- 从 Event 元数据中获取 target --v
            target: event.metadata().target().to_string(),
        };

        // 发送结构化的日志数据
        let _ = self
            .log_tx
            .send(Update::General(GeneralUpdate::NewLog(log_message)));
    }
}


// level_to_str 函数不再需要在此文件中，它属于表现层逻辑，应该移动到 egui 的 UI 代码中。