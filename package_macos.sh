#!/bin/bash

# # =======================================================================
# #       macOS Egui + OpenCV 应用打包脚本 (含摄像头权限) - 修正版
# # =======================================================================
# #
# # 这个脚本会自动完成以下任务:
# # 1. 使用 cargo-bundle 创建 .app 包。
# # 2. 向 Info.plist 文件添加摄像头权限使用描述。
# # 3. 将 Homebrew 安装的 OpenCV .dylib 库复制到 .app 包中。
# # 4. 使用 install_name_tool 修复所有链接，使 .app 包可以脱离
# #    Homebrew 环境在任何 Mac 上独立运行。
# #
# # =======================================================================

# # 当任何命令失败时，立即退出脚本
set -e

# # --- (1) 用户配置 ---
# # 请根据您在 Cargo.toml -> [package.metadata.bundle] 中设置的 name 修改
APP_NAME="Polarimeter"
# # 请根据您在 Cargo.toml -> [[bin]] 或 [package] 中设置的 name 修改
EXECUTABLE_NAME="rust_polarimeter_gui"

# # 这是在向用户申请摄像头权限时，对话框中显示的说明文字。
# CAMERA_USAGE_DESCRIPTION="此应用需要访问摄像头以进行实时图像处理和分析。"


# --- (2) 自动检测和路径设置 ---
echo "[INFO] 正在自动检测 OpenCV 库路径..."
# 使用 brew --prefix 自动找到 OpenCV 的安装目录，这比写死路径更可靠
# 注意：对于 Apple Silicon (arm64)，路径通常是 /opt/homebrew/opt/opencv
# 对于 Intel (x86_64)，路径通常是 /usr/local/opt/opencv
OPENCV_LIB_PATH=$(brew --prefix opencv)/lib
if [ ! -d "$OPENCV_LIB_PATH" ]; then
    echo "[ERROR] 找不到 OpenCV 库。请确认您已通过 Homebrew 安装了 OpenCV。"
    exit 1
fi
echo "[INFO] OpenCV 库路径: $OPENCV_LIB_PATH"

BUNDLE_PATH="target/release/bundle/osx/$APP_NAME.app"
PLIST_PATH="$BUNDLE_PATH/Contents/Info.plist"



# --- (3) 构建和打包流程 ---
echo
echo "[1/6] 正在使用 cargo-bundle 创建基础 .app 包..."
# 清理一下，确保是全新构建
rm -rf "$BUNDLE_PATH"
export xport DYLD_FALLBACK_LIBRARY_PATH="/Library/Developer/CommandLineTools/usr/lib"
cargo bundle --release

echo
echo "[2/6] 正在向 Info.plist 添加摄像头权限描述..."
plutil -insert NSCameraUsageDescription -string "$CAMERA_USAGE_DESCRIPTION" "$PLIST_PATH"
echo "      - 权限描述已添加: '$CAMERA_USAGE_DESCRIPTION'"
