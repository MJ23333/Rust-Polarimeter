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


# # --- (2) 自动检测和路径设置 ---
# echo "[INFO] 正在自动检测 OpenCV 库路径..."
# # 使用 brew --prefix 自动找到 OpenCV 的安装目录，这比写死路径更可靠
# # 注意：对于 Apple Silicon (arm64)，路径通常是 /opt/homebrew/opt/opencv
# # 对于 Intel (x86_64)，路径通常是 /usr/local/opt/opencv
# OPENCV_LIB_PATH=$(brew --prefix opencv)/lib
# if [ ! -d "$OPENCV_LIB_PATH" ]; then
#     echo "[ERROR] 找不到 OpenCV 库。请确认您已通过 Homebrew 安装了 OpenCV。"
#     exit 1
# fi
# echo "[INFO] OpenCV 库路径: $OPENCV_LIB_PATH"

BUNDLE_PATH="target/release/bundle/osx/$APP_NAME.app"
# PLIST_PATH="$BUNDLE_PATH/Contents/Info.plist"



# # --- (3) 构建和打包流程 ---
# echo
# echo "[1/6] 正在使用 cargo-bundle 创建基础 .app 包..."
# # 清理一下，确保是全新构建
# rm -rf "$BUNDLE_PATH"
# export xport DYLD_FALLBACK_LIBRARY_PATH="/Library/Developer/CommandLineTools/usr/lib"
# cargo bundle --release

# echo
# echo "[2/6] 正在向 Info.plist 添加摄像头权限描述..."
# plutil -insert NSCameraUsageDescription -string "$CAMERA_USAGE_DESCRIPTION" "$PLIST_PATH"
# echo "      - 权限描述已添加: '$CAMERA_USAGE_DESCRIPTION'"

APP_BUNDLE_PATH="$BUNDLE_PATH"
if [ -z "$APP_BUNDLE_PATH" ]; then
    echo "Usage: $0 /path/to/YourApp.app" >&2
    exit 1
fi
if [ ! -d "$APP_BUNDLE_PATH" ]; then
    echo "Error: App bundle not found at '$APP_BUNDLE_PATH'" >&2
    exit 1
fi

if [ -x "/opt/homebrew/bin/brew" ]; then
    HOMEBREW_PREFIX="/opt/homebrew"
elif [ -x "/usr/local/bin/brew" ]; then
    HOMEBREW_PREFIX="/usr/local"
else
    echo "Error: Homebrew not found." >&2
    exit 1
fi
echo "INFO: Using Homebrew prefix: $HOMEBREW_PREFIX"

# --- Path Setup ---
FRAMEWORKS_DIR="$APP_BUNDLE_PATH/Contents/Frameworks"
PLIST_PATH="$APP_BUNDLE_PATH/Contents/Info.plist"
# EXECUTABLE_NAME=$(defaults read "$PLIST_PATH" CFBundleExecutable)
EXECUTABLE_PATH="$APP_BUNDLE_PATH/Contents/MacOS/$EXECUTABLE_NAME"

if [ ! -f "$EXECUTABLE_PATH" ]; then
    echo "Error: Executable not found at '$EXECUTABLE_PATH'" >&2
    exit 1
fi

mkdir -p "$FRAMEWORKS_DIR"

# --- State Tracking (sh compatible) & Temp Files ---
TEMP_OTOOL_FILE="/tmp/app_otool_$$"
TEMP_RPATH_FILE="/tmp/app_rpath_$$"
trap 'rm -f "$TEMP_OTOOL_FILE" "$TEMP_RPATH_FILE"' EXIT

LIBS_TO_PROCESS="$EXECUTABLE_PATH"
PROCESSED_LIBS_TRACKER="|"
ALL_COPIED_ORIGINAL_PATHS="|"

# --- Core Functions (sh compatible) ---

# This is the definitively fixed version of the rpath resolver.
# It uses a temporary file to avoid subshells, ensuring the resolved
# path is correctly returned.
resolve_dependency_path() {
    target_file="$1"
    dep_path="$2"
    resolved_path=""

    case "$dep_path" in
        "@rpath/"*)
            dep_name=$(basename -- "$dep_path")
            # Write RPATHs to a temp file to avoid a subshell for the read loop
            otool -l "$target_file" | grep -A2 LC_RPATH | grep ' path ' | awk '{print $2}' > "$TEMP_RPATH_FILE"
            
            while IFS= read -r rpath_base; do
                if [ "$rpath_base" = "@loader_path" ]; then
                    rpath_base=$(dirname -- "$target_file")
                fi
                
                candidate_path="$rpath_base/$dep_name"
                # Resolve symlinks and check for existence
                if [ -f "$candidate_path" ]; then
                    resolved_path=$(realpath "$candidate_path")
                    # Break the loop once we find the first valid path
                    break
                fi
            done < "$TEMP_RPATH_FILE"
            ;;
        /*)
            if [ -f "$dep_path" ]; then
                resolved_path=$(realpath "$dep_path")
            fi
            ;;
    esac
    echo "$resolved_path"
}

# --- Main Processing Logic ---

echo "INFO: Starting dependency analysis for '$EXECUTABLE_NAME'..."
install_name_tool -add_rpath "@executable_path/../Frameworks" "$EXECUTABLE_PATH"

while [ -n "$LIBS_TO_PROCESS" ]; do
    target_file=$(echo "$LIBS_TO_PROCESS" | cut -d' ' -f1)
    LIBS_TO_PROCESS=$(echo "$LIBS_TO_PROCESS" | sed 's/[^ ]* *//')

    echo ""
    echo "--- Scanning: $(basename -- "$target_file")"

    otool -L "$target_file" | tail -n +2 > "$TEMP_OTOOL_FILE"

    while IFS= read -r otool_line; do
        dep_path_string=$(echo "$otool_line" | awk '{print $1}')
        
        # Step 1: Resolve the path first, correctly this time.
        absolute_dep_path=$(resolve_dependency_path "$target_file" "$dep_path_string")

        # Skip if path could not be resolved
        if [ -z "$absolute_dep_path" ]; then continue; fi

        # Step 2: Now, check if the resolved path is a Homebrew dependency.
        case "$absolute_dep_path" in
            "$HOMEBREW_PREFIX"*)
                # It is a homebrew dependency, so we must process it.
                echo "  [DEP] Resolved Homebrew dependency: $absolute_dep_path"
                dep_name=$(basename -- "$absolute_dep_path")
                new_dep_path="@rpath/$dep_name"

                echo "    -> Patching reference in $(basename -- "$target_file") to '$new_dep_path'"
                install_name_tool -change "$dep_path_string" "$new_dep_path" "$target_file"

                case "$PROCESSED_LIBS_TRACKER" in
                    *"|$absolute_dep_path|"*)
                        echo "    -> Decision: SKIP copy, '$dep_name' is already processed."
                        ;;
                    *)
                        echo "    -> Decision: COPY '$dep_name', as it's a new dependency."
                        copied_dep_path="$FRAMEWORKS_DIR/$dep_name"

                        cp -L "$absolute_dep_path" "$copied_dep_path"
                        chmod 755 "$copied_dep_path"

                        echo "       - Setting ID to '$new_dep_path'"
                        install_name_tool -id "$new_dep_path" "$copied_dep_path"
                        echo "       - Adding RPATH '@loader_path/' to '$dep_name' itself"
                        install_name_tool -add_rpath "@loader_path/" "$copied_dep_path"

                        PROCESSED_LIBS_TRACKER="$PROCESSED_LIBS_TRACKER$absolute_dep_path|"
                        ALL_COPIED_ORIGINAL_PATHS="$ALL_COPIED_ORIGINAL_PATHS$absolute_dep_path|"
                        LIBS_TO_PROCESS="$LIBS_TO_PROCESS $copied_dep_path"
                        echo "       - Added '$dep_name' to processing queue."
                        ;;
                esac
                ;;
            *)
                # Not a homebrew dependency, skip
                continue
                ;;
        esac
    done < "$TEMP_OTOOL_FILE"
done

# --- FINAL VERIFICATION STEP ---
echo "\n---\n--- Running Final Verification ---"
SUCCESS=true
if [ "$ALL_COPIED_ORIGINAL_PATHS" = "|" ]; then
    echo "No Homebrew libraries were bundled. Verification skipped."
else
    echo "$ALL_COPIED_ORIGINAL_PATHS" | tr '|' '\n' | grep -v '^$' | while IFS= read -r original_path; do
        dep_name=$(basename -- "$original_path")
        if [ -f "$FRAMEWORKS_DIR/$dep_name" ]; then
            echo "  [OK] Found: $dep_name"
        else
            echo "  [ERROR] MISSING FILE: '$dep_name' was processed but is NOT in the Frameworks directory!"
            SUCCESS=false
        fi
    done
fi

echo "---"
if [ "$SUCCESS" = "true" ]; then
    echo "✅ Verification successful. All libraries are present."
else
    echo "❌ Verification FAILED. One or more library files are missing."
fi
echo "Remember to re-sign the application if you plan to distribute it."
