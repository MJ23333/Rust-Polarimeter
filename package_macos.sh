#!/bin/zsh

# =======================================================================
#       macOS Egui + OpenCV 应用打包脚本 (含摄像头权限)
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

# +++ 新增配置 +++
# 这是在向用户申请摄像头权限时，对话框中显示的说明文字。
# 请务必写真诚、清晰的理由，例如："用于实时人脸识别和添加滤镜特效"
CAMERA_USAGE_DESCRIPTION="此应用需要访问摄像头以进行实时图像处理和分析。"


# --- (2) 自动检测和路径设置 ---
echo "[INFO] 正在自动检测 OpenCV 库路径..."
# 使用 brew --prefix 自动找到 OpenCV 的安装目录，这比写死路径更可靠
OPENCV_LIB_PATH=$(brew --prefix opencv)/lib
if [ ! -d "$OPENCV_LIB_PATH" ]; then
    echo "[ERROR] 找不到 OpenCV 库。请确认您已通过 Homebrew 安装了 OpenCV。"
    exit 1
fi
echo "[INFO] OpenCV 库路径: $OPENCV_LIB_PATH"

BUNDLE_PATH="target/release/bundle/osx/$APP_NAME.app"
PLIST_PATH="$BUNDLE_PATH/Contents/Info.plist" # <--- 新增 Info.plist 路径变量
FRAMEWORKS_PATH="$BUNDLE_PATH/Contents/Frameworks"
MACOS_PATH="$BUNDLE_PATH/Contents/MacOS/$EXECUTABLE_NAME"


# --- (3) 构建和打包流程 ---
echo
echo "[1/5] 正在使用 cargo-bundle 创建基础 .app 包..."
export xport DYLD_FALLBACK_LIBRARY_PATH="/Library/Developer/CommandLineTools/usr/lib"
cargo bundle --release

# +++ 新增步骤 +++
echo
echo "[2/5] 正在向 Info.plist 添加摄像头权限描述..."
# 使用 plutil 工具插入键值对。-insert 指定键名, -string 指定值的类型和内容
plutil -insert NSCameraUsageDescription -string "$CAMERA_USAGE_DESCRIPTION" "$PLIST_PATH"
echo "      - 权限描述已添加: '$CAMERA_USAGE_DESCRIPTION'"

echo
echo "[3/5] 正在创建 Frameworks 目录并复制 OpenCV 库..." # <--- 序号变更
mkdir -p "$FRAMEWORKS_PATH"
echo "    - 正在从 $OPENCV_LIB_PATH 复制 dylib 文件..."
cp "$OPENCV_LIB_PATH"/libopencv_*.dylib "$FRAMEWORKS_PATH/"


echo
echo "[4/5] 正在修复可执行文件的运行时路径 (rpath)..."
# +++ 修复权限的关键步骤 (Part 1) +++
# 确保主可执行文件是可写的
chmod +w "$MACOS_PATH"
# 修改 rpath
install_name_tool -add_rpath "@executable_path/../Frameworks" "$MACOS_PATH"
# 立即对修改后的主可执行文件重新签名 (ad-hoc)
codesign -f -s - "$MACOS_PATH"
echo "      - 主程序 rpath 修改并重签完毕"


echo
echo "[5/5] 正在修复所有 dylib 库的内部依赖链接..."
for lib in "$FRAMEWORKS_PATH"/*.dylib; do
  lib_name=$(basename "$lib")
  echo "  - 正在处理库: $lib_name"

  # +++ 修复权限的关键步骤 (Part 2) +++
  # 1. 确保 dylib 文件是可写的
  chmod +w "$lib"

  # 2. 像之前一样修改库的 ID 和依赖
  install_name_tool -id "@rpath/$lib_name" "$lib"

  otool -L "$lib" | grep 'opencv' | awk '{print $1}' | while read dep; do
    dep_name=$(basename "$dep")
    if [ "$lib_name" != "$dep_name" ]; then
      install_name_tool -change "$dep" "@rpath/$dep_name" "$lib"
    fi
  done

  # 3. 立即对修改后的 dylib 文件重新签名 (ad-hoc)
  #    -f : force (强制替换现有签名)
  #    -s - : sign with an ad-hoc identity (使用一个临时的、匿名的签名)
  codesign -f -s - "$lib"
done
echo "      - 所有 dylib 库依赖修复并重签完毕"

echo
echo "======================================================================="
echo "打包成功!"
echo "您完整独立的应用程序位于: $BUNDLE_PATH"
echo "======================================================================="