#!/bin/bash

# =======================================================================
#       macOS Egui + OpenCV 应用打包脚本 (含摄像头权限) - 修正版
# =======================================================================
#
# 这个脚本会自动完成以下任务:
# 1. 使用 cargo-bundle 创建 .app 包。
# 2. 向 Info.plist 文件添加摄像头权限使用描述。
# 3. 将 Homebrew 安装的 OpenCV .dylib 库复制到 .app 包中。
# 4. 使用 install_name_tool 修复所有链接，使 .app 包可以脱离
#    Homebrew 环境在任何 Mac 上独立运行。
#
# =======================================================================

# 当任何命令失败时，立即退出脚本
set -e

# --- (1) 用户配置 ---
# 请根据您在 Cargo.toml -> [package.metadata.bundle] 中设置的 name 修改
APP_NAME="Polarimeter"
# 请根据您在 Cargo.toml -> [[bin]] 或 [package] 中设置的 name 修改
EXECUTABLE_NAME="rust_polarimeter_gui"

# 这是在向用户申请摄像头权限时，对话框中显示的说明文字。
CAMERA_USAGE_DESCRIPTION="此应用需要访问摄像头以进行实时图像处理和分析。"


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
FRAMEWORKS_PATH="$BUNDLE_PATH/Contents/Frameworks"
MACOS_PATH="$BUNDLE_PATH/Contents/MacOS/$EXECUTABLE_NAME"


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

echo
echo "[3/6] 正在创建 Frameworks 目录并复制 OpenCV 库..."
mkdir -p "$FRAMEWORKS_PATH"
echo "    - 正在从 $OPENCV_LIB_PATH 复制 dylib 文件..."
# 确保复制的是链接的目标，而不是符号链接本身
cp -L "$OPENCV_LIB_PATH"/libopencv_*.dylib "$FRAMEWORKS_PATH/"


echo
echo "[4/6] 正在修复可执行文件的运行时路径 (rpath) 和依赖..."
# 确保主可执行文件是可写的
chmod +w "$MACOS_PATH"
# 1. 添加 rpath，告诉主程序去哪里找库
install_name_tool -add_rpath "@executable_path/../Frameworks" "$MACOS_PATH"
echo "      - 主程序 rpath 已添加"

# +++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++
# +++ 核心修正：修改主程序对 OpenCV 库的依赖，从绝对路径改为 @rpath 相对路径 +++
# +++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++++
echo "      - 正在修正主程序的 dylib 链接..."
# 使用 otool -L 列出所有链接的库，筛选出含 opencv 的
# 然后对每一个找到的库，使用 install_name_tool -change 修改其链接
otool -L "$MACOS_PATH" | grep 'opencv' | awk '{print $1}' | while read -r dep; do
  # dep 是原始的绝对路径, 例如 /opt/homebrew/opt/opencv/lib/libopencv_core.4.9.0.dylib
  dep_name=$(basename "$dep") # 提取出文件名, 例如 libopencv_core.4.9.0.dylib
  echo "        - 修正链接: $dep_name"
  install_name_tool -change "$dep" "@rpath/$dep_name" "$MACOS_PATH"
done


echo
echo "[5/6] 正在修复所有 dylib 库的内部依赖链接和 ID..."
for lib in "$FRAMEWORKS_PATH"/*.dylib; do
  lib_name=$(basename "$lib")
  echo "  - 正在处理库: $lib_name"

  # 确保 dylib 文件是可写的
  chmod +w "$lib"

  # 1. 修改库自己的身份 (ID)，让别人通过 @rpath 找到它
  install_name_tool -id "@rpath/$lib_name" "$lib"

  # 2. 修改它对其他 OpenCV 库的依赖
  otool -L "$lib" | grep 'opencv' | awk '{print $1}' | while read -r dep; do
    dep_name=$(basename "$dep")
    if [ "$lib_name" != "$dep_name" ]; then
      install_name_tool -change "$dep" "@rpath/$dep_name" "$lib"
    fi
  done
done
echo "      - 所有 dylib 库依赖修复完毕"


# # =======================================================================
# #               ↓↓↓ 核心修正部分 ↓↓↓
# # =======================================================================
# echo
# echo "[6/6] 正在进行精确的、由内到外的代码签名 (Ad-Hoc)..."

# # 1. 首先对所有 Frameworks 里的 dylib 进行签名
# echo "  - 正在签名动态链接库 (dylib)..."
# for lib in "$FRAMEWORKS_PATH"/*.dylib; do
#   codesign --force --options=runtime -s - "$lib"
# done

# # 2. 然后对主可执行文件进行签名
# echo "  - 正在签名主可执行文件..."
# codesign --force --options=runtime -s - "$MACOS_PATH"

# # 3. 最后对整个 .app 包进行签名
# echo "  - 正在签名整个应用包..."
# codesign --force --options=runtime -s - "$BUNDLE_PATH"

# echo "      - 应用包已通过精确签名流程完成签名"
# # =======================================================================
# #               ↑↑↑ 核心修正部分 ↑↑↑
# # =======================================================================




echo
echo "======================================================================="
echo "打包成功!"
echo "您完整独立的应用程序位于: $BUNDLE_PATH"
echo "======================================================================="